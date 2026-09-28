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
#[cfg(any(target_os = "macos", test))]
pub const ERRNO_ESRCH: i32 = 3;

/// `ENOENT` — "no such file or directory". On a path that TCC guards, this is
/// the answer you only get once the guard let you look: tccd decides before
/// the lookup reports the missing name, so a denial arrives as [`ERRNO_EPERM`]
/// and never as this. That asymmetry is what lets
/// [`Folder::absent_means_allowed`] read it as a permission answer for the one
/// item whose probe path is not guaranteed to exist.
pub const ERRNO_ENOENT: i32 = 2;

/// The user's TCC store, relative to `$HOME`. Reaching it requires Full Disk
/// Access; its contents are never read.
#[cfg(any(target_os = "macos", test))]
const TCC_DB_RELATIVE: &str = "Library/Application Support/com.apple.TCC/TCC.db";

/// Hard cap on an `Info.plist` read. A plist over the cap reads as absent —
/// unbounded work on a path we do not control is not acceptable in a probe.
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
/// Propagation of a changed grant was not measured for aterm's shape (design
/// §7 S1), so that evidence stays [`FdaScope::Unknown`]. A successful access
/// probe may separately establish [`FdaScope::ThisProcess`] for its observing
/// host; it says nothing about existing sessions or future process launches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FdaScope {
    /// The running process observes the grant.
    ThisProcess,
    /// Only a process started after the grant observes it.
    NewProcesses,
    /// Neither current observation nor measured propagation establishes scope.
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

/// The observation's progress or outcome. Kept separate from [`FdaState`] so
/// an unfinished observation cannot be mistaken for a completed denial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProbeLabel {
    /// No current result: an asynchronous observer is waiting or refreshing.
    /// This is not a denial or a claim that a syscall completed.
    Pending,
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
            Self::Pending => "pending",
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

    /// `true` for a deliberate refusal to consult the probe, including an
    /// unsupported platform. These labels guarantee no probe syscall ran.
    /// `false` does not guarantee completion: [`ProbeLabel::Pending`] may be
    /// queued or executing on a worker and is not a refusal.
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
    /// The observation's progress, refusal, or completed outcome.
    pub label: ProbeLabel,
}

impl FdaProbe {
    /// No current observation. This is neither a denial nor a refusal.
    #[must_use]
    pub const fn pending() -> Self {
        Self {
            state: FdaState::Unknown,
            label: ProbeLabel::Pending,
        }
    }

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
#[cfg(any(target_os = "macos", test))]
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

/// The answer on a platform that has no TCC: the whole implementation off
/// macOS, and compiled into every test build so it is tested everywhere.
#[cfg(any(test, not(target_os = "macos")))]
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
#[cfg(any(target_os = "macos", test))]
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
/// `unsupported_probe`.
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

// ---------------------------------------------------------------------------
// The image anchor — can macOS still FIND the code this process's grants are
// keyed to? (design §, the promoted 2026-09-10 OPEN item)
// ---------------------------------------------------------------------------

/// Whether the bundle macOS keys this process's consent decisions to can still
/// be found on disk.
///
/// A TCC grant is validated against the running code. If the bundle this process
/// runs from is renamed or deleted under it, `tccd` cannot build a code object
/// for it, and every consent decision for the process — and for every descendant
/// attributed to it — falls back to asking. The updater's apply does exactly that:
/// `RENAME_SWAP` moves the running bundle to `aterm.app.rollback`, and boot-health
/// confirmation later deletes it.
///
/// Classified from the KERNEL's path for the image (`proc_pidpath`), never from
/// `std::env::current_exe()`: on macOS that is the `execve`-time string, frozen
/// for the life of the process, and after the swap a new bundle occupies it, so
/// it reads healthy in exactly the broken state. Measured with the updater's
/// shape:
///
/// ```text
///              current_exe()                 proc_pidpath
/// before       …/A.app/Contents/MacOS/p      …/A.app/Contents/MacOS/p
/// after swap   …/A.app/Contents/MacOS/p      …/A.app.rollback/Contents/MacOS/p
/// after GC     …/A.app/Contents/MacOS/p      rc=0, errno ENOENT
/// ```
///
/// One syscall; it reads no protected location and raises no dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageAnchor {
    /// The image sits in a `.app` bundle. Grants keyed to it can be validated.
    Live,
    /// The image sits in a bundle layout (`…/Contents/MacOS/…`) whose directory
    /// is no longer named `<something>.app` — `aterm.app.rollback` after an apply.
    Displaced,
    /// The image no longer has a path: its bundle was deleted while this runs.
    Deleted,
    /// Not a bundle layout: a test binary, `targo run`, a dev build outside a
    /// `.app`, or a host with no bundles. Not a fault, and never reported as one.
    NotBundled,
    /// The kernel would not say (a sandbox denying process-info, say).
    Unknown,
}

impl ImageAnchor {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Displaced => "displaced",
            Self::Deleted => "deleted",
            Self::NotBundled => "not-bundled",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this anchor means a held grant cannot be validated against the
    /// running code. `NotBundled` and `Unknown` are NOT faults — the first is an
    /// ordinary dev run and the second is a failed read, and reporting either as
    /// a broken grant would be the same over-claim the design forbids
    /// everywhere else.
    #[must_use]
    pub const fn grant_unverifiable(self) -> bool {
        matches!(self, Self::Displaced | Self::Deleted)
    }
}

/// What the kernel says about this process's own executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelfImage {
    /// Its current path.
    At(PathBuf),
    /// It has no path: `proc_pidpath` failed with `ENOENT`, which for this
    /// process's own pid means the file was unlinked.
    NoPath,
    /// The lookup failed for another reason — `EPERM` under a sandbox that
    /// denies process-info, or a host with no such call. Says nothing about the
    /// bundle.
    Unreadable,
}

/// [`SelfImage`] for this process.
fn self_image() -> SelfImage {
    #[cfg(target_os = "macos")]
    {
        imp::self_image()
    }
    #[cfg(not(target_os = "macos"))]
    {
        SelfImage::Unreadable
    }
}

/// [`ImageAnchor`] for this process.
#[must_use]
pub fn image_anchor() -> ImageAnchor {
    if cfg!(not(target_os = "macos")) {
        return ImageAnchor::NotBundled;
    }
    classify_image_anchor(std::env::current_exe().ok().as_deref(), &self_image())
}

/// The classification, pure over the exec-time path and the kernel's answer.
///
/// The kernel's path decides whenever there is one. The exec path only tells a
/// deleted bundle from a binary that was never in one, since a deleted image has
/// no current path left to judge.
#[must_use]
pub fn classify_image_anchor(exec: Option<&Path>, image: &SelfImage) -> ImageAnchor {
    match image {
        SelfImage::At(path) if bundle_layout_root(path).is_none() => ImageAnchor::NotBundled,
        SelfImage::At(path) if app_bundle_root(path).is_some() => ImageAnchor::Live,
        SelfImage::At(_) => ImageAnchor::Displaced,
        SelfImage::NoPath => match exec {
            Some(exec) if bundle_layout_root(exec).is_some() => ImageAnchor::Deleted,
            Some(_) => ImageAnchor::NotBundled,
            None => ImageAnchor::Unknown,
        },
        SelfImage::Unreadable => ImageAnchor::Unknown,
    }
}

/// The directory enclosing `<dir>/Contents/MacOS/<exe>`, whatever `<dir>` is
/// named — `aterm.app` and `aterm.app.rollback` alike. [`app_bundle_root`] is the
/// narrower question, "is that directory still named `.app`".
#[must_use]
fn bundle_layout_root(exe: &Path) -> Option<PathBuf> {
    let components: Vec<Component<'_>> = exe.components().collect();
    let last_start = components.len().checked_sub(4)?;
    (0..=last_start).rev().find_map(|index| {
        (matches!(components[index], Component::Normal(_))
            && components[index + 1].as_os_str() == "Contents"
            && components[index + 2].as_os_str() == "MacOS")
            .then(|| components[..=index].iter().collect())
    })
}

/// `true` when the path cannot be handed to a C API because it contains an
/// interior NUL byte.
#[must_use]
#[cfg(any(target_os = "macos", test))]
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
#[cfg(any(target_os = "macos", test))]
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
#[cfg(any(target_os = "macos", test))]
pub fn display_name(exe: &Path, info_plist_text: Option<&str>) -> String {
    if let Some(text) = info_plist_text {
        for key in ["CFBundleDisplayName", "CFBundleName"] {
            if let Some(value) = plist_string(text, key) {
                return value.to_owned();
            }
        }
    }
    exe.file_name().map_or_else(
        || String::from("unknown"),
        |n| n.to_string_lossy().into_owned(),
    )
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
///
/// Rules 2 and 3 read only the text OUTSIDE quoted strings: an identifier, a
/// team or a hash is data the bundle's author chose, so `identifier
/// "com.cdhash.x"` on a Developer ID copy must not read as a cdhash pin — the
/// class the Trash offers to move. `codesign` prints a plain alphanumeric value
/// bare (`identifier mycdhash`), so rule 2 also wants the clause's shape: the
/// word `cdhash` on its own, followed by a hash constant (`H"…"`).
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
    let lower = without_quoted_strings(&lower);
    if has_cdhash_clause(&lower) {
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

/// Whether lowercased `text`, its quoted strings emptied, holds a `cdhash`
/// clause: the word on its own — not part of a bare value such as `mycdhash` —
/// followed by a hash constant.
fn has_cdhash_clause(text: &str) -> bool {
    text.match_indices("cdhash").any(|(at, word)| {
        let joined = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-');
        !text[..at].chars().next_back().is_some_and(joined)
            && text[at + word.len()..].trim_start().starts_with("h\"")
    })
}

/// `text` with the inside of every closed `"…"` string removed and its quotes
/// kept, so `identifier "x"` still reads as an identifier clause. A string
/// honours backslash escapes and does not cross a line; an unclosed quote is
/// kept as written.
fn without_quoted_strings(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let mut rest = line;
        while let Some(open) = rest.find('"') {
            let inner = &rest[open + 1..];
            let mut escaped = false;
            let close = inner.char_indices().find_map(|(at, c)| {
                let closes = c == '"' && !escaped;
                escaped = c == '\\' && !escaped;
                closes.then_some(at)
            });
            let Some(close) = close else {
                break;
            };
            out.push_str(&rest[..=open]);
            out.push('"');
            rest = &inner[close + 1..];
        }
        out.push_str(rest);
    }
    out
}

// ---------------------------------------------------------------------------
// Code identity of a bundle on disk
// ---------------------------------------------------------------------------
//
// Shared by the claimant census below and `aterm_gui::control_privacy`'s
// self-report, so the two cannot disagree about a bundle's requirement.

/// A bounded read of a text plist. A non-file, a file over
/// [`INFO_PLIST_MAX_BYTES`], or unreadable or non-UTF-8 text reads as absent.
#[must_use]
pub fn read_plist_text(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > INFO_PLIST_MAX_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// The string immediately following one exact XML plist key, non-empty and
/// trimmed. The value element must follow the key modulo whitespace, so a
/// malformed or intervening key's string can never be mis-bound. Only XML
/// plists are understood; a binary plist reads as absent.
#[must_use]
pub fn plist_string<'a>(text: &'a str, key: &str) -> Option<&'a str> {
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

/// A bundle's identity: its `CFBundleIdentifier` and its signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleIdentity {
    /// `CFBundleIdentifier` from the bundle's `Info.plist`.
    pub bundle_id: Option<String>,
    /// The `designated => …` clause.
    pub dr_text: String,
    /// How that requirement is shaped — `Unsigned` when `codesign` says the
    /// code is not signed at all, which leaves no clause to classify.
    pub dr: DrClass,
    /// `developer-id` | `adhoc` | `unsigned` | `unknown`.
    pub signing: &'static str,
    /// The Team ID, when there is a real one.
    pub team: Option<String>,
}

/// Read one bundle's identity: `Info.plist`, then `codesign`. `None` when that
/// could raise the very dialog this module exists to explain — the bundle, its
/// `Info.plist`, or anything `codesign` could open through it resolves under a
/// protected root — and whenever the bundle cannot be vouched for
/// (`bundle_reaches_protected` has the whole list): among others, one stray
/// link in its `Contents`, anything but `Contents` and plain files at its top
/// level, or an `Info.plist` naming a main executable by anything but a plain
/// file name. So one such shape in the RUNNING bundle leaves its identity unread,
/// and the census off.
#[must_use]
pub fn bundle_identity(root: &Path, protected: &[PathBuf]) -> Option<BundleIdentity> {
    let plist = root.join("Contents").join("Info.plist");
    if !census_may_read(root, protected)
        || !census_may_read(&plist, protected)
        || bundle_reaches_protected(root, protected)
    {
        return None;
    }
    let bundle_id = read_info_plist(&plist).and_then(|info| info.bundle_id);
    Some(signed_identity(root, bundle_id))
}

fn signed_identity(root: &Path, bundle_id: Option<String>) -> BundleIdentity {
    identity_from_report(codesign_report(root).as_deref(), bundle_id)
}

/// The identity a `codesign` report describes. Pure.
fn identity_from_report(report: Option<&str>, bundle_id: Option<String>) -> BundleIdentity {
    let dr_text = report.and_then(designated_requirement).unwrap_or_default();
    let signing = report.map_or("unknown", classify_signing);
    BundleIdentity {
        bundle_id,
        // Only the whole report can say "unsigned". A requirement clause that
        // merely contains the phrase (a quoted identifier) is signed code whose
        // class could not be read, never an unsigned copy the Trash may take.
        dr: match (signing, classify_dr(&dr_text)) {
            ("unsigned", _) => DrClass::Unsigned,
            (_, DrClass::Unsigned) => DrClass::Unknown,
            (_, class) => class,
        },
        dr_text,
        signing,
        team: report.and_then(team_identifier),
    }
}

/// `codesign -d -r- --verbose=2` over a bundle, stdout and stderr joined
/// (`codesign -d` writes its report to stderr), within [`CODESIGN_CEILING`].
/// `None` when it cannot run or does not finish.
#[cfg(target_os = "macos")]
fn codesign_report(root: &Path) -> Option<String> {
    let (_, out, err) = run_bounded(
        "/usr/bin/codesign",
        &[
            "-d".as_ref(),
            "-r-".as_ref(),
            "--verbose=2".as_ref(),
            root.as_os_str(),
        ],
        CODESIGN_CEILING,
    )?;
    Some(format!("{out}\n{err}"))
}

/// Run one of Apple's tools with stdin closed, within `ceiling` (for
/// `codesign`, [`CODESIGN_CEILING`]): whether it succeeded, then its stdout and
/// its stderr — each read on its own thread, so output larger than a pipe's
/// buffer cannot wedge the tool. `None` when it cannot run or does not finish,
/// and when either stream is over [`TOOL_OUTPUT_MAX_BYTES`] or is not UTF-8: a
/// report read in part is not read. Gated native-only first, as the wasm census
/// reads gates: its reader threads are never part of the wasm module.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(target_os = "macos")]
fn run_bounded(
    program: &str,
    args: &[&std::ffi::OsStr],
    ceiling: std::time::Duration,
) -> Option<(bool, String, String)> {
    use std::io::Read as _;
    use std::process::{Command, Stdio};
    fn drain(
        pipe: impl std::io::Read + Send + 'static,
    ) -> Option<std::thread::JoinHandle<Option<String>>> {
        // `Builder::spawn`, not `thread::spawn`: a thread the OS refuses is no
        // report, never a panic in the census or the Trash's worker.
        std::thread::Builder::new()
            .spawn(move || {
                let mut bytes = Vec::new();
                pipe.take(TOOL_OUTPUT_MAX_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .ok()?;
                let whole =
                    u64::try_from(bytes.len()).is_ok_and(|len| len <= TOOL_OUTPUT_MAX_BYTES);
                whole
                    .then_some(bytes)
                    .and_then(|bytes| String::from_utf8(bytes).ok())
            })
            .ok()
    }
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let (Some(out), Some(err)) = (
        child.stdout.take().and_then(drain),
        child.stderr.take().and_then(drain),
    ) else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    // wasm-clock-guard: allow — `run_bounded` is `#[cfg(target_os = "macos")]`
    // (it runs Apple's tools), so neither clock read below reaches wasm.
    let deadline = std::time::Instant::now() + ceiling;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            // wasm-clock-guard: allow — the same macOS-only function.
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    // The ceiling bounds the readers too, not only the exit (the product
    // fd-hygiene sweep of 2026-09-27). A reader ends at EOF, and EOF needs every
    // copy of its pipe's write end closed. A copy can outlive the tool: a
    // grandchild it left in the background, or a stranger another thread spawned
    // inside std's non-atomic `pipe()`-then-`FIOCLEX` on macOS — an unflagged
    // descriptor survives that child's exec, and an update successor lives for
    // the login session. Joined without a bound, that wedged the census, the
    // Trash's worker or the privacy verb for as long as the stranger lived. A
    // reader left unjoined is detached, and ends when the last copy closes.
    while !(out.is_finished() && err.is_finished()) {
        // wasm-clock-guard: allow — the same macOS-only function.
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let out = out.join().ok()??;
    let err = err.join().ok()??;
    Some((status.success(), out, err))
}

/// The most of one stream [`run_bounded`] reads: a bundle's reports are a few
/// kilobytes.
#[cfg(target_os = "macos")]
const TOOL_OUTPUT_MAX_BYTES: u64 = 1024 * 1024;

#[cfg(not(target_os = "macos"))]
fn codesign_report(_root: &Path) -> Option<String> {
    None
}

/// `codesign -d` on one bundle takes tens of milliseconds; a report that has
/// not arrived in this long is not coming.
#[cfg(target_os = "macos")]
const CODESIGN_CEILING: std::time::Duration = std::time::Duration::from_secs(10);

/// The `designated => …` clause of a `codesign -d -r-` report.
///
/// `codesign` prints an IMPLICIT requirement — every ad-hoc signature's — as a
/// comment, `# designated => cdhash H"…"`. The `#` is stripped so it
/// classifies as [`DrClass::Cdhash`], and two different ad-hoc bundles never
/// both read as the empty requirement.
fn designated_requirement(report: &str) -> Option<String> {
    report.lines().find_map(|line| {
        let line = line.trim();
        let clause = line.strip_prefix('#').map_or(line, str::trim_start);
        clause
            .starts_with("designated =>")
            .then(|| clause.to_owned())
    })
}

/// The Team ID, when the signature carries a real one. Apple prints
/// `TeamIdentifier=not set` for an ad-hoc signature, which is not an id.
fn team_identifier(report: &str) -> Option<String> {
    report.lines().find_map(|line| {
        let value = line.trim().strip_prefix("TeamIdentifier=")?.trim();
        (!value.is_empty() && value != "not set").then(|| value.to_owned())
    })
}

/// How the code is signed, from the same report. Fails toward the weaker
/// claim: anything unrecognised is `unknown`, never `developer-id`.
fn classify_signing(report: &str) -> &'static str {
    // `codesign -d` on unsigned code prints ONE line, `<path>: code object is
    // not signed at all`. Matching that whole shape — not the phrase anywhere —
    // keeps a folder name containing the phrase from passing a signed copy off
    // as unsigned, which the Trash would then offer to move.
    let mut lines = report.lines().filter(|line| !line.trim().is_empty());
    if let (Some(only), None) = (lines.next(), lines.next())
        && only
            .trim_end()
            .ends_with(": code object is not signed at all")
    {
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

// ---------------------------------------------------------------------------
// The claimant census
// ---------------------------------------------------------------------------
//
// macOS keeps ONE code requirement per bundle identifier. A copy that does not
// satisfy it is not merely denied when it asks: `tccd` answers `DB
// Action:Update, UpdateVerifierData` and REPLACES the stored requirement with
// the asker's, resetting the grant for every copy that shares the identifier.
// That write escapes process scope, so no per-process self-report can see it.
//
// The census reports what is ON DISK and whether those copies agree. It never
// reads `TCC.db` — not a supported status API — so it says nothing about what
// `tccd` currently holds.

/// At most this many claimant lines on any surface. Every surface also carries
/// the true totals, so truncation can never read as a smaller census.
pub const MAX_CLAIMANT_ROWS: usize = 8;

/// How much of the candidate space the census got to look at. "Nothing else
/// claims this id" and "I could not finish looking" are different answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Enumeration {
    /// Every source was read and every candidate classified.
    Complete,
    /// Something could not be read, so a claimant may be unseen.
    Partial,
    /// Nothing could be read at all.
    #[default]
    Unavailable,
}

impl Enumeration {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unavailable => "unavailable",
        }
    }
}

/// One bundle on disk that claims the identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimant {
    /// The bundle directory (`aterm.app`, `aterm.app.rollback`, …).
    pub path: PathBuf,
    /// How its designated requirement is shaped.
    pub dr: DrClass,
    /// The `designated => …` clause verbatim.
    pub dr_text: String,
    /// `developer-id` | `adhoc` | `unsigned` | `unknown`.
    pub signing: &'static str,
    /// The Team ID, when the signature carries a real one.
    pub team: Option<String>,
    /// Whether this is the bundle the calling process runs from.
    pub running: bool,
}

impl Claimant {
    /// Whether `tccd` would treat the two as different code. The comparison is
    /// the designated requirement itself, which is what it stores. An
    /// unreadable (empty) requirement conflicts: two unknowns are not one
    /// identity.
    fn conflicts_with(&self, other: &Self) -> bool {
        let (mine, theirs) = (self.dr_text.trim(), other.dr_text.trim());
        mine.is_empty() || theirs.is_empty() || mine != theirs
    }
}

/// Every bundle found claiming one identifier, and how complete the look was.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Claimants {
    /// Every claimant found, running copy first.
    pub found: Vec<Claimant>,
    /// How much of the candidate space was read.
    pub enumeration: Enumeration,
}

impl Claimants {
    /// The claimants whose requirement differs from the running copy's — the
    /// ones that can reset a grant. Empty when the running copy is unknown:
    /// with no reference there is nothing to differ from.
    #[must_use]
    pub fn conflicting(&self) -> Vec<&Claimant> {
        let Some(running) = self.found.iter().find(|c| c.running) else {
            return Vec::new();
        };
        self.found
            .iter()
            .filter(|c| !c.running && c.conflicts_with(running))
            .collect()
    }

    /// The conflicting copies that MAY be moved to the Trash: ad-hoc (signed ad
    /// hoc, with a cdhash requirement) or unsigned ones, beside a running copy
    /// whose grant survives updates. The panel offers the ones of these it
    /// lists ([`Self::offered_for_trash`]). A Developer ID copy is never one,
    /// whatever its requirement says. Anything else is listed and left alone —
    /// beside an unstable running copy the conflicting one may well be the real
    /// release, and a copy whose signature could not be read is not known to be
    /// either.
    #[must_use]
    pub fn retirable(&self) -> Vec<&Claimant> {
        let stable_running = self.found.iter().any(|c| c.running && c.dr.grant_stable());
        if !stable_running {
            return Vec::new();
        }
        self.conflicting()
            .into_iter()
            .filter(|c| {
                matches!(
                    (c.dr, c.signing),
                    (DrClass::Cdhash, "adhoc") | (DrClass::Unsigned, "unsigned")
                )
            })
            .collect()
    }

    /// The copies the Security panel lists AND offers to move to the Trash: the
    /// first [`MAX_CLAIMANT_ROWS`] conflicting copies that are
    /// [`Self::retirable`]. The panel's button and the `privacy` verb's note
    /// both read this one rule.
    #[must_use]
    pub fn offered_for_trash(&self) -> Vec<&Claimant> {
        let retirable = self.retirable();
        self.conflicting()
            .into_iter()
            .take(MAX_CLAIMANT_ROWS)
            .filter(|copy| retirable.contains(copy))
            .collect()
    }

    /// Whether the census may say nothing else claims the id: only after a
    /// COMPLETE look. A partial look that found nothing has not established
    /// absence.
    #[must_use]
    pub fn sole_claimant(&self) -> bool {
        self.enumeration == Enumeration::Complete && self.found.iter().all(|c| c.running)
    }
}

/// One listed directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// Its entries.
    Entries(Vec<PathBuf>),
    /// It does not exist.
    Missing,
    /// It exists and could not be read.
    Unreadable,
}

/// What one candidate turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Candidate {
    /// Not a bundle, gone, or another program's.
    NotOurs,
    /// A bundle that could not be read: a hole in the census, not an absence.
    Hole,
    /// A bundle claiming the identifier.
    Ours(BundleIdentity),
}

/// List one census root.
#[must_use]
pub fn list_root(dir: &Path) -> Listing {
    match std::fs::read_dir(dir) {
        Ok(read) => Listing::Entries(read.flatten().map(|e| e.path()).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Listing::Missing,
        Err(_) => Listing::Unreadable,
    }
}

/// Classify one candidate for `bundle_id`. The `Info.plist` is read first, and
/// `codesign` runs only for a bundle that claims the identifier.
#[must_use]
pub fn read_candidate(root: &Path, bundle_id: &str, protected: &[PathBuf]) -> Candidate {
    if !std::fs::metadata(root).is_ok_and(|m| m.is_dir()) {
        return Candidate::NotOurs;
    }
    let plist = root.join("Contents").join("Info.plist");
    // The plist too: a `Contents` link into a protected root is followed by
    // every read below, `codesign` included.
    if !census_may_read(root, protected) || !census_may_read(&plist, protected) {
        return Candidate::Hole;
    }
    match std::fs::symlink_metadata(&plist) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Candidate::NotOurs,
        Err(_) => return Candidate::Hole,
        Ok(meta) if !meta.is_file() => return Candidate::NotOurs,
        Ok(_) => {}
    }
    // Read as `CFBundle` reads it, so a binary plist — every App Store app's —
    // answers whose it is rather than leaving a hole in the census.
    let Some(info) = read_info_plist(&plist) else {
        return Candidate::Hole;
    };
    match info.bundle_id.as_deref() {
        Some(id) if id == bundle_id => {
            if bundle_reaches_protected(root, protected) {
                return Candidate::Hole;
            }
            Candidate::Ours(signed_identity(root, Some(id.to_owned())))
        }
        _ => Candidate::NotOurs,
    }
}

/// Whether the bundle at `claimant.path` is still the copy the census
/// classified: it still claims `bundle_id`, with the same identity
/// ([`same_identity`]). Runs `codesign`, so it belongs on a worker.
#[must_use]
pub fn still_claims(claimant: &Claimant, bundle_id: &str) -> bool {
    matches!(
        read_candidate(&claimant.path, bundle_id, &protected_roots(&[])),
        Candidate::Ours(id) if same_identity(&id, claimant)
    )
}

/// Whether a fresh read describes the copy the census classified: the same
/// requirement text, class and signing. An unsigned copy has no requirement
/// text, and neither does a read that failed, so the text alone cannot tell
/// them apart — the class and signing do — and a read that could not
/// establish a class is never the same copy.
fn same_identity(fresh: &BundleIdentity, claimant: &Claimant) -> bool {
    fresh.dr != DrClass::Unknown
        && fresh.dr == claimant.dr
        && fresh.signing == claimant.signing
        && fresh.dr_text == claimant.dr_text
}

/// [`list_root`], except that a folder under a protected root is never listed —
/// an app run from `~/Desktop` must not list the Desktop — and is a hole in the
/// look instead.
fn list_unless_protected(dir: &Path, protected: &[PathBuf]) -> Listing {
    if census_may_read(dir, protected) {
        list_root(dir)
    } else {
        Listing::Unreadable
    }
}

/// The most entries [`bundle_reaches_protected`] walks, and how deep: a real
/// bundle has a few dozen. A bundle larger or deeper is not vouched for.
const BUNDLE_WALK_MAX_ENTRIES: usize = 4096;
const BUNDLE_WALK_MAX_DEPTH: usize = 16;

/// Whether the bundle at `root` cannot be vouched for before `codesign` reads it.
///
/// It reaches only what is physically inside the bundle, on the bundle's own
/// volume — its top level (one listing) and its `Contents` (walked) — so it
/// never lists a place `codesign` would not open, and it refuses whatever
/// could lead `codesign` elsewhere:
///
/// * a `Contents` under a protected root, as spelled or resolved, or one that
///   resolves outside the bundle;
/// * a bundle that resolves to a folder the census would not take for one
///   ([`looks_like_bundle`]: `x.app -> ~`), that holds a protected root, or
///   whose path holds a control character (`codesign` prints it raw);
/// * at the top level, where `CFBundle` looks for a main executable it does
///   not find in `Contents/MacOS`, anything but `Contents` and plain files;
/// * in `Contents`, a link that does not resolve inside it — it follows none,
///   and a real bundle's only links are its own aliases (`atpkg -> aterm`) —
///   a folder on another volume (something mounted inside it), or a platform
///   `Info-*.plist`;
/// * an `Info.plist` that names a main executable other than a plain file
///   name, as Apple's own parser reads it ([`plist_names_plain_executables`]).
///
/// `true`, too, when it cannot finish (an unreadable folder, or past the
/// bounds).
fn bundle_reaches_protected(root: &Path, protected: &[PathBuf]) -> bool {
    let contents = root.join("Contents");
    if !census_may_read(&contents, protected) {
        return true;
    }
    let Ok(bundle) = std::fs::canonicalize(root) else {
        return true;
    };
    // `codesign`'s report prints the executable's path raw, a field a line: a
    // control character in it could forge a line the signing readers trust.
    let spelled_plainly = |path: &Path| !path.to_string_lossy().chars().any(char::is_control);
    if !spelled_plainly(root) || !spelled_plainly(&bundle) {
        return true;
    }
    let named_like_a_bundle = looks_like_bundle(&bundle);
    let holds_a_protected_root = protected.iter().any(|guarded| {
        guarded.starts_with(&bundle)
            || std::fs::canonicalize(guarded).is_ok_and(|guarded| guarded.starts_with(&bundle))
    });
    if !named_like_a_bundle || holds_a_protected_root {
        return true;
    }
    let Ok(inside) = std::fs::canonicalize(&contents) else {
        return true;
    };
    let Some(volume) = device_of(&bundle) else {
        return true;
    };
    if !inside.starts_with(&bundle) {
        return true;
    }
    let mut seen = 0usize;
    // CFBundle looks for a main executable it does not find in `Contents/MacOS`
    // at the bundle's top level, so nothing there may lead anywhere: only
    // `Contents` and plain files.
    let Ok(top) = std::fs::read_dir(&bundle) else {
        return true;
    };
    for entry in top {
        let Ok(entry) = entry else {
            return true;
        };
        seen += 1;
        if seen > BUNDLE_WALK_MAX_ENTRIES {
            return true;
        }
        if entry.file_name() != "Contents" && !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            return true;
        }
    }
    let mut stack = vec![(contents, 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if device_of(&dir) != Some(volume) {
            return true;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return true;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return true;
            };
            seen += 1;
            if seen > BUNDLE_WALK_MAX_ENTRIES {
                return true;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                return true;
            };
            // `CFBundle` reads a platform plist (`Info-macos.plist`) before
            // `Info.plist`, and may match either name in any case; only
            // `Info.plist` itself is read here, so no other.
            if depth == 0 {
                let name = entry.file_name();
                let lower = name.to_string_lossy().to_ascii_lowercase();
                if name != "Info.plist"
                    && lower.starts_with("info")
                    && Path::new(&lower)
                        .extension()
                        .is_some_and(|ext| ext == "plist")
                {
                    return true;
                }
            }
            if kind.is_symlink() {
                if !std::fs::canonicalize(&path).is_ok_and(|target| target.starts_with(&inside)) {
                    return true;
                }
            } else if kind.is_dir() {
                if depth + 1 > BUNDLE_WALK_MAX_DEPTH {
                    return true;
                }
                stack.push((path, depth + 1));
            }
        }
    }
    // `codesign` also opens the main executable the plist NAMES, with no link
    // involved: an absolute `CFBundleExecutable`, or one that climbs with
    // `..`, leaves `Contents` as surely as a link does. No plist names none.
    let plist = inside.join("Info.plist");
    match std::fs::symlink_metadata(&plist) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
        Ok(_) => !plist_names_plain_executables(&plist),
    }
}

/// What `CFBundle` reads from a bundle's `Info.plist`: its identifier — the
/// `CFBundleIdentifier-macos` variant where there is one, which `CFBundle`
/// applies over the plain key whatever its value, a value it cannot use
/// leaving none (measured: `-macosx`, `-ios` and `~mac` are not applied) —
/// and, on macOS, whether every key that names an executable names
/// a plain file ([`plain_file_name`]). Every such key is judged —
/// `CFBundleExecutable`, its `-macos` variant, the old `NSExecutable` — since
/// `CFBundle` may use any of them.
#[derive(Debug, PartialEq, Eq)]
struct InfoPlist {
    bundle_id: Option<String>,
    #[cfg(target_os = "macos")]
    plain_executables: bool,
}

/// Whether `name` is a plain file name: one path component with no control
/// character (which would forge a line in `codesign`'s report), so the
/// executable `codesign` opens is inside `Contents/MacOS`, or, not found
/// there, a plain file at the bundle's top level, the only kind the walk lets
/// stand there.
#[cfg(any(target_os = "macos", test))]
fn plain_file_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(_)), None)
    ) && !name.chars().any(char::is_control)
}

/// A bundle's `Info.plist` as `CFBundle` reads it, parsed in process by the
/// same CoreFoundation reader (`CFPropertyListCreateWithData`): binary plists
/// — every App Store app's — every encoding, entity and spelling it accepts,
/// and nothing it does not (a JSON file is no plist to it). `None` when the
/// file cannot be read, or is over [`INFO_PLIST_MAX_BYTES`] — a hole, since
/// `CFBundle` might read it; a file CoreFoundation cannot parse gives
/// `CFBundle` no identifier and no executable name, so it reads as naming
/// nothing.
#[cfg(target_os = "macos")]
fn read_info_plist(plist: &Path) -> Option<InfoPlist> {
    let meta = std::fs::metadata(plist).ok()?;
    if !meta.is_file() || meta.len() > INFO_PLIST_MAX_BYTES {
        return None;
    }
    let bytes = std::fs::read(plist).ok()?;
    Some(imp::parse_info_plist(&bytes).unwrap_or(InfoPlist {
        bundle_id: None,
        plain_executables: true,
    }))
}

/// Off macOS there is no `CFBundle` (and no `codesign`): a text plist's
/// identifier, read as it is, and no executable to vouch for.
#[cfg(not(target_os = "macos"))]
fn read_info_plist(plist: &Path) -> Option<InfoPlist> {
    let text = read_plist_text(plist)?;
    Some(InfoPlist {
        bundle_id: plist_string(&text, "CFBundleIdentifier").map(str::to_owned),
    })
}

/// Whether `plist`, read as `CFBundle` reads it ([`read_info_plist`]), names
/// only plain executables. A plist that cannot be read is not vouched for.
#[cfg(target_os = "macos")]
fn plist_names_plain_executables(plist: &Path) -> bool {
    read_info_plist(plist).is_some_and(|info| info.plain_executables)
}

/// Off macOS nothing runs `codesign`, so nothing follows the name.
#[cfg(not(target_os = "macos"))]
fn plist_names_plain_executables(_plist: &Path) -> bool {
    true
}

/// The volume `path` (followed) lives on, as its device number.
#[cfg(unix)]
fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).ok().map(|meta| meta.dev())
}

/// Off unix there is no device number to compare, and no TCC to ask.
#[cfg(not(unix))]
fn device_of(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|_| 0)
}

/// Whether the census may look inside `path`: not under a protected root as
/// spelled, nor once resolved — a link from `/Applications` into `~/Documents`
/// must not be followed into it. `canonicalize` is metadata-only and raises no
/// dialog; listing or reading under a protected root would.
fn census_may_read(path: &Path, protected: &[PathBuf]) -> bool {
    !is_under_protected_root(path, protected)
        && !std::fs::canonicalize(path)
            .is_ok_and(|resolved| is_under_protected_root(&resolved, protected))
}

/// The directories the census lists, with whether each must be readable.
///
/// aterm's co-claimants are its own outputs, siblings of an install: the
/// updater's `rollback_path` puts the displaced bundle beside the live one. So
/// the running bundle's parent is listed and REQUIRED — this process runs from
/// there, so an unreadable parent is a hole. The conventional app folders are
/// listed too, and a missing one is simply empty. `LaunchServices` supplies
/// every other registered copy.
fn claimant_search_roots(running: Option<&Path>, home: Option<&Path>) -> Vec<(PathBuf, bool)> {
    let mut roots: Vec<(PathBuf, bool)> = Vec::new();
    if let Some(parent) = running.and_then(Path::parent) {
        roots.push((parent.to_path_buf(), true));
    }
    let conventional = [
        Some(PathBuf::from("/Applications")),
        home.map(|h| h.join("Applications")),
    ];
    for dir in conventional.into_iter().flatten() {
        if !roots.iter().any(|(d, _)| *d == dir) {
            roots.push((dir, false));
        }
    }
    roots
}

/// Whether a directory entry is named like a bundle: `foo.app`, or
/// `foo.app.<suffix>` — `aterm.app.rollback` is a complete bundle carrying the
/// install's identifier. Hidden names are skipped.
fn looks_like_bundle(path: &Path) -> bool {
    let Some(name) = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
    else {
        return false;
    };
    let is_app = Path::new(&name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("app"));
    !name.starts_with('.')
        && (is_app
            || name
                .rsplit_once(".app.")
                .is_some_and(|(head, tail)| !head.is_empty() && !tail.is_empty()))
}

/// The census, pure over its sources.
///
/// `running` is the bundle this process runs from, `roots` the directories to
/// list (with whether each is required), `registered` the copies `LaunchServices`
/// knows (`None` when it could not be asked), `list` and `read` the two
/// filesystem reads. Each injected so a table test drives every arm with no
/// filesystem and no `codesign`.
#[must_use]
pub fn classify_claimants(
    running: Option<&Path>,
    roots: &[(PathBuf, bool)],
    registered: Option<&[PathBuf]>,
    list: impl Fn(&Path) -> Listing,
    read: impl Fn(&Path) -> Candidate,
) -> Claimants {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut holes = 0usize;
    let mut looked = 0usize;
    for (dir, required) in roots {
        match list(dir) {
            Listing::Entries(entries) => {
                looked += 1;
                candidates.extend(entries.into_iter().filter(|p| looks_like_bundle(p)));
            }
            Listing::Missing if !required => looked += 1,
            Listing::Missing | Listing::Unreadable => holes += 1,
        }
    }
    match registered {
        Some(paths) => {
            looked += 1;
            candidates.extend(paths.iter().filter(|p| !in_trash(p)).cloned());
        }
        None => holes += 1,
    }
    candidates.extend(running.map(Path::to_path_buf));

    let mut found: Vec<Claimant> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        if seen.contains(&candidate) {
            continue;
        }
        seen.push(candidate.clone());
        match read(&candidate) {
            Candidate::NotOurs => {}
            Candidate::Hole => holes += 1,
            Candidate::Ours(id) => found.push(Claimant {
                running: running == Some(candidate.as_path()),
                path: candidate,
                dr: id.dr,
                dr_text: id.dr_text,
                signing: id.signing,
                team: id.team,
            }),
        }
    }
    found.sort_by(|a, b| b.running.cmp(&a.running).then_with(|| a.path.cmp(&b.path)));
    let enumeration = if looked == 0 && found.is_empty() {
        Enumeration::Unavailable
    } else if holes > 0 || looked == 0 {
        Enumeration::Partial
    } else {
        Enumeration::Complete
    };
    Claimants { found, enumeration }
}

/// The census for one identifier, against this machine. Runs `codesign` on
/// each bundle that claims the identifier, so it belongs on a worker, never on
/// a draw path.
#[must_use]
pub fn claimants_for(bundle_id: &str) -> Claimants {
    let running = running_bundle();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let roots = claimant_search_roots(running.as_deref(), home.as_deref());
    let registered = registered_bundles(bundle_id);
    let protected = protected_roots(&[]);
    census_in(
        bundle_id,
        running.as_deref(),
        &roots,
        registered.as_deref(),
        &protected,
    )
}

/// [`claimants_for`] with its sources given: the real listing and candidate
/// reads, each behind the protected-root fence.
fn census_in(
    bundle_id: &str,
    running: Option<&Path>,
    roots: &[(PathBuf, bool)],
    registered: Option<&[PathBuf]>,
    protected: &[PathBuf],
) -> Claimants {
    classify_claimants(
        running,
        roots,
        registered,
        |dir| list_unless_protected(dir, protected),
        |root| read_candidate(root, bundle_id, protected),
    )
}

/// The bundle this process runs from, by the kernel's path — so after an
/// update's swap it is the `aterm.app.rollback` actually executing, not the
/// new bundle that took the name. Falls back to the exec path only when the
/// kernel will not say.
fn running_bundle() -> Option<PathBuf> {
    match self_image() {
        SelfImage::At(path) => bundle_layout_root(&path),
        SelfImage::NoPath => None,
        SelfImage::Unreadable => std::env::current_exe()
            .ok()
            .and_then(|exe| std::fs::canonicalize(exe).ok())
            .and_then(|exe| bundle_layout_root(&exe)),
    }
}

/// Every bundle `LaunchServices` has registered for `bundle_id` — every copy
/// that has launched, wherever it sits. `None` when it cannot be asked.
fn registered_bundles(bundle_id: &str) -> Option<Vec<PathBuf>> {
    #[cfg(target_os = "macos")]
    {
        imp::registered_bundles(bundle_id)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bundle_id;
        None
    }
}

/// Whether `path` sits in a Trash (`~/.Trash`, a volume's `.Trashes`).
/// `LaunchServices` keeps a copy's registration after it is moved there, the
/// Finder will not open an item in the Trash, and moving a copy there is how
/// the Security panel retires one — so the census does not count it.
fn in_trash(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(c, std::path::Component::Normal(name) if name == ".Trash" || name == ".Trashes")
    })
}

/// What the Security panel's *Move to Trash* did with one copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Retired {
    /// Moved to the Trash.
    Moved,
    /// Left in place: a fresh check no longer offers it — it moved, it was
    /// re-signed, its signature could not be read, or the copy running here
    /// changed.
    NotOffered,
    /// Left in place: a process runs from it, or that could not be ruled out.
    InUse,
    /// The move was refused, in the system's words.
    Failed(String),
}

/// Move to the Trash each copy in `plan` — the copies the owner was shown —
/// that a FRESH census still offers ([`Claimants::retirable`]), that is still
/// the same bundle right before the move (`unchanged`, see [`still_claims`]),
/// and that no process runs from. Nothing outside `plan` is touched, whatever
/// the fresh census finds. The three checks are injected so a table test
/// drives every arm.
pub fn retire_claimants(
    plan: &[PathBuf],
    fresh: &Claimants,
    unchanged: impl Fn(&Claimant) -> bool,
    in_use: impl Fn(&Path) -> Option<bool>,
    trash: impl Fn(&Path) -> Result<(), String>,
) -> Vec<(PathBuf, Retired)> {
    let offered = fresh.retirable();
    plan.iter()
        .map(|path| {
            let copy = offered.iter().find(|c| c.path == *path);
            let outcome = if !copy.is_some_and(|c| unchanged(c)) {
                Retired::NotOffered
            } else if in_use(path) != Some(false) {
                Retired::InUse
            } else {
                trash(path).map_or_else(Retired::Failed, |()| Retired::Moved)
            };
            (path.clone(), outcome)
        })
        .collect()
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
    /// Another application's own data — `kTCCServiceSystemPolicyAppData`,
    /// whose dialog reads "would like to access data from other apps."
    ///
    /// NOT a folder of the human's, which is why it is last in [`ALL`](Self::ALL)
    /// and carries its own [`label`](Self::label): the thing being asked about
    /// is every other app's container, and a terminal reaches them constantly —
    /// a script that reads another app's settings, a build that walks a cache.
    /// It joins this roster because it is promptable, resettable and
    /// warm-up-able exactly as the three folders are, so one gesture can ask
    /// for all four and one panel can report them side by side.
    ///
    /// Its probe path is one first-party container ([`path`](Self::path)) — a
    /// stable name that needs no directory listing to choose, because choosing
    /// one by listing would be a guarded access performed wherever the list was
    /// built. It may legitimately not exist, which is what
    /// [`absent_means_allowed`](Self::absent_means_allowed) is for.
    AppData,
}

impl Folder {
    /// Every folder this module knows, in the order the warm-up walks them.
    pub const ALL: &'static [Self] = &[
        Self::Documents,
        Self::Desktop,
        Self::Downloads,
        Self::AppData,
    ];

    /// The configuration / report name (`documents`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Documents => "documents",
            Self::Desktop => "desktop",
            Self::Downloads => "downloads",
            Self::AppData => "app-data",
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
            Self::AppData => "SystemPolicyAppData",
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
            // Three spellings for the one item whose name is not a single
            // word: a config file written by hand should not have to guess
            // which separator this roster chose.
            "app-data" | "app_data" | "appdata" => Some(Self::AppData),
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
            // ONE FIRST-PARTY CONTAINER, NAMED RATHER THAN DISCOVERED. Any
            // other app's container raises the same `kTCCServiceSystemPolicyAppData`
            // question, so the probe needs exactly one — and picking "whichever
            // one is there" would mean listing the container root to choose,
            // which is itself the guarded access, performed on whatever thread
            // built the list. A fixed name is a pure function of `home`, so the
            // only guarded access in the pass is the warm-up worker's own.
            Self::AppData => home
                .join("Library")
                .join("Containers")
                .join(APP_DATA_PROBE_BUNDLE),
        }
    }

    /// The display name, so every surface spells it the same way and none of
    /// them derives a human sentence from the config token.
    ///
    /// The three folders answer their own name; the fourth is not a folder and
    /// says what it is.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Documents => "Documents",
            Self::Desktop => "Desktop",
            Self::Downloads => "Downloads",
            Self::AppData => "Other applications' data",
        }
    }

    /// The noun a sentence about this item uses: "this folder" for the three,
    /// "this data" for the fourth. Denial copy is per-item for that reason.
    #[must_use]
    pub const fn noun(self) -> &'static str {
        match self {
            Self::Documents | Self::Desktop | Self::Downloads => "folder",
            Self::AppData => "data",
        }
    }

    /// Whether a missing probe path is a PERMISSION answer rather than a
    /// failure.
    ///
    /// The three folders are created by macOS at account setup and their
    /// absence is a broken home, not a verdict — [`ERRNO_ENOENT`] there means
    /// "could not be read". The app-data probe names one app's container, and
    /// an account that has never opened that app does not have it: there,
    /// `ENOENT` is proof the access was PERMITTED, because a TCC refusal
    /// answers [`ERRNO_EPERM`] before the lookup can report a missing name.
    #[must_use]
    pub const fn absent_means_allowed(self) -> bool {
        match self {
            Self::Documents | Self::Desktop | Self::Downloads => false,
            Self::AppData => true,
        }
    }
}

/// The bundle whose container stands for every other app's data.
///
/// Notes ships with macOS and is not removable, so the name resolves on any
/// account; whether the container EXISTS is irrelevant to the question being
/// asked (see [`Folder::absent_means_allowed`]). Nothing about the choice is
/// specific to Notes: TCC keys the grant to the SERVICE, so one container
/// answers for all of them.
const APP_DATA_PROBE_BUNDLE: &str = "com.apple.Notes";

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
#[cfg(any(unix, test))]
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
#[cfg(any(unix, test))]
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
    /// `unknown` and NEVER `denied`. Measuring it needs a real in-place
    /// `update apply` of an installed Developer-ID `aterm.app`, run by the owner
    /// (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §7 S2); only then flip it
    /// where `aterm-gui`'s `control_privacy.rs` passes [`Self::UNMEASURED`].
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

// ---------------------------------------------------------------------------
// macOS implementation — the only code here that touches the OS
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod imp {
    use super::{
        ProbeOutcome, ResponsibleApp, ResponsibleError, SelfImage, display_name, read_plist_text,
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
        match probe_descriptor(db) {
            Ok(fd) => {
                // Closed at once; the contents are never read.
                drop(fd);
                ProbeOutcome::Ok
            }
            Err(errno) => ProbeOutcome::Errno(errno),
        }
    }

    /// The probe's `open`, close-on-exec from its creation (the product
    /// fd-hygiene sweep of 2026-09-27). The GUI probes while other threads
    /// spawn, and a child spawned between this open and its close keeps every
    /// descriptor that is not flagged: an unflagged one survives the exec, so
    /// the child would hold a handle on `TCC.db` it never passed TCC for — and,
    /// flag still clear, hand it on to every child of its own. `O_CLOEXEC` is
    /// applied atomically by `open(2)`, so no spawn can see it unflagged.
    pub(super) fn probe_descriptor(db: &Path) -> Result<std::os::fd::OwnedFd, i32> {
        use std::os::fd::FromRawFd as _;
        let Ok(path) = CString::new(db.as_os_str().as_bytes()) else {
            // Unreachable: the caller already rejected interior NULs. Report
            // an errno rather than panicking in a probe.
            return Err(libc::EINVAL);
        };
        // SAFETY: `path` is a valid NUL-terminated C string that outlives the
        // call; `O_RDONLY | O_CLOEXEC` takes no third argument.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(errno());
        }
        // SAFETY: `fd` is a descriptor this call just created, owned by nothing
        // else; the `OwnedFd` closes it exactly once.
        Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
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
        let path = pid_path(responsible).ok()?;
        let plist = readable_info_plist(&path, protected).and_then(|p| read_plist_text(&p));
        let name = display_name(&path, plist.as_deref());
        Some(ResponsibleApp {
            pid: responsible,
            path,
            display_name: name,
        })
    }

    /// [`SelfImage`] from `proc_pidpath` on our own pid. `ENOENT` is the unlink
    /// signal; any other failure is [`SelfImage::Unreadable`].
    pub(super) fn self_image() -> SelfImage {
        let Ok(me) = i32::try_from(std::process::id()) else {
            return SelfImage::Unreadable;
        };
        match pid_path(me) {
            Ok(path) => SelfImage::At(path),
            Err(libc::ENOENT) => SelfImage::NoPath,
            Err(_) => SelfImage::Unreadable,
        }
    }

    /// `proc_pidpath` for one pid, or the errno it failed with.
    fn pid_path(pid: i32) -> Result<PathBuf, i32> {
        let mut buf = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
        // SAFETY: `__error()` returns a valid pointer to this thread's errno;
        // cleared so a stale value cannot be read back as this call's.
        unsafe {
            *libc::__error() = 0;
        }
        // SAFETY: `buf` is a live allocation of exactly the length passed, and
        // `proc_pidpath` writes at most that many bytes.
        let written = unsafe {
            proc_pidpath(
                pid,
                buf.as_mut_ptr().cast::<libc::c_void>(),
                u32::try_from(buf.len()).unwrap_or(u32::MAX),
            )
        };
        match usize::try_from(written) {
            Ok(n) if n > 0 && n <= buf.len() => Ok(PathBuf::from(OsStr::from_bytes(&buf[..n]))),
            _ => Err(errno()),
        }
    }

    /// Every bundle `LaunchServices` has registered for `bundle_id`, through the
    /// public `LSCopyApplicationURLsForBundleIdentifier`: a read of the
    /// `LaunchServices` database, which touches no protected path, raises no
    /// dialog and does not reach `WindowServer`. `kLSApplicationNotFoundErr` is
    /// an empty answer; any other failure is `None`. Registrations outlive
    /// their bundles, so callers must treat a path as a candidate, not a fact.
    pub(super) fn registered_bundles(bundle_id: &str) -> Option<Vec<PathBuf>> {
        type CfRef = *const libc::c_void;
        #[link(name = "CoreServices", kind = "framework")]
        unsafe extern "C" {
            fn LSCopyApplicationURLsForBundleIdentifier(id: CfRef, error: *mut CfRef) -> CfRef;
        }
        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            fn CFStringCreateWithBytes(
                alloc: CfRef,
                bytes: *const u8,
                num_bytes: isize,
                encoding: u32,
                is_external: u8,
            ) -> CfRef;
            fn CFArrayGetCount(array: CfRef) -> isize;
            fn CFArrayGetValueAtIndex(array: CfRef, index: isize) -> CfRef;
            fn CFURLGetFileSystemRepresentation(
                url: CfRef,
                resolve_against_base: u8,
                buffer: *mut u8,
                max_len: isize,
            ) -> u8;
            fn CFErrorGetCode(error: CfRef) -> isize;
            fn CFRelease(cf: CfRef);
        }
        const UTF8: u32 = 0x0800_0100;
        const APPLICATION_NOT_FOUND: isize = -10814;
        let len = isize::try_from(bundle_id.len()).ok()?;
        // SAFETY: `bundle_id` is a live UTF-8 buffer of exactly `len` bytes; a
        // null allocator is the documented default.
        let id =
            unsafe { CFStringCreateWithBytes(std::ptr::null(), bundle_id.as_ptr(), len, UTF8, 0) };
        if id.is_null() {
            return None;
        }
        let mut error: CfRef = std::ptr::null();
        // SAFETY: `id` is a valid CFString we own; `error` is a valid out slot.
        let urls = unsafe { LSCopyApplicationURLsForBundleIdentifier(id, &raw mut error) };
        // SAFETY: `id` was created above and is released exactly once.
        unsafe { CFRelease(id) };
        if urls.is_null() {
            if error.is_null() {
                return None;
            }
            // SAFETY: a non-null `error` is a CFError we own (Copy rule).
            let code = unsafe { CFErrorGetCode(error) };
            // SAFETY: released exactly once.
            unsafe { CFRelease(error) };
            return (code == APPLICATION_NOT_FOUND).then(Vec::new);
        }
        let mut out = Vec::new();
        let mut buf = vec![0u8; 4096];
        let max = isize::try_from(buf.len()).unwrap_or(isize::MAX);
        // SAFETY: `urls` is a valid CFArray of CFURLs we own; every index is in
        // range; `buf` holds `max` bytes and the call NUL-terminates within it.
        unsafe {
            for index in 0..CFArrayGetCount(urls) {
                let url = CFArrayGetValueAtIndex(urls, index);
                if CFURLGetFileSystemRepresentation(url, 1, buf.as_mut_ptr(), max) != 0
                    && let Some(end) = buf.iter().position(|&b| b == 0)
                {
                    out.push(PathBuf::from(OsStr::from_bytes(&buf[..end])));
                }
            }
            CFRelease(urls);
        }
        Some(out)
    }

    /// `bytes` — a bundle's `Info.plist`, read by the caller — parsed by the
    /// reader `CFBundle` uses, `CFPropertyListCreateWithData`: XML, binary or
    /// old-style text, never JSON. `None` when it is not a plist that reader
    /// accepts; a plist that is not a dictionary reads as naming nothing.
    ///
    /// Nothing is kept but the identifier and one verdict, and no string is
    /// converted past a bound: a binary plist can point thousands of keys at
    /// ONE large string, which `CoreFoundation` shares but a copy per key would
    /// multiply. A key longer than [`KEY_MAX_UNITS`] is none `CFBundle` reads;
    /// a value longer than [`VALUE_MAX_UNITS`] is no bundle identifier and no
    /// file name. A string that does not come back whole (an embedded NUL) is
    /// unread, so it can never compare equal to a name it is not.
    pub(super) fn parse_info_plist(bytes: &[u8]) -> Option<super::InfoPlist> {
        type CfRef = *const libc::c_void;
        #[link(name = "CoreFoundation", kind = "framework")]
        unsafe extern "C" {
            fn CFDataCreate(alloc: CfRef, bytes: *const u8, length: isize) -> CfRef;
            fn CFPropertyListCreateWithData(
                alloc: CfRef,
                data: CfRef,
                options: usize,
                format: *mut isize,
                error: *mut CfRef,
            ) -> CfRef;
            fn CFGetTypeID(cf: CfRef) -> usize;
            fn CFDictionaryGetTypeID() -> usize;
            fn CFStringGetTypeID() -> usize;
            fn CFDictionaryGetCount(dict: CfRef) -> isize;
            fn CFDictionaryGetKeysAndValues(dict: CfRef, keys: *mut CfRef, values: *mut CfRef);
            fn CFStringGetLength(string: CfRef) -> isize;
            fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
            fn CFStringGetCString(string: CfRef, buffer: *mut u8, size: isize, encoding: u32)
            -> u8;
            fn CFRelease(cf: CfRef);
        }
        const UTF8: u32 = 0x0800_0100;
        const IMMUTABLE: usize = 0;
        // A CFString of at most `max` UTF-16 units as Rust text, whole or not
        // at all; the length is checked before anything is allocated.
        let text = |value: CfRef, max: isize| -> Option<String> {
            // SAFETY: `value` is a live CF object owned by the dictionary the
            // caller holds; its type is checked before any string call.
            unsafe {
                if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
                    return None;
                }
                let units = CFStringGetLength(value);
                if units > max {
                    return None;
                }
                let size = CFStringGetMaximumSizeForEncoding(units, UTF8).checked_add(1)?;
                let mut buf = vec![0u8; usize::try_from(size).ok()?];
                if CFStringGetCString(value, buf.as_mut_ptr(), size, UTF8) == 0 {
                    return None;
                }
                let end = buf.iter().position(|&b| b == 0)?;
                buf.truncate(end);
                let string = String::from_utf8(buf).ok()?;
                let whole = isize::try_from(string.encode_utf16().count()).ok() == Some(units);
                whole.then_some(string)
            }
        };
        let len = isize::try_from(bytes.len()).ok()?;
        // SAFETY: `bytes` is a live buffer of exactly `len` bytes, copied by
        // `CFDataCreate`; a null allocator is the documented default.
        let data = unsafe { CFDataCreate(std::ptr::null(), bytes.as_ptr(), len) };
        if data.is_null() {
            return None;
        }
        // SAFETY: `data` is a valid CFData we own; null format and error slots
        // are documented as allowed.
        let plist = unsafe {
            CFPropertyListCreateWithData(
                std::ptr::null(),
                data,
                IMMUTABLE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        // SAFETY: `data` was created above and is released exactly once.
        unsafe { CFRelease(data) };
        if plist.is_null() {
            return None;
        }
        // The `-macos` key, when present, IS the identifier `CFBundle` reports —
        // even a value it cannot use (a number, a string with a NUL), which
        // leaves no usable identifier rather than falling back to the plain key.
        let mut plain_id = None;
        let mut macos_id: Option<Option<String>> = None;
        let mut plain_executables = true;
        // SAFETY: `plist` is a valid property list we own (Create rule); the
        // dictionary calls run only once its type is checked, the key and
        // value buffers hold exactly `count` slots, and `plist` is released
        // exactly once, after the last read of anything it owns.
        unsafe {
            if CFGetTypeID(plist) == CFDictionaryGetTypeID() {
                let count = usize::try_from(CFDictionaryGetCount(plist)).unwrap_or(0);
                let mut keys: Vec<CfRef> = vec![std::ptr::null(); count];
                let mut values: Vec<CfRef> = vec![std::ptr::null(); count];
                CFDictionaryGetKeysAndValues(plist, keys.as_mut_ptr(), values.as_mut_ptr());
                for (key, value) in keys.iter().zip(&values) {
                    let Some(key) = text(*key, KEY_MAX_UNITS) else {
                        continue;
                    };
                    match key.as_str() {
                        "CFBundleIdentifier" => plain_id = text(*value, VALUE_MAX_UNITS),
                        "CFBundleIdentifier-macos" => {
                            macos_id = Some(text(*value, VALUE_MAX_UNITS));
                        }
                        _ => {}
                    }
                    if plain_executables && key.to_ascii_lowercase().contains("executable") {
                        plain_executables = text(*value, VALUE_MAX_UNITS)
                            .is_some_and(|name| super::plain_file_name(&name));
                    }
                }
            }
            CFRelease(plist);
        }
        Some(super::InfoPlist {
            bundle_id: macos_id.unwrap_or(plain_id),
            plain_executables,
        })
    }

    /// The longest key [`parse_info_plist`] reads: `CFBundle`'s own keys, the
    /// platform variants among them, are a few dozen units.
    const KEY_MAX_UNITS: isize = 128;
    /// The longest value it reads: a bundle identifier, or a file name (at most
    /// 255 bytes on APFS).
    const VALUE_MAX_UNITS: isize = 1024;
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

    // -----------------------------------------------------------------
    // The claimant census
    // -----------------------------------------------------------------

    const DEV_ID: &str = "designated => identifier \"x\" and anchor apple generic and \
                          certificate leaf[subject.OU] = A66A9P66Z7";

    fn ours(dr: &str) -> Candidate {
        Candidate::Ours(BundleIdentity {
            bundle_id: Some("x".to_string()),
            dr_text: dr.to_string(),
            dr: classify_dr(dr),
            signing: if dr.contains("cdhash") {
                "adhoc"
            } else {
                "developer-id"
            },
            team: None,
        })
    }

    /// The signing readers, over recorded `codesign -d -r- --verbose=2`
    /// output. They fail toward the WEAKER claim: anything unrecognised is
    /// `unknown`, never `developer-id`.
    #[test]
    fn the_signing_readers_fail_toward_the_weaker_claim() {
        let devid = "Executable=/Applications/aterm.app/Contents/MacOS/aterm\n\
                     TeamIdentifier=A66A9P66Z7\n\
                     designated => identifier \"com.aterm.aterm\" and anchor apple generic\n";
        assert_eq!(classify_signing(devid), "developer-id");
        assert_eq!(team_identifier(devid).as_deref(), Some("A66A9P66Z7"));
        assert_eq!(
            classify_dr(&designated_requirement(devid).unwrap()),
            DrClass::Identity
        );

        // An ad-hoc signature's requirement is IMPLICIT, and codesign prints it
        // commented. It must still read, as cdhash, with the `#` stripped.
        let adhoc = "Signature=adhoc\n\
                     TeamIdentifier=not set\n\
                     # designated => cdhash H\"4249c4f5\"\n";
        assert_eq!(classify_signing(adhoc), "adhoc");
        assert_eq!(team_identifier(adhoc), None, "`not set` is not a team id");
        let clause = designated_requirement(adhoc).expect("the commented form reads");
        assert_eq!(clause, "designated => cdhash H\"4249c4f5\"");
        assert_eq!(classify_dr(&clause), DrClass::Cdhash);

        let unsigned = "/x: code object is not signed at all\n";
        assert_eq!(classify_signing(unsigned), "unsigned");
        assert_eq!(designated_requirement(unsigned), None);
        assert_eq!(classify_signing("something else entirely\n"), "unknown");
    }

    /// The one plist reader: XML only, exact key, and a key whose value is not
    /// the next element never binds a later string.
    #[test]
    fn the_plist_reader_is_exact_and_xml_only() {
        let xml = "<plist><dict>\
                   <key>CFBundleIdentifier</key><string>com.aterm.aterm.dev</string>\
                   <key>CFBundleName</key><string>aterm (dev)</string>\
                   </dict></plist>";
        assert_eq!(
            plist_string(xml, "CFBundleIdentifier"),
            Some("com.aterm.aterm.dev")
        );
        assert_eq!(plist_string(xml, "CFBundleName"), Some("aterm (dev)"));
        assert_eq!(plist_string(xml, "CFBundleDisplayName"), None);
        assert_eq!(
            plist_string("<key>A</key><key>B</key><string>v</string>", "A"),
            None
        );
        assert_eq!(plist_string("bplist00\u{0}\u{1}", "A"), None);
    }

    /// Two unknowns are not one identity: an unreadable requirement conflicts.
    #[test]
    fn an_unreadable_requirement_conflicts_rather_than_matching() {
        let mk = |dr: &str| Claimant {
            path: PathBuf::from("/x/aterm.app"),
            dr: classify_dr(dr),
            dr_text: dr.to_string(),
            signing: "adhoc",
            team: None,
            running: false,
        };
        assert!(!mk(DEV_ID).conflicts_with(&mk(DEV_ID)));
        assert!(
            mk("designated => cdhash H\"aa\"").conflicts_with(&mk("designated => cdhash H\"bb\""))
        );
        assert!(mk("").conflicts_with(&mk("")));
        assert!(mk("").conflicts_with(&mk(DEV_ID)));
    }

    /// THE CENSUS, as a table: a Developer-ID install running from `/Apps`,
    /// its `.rollback` sibling and a backup (both ad-hoc), a copy whose
    /// signature cannot be read, another program, and a copy only
    /// LaunchServices knows about. All five copies of `x` are found, the running
    /// one is the reference, and the four that differ conflict — of which only
    /// the two ad-hoc ones are retirable.
    #[test]
    fn the_census_finds_every_claimant_including_rollback_and_registered_copies() {
        let apps = PathBuf::from("/Apps");
        let running = apps.join("x.app");
        let far = PathBuf::from("/far/away/x.app");
        let roots = vec![
            (apps.clone(), true),
            (PathBuf::from("/Applications"), false),
        ];
        let list = |dir: &Path| {
            if dir == apps {
                Listing::Entries(vec![
                    apps.join("x.app"),
                    apps.join("x.app.rollback"),
                    apps.join("x-backup.app"),
                    apps.join("x-unread.app"),
                    apps.join("Other.app"),
                    apps.join("notes.txt"),
                ])
            } else {
                Listing::Missing
            }
        };
        let read = |root: &Path| match root.file_name().and_then(|n| n.to_str()) {
            Some("x.app") if root.starts_with("/Apps") => ours(DEV_ID),
            Some("x.app.rollback") => ours("designated => cdhash H\"aa\""),
            Some("x-backup.app") => ours("designated => cdhash H\"bb\""),
            Some("x-unread.app") => ours(""),
            Some("x.app") => ours("designated => identifier \"x\" and anchor apple generic"),
            _ => Candidate::NotOurs,
        };
        let trashed = PathBuf::from("/Users//u/.Trash/x.app");
        let registered = [far.clone(), apps.join("x.app"), trashed.clone()];
        let c = classify_claimants(Some(&running), &roots, Some(&registered), list, read);
        assert!(
            !c.found.iter().any(|x| x.path == trashed),
            "a copy in the Trash is not a claimant, though LaunchServices still lists it"
        );

        assert_eq!(
            c.enumeration,
            Enumeration::Complete,
            "a missing /Applications is empty"
        );
        assert_eq!(c.found.len(), 5, "{:?}", c.found);
        assert!(c.found[0].running && c.found[0].path == running);
        assert!(
            c.found
                .iter()
                .any(|x| x.path == apps.join("x.app.rollback"))
        );
        assert!(
            c.found.iter().any(|x| x.path == far),
            "LaunchServices' copy is found"
        );
        assert_eq!(c.conflicting().len(), 4);
        let retirable: Vec<_> = c.retirable().iter().map(|x| x.path.clone()).collect();
        assert_eq!(
            retirable,
            vec![apps.join("x-backup.app"), apps.join("x.app.rollback")],
            "an unreadable signature is listed as conflicting and never offered"
        );
        assert!(!c.sole_claimant());
    }

    /// Only a COMPLETE look may claim absence. An unreadable required root, an
    /// unreadable bundle, and LaunchServices not answering each downgrade the
    /// verdict rather than shrink the answer.
    #[test]
    fn every_hole_downgrades_the_census_and_a_complete_lone_install_is_sole() {
        let apps = PathBuf::from("/Apps");
        let running = apps.join("x.app");
        let lone = |dir: &Path| {
            if dir == apps {
                Listing::Entries(vec![apps.join("x.app")])
            } else {
                Listing::Missing
            }
        };
        let roots = vec![
            (apps.clone(), true),
            (PathBuf::from("/Applications"), false),
        ];
        let only = |root: &Path| {
            if root == running.as_path() {
                ours(DEV_ID)
            } else {
                Candidate::NotOurs
            }
        };

        let complete = classify_claimants(Some(&running), &roots, Some(&[]), lone, only);
        assert!(complete.sole_claimant() && complete.conflicting().is_empty());

        let no_ls = classify_claimants(Some(&running), &roots, None, lone, only);
        assert_eq!(
            no_ls.enumeration,
            Enumeration::Partial,
            "LaunchServices unasked"
        );
        assert!(!no_ls.sole_claimant());

        let required_missing = classify_claimants(
            Some(&running),
            &roots,
            Some(&[]),
            |_| Listing::Missing,
            only,
        );
        assert_eq!(
            required_missing.enumeration,
            Enumeration::Partial,
            "the running bundle's own parent is required"
        );
        assert_eq!(
            required_missing.found.len(),
            1,
            "the running copy is still counted"
        );

        let unreadable_bundle = classify_claimants(
            Some(&running),
            &roots,
            Some(&[apps.join("y.app")]),
            lone,
            |root: &Path| {
                if root == running.as_path() {
                    ours(DEV_ID)
                } else {
                    Candidate::Hole
                }
            },
        );
        assert_eq!(unreadable_bundle.enumeration, Enumeration::Partial);

        let blind = classify_claimants(None, &[], None, lone, |_| Candidate::NotOurs);
        assert_eq!(blind.enumeration, Enumeration::Unavailable);
        assert!(
            blind.conflicting().is_empty(),
            "no reference identity, no claim"
        );
    }

    /// Beside an UNSTABLE running copy, a conflicting Developer-ID copy may
    /// well be the real release, so nothing is retirable.
    #[test]
    fn nothing_is_retirable_beside_an_unstable_running_copy() {
        let running = PathBuf::from("/A/x.app.rollback");
        let release = PathBuf::from("/A/x.app");
        let c = classify_claimants(
            Some(&running),
            &[(PathBuf::from("/A"), true)],
            Some(&[]),
            |_| Listing::Entries(vec![running.clone(), release.clone()]),
            |root: &Path| {
                if root == release.as_path() {
                    ours(DEV_ID)
                } else {
                    ours("designated => cdhash H\"aa\"")
                }
            },
        );
        assert_eq!(c.conflicting().len(), 1);
        assert!(c.retirable().is_empty());
    }

    /// A Developer ID copy whose requirement pins a cdhash is unstable and
    /// conflicts, but it is not the ad-hoc or unsigned copy the Trash offers to
    /// move: it is listed and left alone, beside an ad-hoc copy that is offered.
    #[test]
    fn a_developer_id_copy_is_never_retirable() {
        let running = PathBuf::from("/A/x.app");
        let pinned = PathBuf::from("/A/x-pinned.app");
        let adhoc = PathBuf::from("/A/x-adhoc.app");
        let c = classify_claimants(
            Some(&running),
            &[(PathBuf::from("/A"), true)],
            Some(&[]),
            |_| Listing::Entries(vec![running.clone(), pinned.clone(), adhoc.clone()]),
            |root: &Path| {
                if root == running.as_path() {
                    ours(DEV_ID)
                } else if root == pinned.as_path() {
                    Candidate::Ours(BundleIdentity {
                        bundle_id: Some("x".to_string()),
                        dr_text: "cdhash H\"aa\"".to_string(),
                        dr: DrClass::Cdhash,
                        signing: "developer-id",
                        team: Some("T".to_string()),
                    })
                } else {
                    ours("designated => cdhash H\"bb\"")
                }
            },
        );
        assert_eq!(c.conflicting().len(), 2);
        let retirable: Vec<&Path> = c.retirable().iter().map(|c| c.path.as_path()).collect();
        assert_eq!(retirable, vec![adhoc.as_path()]);
    }

    /// THE TRASH GESTURE'S CORE: a copy is moved only if the owner was shown
    /// it, a fresh census still offers it, it is still the same bundle right
    /// before the move, and nothing runs from it. Every other
    /// copy stays where it is, with its reason, and nothing outside the plan is
    /// touched.
    #[test]
    fn only_a_shown_still_offered_idle_copy_is_moved_to_the_trash() {
        let dir = PathBuf::from("/A");
        let running = dir.join("x.app");
        let [rollback, busy, unsure, refused, unshown, resigned] = [
            "x.app.rollback",
            "x-busy.app",
            "x-unsure.app",
            "x-refused.app",
            "x-unshown.app",
            "x-resigned.app",
        ]
        .map(|name| dir.join(name));
        let gone = dir.join("x-gone.app");
        let entries = vec![
            running.clone(),
            rollback.clone(),
            busy.clone(),
            unsure.clone(),
            refused.clone(),
            unshown.clone(),
            resigned.clone(),
        ];
        let fresh = classify_claimants(
            Some(&running),
            &[(dir.clone(), true)],
            Some(&[]),
            |_| Listing::Entries(entries.clone()),
            |root: &Path| {
                if root == running.as_path() {
                    ours(DEV_ID)
                } else {
                    ours("designated => cdhash H\"aa\"")
                }
            },
        );
        let trashed = std::cell::RefCell::new(Vec::new());
        let plan = [
            &rollback, &busy, &unsure, &refused, &resigned, &gone, &running,
        ]
        .map(|p| (*p).clone());
        let out = retire_claimants(
            &plan,
            &fresh,
            |c| c.path != resigned,
            |p| {
                if p == busy {
                    Some(true)
                } else if p == unsure {
                    None
                } else {
                    Some(false)
                }
            },
            |p| {
                trashed.borrow_mut().push(p.to_path_buf());
                if p == refused {
                    Err("denied".to_string())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(
            out,
            vec![
                (rollback.clone(), Retired::Moved),
                (busy, Retired::InUse),
                (unsure, Retired::InUse),
                (refused.clone(), Retired::Failed("denied".to_string())),
                (resigned, Retired::NotOffered),
                (gone, Retired::NotOffered),
                (running, Retired::NotOffered),
            ]
        );
        assert_eq!(*trashed.borrow(), vec![rollback, refused]);
        assert!(
            !trashed.borrow().contains(&unshown),
            "never outside the plan"
        );
    }

    /// The census never looks inside a protected root — through a link to the
    /// bundle, through a `Contents` link, through a plist link, or through any
    /// link out of the bundle `codesign` would follow — and never lists a
    /// protected folder. Here every
    /// path is already resolved, so the resolved arm is what refuses; the
    /// spelled arm is pinned on its own by
    /// `the_spelled_protected_arm_refuses_on_its_own`.
    #[cfg(unix)]
    #[test]
    fn the_census_does_not_look_inside_a_protected_root() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("aterm-census-protected-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let documents = dir.join("Documents");
        let plist = "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>";
        let hidden = documents.join("x.app");
        std::fs::create_dir_all(hidden.join("Contents")).unwrap();
        std::fs::write(hidden.join("Contents/Info.plist"), plist).unwrap();
        let apps = dir.join("Applications");
        std::fs::create_dir_all(&apps).unwrap();
        let protected = vec![documents.clone()];

        // As spelled: the path is under the root with no link involved.
        assert!(!census_may_read(&hidden, &protected));
        assert_eq!(
            list_unless_protected(&documents, &protected),
            Listing::Unreadable
        );
        assert!(census_may_read(&apps, &protected));
        assert!(matches!(
            list_unless_protected(&apps, &protected),
            Listing::Entries(_)
        ));

        // Resolved: the bundle is a link into the root.
        let link = apps.join("x.app");
        std::os::unix::fs::symlink(&hidden, &link).unwrap();
        assert!(is_under_protected_root(&hidden, &protected));
        assert!(
            !is_under_protected_root(&link, &protected),
            "not as spelled"
        );
        assert!(!census_may_read(&link, &protected), "but once resolved");
        assert_eq!(read_candidate(&link, "x", &protected), Candidate::Hole);

        // A real bundle directory whose `Contents` is a link into the root.
        let shell = apps.join("y.app");
        std::fs::create_dir_all(&shell).unwrap();
        std::os::unix::fs::symlink(hidden.join("Contents"), shell.join("Contents")).unwrap();
        assert!(
            census_may_read(&shell, &protected),
            "the bundle itself is outside"
        );
        assert_eq!(read_candidate(&shell, "x", &protected), Candidate::Hole);
        assert_eq!(bundle_identity(&shell, &protected), None);

        // A real bundle whose executable is a link into the root.
        let exe_link = apps.join("z.app");
        std::fs::create_dir_all(exe_link.join("Contents/MacOS")).unwrap();
        std::fs::write(
            exe_link.join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>x</string>\
             <key>CFBundleExecutable</key><string>z</string></dict></plist>",
        )
        .unwrap();
        std::fs::write(documents.join("z"), b"").unwrap();
        std::os::unix::fs::symlink(documents.join("z"), exe_link.join("Contents/MacOS/z")).unwrap();
        assert_eq!(
            read_candidate(&exe_link, "x", &protected),
            Candidate::Hole,
            "codesign would open the executable under the root"
        );
        assert_eq!(bundle_identity(&exe_link, &protected), None);

        // A file inside `_CodeSignature` that links into the root: whatever
        // `codesign` could open is checked, not a list of paths.
        let sig_link = apps.join("s.app");
        std::fs::create_dir_all(sig_link.join("Contents/_CodeSignature")).unwrap();
        std::fs::write(
            sig_link.join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
        )
        .unwrap();
        let resources = sig_link.join("Contents/_CodeSignature/CodeResources");
        std::os::unix::fs::symlink(documents.join("z"), &resources).unwrap();
        assert_eq!(read_candidate(&sig_link, "x", &protected), Candidate::Hole);
        assert_eq!(bundle_identity(&sig_link, &protected), None);
        std::fs::remove_file(&resources).unwrap();
        std::fs::write(&resources, b"").unwrap();
        assert!(
            !bundle_reaches_protected(&sig_link, &protected),
            "the control: the same bundle without the link is vouched for"
        );

        // A link out of the bundle — here to a folder outside the root that
        // holds a second link into it — is unvouched, and never followed.
        let elsewhere = dir.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(documents.join("z"), elsewhere.join("CodeResources")).unwrap();
        let hop = apps.join("h.app");
        std::fs::create_dir_all(hop.join("Contents")).unwrap();
        std::fs::write(
            hop.join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
        )
        .unwrap();
        std::os::unix::fs::symlink(&elsewhere, hop.join("Contents/_CodeSignature")).unwrap();
        assert_eq!(
            read_candidate(&hop, "x", &protected),
            Candidate::Hole,
            "a link out of the bundle"
        );
        assert_eq!(bundle_identity(&hop, &protected), None);
        // A link to a folder ABOVE the root (here the one that holds Documents):
        // out of the bundle, so unvouched without listing what is there.
        let above = apps.join("a.app");
        std::fs::create_dir_all(above.join("Contents")).unwrap();
        std::fs::write(
            above.join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
        )
        .unwrap();
        std::os::unix::fs::symlink(&dir, above.join("Contents/Resources")).unwrap();
        assert!(bundle_reaches_protected(&above, &protected));
        assert_eq!(read_candidate(&above, "x", &protected), Candidate::Hole);
        // A `Contents` that is itself a link out of the bundle, to the folder
        // above the root: unvouched before anything is listed.
        let out = apps.join("u.app");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(
            dir.join("Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
        )
        .unwrap();
        std::os::unix::fs::symlink(&dir, out.join("Contents")).unwrap();
        assert!(bundle_reaches_protected(&out, &protected));
        assert_eq!(read_candidate(&out, "x", &protected), Candidate::Hole);
        assert_eq!(bundle_identity(&out, &protected), None);
        // The same, to a folder that holds nothing protected and no link at
        // all: only the rule that `Contents` stays inside the bundle refuses
        // it, since a walk of what it names would find nothing.
        let away = dir.join("away");
        std::fs::create_dir_all(&away).unwrap();
        std::fs::write(
            away.join("Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
        )
        .unwrap();
        let w = apps.join("w.app");
        std::fs::create_dir_all(&w).unwrap();
        std::os::unix::fs::symlink(&away, w.join("Contents")).unwrap();
        assert!(bundle_reaches_protected(&w, &protected));
        assert_eq!(read_candidate(&w, "x", &protected), Candidate::Hole);
        assert_eq!(bundle_identity(&w, &protected), None);
        // A plist that is itself a link into the root is never read: an id
        // that is not ours would otherwise have answered NotOurs.
        let other = apps.join("o.app");
        std::fs::create_dir_all(other.join("Contents")).unwrap();
        std::fs::write(
            documents.join("other.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>other</string></dict></plist>",
        )
        .unwrap();
        std::os::unix::fs::symlink(
            documents.join("other.plist"),
            other.join("Contents/Info.plist"),
        )
        .unwrap();
        assert_eq!(read_candidate(&other, "x", &protected), Candidate::Hole);
        // A `Contents` that is itself a link into the root, with no plist to
        // stop the read earlier: the walk refuses before listing it.
        let bare = apps.join("b.app");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(documents.join("no-plist")).unwrap();
        std::os::unix::fs::symlink(documents.join("no-plist"), bare.join("Contents")).unwrap();
        assert!(bundle_reaches_protected(&bare, &protected));
        assert_eq!(bundle_identity(&bare, &protected), None);
        // A link inside the bundle (its own alias) is fine and never followed,
        // so even one that loops ends nothing but itself.
        let cyc = apps.join("c.app");
        std::fs::create_dir_all(cyc.join("Contents/MacOS")).unwrap();
        std::fs::write(cyc.join("Contents/MacOS/aterm"), b"").unwrap();
        std::os::unix::fs::symlink("aterm", cyc.join("Contents/MacOS/atpkg")).unwrap();
        std::os::unix::fs::symlink(cyc.join("Contents"), cyc.join("Contents/loop")).unwrap();
        assert!(!bundle_reaches_protected(&cyc, &protected));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The walk reaches only what is physically inside a bundle-named folder —
    /// its top level and its own `Contents` — so it cannot list a place
    /// `codesign` would not open. Each refusal has a control that differs from
    /// it in that one respect and is vouched for.
    #[cfg(unix)]
    #[test]
    fn the_walk_stays_inside_the_bundles_own_contents() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("aterm-census-reach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let apps = dir.join("Applications");
        std::fs::create_dir_all(&apps).unwrap();
        let lay = |at: &Path| {
            std::fs::create_dir_all(at.join("Contents/MacOS")).unwrap();
            std::fs::write(
                at.join("Contents/Info.plist"),
                "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
            )
            .unwrap();
            std::fs::write(at.join("Contents/MacOS/x"), b"").unwrap();
        };

        // A bundle that resolves to a folder not named like one (`x.app -> ~`):
        // what is inside that folder is not a bundle's.
        let home = dir.join("home");
        lay(&home);
        let r = apps.join("r.app");
        std::os::unix::fs::symlink(&home, &r).unwrap();
        assert!(bundle_reaches_protected(&r, &[]));
        let named = dir.join("home.app");
        lay(&named);
        let r2 = apps.join("r2.app");
        std::os::unix::fs::symlink(&named, &r2).unwrap();
        assert!(
            !bundle_reaches_protected(&r2, &[]),
            "the control: the same link, to a folder named like a bundle"
        );
        // The updater's displaced copy is named like one too.
        let rollback = dir.join("home.app.rollback");
        lay(&rollback);
        let r3 = apps.join("r3.app");
        std::os::unix::fs::symlink(&rollback, &r3).unwrap();
        assert!(!bundle_reaches_protected(&r3, &[]), "`.app.rollback`");

        // A bundle that holds a protected root is refused before anything in it
        // is listed.
        let p = apps.join("p.app");
        lay(&p);
        std::fs::create_dir_all(p.join("Contents/Private")).unwrap();
        std::fs::write(p.join("Contents/Private/secret.txt"), b"").unwrap();
        assert!(bundle_reaches_protected(&p, &[p.join("Contents/Private")]));
        assert!(
            !bundle_reaches_protected(&p, &[]),
            "the control: nothing protected inside it"
        );

        // A link that stays inside the bundle but leaves `Contents` is refused
        // too: the walk vouches for `Contents`, and a real bundle's only links
        // are its own aliases there.
        let t = apps.join("t.app");
        std::fs::create_dir_all(t.join("Contents")).unwrap();
        std::fs::write(
            t.join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
        )
        .unwrap();
        std::os::unix::fs::symlink("..", t.join("Contents/MacOS")).unwrap();
        assert!(bundle_reaches_protected(&t, &[]));
        let t2 = apps.join("t2.app");
        lay(&t2);
        std::os::unix::fs::symlink("MacOS", t2.join("Contents/Alias")).unwrap();
        assert!(
            !bundle_reaches_protected(&t2, &[]),
            "the control: a link that stays inside Contents"
        );

        // `CFBundle` reads a platform plist before `Info.plist`, and may match
        // either name in any case: only `Info.plist` itself is let stand.
        let platform = apps.join("i.app");
        lay(&platform);
        std::fs::write(platform.join("Contents/Info-macos.plist"), b"").unwrap();
        assert!(bundle_reaches_protected(&platform, &[]));
        let lower = apps.join("i2.app");
        lay(&lower);
        std::fs::remove_file(lower.join("Contents/Info.plist")).unwrap();
        std::fs::write(
            lower.join("Contents/info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
        )
        .unwrap();
        assert!(bundle_reaches_protected(&lower, &[]));
        // `codesign` prints the path raw, a field a line: a newline in it
        // could forge a line the signing readers trust.
        let forged = apps.join("c\nSignature=adhoc\n.app");
        lay(&forged);
        assert!(bundle_reaches_protected(&forged, &[]));
        // A main executable CFBundle does not find in `Contents/MacOS` is looked
        // for at the bundle's top level, so only `Contents` and plain files may
        // be there: a link there, or a folder, is refused.
        let top_link = apps.join("l.app");
        lay(&top_link);
        std::os::unix::fs::symlink(dir.join("home/Contents/MacOS/x"), top_link.join("x")).unwrap();
        assert!(bundle_reaches_protected(&top_link, &[]));
        let top_dir = apps.join("f.app");
        lay(&top_dir);
        std::fs::create_dir_all(top_dir.join("Stuff")).unwrap();
        assert!(bundle_reaches_protected(&top_dir, &[]));
        let top_file = apps.join("g.app");
        lay(&top_file);
        std::fs::write(top_file.join("Icon\r"), b"").unwrap();
        assert!(
            !bundle_reaches_protected(&top_file, &[]),
            "the control: a plain file at the top level"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The main executable is found by NAME, with no link: a name that is a
    /// path takes `codesign` wherever it points. The plist is parsed by the
    /// reader `CFBundle` uses, so every spelling it accepts — an entity, a CDATA
    /// key, a tag with a space, another encoding, the old text format, binary —
    /// is read as `codesign` reads it. A plist it cannot parse gives `CFBundle`
    /// no executable name at all, so it names nothing; a key inside a comment
    /// is no key.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_walk_reads_the_plist_as_codesign_does() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("aterm-census-plist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let refused = |app: &str, plist: &str| {
            let at = dir.join(app);
            std::fs::create_dir_all(at.join("Contents/MacOS")).unwrap();
            std::fs::write(at.join("Contents/MacOS/x"), b"").unwrap();
            std::fs::write(at.join("Contents/Info.plist"), plist).unwrap();
            bundle_reaches_protected(&at, &[])
        };
        let dict = |body: &str| {
            format!(
                "<plist><dict><key>CFBundleIdentifier</key><string>x</string>{body}</dict></plist>"
            )
        };
        let exe = |value: &str| format!("<key>CFBundleExecutable</key><string>{value}</string>");
        let abs = "<string>/Users//u/Documents/z</string>";
        assert!(refused("e1.app", &dict(&exe("/Users//u/Documents/z"))));
        assert!(refused("e2.app", &dict(&exe("../../../Documents/z"))));
        assert!(refused("e3.app", &dict(&exe("MacOS/x"))));
        assert!(refused("e4.app", &dict(&exe(".."))));
        assert!(refused("e5.app", &dict(&exe(""))));
        assert!(refused(
            "e6.app",
            &dict(&exe("&#47;Users&#47;u&#47;Documents&#47;z"))
        ));
        assert!(refused(
            "e7.app",
            &dict(&format!("<key><![CDATA[CFBundleExecutable]]></key>{abs}"))
        ));
        assert!(refused(
            "e9.app",
            &dict("<key>CFBundleExecutable</key><array/>")
        ));
        assert!(refused(
            "e11.app",
            &dict(&format!("<key >CFBundleExecutable</key>{abs}"))
        ));
        assert!(refused(
            "e12.app",
            &dict(&format!("<key>CFBundleExecutable</key >{abs}"))
        ));
        assert!(refused(
            "e13.app",
            &dict(&format!(
                "{}<key>CFBundleExecutable-macos</key>{abs}",
                exe("x")
            ))
        ));
        assert!(refused(
            "e15.app",
            &dict(&format!("<key>NSExecutable</key>{abs}"))
        ));
        assert!(refused("e17.app", &dict(&exe("x\nSignature=adhoc"))));
        // No file name is two thousand units long: past the bound it is not
        // read at all, and so not vouched for.
        assert!(refused("e18.app", &dict(&exe(&"a".repeat(2000)))));
        // A key hundreds of units long is none `CFBundle` reads, whatever it
        // says: its value is not judged (nor copied).
        assert!(!refused(
            "e19.app",
            &dict(&format!("<key>{}Executable</key>{abs}", "x".repeat(200)))
        ));
        // UTF-7 spells the key in bytes with no `<key` in them — declared
        // plainly, or behind a first `encoding` that says UTF-8.
        let utf7 = "+ADw-key+AD4-CFBundleExecutable+ADw-/key+AD4-\
                    +ADw-string+AD4-/Users//u/Documents/z+ADw-/string+AD4-";
        for (app, declaration) in [
            ("d1.app", "<?xml version=\"1.0\" encoding=\"UTF-7\"?>"),
            (
                "d3.app",
                "<?xml version=\"1.0\" encoding =\"UTF-8\" encoding=\"UTF-7\"?>",
            ),
        ] {
            assert!(
                refused(app, &format!("{declaration}{}", dict(utf7))),
                "{app}"
            );
        }
        assert!(refused(
            "n.app",
            "{ CFBundleIdentifier = x; CFBundleExecutable = \"/Users//u/Documents/z\"; }"
        ));
        // A plist CoreFoundation cannot parse — markup inside a key, a
        // `<string>` where a key goes, JSON — gives `CFBundle` no executable
        // name at all: it falls back to the bundle's own, which the walk has
        // vouched for. It names nothing, so it is not refused for its name.
        for (app, plist) in [
            (
                "e14.app",
                dict(&format!("<key>CFBundleExec<!-- -->utable</key>{abs}")),
            ),
            (
                "e16.app",
                dict(&format!("<string>CFBundleExecutable</string>{abs}")),
            ),
            (
                "j.app",
                "{\"CFBundleExecutable\": \"/Users//u/Documents/z\"}".to_string(),
            ),
        ] {
            assert!(!refused(app, &plist), "{app}");
        }
        // The controls: a plain name; a key inside a comment, which `CFBundle`
        // never reads; a plain name declared UTF-8.
        assert!(!refused("e10.app", &dict(&exe("x"))), "a plain name");
        assert!(
            !refused(
                "e8.app",
                &dict(&format!(
                    "<!-- {} -->{}",
                    exe("/Users//u/Documents/z"),
                    exe("x")
                ))
            ),
            "a key inside a comment is no key"
        );
        assert!(
            !refused(
                "d2.app",
                &format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>{}",
                    dict(&exe("x"))
                )
            ),
            "UTF-8 declared"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The executable rule: one plain path component with no control
    /// character.
    #[test]
    fn only_plain_file_names_are_plain() {
        assert!(plain_file_name("aterm"));
        for name in [
            "/Users//u/Documents/z",
            "../z",
            "",
            "MacOS/x",
            "..",
            "x\nSignature=adhoc",
        ] {
            assert!(!plain_file_name(name), "{name:?}");
        }
    }

    /// A binary `Info.plist` — what every App Store app ships — is read as
    /// `CFBundle` reads it: another program's is simply not ours, never a hole
    /// that leaves the census partial on every Mac; one naming ours is ours.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_binary_info_plist_answers_whose_bundle_it_is() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("aterm-census-binary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let binary = |app: &str, id: &str| {
            let at = dir.join(app);
            std::fs::create_dir_all(at.join("Contents/MacOS")).unwrap();
            let plist = at.join("Contents/Info.plist");
            std::fs::write(
                &plist,
                format!(
                    "<plist><dict><key>CFBundleIdentifier</key><string>{id}</string></dict></plist>"
                ),
            )
            .unwrap();
            let converted = std::process::Command::new("/usr/bin/plutil")
                .args(["-convert", "binary1"])
                .arg(&plist)
                .status()
                .expect("plutil runs");
            assert!(converted.success());
            assert!(
                read_plist_text(&plist).is_none(),
                "the text reader cannot read it"
            );
            at
        };
        let other = binary("Numbers.app", "com.apple.iWork.Numbers");
        assert_eq!(read_candidate(&other, "x", &[]), Candidate::NotOurs);
        let ours = binary("x.app", "x");
        assert!(matches!(
            read_candidate(&ours, "x", &[]),
            Candidate::Ours(BundleIdentity { bundle_id: Some(ref id), .. }) if id == "x"
        ));
        // `CFBundleIdentifier-macos` is the identifier `CFBundle` applies over
        // the plain key (measured with NSBundle), in both directions.
        let variant = |app: &str, plain: &str, macos: &str| {
            let at = dir.join(app);
            std::fs::create_dir_all(at.join("Contents/MacOS")).unwrap();
            std::fs::write(
                at.join("Contents/Info.plist"),
                format!(
                    "<plist><dict><key>CFBundleIdentifier</key><string>{plain}</string>\
                     <key>CFBundleIdentifier-macos</key><string>{macos}</string></dict></plist>"
                ),
            )
            .unwrap();
            at
        };
        assert!(matches!(
            read_candidate(&variant("v1.app", "other", "x"), "x", &[]),
            Candidate::Ours(_)
        ));
        assert_eq!(
            read_candidate(&variant("v2.app", "x", "other"), "x", &[]),
            Candidate::NotOurs
        );
        // A `-macos` value it cannot use still overrides the plain key: the
        // bundle has no identifier, so it is not ours.
        let unusable = dir.join("v3.app");
        std::fs::create_dir_all(unusable.join("Contents/MacOS")).unwrap();
        std::fs::write(
            unusable.join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>x</string>\
             <key>CFBundleIdentifier-macos</key><integer>1</integer></dict></plist>",
        )
        .unwrap();
        assert_eq!(read_candidate(&unusable, "x", &[]), Candidate::NotOurs);
        // A JSON file is no plist to `CFBundle`: it names no identifier, so a
        // copy whose `Info.plist` is JSON naming ours cannot claim it.
        let json = dir.join("j.app");
        std::fs::create_dir_all(json.join("Contents/MacOS")).unwrap();
        std::fs::write(
            json.join("Contents/Info.plist"),
            "{\"CFBundleIdentifier\": \"x\", \"CFBundlePackageType\": \"APPL\"}",
        )
        .unwrap();
        assert_eq!(read_candidate(&json, "x", &[]), Candidate::NotOurs);
        // An identifier that is `x`, a NUL and more is not `x`: a string that
        // does not come back whole is unread, never cut short.
        let nul = binary("n.app", "x");
        // `bplist00`, one dictionary: CFBundleIdentifier = "x\0junk" (an ASCII
        // string object holding a NUL), then its offset table and trailer.
        let mut bplist = b"bplist00\xd1\x01\x02\x5f\x10\x12CFBundleIdentifier\x56x\0junk".to_vec();
        bplist.extend_from_slice(&[8, 11, 32, 0, 0, 0, 0, 0, 0, 1, 1]);
        bplist.extend_from_slice(&3u64.to_be_bytes());
        bplist.extend_from_slice(&0u64.to_be_bytes());
        bplist.extend_from_slice(&39u64.to_be_bytes());
        std::fs::write(nul.join("Contents/Info.plist"), &bplist).unwrap();
        assert_eq!(read_candidate(&nul, "x", &[]), Candidate::NotOurs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A report read in part is not read: a stream one byte over the cap, or one
    /// that is not UTF-8, makes the whole run `None`; at the cap it is whole.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_tool_report_read_in_part_is_no_report() {
        let bytes = |count: u64| format!("head -c {count} /dev/zero");
        let sh = |script: &str| {
            run_bounded(
                "/bin/sh",
                &["-c".as_ref(), script.as_ref()],
                CODESIGN_CEILING,
            )
        };
        assert_eq!(sh(&bytes(TOOL_OUTPUT_MAX_BYTES + 1)), None);
        assert_eq!(sh("printf '\\377'"), None);
        let whole = sh(&bytes(TOOL_OUTPUT_MAX_BYTES)).expect("a report at the cap");
        assert!(whole.0 && whole.2.is_empty());
        assert_eq!(
            u64::try_from(whole.1.len()).ok(),
            Some(TOOL_OUTPUT_MAX_BYTES)
        );
    }

    /// The ceiling bounds the whole run, not only the tool's exit (the product
    /// fd-hygiene sweep of 2026-09-27): a grandchild the tool left holding its
    /// stdout and stderr keeps both readers short of EOF after the tool is gone,
    /// the shape a stranger that inherited a write end takes too. The run gives
    /// up at its ceiling rather than waiting the holder out.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_pipe_held_open_past_the_tool_cannot_outlast_the_ceiling() {
        let dir =
            std::env::temp_dir().join(format!("aterm-run-bounded-hold-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("holder.pid");
        let script = format!("sleep 20 & echo $! > '{}'", pid_file.display());
        let started = std::time::Instant::now();
        let ran = run_bounded(
            "/bin/sh",
            &["-c".as_ref(), script.as_ref()],
            std::time::Duration::from_secs(2),
        );
        let took = started.elapsed();
        let holder = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|pid| pid.trim().parse::<libc::pid_t>().ok());
        if let Some(pid) = holder {
            // SAFETY: kill(2) of the background `sleep` this test started.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            holder.is_some(),
            "the tool ran to its end and left a holder"
        );
        assert_eq!(ran, None, "a stream still open at the ceiling is no report");
        assert!(
            took < std::time::Duration::from_secs(10),
            "the run outlasted its ceiling waiting on the holder: {took:?}"
        );
    }

    /// The FDA probe's descriptor is close-on-exec from its open (the product
    /// fd-hygiene sweep of 2026-09-27): a child another thread spawns between the
    /// probe's open and its close must not carry a handle on `TCC.db` past its
    /// exec.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_fda_probe_descriptor_is_close_on_exec_from_its_open() {
        use std::os::fd::AsRawFd as _;
        let path = std::env::temp_dir().join(format!("aterm-fda-probe-{}", std::process::id()));
        std::fs::write(&path, b"").unwrap();
        let fd = imp::probe_descriptor(&path).expect("the probe opens a readable file");
        // SAFETY: F_GETFD only reads the flags of the descriptor `fd` owns.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        drop(fd);
        assert_eq!(imp::open_probe(&path), ProbeOutcome::Ok);
        let _ = std::fs::remove_file(&path);
        assert!(flags >= 0, "F_GETFD failed");
        assert_ne!(
            flags & libc::FD_CLOEXEC,
            0,
            "the probe's descriptor must not survive a concurrent spawn's exec"
        );
        assert_eq!(
            imp::open_probe(&path),
            ProbeOutcome::Errno(libc::ENOENT),
            "a missing file is its errno"
        );
    }

    /// A folder on another volume — a disk image mounted inside the bundle — is
    /// refused without being listed. The control is the same bundle once the
    /// image is detached. `hdiutil` needs no privilege and raises no dialog.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_walk_does_not_cross_into_another_volume() {
        struct Detach(PathBuf);
        impl Drop for Detach {
            fn drop(&mut self) {
                let _ = std::process::Command::new("/usr/bin/hdiutil")
                    .args(["detach", "-quiet", "-force"])
                    .arg(&self.0)
                    .output();
            }
        }
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("aterm-census-volume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bundle = dir.join("m.app");
        let mount = bundle.join("Contents/Vol");
        std::fs::create_dir_all(&mount).unwrap();
        std::fs::write(
            bundle.join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
        )
        .unwrap();
        let image = dir.join("vol.dmg");
        let hdiutil = |args: &[&std::ffi::OsStr]| {
            let out = std::process::Command::new("/usr/bin/hdiutil")
                .args(args)
                .output()
                .expect("hdiutil runs");
            assert!(
                out.status.success(),
                "hdiutil {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        hdiutil(&[
            "create".as_ref(),
            "-quiet".as_ref(),
            "-size".as_ref(),
            "1m".as_ref(),
            "-fs".as_ref(),
            "HFS+".as_ref(),
            image.as_os_str(),
        ]);
        hdiutil(&[
            "attach".as_ref(),
            "-quiet".as_ref(),
            "-nobrowse".as_ref(),
            "-readonly".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            image.as_os_str(),
        ]);
        let attached = Detach(mount.clone());
        let refused = bundle_reaches_protected(&bundle, &[]);
        drop(attached);
        assert!(refused, "a volume mounted inside the bundle");
        assert!(
            !bundle_reaches_protected(&bundle, &[]),
            "the control: the same bundle, detached"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The census's refusals, one arm at a time. The spelled arm alone refuses
    /// a path under a protected root that does not exist yet, where there is
    /// nothing to resolve; and the listing takes the same refusal.
    #[test]
    fn the_spelled_protected_arm_refuses_on_its_own() {
        let root = PathBuf::from("/nonexistent-aterm-census/Documents");
        let protected = vec![root.clone()];
        let missing = root.join("not-yet.app");
        assert!(
            std::fs::canonicalize(&missing).is_err(),
            "nothing to resolve"
        );
        assert!(!census_may_read(&missing, &protected));
        assert_eq!(
            list_unless_protected(&missing, &protected),
            Listing::Unreadable
        );
        assert_eq!(
            list_unless_protected(Path::new("/nonexistent-aterm-census/Apps"), &protected),
            Listing::Missing,
            "outside the root, a missing folder is simply missing"
        );
    }

    /// Only codesign's whole one-line "not signed" report is unsigned: the phrase
    /// inside a path on a signed copy's report is not.
    #[test]
    fn unsigned_is_the_whole_report_not_a_phrase_in_it() {
        assert_eq!(
            classify_signing("/A/x.app: code object is not signed at all\n"),
            "unsigned"
        );
        let planted = "Executable=/A/x: code object is not signed at all/x\n\
                       Identifier=x\nSignature=adhoc\n\
                       # designated => cdhash H\"aa\"\n";
        assert_eq!(classify_signing(planted), "adhoc");
        assert_ne!(
            identity_from_report(Some(planted), None).dr,
            DrClass::Unsigned
        );
        // The phrase inside a signed copy's requirement clause is not unsigned.
        let quoted = "Identifier=x\nTeamIdentifier=T\n\
                      designated => identifier \"code object is not signed at all\" and \
                      anchor apple generic and certificate leaf[subject.OU] = \"T\"\n";
        assert_eq!(classify_signing(quoted), "developer-id");
        assert_eq!(
            identity_from_report(Some(quoted), None).dr,
            DrClass::Unknown
        );
    }

    /// The census's own wiring lists through the protected-root fence: a
    /// required folder under a protected root is a hole in the look, where a
    /// plain listing would have called the look complete.
    #[test]
    fn the_census_lists_through_the_protected_fence() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("aterm-census-in-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let desktop = dir.join("Desktop");
        std::fs::create_dir_all(&desktop).unwrap();
        std::fs::write(desktop.join("notes.txt"), b"").unwrap();
        let roots = [(desktop.clone(), true)];
        let fenced = census_in("x", None, &roots, Some(&[]), std::slice::from_ref(&desktop));
        assert_eq!(fenced.enumeration, Enumeration::Partial, "never listed");
        let open = census_in("x", None, &roots, Some(&[]), &[]);
        assert_eq!(
            open.enumeration,
            Enumeration::Complete,
            "the control: listed"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The last check before a move compares the whole identity: an unsigned
    /// copy and a failed read both have no requirement text, and must not
    /// compare equal; a read with no class is never the same copy.
    #[test]
    fn the_identity_recheck_tells_an_unsigned_copy_from_a_failed_read() {
        let unsigned = identity_from_report(
            Some("/x.app: code object is not signed at all\n"),
            Some("x".to_string()),
        );
        let failed = identity_from_report(None, Some("x".to_string()));
        let copy = Claimant {
            path: PathBuf::from("/A/x.app"),
            dr: unsigned.dr,
            dr_text: unsigned.dr_text.clone(),
            signing: unsigned.signing,
            team: None,
            running: false,
        };
        assert!(same_identity(&unsigned, &copy));
        assert!(
            !same_identity(&failed, &copy),
            "a failed read is not the copy"
        );
        let unknown = Claimant {
            dr: DrClass::Unknown,
            signing: "unknown",
            ..copy.clone()
        };
        assert!(!same_identity(&failed, &unknown), "Unknown never matches");
        let adhoc = identity_from_report(
            Some("Signature=adhoc\n# designated => cdhash H\"aa\"\n"),
            Some("x".to_string()),
        );
        assert!(!same_identity(&adhoc, &copy), "re-signed ad hoc");
    }

    /// The Trash is offered only among the rows the owner can see: a retirable
    /// copy past the first MAX_CLAIMANT_ROWS conflicting ones is never offered.
    #[test]
    fn only_listed_copies_are_offered_for_the_trash() {
        let dir = PathBuf::from("/A");
        let running = dir.join("x.app");
        let stable_other = |i: usize| dir.join(format!("x-stable-{i:02}.app"));
        let mut entries = vec![running.clone()];
        entries.extend((0..MAX_CLAIMANT_ROWS).map(stable_other));
        let late = dir.join("z-adhoc.app");
        entries.push(late.clone());
        let c = classify_claimants(
            Some(&running),
            &[(dir.clone(), true)],
            Some(&[]),
            |_| Listing::Entries(entries.clone()),
            |root: &Path| {
                if root == running.as_path() {
                    ours(DEV_ID)
                } else if root == late.as_path() {
                    ours("designated => cdhash H\"aa\"")
                } else {
                    ours(
                        "designated => identifier \"x\" and anchor apple generic and \
                          certificate leaf[subject.OU] = \"OTHER\"",
                    )
                }
            },
        );
        assert_eq!(c.conflicting().len(), MAX_CLAIMANT_ROWS + 1);
        assert_eq!(c.retirable().len(), 1, "the ad-hoc copy is retirable");
        assert!(
            c.offered_for_trash().is_empty(),
            "but it is past the listed rows, so it is not offered"
        );
    }

    /// An unsigned bundle leaves `codesign` no requirement to print, so its
    /// class comes from the report as a whole: `Unsigned`, never `Unknown` —
    /// and beside a Developer-ID running copy it is offered for the Trash,
    /// while a copy whose signature could not be read is not.
    #[test]
    fn an_unsigned_copy_is_unsigned_and_offered_an_unreadable_one_is_not() {
        let unsigned = identity_from_report(
            Some("/x/y.app: code object is not signed at all\n"),
            Some("x".to_string()),
        );
        assert_eq!(unsigned.dr, DrClass::Unsigned);
        assert_eq!(unsigned.signing, "unsigned");
        assert_eq!(identity_from_report(None, None).dr, DrClass::Unknown);

        let dir = PathBuf::from("/A");
        let running = dir.join("x.app");
        let bare = dir.join("x-unsigned.app");
        let unread = dir.join("x-unread.app");
        let entries = vec![running.clone(), bare.clone(), unread.clone()];
        let c = classify_claimants(
            Some(&running),
            &[(dir.clone(), true)],
            Some(&[]),
            |_| Listing::Entries(entries.clone()),
            |root: &Path| {
                if root == running.as_path() {
                    ours(DEV_ID)
                } else if root == bare.as_path() {
                    Candidate::Ours(unsigned.clone())
                } else {
                    Candidate::Ours(identity_from_report(None, Some("x".to_string())))
                }
            },
        );
        assert_eq!(c.conflicting().len(), 2);
        let offered: Vec<_> = c
            .offered_for_trash()
            .iter()
            .map(|x| x.path.clone())
            .collect();
        assert_eq!(offered, vec![bare]);
    }

    #[test]
    fn bundle_names_include_the_rollback_suffix() {
        for yes in [
            "aterm.app",
            "aterm.app.rollback",
            "aterm.APP",
            "Some Name.app.previous",
        ] {
            assert!(looks_like_bundle(Path::new(yes)), "{yes}");
        }
        for no in [
            "notes.txt",
            "aterm",
            ".app",
            ".aterm.app",
            "apple.appointments",
            "",
        ] {
            assert!(!looks_like_bundle(Path::new(no)), "{no}");
        }
    }

    /// The running bundle's parent is listed first and REQUIRED; the
    /// conventional folders follow, optional and de-duplicated.
    #[test]
    fn the_search_roots_start_where_this_process_runs_from() {
        let home = PathBuf::from("/Users//a");
        let roots = claimant_search_roots(
            Some(Path::new("/Users//a/aterm/dist/aterm.app")),
            Some(&home),
        );
        assert_eq!(
            roots,
            vec![
                (PathBuf::from("/Users//a/aterm/dist"), true),
                (PathBuf::from("/Applications"), false),
                (home.join("Applications"), false),
            ]
        );
        let roots = claimant_search_roots(Some(Path::new("/Applications/aterm.app")), Some(&home));
        assert_eq!(roots[0], (PathBuf::from("/Applications"), true));
        assert_eq!(roots.len(), 2, "{roots:?}");
        assert_eq!(
            claimant_search_roots(None, None),
            vec![(PathBuf::from("/Applications"), false)]
        );
    }

    /// The candidate reader on a real directory tree: a bundle claiming the id
    /// is ours, another program's is not, a directory with no Info.plist is not
    /// a bundle, a plain file named `.app.zip` is not a bundle, and an
    /// unreadable Info.plist is a hole.
    #[test]
    fn the_candidate_reader_tells_ours_from_not_a_bundle_from_a_hole() {
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let lay = |name: &str, id: &str| {
            let contents = tmp.path().join(name).join("Contents");
            std::fs::create_dir_all(&contents).unwrap();
            std::fs::write(
                contents.join("Info.plist"),
                format!(
                    "<plist><dict><key>CFBundleIdentifier</key><string>{id}</string></dict></plist>"
                ),
            )
            .unwrap();
        };
        lay("mine.app", "x");
        lay("theirs.app", "y");
        std::fs::create_dir_all(tmp.path().join("empty.app")).unwrap();
        std::fs::write(tmp.path().join("archive.app.zip"), b"zip").unwrap();

        assert!(matches!(
            read_candidate(&tmp.path().join("mine.app"), "x", &[]),
            Candidate::Ours(_)
        ));
        assert_eq!(
            read_candidate(&tmp.path().join("theirs.app"), "x", &[]),
            Candidate::NotOurs
        );
        assert_eq!(
            read_candidate(&tmp.path().join("empty.app"), "x", &[]),
            Candidate::NotOurs
        );
        assert_eq!(
            read_candidate(&tmp.path().join("archive.app.zip"), "x", &[]),
            Candidate::NotOurs
        );
        assert_eq!(
            read_candidate(&tmp.path().join("gone.app"), "x", &[]),
            Candidate::NotOurs
        );
        assert_eq!(
            read_candidate(
                &tmp.path().join("mine.app"),
                "x",
                &[tmp.path().to_path_buf()]
            ),
            Candidate::Hole,
            "a bundle under a protected root is not read"
        );
        assert_eq!(list_root(&tmp.path().join("nowhere")), Listing::Missing);
    }

    /// THE IMAGE ANCHOR, as a table: every arm, classified from the kernel's
    /// path, with the exec path consulted only once the image has none.
    #[test]
    fn the_image_anchor_names_every_way_the_code_can_go_missing() {
        let exec = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
        let rolled = Path::new("/Applications/aterm.app.rollback/Contents/MacOS/aterm");
        let unbundled = Path::new("/Users//a/aterm/target/debug/deps/aterm_gui-abc");
        let at = |p: &Path| SelfImage::At(p.to_path_buf());

        assert_eq!(
            classify_image_anchor(Some(exec), &at(exec)),
            ImageAnchor::Live
        );
        // The updater's swap: the frozen exec path still names a `.app`, the
        // kernel's path does not.
        assert_eq!(
            classify_image_anchor(Some(exec), &at(rolled)),
            ImageAnchor::Displaced
        );
        // A kernel path under ANOTHER valid `.app` is still a bundle macOS can
        // build an identity for — a retargeted launch symlink, not a fault.
        let other = Path::new("/Applications/aterm-new.app/Contents/MacOS/aterm");
        assert_eq!(
            classify_image_anchor(Some(exec), &at(other)),
            ImageAnchor::Live
        );
        assert_eq!(
            classify_image_anchor(Some(unbundled), &at(unbundled)),
            ImageAnchor::NotBundled
        );

        // No path left: deleted only if this was ever a bundled image.
        assert_eq!(
            classify_image_anchor(Some(exec), &SelfImage::NoPath),
            ImageAnchor::Deleted
        );
        assert_eq!(
            classify_image_anchor(Some(unbundled), &SelfImage::NoPath),
            ImageAnchor::NotBundled
        );
        assert_eq!(
            classify_image_anchor(None, &SelfImage::NoPath),
            ImageAnchor::Unknown
        );
        // A failed lookup (EPERM under a sandbox) says nothing about the bundle.
        assert_eq!(
            classify_image_anchor(Some(exec), &SelfImage::Unreadable),
            ImageAnchor::Unknown
        );

        assert!(ImageAnchor::Displaced.grant_unverifiable());
        assert!(ImageAnchor::Deleted.grant_unverifiable());
        for ok in [
            ImageAnchor::Live,
            ImageAnchor::NotBundled,
            ImageAnchor::Unknown,
        ] {
            assert!(!ok.grant_unverifiable(), "{ok:?}");
        }

        // Wire spellings are stable: they are read by agents.
        assert_eq!(ImageAnchor::Live.as_str(), "live");
        assert_eq!(ImageAnchor::Displaced.as_str(), "displaced");
        assert_eq!(ImageAnchor::Deleted.as_str(), "deleted");
        assert_eq!(ImageAnchor::NotBundled.as_str(), "not-bundled");
        assert_eq!(ImageAnchor::Unknown.as_str(), "unknown");
    }

    #[test]
    fn the_bundle_layout_root_ignores_the_directory_name() {
        assert_eq!(
            bundle_layout_root(Path::new("/A/aterm.app.rollback/Contents/MacOS/aterm")),
            Some(PathBuf::from("/A/aterm.app.rollback"))
        );
        assert_eq!(
            bundle_layout_root(Path::new("/A/aterm.app/Contents/MacOS/aterm")),
            Some(PathBuf::from("/A/aterm.app"))
        );
        assert_eq!(bundle_layout_root(Path::new("/usr/local/bin/aterm")), None);
        assert_eq!(bundle_layout_root(Path::new("/Contents/MacOS/x")), None);
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
            // A Developer ID copy named for a cdhash is still stable.
            (
                "identifier \"com.cdhash.x\" and anchor apple generic and \
                 certificate leaf[subject.OU] = \"CDHASH\"",
                DrClass::Identity,
            ),
            // A quoted string is the author's data, not clauses: even a whole
            // clause's shape inside one is neither rule 2 nor rule 3.
            (
                "identifier \"x cdhash H\" and anchor apple generic",
                DrClass::Identity,
            ),
            ("identifier \"anchor apple\"", DrClass::Unknown),
            // `codesign` prints a plain alphanumeric value bare: a word that
            // merely contains `cdhash` is not the clause, which is the word on
            // its own followed by a hash constant.
            (
                "designated => identifier mycdhash and anchor apple generic and \
                 certificate leaf[subject.OU] = A66A9P66Z7",
                DrClass::Identity,
            ),
            ("identifier cdhash", DrClass::Unknown),
            ("(cdhash H\"abc\")", DrClass::Cdhash),
            // An escaped quote does not close the string: the anchor clause
            // here is inside it.
            (
                "identifier \"a\\\" and anchor apple generic\"",
                DrClass::Unknown,
            ),
            // A string does not cross a line: a stray quote in an earlier line
            // of codesign's report cannot swallow the requirement after it.
            (
                "Executable=/A/we\"ird\ndesignated => cdhash H\"aa\"",
                DrClass::Cdhash,
            ),
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
            (
                Folder::AppData,
                "app-data",
                "SystemPolicyAppData",
                "/Users//a/Library/Containers/com.apple.Notes",
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
        assert_eq!(Folder::ALL.len(), 4);
        assert_eq!(*Folder::ALL.last().expect("non-empty"), Folder::AppData);
    }

    /// The app-data item answers to the three spellings a hand-written config
    /// might use, and to nothing else.
    #[test]
    fn the_app_data_item_answers_to_every_spelling_of_its_name() {
        for spelling in ["app-data", "app_data", "appdata", "APP-DATA", " App_Data "] {
            assert_eq!(
                Folder::parse(spelling),
                Some(Folder::AppData),
                "spelling {spelling:?}"
            );
        }
        assert_eq!(Folder::parse("app data"), None);
        assert_eq!(Folder::parse("apps"), None);
    }

    /// Every item says what it is and what noun a sentence about it takes, and
    /// no two share a label — the panel prints these side by side.
    #[test]
    fn every_item_has_its_own_label_and_noun() {
        let mut labels: Vec<&str> = Folder::ALL.iter().map(|f| f.label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), Folder::ALL.len(), "labels are distinct");
        assert_eq!(Folder::Documents.label(), "Documents");
        assert_eq!(Folder::Documents.noun(), "folder");
        assert_eq!(Folder::AppData.label(), "Other applications' data");
        assert_eq!(Folder::AppData.noun(), "data");
    }

    /// A MISSING PATH IS A VERDICT FOR EXACTLY ONE ITEM. The three folders are
    /// made by macOS at account setup, so their absence is a broken home; the
    /// app-data probe names one app's container, which an account that never
    /// opened that app does not have — and being told it is missing means the
    /// look was allowed, since a TCC refusal answers EPERM first.
    #[test]
    fn only_the_app_data_probe_reads_a_missing_path_as_permission() {
        assert!(Folder::AppData.absent_means_allowed());
        for folder in [Folder::Documents, Folder::Desktop, Folder::Downloads] {
            assert!(!folder.absent_means_allowed(), "{folder:?}");
        }
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
            (true, true, run, true),
            (true, false, notx, false),
            (false, false, gone, false),
            // A directory (or anything that is not a regular file) at the path
            // is "no tool there", never "present but unusable".
            (false, true, gone, false),
        ];
        for (is_file, executable, want, can_run) in cases {
            let got = classify_tccutil(is_file, executable);
            assert_eq!(got, want, "is_file={is_file} executable={executable}");
            assert_eq!(got.can_run(), can_run);
        }
        // The default fails toward "do not offer a destructive button".
        assert_eq!(TccutilPresence::default(), TccutilPresence::Unknown);
        assert!(!TccutilPresence::Unknown.can_run());
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
        assert_eq!(plan.folders(), Folder::ALL);

        let commands = plan.commands();
        assert_eq!(commands.len(), Folder::ALL.len());
        for (folder, argv) in &commands {
            assert_eq!(argv[2], folder.tcc_service());
            assert_eq!(argv[3], "a.b.c", "every invocation names the same subject");
        }
        assert_eq!(
            commands.iter().map(|(f, _)| *f).collect::<Vec<_>>(),
            vec![
                Folder::Documents,
                Folder::Desktop,
                Folder::Downloads,
                Folder::AppData
            ]
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
        assert_eq!(offered.map(|p| p.commands().len()), Some(Folder::ALL.len()));

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
        assert_eq!(
            ResetStatus::from_exit_status(Some(1)),
            ResetStatus::Declined { code: Some(1) }
        );
        assert_eq!(
            ResetStatus::from_exit_status(Some(64)),
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
        let cases: [(&[ResetAttempt], ResetOutcome); 7] = [
            // all ok
            (
                &[
                    ok(Folder::Documents),
                    ok(Folder::Desktop),
                    ok(Folder::Downloads),
                ],
                ResetOutcome::AllReset,
            ),
            // one nonzero
            (
                &[
                    ok(Folder::Documents),
                    bad(Folder::Desktop, 1),
                    ok(Folder::Downloads),
                ],
                ResetOutcome::Partial,
            ),
            // all nonzero
            (
                &[
                    bad(Folder::Documents, 1),
                    bad(Folder::Desktop, 1),
                    bad(Folder::Downloads, 70),
                ],
                ResetOutcome::NoneReset,
            ),
            // tccutil absent — a property of the machine, not of a folder
            (
                &[
                    gone(Folder::Documents),
                    gone(Folder::Desktop),
                    gone(Folder::Downloads),
                ],
                ResetOutcome::ToolAbsent,
            ),
            // the tool vanished after one folder had already been reset
            (
                &[ok(Folder::Documents), gone(Folder::Desktop)],
                ResetOutcome::Partial,
            ),
            // nothing ran, and not because the tool is missing
            (
                &[bad(Folder::Documents, 1), gone(Folder::Desktop)],
                ResetOutcome::NoneReset,
            ),
            // nothing to do is not a success
            (&[], ResetOutcome::NotAttempted),
        ];
        for (attempts, want) in cases {
            assert_eq!(reset_outcome(attempts), want, "{attempts:?}");
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
        assert_eq!(
            outcome,
            ResetOutcome::Partial,
            "one success must never speak for the whole set"
        );

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
