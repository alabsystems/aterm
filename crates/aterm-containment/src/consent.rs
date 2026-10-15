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
//! Beside them sits one peer that is NOT TCC: macOS **debugger access**
//! (developer mode), the Authorization Services right
//! [`TASKPORT_DEBUG_RIGHT`] (`docs/DESIGN-macos-debugger-access-2026-09-29.md`
//! §3.2). It is observed the same way — the rule's mode ([`DebuggerMode`]),
//! the account's membership ([`DebuggerMember`]) and aterm's own hardening
//! ([`SelfHardening`]). It is changed in one place only, and only for the
//! owner: [`run_devtools_change`], which the owner's confirmed gesture in
//! Settings ▸ Security reaches and nothing else does (design §3.4, owner ruling
//! O7). It runs Apple's own tool unprivileged, so macOS should raise its own
//! administrator dialog (spike D2), which aterm never sees or answers.
//!
//! And one TCC service it reads twice over: the Privacy & Security ▸
//! Developer Tools list (debugger design §9.1, O18), which decides whether
//! macOS checks each new program run in aterm before its first start — from
//! `tccd`'s own log ([`DevToolState`]: what tccd decided) and from the switch's
//! row in the system `TCC.db` ([`DevToolListing`]: what the owner set), joined
//! by [`devtool_grant`]. It never prompts, so only a human changes it, in
//! System Settings.
//!
//! ## What this module deliberately does NOT do
//!
//! It never answers a consent dialog, never asks macOS for consent, never
//! writes to a `TCC.db` (it reads one table's Developer Tools rows, read-only:
//! [`probe_devtool_list`]), and never touches the private write-side TCC SPI. The
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
//! ## Purity, and the thin OS wrappers
//!
//! Everything here is a pure function over data except a few thin wrappers:
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
//! * [`probe_debugger`] — at most one `/usr/sbin/DevToolsSecurity -status`,
//!   bounded, behind the same gate and in-bundle fence as [`probe_fda`]. The
//!   `-status` path exits before it creates an authorization, so it raises no
//!   dialog (debugger design §1.3, spike D1).
//! * [`debugger_membership`] — `mbr_check_membership` against `admin`, then
//!   `_developer`: a directory query, no spawn.
//! * [`self_hardening`] — one `csops(getpid(), CS_OPS_STATUS)`.
//! * [`process_birth`] — one `proc_pidinfo(PROC_PIDTBSDINFO)`: a pid's start
//!   time, so a recorded change child is told from a reused pid.
//! * [`probe_devtool`] — at most one `/usr/bin/log show` of tccd's own
//!   Developer Tools answers (`kTCCServiceDeveloperTool`), bounded, behind the
//!   same gate and in-bundle fence as [`probe_fda`], with a constant argv. It
//!   opens no `TCC.db` and asks `tccd` nothing.
//! * [`probe_devtool_list`] — one `open(…, O_RDONLY)` of the SYSTEM `TCC.db`
//!   (prompt-free as [`probe_fda`]'s), then at most one `/usr/bin/sqlite3
//!   -readonly` with a constant argv, bounded, behind the same gate and
//!   in-bundle fence. It writes nothing.
//!
//! And one that is not an observation: [`run_devtools_change`] — a fresh
//! `-status` read (through [`probe_debugger`], so behind the same gate and
//! in-bundle fence), then at most one `DevToolsSecurity -enable` or `-disable`,
//! waited for with no deadline. Only the owner's gesture reaches it.
//!
//! Each wrapper has a pure classifier beside it ([`classify_probe`],
//! [`classify_responsible`], [`display_name`], [`classify_devtools_status`],
//! [`classify_membership`], [`classify_cs_status`], [`classify_devtool_log`]) so
//! the interesting logic is table-testable without an operating system.
//!
//! ## The in-bundle guard is the safety gate, and it is not optional
//!
//! On 2026-08-17 a unit test's first `WindowServer` touch made `tccd`
//! `readdir` a 1.1-million-entry `target/debug/deps`; `WindowServer` outran its 40 s
//! watchdog and was killed, taking every GUI session on the machine with it
//! (`AGENTS.md`, `tools/grep_guard.sh` B9). The standing rule for the
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
    /// Refused: the running executable is not inside a `.app` bundle. No
    /// syscall was performed.
    RefusedOutOfBundle,
    /// Refused: the running executable could not be resolved at all. No
    /// syscall was performed.
    RefusedNoExe,
    /// Refused: `[privacy] enabled` or `[privacy] check` is off. No syscall was
    /// performed; the row names the config rather than implying a denial.
    RefusedDisabled,
    /// Refused: `$HOME` is unset, so there is no store to reach for. No
    /// syscall was performed.
    RefusedNoHome,
    /// Refused: the resolved store path cannot be handed to a C API (it
    /// contains an interior NUL). No syscall was performed.
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
    // The fence (design §3.3 guardrail 1). A binary outside a `.app` — a test
    // binary under target/debug/deps above all — never reaches the syscall.
    if !path_is_in_app_bundle(exe) {
        return FdaProbe::refused(ProbeLabel::RefusedOutOfBundle);
    }
    let Some(home) = home else {
        return FdaProbe::refused(ProbeLabel::RefusedNoHome);
    };
    let db = home.join(TCC_DB_RELATIVE);
    if path_has_interior_nul(&db) {
        return FdaProbe::refused(ProbeLabel::RefusedBadPath);
    }
    classify_probe(syscall(&db))
}

/// Probe Full Disk Access for the running process.
///
/// Performs at most one `open(…, O_RDONLY)` of the user's `TCC.db`, closes the
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
        let exe = fence_exe();
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
    // `Path::extension` rather than a suffix test: it answers `None` for a
    // component that is exactly `.app` (a hidden file, not a bundle), it needs
    // no UTF-8 round trip, and macOS volumes are conventionally
    // case-insensitive.
    bundle_root_where(exe, |dir| {
        Path::new(dir.as_os_str())
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
    })
}

/// The executable the in-bundle fence judges, for [`probe_fda`],
/// [`probe_debugger`] and [`probe_devtool`]: `std::env::current_exe()` when
/// that is inside a `.app`, else the kernel's path for this process's image
/// when that one is, else `current_exe()` again.
///
/// On macOS `current_exe()` is the `execve`-time string, so a program started
/// through a symlink (`~/.local/bin/aterm` →
/// `/Applications/aterm.app/Contents/MacOS/aterm`, the one name
/// `tools/install.sh` lays) read as outside the bundle although the code
/// running is the bundle's, and every probe refused: `aterm doctor` typed at a
/// shell measured nothing (found 2026-10-14). The kernel's path
/// (`proc_pidpath`, one syscall on this process's own pid, which reads no
/// protected location and raises no dialog) names the image macOS attributes.
/// The exec path still wins whenever it is bundled, so a window whose bundle an
/// update renamed under it (`aterm.app.rollback`, [`ImageAnchor`]) is judged
/// exactly as before, and a test binary under `target/` is outside a bundle by
/// both paths, so the fence holds for it.
#[must_use]
pub fn fence_exe() -> Option<PathBuf> {
    fence_exe_from(std::env::current_exe().ok(), &self_image())
}

/// [`fence_exe`], pure over the exec-time path and the kernel's answer.
#[must_use]
pub fn fence_exe_from(exec: Option<PathBuf>, image: &SelfImage) -> Option<PathBuf> {
    if exec.as_deref().is_some_and(path_is_in_app_bundle) {
        return exec;
    }
    match image {
        SelfImage::At(path) if path_is_in_app_bundle(path) => Some(path.clone()),
        SelfImage::At(_) | SelfImage::NoPath | SelfImage::Unreadable => exec,
    }
}

// The image anchor: can macOS still find the code this process's grants are
// keyed to?

/// Whether the bundle macOS keys this process's consent decisions to can still
/// be found on disk.
///
/// A TCC grant is validated against the running code. If the running bundle is
/// renamed or deleted, `tccd` cannot build a code object for it, and every
/// consent decision for the process and its attributed descendants falls back
/// to asking. The updater's apply does exactly that: `RENAME_SWAP` moves the
/// running bundle to `aterm.app.rollback`, and boot-health confirmation later
/// deletes it.
///
/// Classified from the kernel's path for the image (`proc_pidpath`), never
/// `std::env::current_exe()`, which on macOS is the frozen `execve`-time string
/// and reads healthy after the swap:
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
    /// running code. `NotBundled` (a dev run) and `Unknown` (a failed read) are
    /// not faults.
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
    bundle_root_where(exe, |dir| matches!(dir, Component::Normal(_)))
}

/// The innermost `<dir>` of a `<dir>/Contents/MacOS/<exe>` layout in `exe` —
/// four components at least — whose `<dir>` passes `is_bundle_dir`.
fn bundle_root_where(
    exe: &Path,
    is_bundle_dir: impl Fn(&Component<'_>) -> bool,
) -> Option<PathBuf> {
    let components: Vec<Component<'_>> = exe.components().collect();
    let last_start = components.len().checked_sub(4)?;
    (0..=last_start).rev().find_map(|index| {
        (is_bundle_dir(&components[index])
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

/// The app macOS holds responsible for `pid`, resolved far enough to name it.
///
/// The verdict belongs to the responsible app — a probe run from a shell under
/// another terminal reports that terminal's Full Disk Access, not aterm's — so
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
    #[cfg(target_os = "macos")]
    {
        imp::responsible_app_with_roots(pid, &protected_roots(&[]))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
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

/// Classify a `codesign -d -r-` requirement string. Pure — it takes the
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
/// Rules 2 and 3 read only the text outside quoted strings: an identifier, a
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
    String::from_utf8(read_plist_bytes(path)?).ok()
}

/// A plist's bytes, or `None` for a non-file, a file over
/// [`INFO_PLIST_MAX_BYTES`], or one that cannot be read.
fn read_plist_bytes(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > INFO_PLIST_MAX_BYTES {
        return None;
    }
    std::fs::read(path).ok()
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
/// file name. So one such shape in the running bundle leaves its identity unread,
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
/// its stderr. `None` for every outcome [`run_bounded_exit`] does not report as
/// [`ToolRun::Exited`]: the tool cannot run or does not finish, or either
/// stream is over [`TOOL_OUTPUT_MAX_BYTES`] or is not UTF-8 — a report read in
/// part is not read. Gated native-only first, as the wasm census reads gates:
/// its reader threads are never part of the wasm module.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(target_os = "macos")]
fn run_bounded(
    program: &str,
    args: &[&std::ffi::OsStr],
    ceiling: std::time::Duration,
) -> Option<(bool, String, String)> {
    match run_bounded_exit(program, args, ceiling, TOOL_OUTPUT_MAX_BYTES) {
        // `ExitStatus::success` is exactly "exited with code 0" on unix.
        ToolRun::Exited {
            code,
            stdout,
            stderr,
        } => Some((code == Some(0), stdout, stderr)),
        ToolRun::Absent | ToolRun::SpawnFailed | ToolRun::TimedOut | ToolRun::Unreadable => None,
    }
}

/// [`run_bounded`] with the outcome kept apart: the exit code, or which way the
/// run failed. Same machinery — stdin closed, stdout and stderr each read on its
/// own thread so output larger than a pipe's buffer cannot wedge the tool, both
/// streams capped at `max_bytes` ([`TOOL_OUTPUT_MAX_BYTES`] for every caller but
/// the Developer Tools log read) and required to be UTF-8, and `ceiling`
/// bounding the readers as well as the exit.
#[cfg(not(target_arch = "wasm32"))]
#[cfg(target_os = "macos")]
fn run_bounded_exit(
    program: &str,
    args: &[&std::ffi::OsStr],
    ceiling: std::time::Duration,
    max_bytes: u64,
) -> ToolRun {
    use aterm_uds::spawnfd::Stdio;
    use std::io::Read as _;
    fn drain(
        pipe: impl std::io::Read + Send + 'static,
        max_bytes: u64,
    ) -> Option<std::thread::JoinHandle<Option<String>>> {
        // `Builder::spawn`, not `thread::spawn`: a thread the OS refuses is no
        // report, never a panic in the census or the Trash's worker.
        std::thread::Builder::new()
            .spawn(move || {
                let mut bytes = Vec::new();
                pipe.take(max_bytes.saturating_add(1))
                    .read_to_end(&mut bytes)
                    .ok()?;
                let whole = u64::try_from(bytes.len()).is_ok_and(|len| len <= max_bytes);
                whole
                    .then_some(bytes)
                    .and_then(|bytes| String::from_utf8(bytes).ok())
            })
            .ok()
    }
    let mut child = match aterm_uds::spawnfd::command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return ToolRun::Absent,
        Err(_) => return ToolRun::SpawnFailed,
    };
    let (Some(out), Some(err)) = (
        child.stdout.take().and_then(|pipe| drain(pipe, max_bytes)),
        child.stderr.take().and_then(|pipe| drain(pipe, max_bytes)),
    ) else {
        let _ = child.kill();
        let _ = child.wait();
        return ToolRun::SpawnFailed;
    };
    // wasm-clock-guard: allow — `run_bounded_exit` is `#[cfg(target_os = "macos")]`
    // (it runs Apple's tools), so neither clock read below reaches wasm.
    let deadline = std::time::Instant::now() + ceiling;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            // wasm-clock-guard: allow — the same macOS-only function.
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return ToolRun::TimedOut;
            }
            // The exit could not be read: no code to report, and no claim
            // that the tool ran long.
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return ToolRun::Unreadable;
            }
        }
    };
    // The ceiling bounds the readers too, not only the exit. A reader ends at
    // EOF, which needs every copy of the pipe's write end closed, and a copy
    // can outlive the tool (a background grandchild, or a child another thread
    // spawned inside std's non-atomic `pipe()`-then-`FIOCLEX` on macOS). A
    // reader left unjoined is detached and ends when the last copy closes.
    while !(out.is_finished() && err.is_finished()) {
        // wasm-clock-guard: allow — the same macOS-only function.
        if std::time::Instant::now() >= deadline {
            return ToolRun::TimedOut;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let (Ok(Some(stdout)), Ok(Some(stderr))) = (out.join(), err.join()) else {
        return ToolRun::Unreadable;
    };
    ToolRun::Exited {
        code: status.code(),
        stdout,
        stderr,
    }
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
/// `codesign` prints an implicit requirement — every ad-hoc signature's — as a
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
    // `codesign -d` on unsigned code prints one line, `<path>: code object is
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
// macOS keeps one code requirement per bundle identifier. A copy that does not
// satisfy it is not merely denied when it asks: `tccd` answers `DB
// Action:Update, UpdateVerifierData` and replaces the stored requirement with
// the asker's, resetting the grant for every copy that shares the identifier.
// That write escapes process scope, so no per-process self-report can see it.
//
// The census reports what is on disk and whether those copies agree. It never
// reads `TCC.db` — not a supported status API — so it says nothing about what
// `tccd` currently holds.
//
// It counts bundles, not names (2026-10-02). A copy is identified by its
// directory's volume and file number, links followed ([`Located`]): a link in
// an app folder, a firmlink spelling and the bundle itself are one claimant,
// which is the running copy when the running bundle is that file. A link names
// a bundle someone chose to keep reaching, so the copy it leads to is listed
// and never offered for the Trash, and nothing ever moves the link: moving the
// bundle would strand the link, and moving the link would leave the bundle,
// which is the code `tccd` sees.

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
    /// The bundle directory (`aterm.app`, `aterm.app.rollback`, …) — the
    /// bundle itself, never a link to it ([`Located::path`]).
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
    /// The links the census found that lead to this bundle — a hand-made
    /// `/Applications/aterm-dev.app` naming a build elsewhere. Someone keeps
    /// each one, so a linked copy is never offered for the Trash
    /// ([`TrashOffer::Linked`]).
    pub links: Vec<PathBuf>,
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

/// Whether the Security panel's *Move to Trash* takes one conflicting copy
/// and, when it does not, the first condition it misses, in this order. This
/// is the one spelling of that rule ([`Self::of`]): the move
/// ([`Claimants::retirable`]), the panel's button and rows and the `privacy`
/// verb's rows and note ([`Claimants::listed`]) all read it, and a surface
/// says why a copy stays by naming its variant, never by restating the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrashOffer {
    /// The button takes it.
    Offered,
    /// Signed with a certificate, as a release is — a Developer ID copy is
    /// never moved, whatever its requirement says — or with a signature that
    /// could not be read, so it is not known to be a build either.
    Signature,
    /// Signed ad hoc, but its requirement does not read as its cdhash: one
    /// someone chose, or one that could not be read.
    Requirement,
    /// A link leads to it ([`Claimant::links`]): moving it would leave the link
    /// someone keeps pointing at nothing, and moving the link instead would
    /// leave the copy, which is what `tccd` sees.
    Linked,
    /// The copy running here does not keep its grant across updates, so the
    /// conflicting copy may well be the real release.
    RunningUnstable,
    /// This Mac has no trash tool to move it with.
    NoTool,
}

impl TrashOffer {
    /// The `trash=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Offered => "offered",
            Self::Signature => "signature",
            Self::Requirement => "requirement",
            Self::Linked => "linked",
            Self::RunningUnstable => "running-unstable",
            Self::NoTool => "no-tool",
        }
    }

    /// The rule for one conflicting copy, the trash tool aside: signed ad hoc
    /// to its cdhash or not signed at all, no link leading to it, beside a
    /// running copy whose grant survives updates.
    fn of(copy: &Claimant, running_stable: bool) -> Self {
        match (copy.signing, copy.dr) {
            ("adhoc", DrClass::Cdhash) | ("unsigned", DrClass::Unsigned) => {}
            ("adhoc", _) => return Self::Requirement,
            _ => return Self::Signature,
        }
        if !copy.links.is_empty() {
            Self::Linked
        } else if running_stable {
            Self::Offered
        } else {
            Self::RunningUnstable
        }
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

    /// Whether the running copy keeps its grant across updates.
    fn running_stable(&self) -> bool {
        self.found.iter().any(|c| c.running && c.dr.grant_stable())
    }

    /// Every conflicting copy the rule ([`TrashOffer`]) lets the move take, the
    /// trash tool aside and listed or not: the worker's fresh check before each
    /// move ([`retire_claimants`]), which touches only the copies the owner
    /// was shown.
    #[must_use]
    pub fn retirable(&self) -> Vec<&Claimant> {
        let stable = self.running_stable();
        self.conflicting()
            .into_iter()
            .filter(|copy| TrashOffer::of(copy, stable) == TrashOffer::Offered)
            .collect()
    }

    /// The rows every surface lists — the first [`MAX_CLAIMANT_ROWS`]
    /// conflicting copies — each with whether the Security panel's *Move to
    /// Trash* takes it ([`TrashOffer`]). `trash_tool` is whether this Mac has
    /// the tool the move needs. The panel's rows and button and the `privacy`
    /// verb's rows and note all read this, so none can say a copy is offered
    /// that the button does not take.
    #[must_use]
    pub fn listed(&self, trash_tool: bool) -> Vec<(&Claimant, TrashOffer)> {
        let stable = self.running_stable();
        self.conflicting()
            .into_iter()
            .take(MAX_CLAIMANT_ROWS)
            .map(|copy| {
                let offer = match TrashOffer::of(copy, stable) {
                    TrashOffer::Offered if !trash_tool => TrashOffer::NoTool,
                    offer => offer,
                };
                (copy, offer)
            })
            .collect()
    }

    /// The copies the Security panel offers to move to the Trash: the listed
    /// ones that read [`TrashOffer::Offered`].
    #[must_use]
    pub fn offered_for_trash(&self, trash_tool: bool) -> Vec<&Claimant> {
        self.listed(trash_tool)
            .into_iter()
            .filter_map(|(copy, offer)| (offer == TrashOffer::Offered).then_some(copy))
            .collect()
    }

    /// Whether the census may say nothing else claims the id: only after a
    /// complete look. A partial look that found nothing has not established
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

/// Which bundle one spelling of a candidate names.
///
/// A bundle has one identity on disk however it is spelled: through a link, a
/// firmlink (`/System/Volumes/Data/Applications/x.app` is `/Applications/x.app`
/// to the kernel, though no path resolution says so), or another letter case
/// on a volume that ignores it. The census counts bundles, so it compares
/// these, never spellings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    /// The bundle directory's volume and file number, links followed.
    pub file: (u64, u64),
    /// The bundle itself: the spelling, or where a link to it resolves.
    pub path: PathBuf,
    /// Whether the spelling is a link to the bundle rather than its own name.
    pub link: bool,
}

/// Locate one spelling ([`Located`]). `None` when it cannot be read, or when it
/// is a link to a folder not named like a bundle: it then stays as spelled,
/// for the candidate read to refuse as it always has. Metadata only, like the
/// protected-root fence's own resolution.
#[cfg(unix)]
#[must_use]
pub fn locate_candidate(spelled: &Path) -> Option<Located> {
    use std::os::unix::fs::MetadataExt as _;
    let link = std::fs::symlink_metadata(spelled)
        .ok()?
        .file_type()
        .is_symlink();
    let path = if link {
        let real = std::fs::canonicalize(spelled).ok()?;
        if !looks_like_bundle(&real) {
            return None;
        }
        real
    } else {
        spelled.to_path_buf()
    };
    let meta = std::fs::metadata(&path).ok()?;
    Some(Located {
        file: (meta.dev(), meta.ino()),
        path,
        link,
    })
}

/// Off unix there is no file number to compare: every spelling stays its own.
#[cfg(not(unix))]
#[must_use]
pub fn locate_candidate(_spelled: &Path) -> Option<Located> {
    None
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
    // `codesign` also opens the main executable the plist names, with no link
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
    let bytes = read_plist_bytes(plist)?;
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
/// the running bundle's parent is listed and required — this process runs from
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

/// What makes two candidates one bundle: the file they locate to, or, for a
/// spelling that could not be located, the spelling itself.
#[derive(PartialEq, Eq)]
enum SameBundle {
    File((u64, u64)),
    Spelled(PathBuf),
}

/// One bundle the census reached, however many times.
struct Reached {
    same: SameBundle,
    /// The path it is read and reported by.
    path: PathBuf,
    /// Whether `path` is only where a link resolves: the bundle's own name,
    /// once a source spells it, replaces it.
    resolved: bool,
    /// The links that lead to it.
    links: Vec<PathBuf>,
}

/// The census, pure over its sources.
///
/// `running` is the bundle this process runs from, `roots` the directories to
/// list (with whether each is required), `registered` the copies `LaunchServices`
/// knows (`None` when it could not be asked), `list`, `locate` and `read` the
/// three filesystem reads. Each injected so a table test drives every arm with
/// no filesystem and no `codesign`.
///
/// One bundle is one claimant however many names reach it: candidates are told
/// apart by the bundle each locates to ([`Located`]), never by spelling — so a
/// link to the running copy is the running copy, and a link to another build
/// is that build, read and reported under the bundle's own name (the first
/// one a source spells it by, else where the link resolves), with the link in
/// [`Claimant::links`].
#[must_use]
pub fn classify_claimants(
    running: Option<&Path>,
    roots: &[(PathBuf, bool)],
    registered: Option<&[PathBuf]>,
    list: impl Fn(&Path) -> Listing,
    locate: impl Fn(&Path) -> Option<Located>,
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

    let bundle_of = |spelled: &Path| match locate(spelled) {
        Some(at) => (SameBundle::File(at.file), at.path, at.link),
        None => (
            SameBundle::Spelled(spelled.to_path_buf()),
            spelled.to_path_buf(),
            false,
        ),
    };
    let running_bundle = running.map(|path| bundle_of(path).0);
    let mut bundles: Vec<Reached> = Vec::new();
    for spelled in candidates {
        let (same, path, link) = bundle_of(&spelled);
        let index = bundles
            .iter()
            .position(|b| b.same == same)
            .unwrap_or_else(|| {
                bundles.push(Reached {
                    same,
                    path: path.clone(),
                    resolved: link,
                    links: Vec::new(),
                });
                bundles.len() - 1
            });
        let bundle = &mut bundles[index];
        if link {
            if !bundle.links.contains(&spelled) {
                bundle.links.push(spelled);
            }
        } else if bundle.resolved {
            bundle.path = path;
            bundle.resolved = false;
        }
    }

    let mut found: Vec<Claimant> = Vec::new();
    for bundle in bundles {
        match read(&bundle.path) {
            Candidate::NotOurs => {}
            Candidate::Hole => holes += 1,
            Candidate::Ours(id) => found.push(Claimant {
                running: running_bundle.as_ref() == Some(&bundle.same),
                path: bundle.path,
                dr: id.dr,
                dr_text: id.dr_text,
                signing: id.signing,
                team: id.team,
                links: bundle.links,
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
        locate_candidate,
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
/// that a fresh census still offers ([`Claimants::retirable`]), that is still
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
/// `$HOME` — the same list the Containment tier denies, on purpose: the
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
/// names it. The path literals live only here; `aterm-gui` and `aterm-cli`
/// receive resolved [`PathBuf`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Folder {
    /// `~/Documents` — `kTCCServiceSystemPolicyDocumentsFolder`.
    Documents,
    /// `~/Desktop` — `kTCCServiceSystemPolicyDesktopFolder`.
    Desktop,
    /// `~/Downloads` — `kTCCServiceSystemPolicyDownloadsFolder`.
    Downloads,
    /// Another application's data (`kTCCServiceSystemPolicyAppData`, "would
    /// like to access data from other apps"). Not a folder of the human's,
    /// hence last in [`ALL`](Self::ALL) with its own [`label`](Self::label); it
    /// is promptable, resettable and warm-up-able like the folders.
    ///
    /// Its probe path is one fixed first-party container
    /// ([`path`](Self::path)), so no directory listing (itself a guarded
    /// access) is needed. It may legitimately not exist, hence
    /// [`absent_means_allowed`](Self::absent_means_allowed).
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

    /// What the warm-up asks for when `[privacy] warmup_folders` is absent: the
    /// three folders of the human's, and NOT [`AppData`](Self::AppData).
    ///
    /// The app-data Allow is forgotten when the responsible instance ends
    /// (design `docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §1.6a): the warm-up
    /// would raise a dialog whose answer dies with the process that received it,
    /// which is the dialog an owner cannot make stick. The name stays valid in an
    /// explicit list, and [`ALL`](Self::ALL) still carries it for the reset and
    /// the report rows.
    pub const WARMUP_DEFAULT: &'static [Self] = &[Self::Documents, Self::Desktop, Self::Downloads];

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
            // One first-party container, named rather than discovered: any
            // container raises the same question, and listing the root to
            // choose one would itself be a guarded access. A fixed name is a
            // pure function of `home`.
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

    /// Whether a missing probe path is a permission answer rather than a
    /// failure.
    ///
    /// The three folders are created by macOS at account setup and their
    /// absence is a broken home, not a verdict — [`ERRNO_ENOENT`] there means
    /// "could not be read". The app-data probe names one app's container, and
    /// an account that has never opened that app does not have it: there,
    /// `ENOENT` is proof the access was permitted, because a TCC refusal
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
/// account; whether the container exists is irrelevant to the question being
/// asked (see [`Folder::absent_means_allowed`]). Nothing about the choice is
/// specific to Notes: TCC keys the grant to the service, so one container
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

/// Build the `tccutil reset <service> <bundle id>` argv for one folder. Pure —
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

// Denied-state repair (design §3.7), pure.
//
// A persisted "Don't Allow" and a stale-`csreq` silent failure look the same
// from outside: `EPERM`, no dialog, and macOS never asks again. The only
// unprivileged repair is `tccutil reset`, which removes this app's row for one
// service so macOS can ask again. It grants nothing and cannot help with Full
// Disk Access. This module never runs it; it builds argv and folds recorded
// results. The spawn lives in the Settings panel behind an owner gesture,
// fenced off every control verb, so a program in a session cannot re-arm a
// prompt the human already answered. Nothing here may imply what a later
// grant covers (§7 S4 is unmeasured).

/// The `tccutil` binary, absolute.
///
/// Absolute on purpose: a destructive mutation of the human's privacy state may
/// not be resolved through `PATH`, where anything could answer to the name.
#[cfg(any(unix, test))]
pub const TCCUTIL_PATH: &str = "/usr/bin/tccutil";

/// Whether [`TCCUTIL_PATH`] can be run at all.
///
/// §3.7: "if `tccutil` is missing or not executable, the button is not shown at
/// all". This is a property of the file: it says nothing about the privacy
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

/// Classify a stat of [`TCCUTIL_PATH`]. Pure — the caller does the stat.
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
/// decision — the panel owns the prose; this crate owns the choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetOffer {
    /// Show the button. It re-arms macOS's question for the folders in the
    /// plan; it makes no claim about what a grant that follows will cover.
    Offer,
    /// Show no button; say the grant was invalidated by a rebuild rather than
    /// the owner, and point at the dev-identity fix (§3.1): this identity does
    /// not survive a build, so a reset could not hold. Worded from the
    /// [`DrClass`].
    ExplainRebuild,
    /// Show nothing: [`TCCUTIL_PATH`] cannot be run here (missing, not
    /// executable, or not determinable).
    HideNoTool,
    /// Show nothing: the running bundle id is unknown, so no argv can be built
    /// from the running bundle, and there is no literal fallback (a dev channel
    /// must never reset the release's rows).
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
    /// The designated-requirement class of the running code object
    /// ([`classify_dr`]).
    pub dr: DrClass,
    /// Whether [`TCCUTIL_PATH`] can be run ([`tccutil_presence`]).
    pub tccutil: TccutilPresence,
    /// The running bundle's `CFBundleIdentifier`; `None` when it could not be
    /// read. Never a literal, never the release's id on a dev build.
    pub bundle_id: Option<&'a str>,
}

/// Decide whether the denied-state repair may be offered. Pure.
///
/// Precedence:
///
/// 1. Identity. [`DrClass::Cdhash`] (§3.7) and [`DrClass::Unsigned`] (§3.1)
///    mean a grant does not survive a build, so the panel explains the rebuild
///    instead. This outranks the tool check.
/// 2. The tool: no runnable [`TCCUTIL_PATH`], no button.
/// 3. The subject: no usable running bundle id, no button (no fallback).
///
/// [`DrClass::Unknown`] does not suppress the offer: an unstable identity was
/// not observed. The reset still re-arms the question; the panel promises
/// nothing about a later grant's durability. When to render the repair at all
/// stays with the panel.
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
/// built from the same running bundle id.
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

    /// The argv for each folder, in plan order. Pure — it builds commands; it
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
    /// Ran and exited nonzero — this folder's row is unchanged. `code` is the
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
            code => Self::Declined { code },
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

/// The folded result of one *Ask again* gesture.
///
/// The distinction between [`Self::AllReset`] and [`Self::Partial`] is the
/// whole point of folding rather than counting: §3.7 forbids the panel from
/// claiming every folder reset because one did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetOutcome {
    /// Every invocation exited 0.
    AllReset,
    /// At least one folder reset and at least one did not. Reported as a
    /// partial success, naming the folders that failed and no others.
    Partial,
    /// At least one invocation ran and no folder reset. No row changed.
    NoneReset,
    /// Nothing ran at all: [`TCCUTIL_PATH`] is missing or not executable. §3.7:
    /// the button is not shown in the first place.
    ToolAbsent,
    /// There was nothing to attempt. Not a success, and never rendered as one.
    NotAttempted,
}

/// Fold the per-invocation results of one gesture into one outcome. Pure and
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
/// line. A [`ResetStatus::ToolAbsent`] attempt is not here: nothing was asked,
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
// Debugger access (developer mode) — observation
// ---------------------------------------------------------------------------
//
// `docs/DESIGN-macos-debugger-access-2026-09-29.md` §3.2. The "Developer Tools
// Access needs to take control of another process" dialog is not TCC: it is
// the Authorization Services right [`TASKPORT_DEBUG_RIGHT`], keyed to the USER
// and the Mac, never to aterm's code identity. Apple's one sanctioned switch
// for it is `DevToolsSecurity`, and this section only ever asks it `-status`
// — a compare that exits before `AuthorizationCreate`, so it raises no dialog
// (§1.3; spike D1 confirmed it live, 2026-09-29). Nothing in THIS section
// changes the rule: the owner's change is the next section's
// [`run_devtools_change`], the only spawner of [`devtools_enable_command`] and
// [`devtools_disable_command`]. Every argv carries its flag, and a bare
// invocation — which means `-enable` — is never built.
// [`DEBUGGER_ACCESS_REVERT`] is copy for a human to read; it is never spawned.
//
// Three facts, each a thin wrapper beside a pure classifier:
//
// * the rule's mode: [`probe_debugger`] runs `-status` through
//   [`run_bounded_exit`], behind the same gate and fence as [`probe_fda`];
//   [`classify_devtools_status`] reads it;
// * the account's membership: [`debugger_membership`] asks the directory
//   (`mbr_check_membership` resolves nested groups: every admin is a
//   `_developer` member through admin's GUID); [`classify_membership`] folds
//   the answers;
// * aterm's own hardening: [`self_hardening`] reads the image's code-signing
//   flags (`csops`); [`classify_cs_status`] reads them.
//
// Only `DevToolsSecurity -status` is spawned, and nothing reaches `tccd` or
// `WindowServer`.

/// Apple's switch for the debugger right, absolute: a tool that edits the
/// authorization database may not be resolved through `PATH`. Private, like
/// every argv built from it: outside this module the tool is reached only
/// through [`probe_debugger`] and [`run_devtools_change`].
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const DEVTOOLSSECURITY_PATH: &str = "/usr/sbin/DevToolsSecurity";

/// The one right `DevToolsSecurity` reads and writes (§1.3). The plain
/// `system.privilege.taskport` right — third-party debuggers' "Take Control"
/// dialog — is a different rule, and developer mode does not change it.
pub const TASKPORT_DEBUG_RIGHT: &str = "system.privilege.taskport.debug";

/// The one-line undo every surface that mentions developer mode prints (the
/// `UNIVERSAL_CONTROL_REVERT` convention, `crates/atpkg/src/machine.rs`). COPY
/// ONLY: this string is never spawned. The owner's *Turn Off Debugger
/// Access…* gesture runs the same argv through `devtools_disable_command`
/// and [`run_devtools_change`]. Run unprivileged it should raise macOS's
/// own administrator dialog (spike D2).
pub const DEBUGGER_ACCESS_REVERT: &str = "/usr/sbin/DevToolsSecurity -disable";

/// `-status` answers in about 10 ms (spike D1: `real 0.00`–`0.01` s over
/// three runs); an answer that has not arrived in this long is not coming.
#[cfg(target_os = "macos")]
const DEVTOOLS_STATUS_CEILING: std::time::Duration = std::time::Duration::from_secs(2);

/// The directory group `is-admin` names — the first rule developer mode tries.
pub const ADMIN_GROUP: &str = "admin";

/// The directory group `is-developer` names — the second rule.
pub const DEVELOPER_GROUP: &str = "_developer";

/// `CS_OPS_STATUS` — `csops`' "read the status flags" operation
/// (xnu `bsd/sys/codesign.h`). Pinned so [`classify_cs_status`] stays pure on
/// every platform.
pub const CS_OPS_STATUS: u32 = 0;

/// `CS_GET_TASK_ALLOW` — the image carries `get-task-allow`, so any debugger
/// may take its task port without a password (SDK
/// `Kernel.framework/Headers/kern/cs_blobs.h`).
pub const CS_GET_TASK_ALLOW: u32 = 0x0000_0004;

/// `CS_RUNTIME` — the hardened runtime, what `codesign` prints as
/// `0x10000(runtime)` (same header).
pub const CS_RUNTIME: u32 = 0x0001_0000;

/// What `DevToolsSecurity -status` says about the rule. The `mode=` token.
///
/// [`Self::Unrecognized`] and [`Self::Unknown`] are different answers:
/// `unrecognized` is Apple's tool saying it does not recognise the definition
/// (an administrator's rule, a changed OS default, a partial write — no copy
/// names a cause, because nothing measured supports one); `unknown` is aterm
/// not having an answer at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DebuggerMode {
    /// `Developer mode is currently enabled.`
    Enabled,
    /// `Developer mode is currently disabled.`
    Disabled,
    /// `Developer mode rights are currently in an unknown state.`
    Unrecognized,
    /// Not observed: refused, not run, or an answer this classifier does not
    /// claim to understand. Never rendered as `disabled`.
    #[default]
    Unknown,
}

impl DebuggerMode {
    /// The `mode=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Unrecognized => "unrecognized",
            Self::Unknown => "unknown",
        }
    }
}

/// Which developer-mode rule this account would match. The `member=` token.
///
/// Rules are tried in order and the first match wins (§1.4), so the answer
/// names the first group that holds: an admin is also a `_developer` member
/// (nested through admin's GUID) and reads [`Self::Admin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DebuggerMember {
    /// In [`ADMIN_GROUP`]: `is-admin`, which needs no authentication.
    Admin,
    /// Not an admin, but in [`DEVELOPER_GROUP`]: `is-developer`.
    Developer,
    /// In neither. Developer mode would still prompt this account
    /// (`authenticate-developer`).
    NotMember,
    /// Not determined. Never rendered as `no`.
    #[default]
    Unknown,
}

impl DebuggerMember {
    /// The `member=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Developer => "developer",
            Self::NotMember => "no",
            Self::Unknown => "unknown",
        }
    }
}

/// Whether the running aterm is protected from a same-user debugger: the
/// hardened runtime without `get-task-allow`. The `self_hardened=` token.
///
/// The notarized release reads [`Self::Hardened`]; `tools/dev-app.sh` and
/// `target/release` builds read [`Self::NotHardened`] (§1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SelfHardening {
    /// [`CS_RUNTIME`] set and [`CS_GET_TASK_ALLOW`] clear.
    Hardened,
    /// The flags were read and say otherwise.
    NotHardened,
    /// The flags could not be read, or this is not macOS.
    #[default]
    Unknown,
}

impl SelfHardening {
    /// The `self_hardened=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hardened => "yes",
            Self::NotHardened => "no",
            Self::Unknown => "unknown",
        }
    }
}

/// The debugger probe's progress or outcome. The `probe=` token. Kept apart
/// from [`DebuggerMode`] so an unfinished or refused observation can never be
/// read as a completed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DebuggerProbeLabel {
    /// No current result: an asynchronous observer is waiting or refreshing.
    Pending,
    /// `-status` exited and said something this classifier recognises —
    /// including `unrecognized`, which is a recognised answer about the rule.
    StatusOk,
    /// `-status` ran, but its answer is not one this classifier recognises.
    StatusUnrecognized,
    /// There is no `DevToolsSecurity` to run.
    ToolAbsent,
    /// The tool could not be started.
    SpawnFailed,
    /// The tool did not finish within its ceiling.
    TimedOut,
    /// Its output was over the cap or not UTF-8, or its exit could not be read.
    OutputUnreadable,
    /// Refused: `[privacy] enabled` or `[privacy] check` is off. Nothing was
    /// spawned.
    RefusedDisabled,
    /// Refused: the running executable is not inside a `.app` bundle (the same
    /// fence as [`probe_fda_with`]). Nothing was spawned.
    RefusedOutOfBundle,
    /// Refused: the running executable could not be resolved. Nothing was
    /// spawned.
    RefusedNoExe,
    /// Not macOS. There is no `DevToolsSecurity` here.
    UnsupportedPlatform,
}

impl DebuggerProbeLabel {
    /// The `probe=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::StatusOk => "status_ok",
            Self::StatusUnrecognized => "status_unrecognized",
            Self::ToolAbsent => "tool_absent",
            Self::SpawnFailed => "spawn_failed",
            Self::TimedOut => "timed_out",
            Self::OutputUnreadable => "output_unreadable",
            Self::RefusedDisabled => "refused_disabled",
            Self::RefusedOutOfBundle => "refused_out_of_bundle",
            Self::RefusedNoExe => "refused_no_exe",
            Self::UnsupportedPlatform => "unsupported_platform",
        }
    }

    /// `true` for a deliberate refusal to consult the tool, including an
    /// unsupported platform. These labels guarantee nothing was spawned.
    /// [`Self::Pending`] is not a refusal.
    #[must_use]
    pub const fn refused(self) -> bool {
        matches!(
            self,
            Self::RefusedDisabled
                | Self::RefusedOutOfBundle
                | Self::RefusedNoExe
                | Self::UnsupportedPlatform
        )
    }
}

/// One debugger probe: the rule's mode, the account's membership, and why.
///
/// It carries no timestamp: the slot that dispatches the probe stamps its own
/// completion, as the claimant census's does, so this crate reads no clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DebuggerProbe {
    /// What `-status` said, classified.
    pub mode: DebuggerMode,
    /// Which rule the account matches.
    pub member: DebuggerMember,
    /// The observation's progress, refusal, or completed outcome.
    pub label: DebuggerProbeLabel,
}

impl DebuggerProbe {
    /// No current observation. Neither an answer nor a refusal.
    #[must_use]
    pub const fn pending() -> Self {
        Self::refused(DebuggerProbeLabel::Pending)
    }

    /// Nothing learned: both facts [`DebuggerMode::Unknown`] /
    /// [`DebuggerMember::Unknown`], with the given reason.
    #[must_use]
    pub const fn refused(label: DebuggerProbeLabel) -> Self {
        Self {
            mode: DebuggerMode::Unknown,
            member: DebuggerMember::Unknown,
            label,
        }
    }
}

/// The answer on a platform with no `DevToolsSecurity`: the whole probe off
/// macOS, and compiled into every test build so it is tested everywhere.
#[cfg(any(test, not(target_os = "macos")))]
#[must_use]
pub const fn unsupported_debugger_probe() -> DebuggerProbe {
    DebuggerProbe::refused(DebuggerProbeLabel::UnsupportedPlatform)
}

/// The `-status` argv. RUNS NOTHING. A fixed array that always carries its
/// flag, because a bare `DevToolsSecurity` means `-enable` (§1.3). The only
/// `DevToolsSecurity` argv the observation path builds; the owner's gesture
/// builds the other two ([`devtools_enable_command`],
/// [`devtools_disable_command`]).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const fn devtools_status_command() -> [&'static str; 2] {
    [DEVTOOLSSECURITY_PATH, "-status"]
}

/// The `-enable` argv. RUNS NOTHING. Spelled with its flag even though a bare
/// invocation means the same, so no argv without a flag exists anywhere. Only
/// [`run_devtools_change`] spawns it, for [`DebuggerChange::On`], after the
/// owner's gesture (design §3.4).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const fn devtools_enable_command() -> [&'static str; 2] {
    [DEVTOOLSSECURITY_PATH, "-enable"]
}

/// The `-disable` argv. RUNS NOTHING. Only [`run_devtools_change`] spawns it,
/// for [`DebuggerChange::Off`]. The same line, as copy for a human, is
/// [`DEBUGGER_ACCESS_REVERT`].
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const fn devtools_disable_command() -> [&'static str; 2] {
    [DEVTOOLSSECURITY_PATH, "-disable"]
}

/// Which way the owner's gesture changes the debugger rule (design §3.4). The
/// press carries its intent — two action ids, never one toggle — so a render
/// and a press that disagree about the state can never flip its meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DebuggerChange {
    /// *Allow debugger access…*: `DevToolsSecurity -enable`.
    On,
    /// *Turn off debugger access…*: `DevToolsSecurity -disable`.
    Off,
}

impl DebuggerChange {
    /// The argv this change runs. RUNS NOTHING. Private: only the order
    /// ([`devtools_change_with`]) takes it, after its re-read.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    const fn command(self) -> [&'static str; 2] {
        match self {
            Self::On => devtools_enable_command(),
            Self::Off => devtools_disable_command(),
        }
    }

    /// The mode a fresh `-status` read must show for the change to run: `On`
    /// only over [`DebuggerMode::Disabled`], `Off` only over
    /// [`DebuggerMode::Enabled`]. Never [`DebuggerMode::Unrecognized`]: `-enable`
    /// would overwrite a definition Apple's tool does not recognise (§1.3).
    #[must_use]
    pub const fn precondition(self) -> DebuggerMode {
        match self {
            Self::On => DebuggerMode::Disabled,
            Self::Off => DebuggerMode::Enabled,
        }
    }

    /// The mode the change sets, when it lands. Only a later probe says whether
    /// it did: an exit code never does.
    #[must_use]
    pub const fn target(self) -> DebuggerMode {
        match self {
            Self::On => DebuggerMode::Enabled,
            Self::Off => DebuggerMode::Disabled,
        }
    }

    /// The direction token a marker records (`on` / `off`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }

    /// Read a direction token back ([`Self::as_str`]); anything else is `None`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "on" => Some(Self::On),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// How one bounded run of a tool ended ([`run_bounded_exit`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ToolRun {
    /// It exited within its ceiling and both streams were read whole.
    /// `code` is `None` when a signal ended it.
    Exited {
        /// The exit code, or `None` for a signal.
        code: Option<i32>,
        /// Everything it wrote to stdout.
        stdout: String,
        /// Everything it wrote to stderr.
        stderr: String,
    },
    /// There is no program at the path.
    Absent,
    /// It could not be started, or its output could not be collected.
    SpawnFailed,
    /// It outlasted the ceiling and was killed, or it exited but a holder of
    /// its output pipes kept them open past the ceiling (the readers are left
    /// detached, and nothing is killed).
    TimedOut,
    /// A stream was over the cap or not UTF-8, or the exit could not be read:
    /// an answer read in part is not read.
    Unreadable,
}

/// The adverbs `DevToolsSecurity` fills its `%s%s` with: its only such strings
/// are `currently`, `already` and `now`, and the slot may be empty. `now`
/// (presumably after a write) still claims the mode, so it reads like
/// `currently`.
const DEVTOOLS_ADVERBS: [&str; 4] = ["", "currently", "already", "now"];

/// One line of `DevToolsSecurity` output, as a mode. Pure.
///
/// Only `Developer mode is currently disabled.` has been captured from a live
/// run (§1.1, spike D1). The enabled and unknown-state lines are derived from
/// the binary's format strings — `Developer mode is%s%s enabled.` and
/// `Developer mode rights are%s%s in an unknown state.` — with the adverb slot
/// taken from [`DEVTOOLS_ADVERBS`]; spike D2 captures the enabled line as a
/// fixture. Surrounding whitespace is ignored. Anything else, a negation or a
/// new adverb included, is `None`.
fn devtools_status_line(line: &str) -> Option<DebuggerMode> {
    let line = line.trim();
    let adverb_ok = |middle: &str| DEVTOOLS_ADVERBS.contains(&middle.trim());
    if let Some(rest) = line.strip_prefix("Developer mode rights are") {
        return rest
            .strip_suffix(" in an unknown state.")
            .filter(|middle| adverb_ok(middle))
            .map(|_| DebuggerMode::Unrecognized);
    }
    let rest = line.strip_prefix("Developer mode is")?;
    if let Some(middle) = rest.strip_suffix(" enabled.") {
        return adverb_ok(middle).then_some(DebuggerMode::Enabled);
    }
    if let Some(middle) = rest.strip_suffix(" disabled.") {
        return adverb_ok(middle).then_some(DebuggerMode::Disabled);
    }
    None
}

/// Classify one `DevToolsSecurity -status` run. Pure.
///
/// * `enabled` / `disabled` need exit code `0` as well as the line (the
///   measured `disabled` run exited `0`); the same line with any other code is
///   a tool in a state nobody measured, and reads [`DebuggerMode::Unknown`].
/// * `rights are … in an unknown state` is [`DebuggerMode::Unrecognized`]
///   whatever the code: the exit status of that path was never measured, and
///   the line is the tool's whole claim.
/// * No recognised line, or lines that disagree, is [`DebuggerMode::Unknown`]
///   with [`DebuggerProbeLabel::StatusUnrecognized`] — never `Disabled`.
/// * A run that did not exit maps to its failure label.
///
/// Lines are read from both streams: which one the tool writes is not part of
/// its contract.
#[must_use]
pub fn classify_devtools_status(run: &ToolRun) -> (DebuggerMode, DebuggerProbeLabel) {
    let (code, stdout, stderr) = match run {
        ToolRun::Exited {
            code,
            stdout,
            stderr,
        } => (*code, stdout, stderr),
        ToolRun::Absent => return (DebuggerMode::Unknown, DebuggerProbeLabel::ToolAbsent),
        ToolRun::SpawnFailed => return (DebuggerMode::Unknown, DebuggerProbeLabel::SpawnFailed),
        ToolRun::TimedOut => return (DebuggerMode::Unknown, DebuggerProbeLabel::TimedOut),
        ToolRun::Unreadable => {
            return (DebuggerMode::Unknown, DebuggerProbeLabel::OutputUnreadable);
        }
    };
    let mut said = stdout
        .lines()
        .chain(stderr.lines())
        .filter_map(devtools_status_line);
    let unrecognized = (
        DebuggerMode::Unknown,
        DebuggerProbeLabel::StatusUnrecognized,
    );
    let Some(first) = said.next() else {
        return unrecognized;
    };
    if said.any(|mode| mode != first) {
        return unrecognized;
    }
    match (first, code) {
        (DebuggerMode::Unrecognized, _)
        | (DebuggerMode::Enabled | DebuggerMode::Disabled, Some(0)) => {
            (first, DebuggerProbeLabel::StatusOk)
        }
        _ => unrecognized,
    }
}

/// Fold the two directory answers, asked in rule order. Pure.
///
/// `admin` is `is-admin`'s group and `developer` is `is-developer`'s; each is
/// `None` when the directory could not answer. `developer` is not consulted
/// once `admin` is `Some(true)` — the first matching rule wins — and an
/// unanswered `admin` leaves the whole answer [`DebuggerMember::Unknown`]:
/// every admin is also a `_developer` member, so `developer` alone cannot say
/// which rule would match.
#[must_use]
pub const fn classify_membership(admin: Option<bool>, developer: Option<bool>) -> DebuggerMember {
    match (admin, developer) {
        (Some(true), _) => DebuggerMember::Admin,
        (Some(false), Some(true)) => DebuggerMember::Developer,
        (Some(false), Some(false)) => DebuggerMember::NotMember,
        (Some(false), None) | (None, _) => DebuggerMember::Unknown,
    }
}

/// The running user's developer-mode membership: [`ADMIN_GROUP`] first, then —
/// only when that answer is "no" — [`DEVELOPER_GROUP`].
///
/// Each group's gid comes from `getgrnam_r` (no gid is written here), and each
/// check is `mbr_check_membership`, which asks the directory and so resolves
/// nested groups; `getgroups` reads the process's credential and would not. It
/// is libSystem talking to `opendirectoryd`: no spawn, no `tccd`, no
/// `WindowServer`, no dialog. Off macOS it is [`DebuggerMember::Unknown`].
#[must_use]
pub fn debugger_membership() -> DebuggerMember {
    #[cfg(target_os = "macos")]
    {
        let admin = imp::in_group(ADMIN_GROUP);
        let developer = match admin {
            Some(false) => imp::in_group(DEVELOPER_GROUP),
            Some(true) | None => None,
        };
        classify_membership(admin, developer)
    }
    #[cfg(not(target_os = "macos"))]
    {
        DebuggerMember::Unknown
    }
}

/// Classify the running image's code-signing status flags. Pure.
///
/// `None` (the flags could not be read) is [`SelfHardening::Unknown`];
/// [`CS_RUNTIME`] without [`CS_GET_TASK_ALLOW`] is [`SelfHardening::Hardened`];
/// any other readable flags are [`SelfHardening::NotHardened`].
#[must_use]
pub const fn classify_cs_status(flags: Option<u32>) -> SelfHardening {
    match flags {
        None => SelfHardening::Unknown,
        Some(flags) if flags & CS_RUNTIME != 0 && flags & CS_GET_TASK_ALLOW == 0 => {
            SelfHardening::Hardened
        }
        Some(_) => SelfHardening::NotHardened,
    }
}

/// Whether the running aterm is hardened against a same-user debugger.
///
/// One `csops(getpid(), CS_OPS_STATUS)` — a syscall about our own image, with
/// no spawn, no `tccd`, no `WindowServer` and no dialog. Not cached here (this
/// crate keeps lazy-init statics off paths a GUI main thread can reach): the
/// two bits it reads are fixed at exec, so a caller reads it once. Off macOS it
/// is [`SelfHardening::Unknown`].
#[must_use]
pub fn self_hardening() -> SelfHardening {
    #[cfg(target_os = "macos")]
    {
        classify_cs_status(imp::own_cs_status())
    }
    #[cfg(not(target_os = "macos"))]
    {
        SelfHardening::Unknown
    }
}

/// The debugger probe, with the executable path, the spawn and the membership
/// query all injected — the seam every refusal is enforced at, and the one a
/// test uses to prove a refusal spawns nothing.
///
/// Refuses, calling neither closure, when the gate is closed
/// ([`DebuggerProbeLabel::RefusedDisabled`]), when there is no executable
/// ([`DebuggerProbeLabel::RefusedNoExe`]), or when it is not inside a `.app`
/// ([`DebuggerProbeLabel::RefusedOutOfBundle`], [`probe_fda_with`]'s fence).
/// There is no HOME refusal: `DevToolsSecurity` reads no home directory.
/// Otherwise `spawn` is called exactly once, with `devtools_status_command`,
/// and `membership` exactly once.
#[cfg(any(target_os = "macos", test))]
pub fn probe_debugger_with<S, M>(
    gate: ProbeGate,
    exe: Option<&Path>,
    spawn: S,
    membership: M,
) -> DebuggerProbe
where
    S: FnOnce(&[&str]) -> ToolRun,
    M: FnOnce() -> DebuggerMember,
{
    if !gate.permits() {
        return DebuggerProbe::refused(DebuggerProbeLabel::RefusedDisabled);
    }
    let Some(exe) = exe else {
        return DebuggerProbe::refused(DebuggerProbeLabel::RefusedNoExe);
    };
    // The fence, shared with the Full Disk Access probe: a binary outside a
    // `.app` — a test binary under target/debug/deps above all — spawns nothing.
    if !path_is_in_app_bundle(exe) {
        return DebuggerProbe::refused(DebuggerProbeLabel::RefusedOutOfBundle);
    }
    let (mode, label) = classify_devtools_status(&spawn(&devtools_status_command()));
    DebuggerProbe {
        mode,
        member: membership(),
        label,
    }
}

/// Probe the debugger rule for the running process: at most one
/// `DevToolsSecurity -status`, within a 2 s ceiling, plus the membership query.
///
/// Refuses — spawning nothing — when the gate is off or the running executable
/// is not inside a `.app` bundle. Off macOS it is
/// [`unsupported_debugger_probe`].
#[must_use]
pub fn probe_debugger(gate: ProbeGate) -> DebuggerProbe {
    #[cfg(target_os = "macos")]
    {
        let exe = fence_exe();
        probe_debugger_with(
            gate,
            exe.as_deref(),
            |argv| match argv.split_first() {
                Some((program, args)) => {
                    let args: Vec<&std::ffi::OsStr> =
                        args.iter().map(std::ffi::OsStr::new).collect();
                    run_bounded_exit(
                        program,
                        &args,
                        DEVTOOLS_STATUS_CEILING,
                        TOOL_OUTPUT_MAX_BYTES,
                    )
                }
                None => ToolRun::SpawnFailed,
            },
            debugger_membership,
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = gate;
        unsupported_debugger_probe()
    }
}

// ---------------------------------------------------------------------------
// Developer Tools list (kTCCServiceDeveloperTool) — observation
// ---------------------------------------------------------------------------
//
// `docs/DESIGN-macos-debugger-access-2026-09-29.md` §9.1. macOS's Privacy &
// Security ▸ Developer Tools list is a TCC service with no dialog: only a human
// adds an app, in System Settings. On the first run of each new program under
// aterm, syspolicyd asks tccd whether the responsible app is on that list, and
// tccd logs its answer for that app's identity (`Sub:{<bundle id>}`): 519
// answers in 8.5 minutes on 2026-10-07, every one `Auth Right: Unknown (None)`
// for an identity with no Developer Tools row in the system TCC.db.
// [`probe_devtool`] reads those answers back with ONE bounded `/usr/bin/log
// show` whose argv is a constant ([`DEVTOOL_LOG_ARGS`]); the subject is
// matched here, in Rust, and never reaches an argv. No TCC.db is opened, so
// Full Disk Access plays no part, and the answer is tccd's effective decision
// for aterm's bundle id, never a mirror of the System Settings switch.
//
// Any program can choose the text tccd logs about it: `binary_path=` carries
// the new program's path verbatim. Measured 2026-10-07: in the `compact` style
// a path holding a newline followed by a forged record prints as two records,
// and a path near `PATH_MAX` is cut with `<…>` and leaves tccd's own answer as
// `<decode: missing data>`. So [`classify_devtool_log`] reads the `ndjson`
// style, whose framing JSON-escapes the message, accepts only records from
// tccd's own image with tccd's own format string, and recognises an answer
// only in tccd's own suffix, ending the message. Anything else makes the whole
// probe `unknown` with [`DevToolProbeLabel::FormatUnrecognized`]: the worst a
// planted path, or a name that is not aterm's bundle id, can force is
// `unknown`, never `allowed`.
//
// A program that carries aterm's EXACT bundle id is the same `Sub:{…}`, and
// its answers count as aterm's (design §9.1, O10): a copy that does not meet
// the code requirement the list holds for aterm can make the row read
// `unlisted` or `denied` while aterm is allowed, or `allowed` if that copy is
// the one on the list. Requiring `responsible_path=` inside the running bundle
// waits on spike D13's apply step, which records that path across an in-place
// apply: the apply renames the bundle under the running process, so the check
// could reject aterm's own answers.

/// The unified log's tool, named ABSOLUTELY: a bare `log` is a zsh builtin
/// that shadows it, exits 0 and prints nothing, the artifact that once
/// produced a false "tccd is invisible" finding. The one spelling: the GUI's
/// consent observer re-exports it.
pub const LOG_TOOL: &str = "/usr/bin/log";

/// The directory whose readability decides whether this account can read the
/// unified log at all: `root:admin drwxr-x---` on macOS, so a standard account
/// gets `EACCES`. Ordinary Unix permissions, no TCC service, no dialog.
pub const DIAGNOSTICS_DIR: &str = "/var/db/diagnostics";

/// Whether this account can read the unified log's backing store: the
/// admin-group test on [`DIAGNOSTICS_DIR`], one `opendir`.
#[cfg(target_os = "macos")]
#[must_use]
pub fn log_store_readable() -> bool {
    std::fs::read_dir(DIAGNOSTICS_DIR).is_ok()
}

/// How far back [`probe_devtool`] reads tccd's answers, in seconds. Must match
/// the `--last` argument of [`DEVTOOL_LOG_ARGS`] (a test pins the two).
pub const DEVTOOL_WINDOW_S: u64 = 600;

/// Where a person adds aterm to the Developer Tools list, in words. The one
/// spelling: the window's *Open Developer Tools…* route says it beside the pane
/// it opens, and `aterm doctor`'s `devtools:` row prints it as the fix.
pub const DEVTOOLS_SETTINGS_WORDS: &str =
    "System Settings \u{25b8} Privacy & Security \u{25b8} Developer Tools";

/// The Developer Tools list through the modern Settings extension, at its
/// registered `Privacy_DevTools` anchor (debugger design §9.1 O14). The one
/// spelling: the window's menu opens it, and `aterm doctor` prints it for
/// `open`. Opening it only navigates; nothing in aterm adds itself to the list.
/// The scheme is `x-apple.systempreferences`, with a dot.
pub const DEVTOOLS_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_DevTools";

/// The same pane through its legacy pane id, which System Settings maps to the
/// extension: the window's second try, and what `aterm doctor`'s `open` prints
/// and runs (the spelling the owner gave, 2026-10-14). Opening it only navigates.
pub const DEVTOOLS_SETTINGS_URL_LEGACY: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_DevTools";

/// The constant predicate: tccd's Developer Tools answers, for every subject.
/// The subject is never interpolated here; [`classify_devtool_log`] matches it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const DEVTOOL_PREDICATE: &str = "process == \"tccd\" AND eventMessage BEGINSWITH \"Handling access \
                                 request to kTCCServiceDeveloperTool, from Sub:{\"";

/// The one argv the probe runs after [`LOG_TOOL`]. RUNS NOTHING by itself.
/// `--info` because tccd logs these at Info level; `ndjson` because its
/// framing escapes the newline a program can put in its own path; UTC so a
/// timestamp converts without a time-zone lookup.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const DEVTOOL_LOG_ARGS: [&str; 10] = [
    "show",
    "--last",
    "10m",
    "--info",
    "--style",
    "ndjson",
    "--timezone",
    "UTC",
    "--predicate",
    DEVTOOL_PREDICATE,
];

/// A 10-minute read measured 0.86 s wall at load 4 and 0.87 s at load 25–40
/// (2026-10-07, 255 and 234 answers); an earlier spike measured about 2.6 s at
/// load 190. An answer that has not arrived in this long is not coming.
#[cfg(target_os = "macos")]
const DEVTOOL_CEILING: std::time::Duration = std::time::Duration::from_secs(8);

/// The most of one stream the probe reads. An `ndjson` answer measured about
/// 1,270 bytes (2026-10-07), and syspolicyd's first-run checks measured serial
/// at about 180 ms each (a median, not a floor), so a 10-minute window holds
/// about 3,300 answers, about 4.2 MB. Output over this cap is `output_unreadable`, never a
/// partial count.
#[cfg(target_os = "macos")]
const DEVTOOL_OUTPUT_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// tccd's image, the only process whose records are read.
const TCCD_IMAGE_PATH: &str = "/System/Library/PrivateFrameworks/TCC.framework/Support/tccd";

/// tccd's format string for an access request: three public arguments, the
/// service, the subject and responsible process, and the result.
const TCCD_ACCESS_FORMAT: &str =
    "Handling access request to %{public}@, from %{public}@, %{public}@";

/// The start of every message the predicate selects, up to the subject.
const DEVTOOL_MESSAGE_PREFIX: &str =
    "Handling access request to kTCCServiceDeveloperTool, from Sub:{";

/// tccd's reason words, from its own strings (`strings tccd`, macOS 26.6.2).
/// An answer whose reason is not one of these is not recognised.
const TCCD_REASONS: [&str; 12] = [
    "None",
    "System Set",
    "User Set",
    "Service Policy",
    "MDM Policy",
    "Override Policy",
    "Entitled",
    "App Type Policy",
    "Prompt Cancel",
    "Prompt Timeout",
    "Preflight Unknown",
    "User Consent",
];

/// tccd's `DB Action:` words, from the same strings.
const TCCD_DB_ACTIONS: [&str; 3] = ["None", "Update", "Retry"];

/// What tccd's newest Developer Tools answer for aterm's identity said. The
/// `devtools state=` token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DevToolState {
    /// `Auth Right: Allowed (<reason>)`.
    Allowed,
    /// `Auth Right: Denied (<reason>)`, for any reason in tccd's table. Which
    /// listed state that is (switched off, or refused by policy) is unmeasured
    /// until spike D13.
    Denied,
    /// `Auth Right: Unknown (None)`: tccd's measured answer while aterm is not
    /// on the list. The one shape captured live (2026-10-07).
    Unlisted,
    /// No answer could be read. Never rendered as `unlisted`.
    #[default]
    Unknown,
}

impl DevToolState {
    /// The `state=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Unlisted => "unlisted",
            Self::Unknown => "unknown",
        }
    }
}

/// The Developer Tools probe's progress or outcome. The `probe=` token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DevToolProbeLabel {
    /// No current result: an asynchronous observer is waiting or refreshing.
    Pending,
    /// tccd's log was read whole and held at least one answer for the subject.
    Observed,
    /// tccd's log was read whole and held no answer for the subject in the
    /// window: no new program started under aterm, so macOS gave none.
    NoRecentRequest,
    /// The log store could not be read: [`DIAGNOSTICS_DIR`] is not readable by
    /// this account (not an administrator). Decided before any spawn.
    LogUnavailable,
    /// `log` ran on an account that can read the store, and exited non-zero or
    /// by a signal.
    LogFailed,
    /// The log answered in a shape this classifier does not recognise.
    FormatUnrecognized,
    /// `log` did not finish within its ceiling.
    TimedOut,
    /// `log` could not be started.
    SpawnFailed,
    /// Its output was over the cap or not UTF-8, or its exit could not be read.
    OutputUnreadable,
    /// Refused: `[privacy] enabled` or `[privacy] check` is off. Nothing ran.
    RefusedDisabled,
    /// Refused: `[privacy] observer = false`, which stops aterm reading tccd's
    /// log. Nothing ran.
    RefusedObserverOff,
    /// Refused: the running executable is not inside a `.app` bundle (the same
    /// fence as [`probe_fda_with`]). Nothing ran.
    RefusedOutOfBundle,
    /// Refused: the running executable could not be resolved. Nothing ran.
    RefusedNoExe,
    /// Refused: there is no usable bundle identifier to match. Nothing ran.
    RefusedNoSubject,
    /// Not macOS: there is no such list here.
    UnsupportedPlatform,
}

impl DevToolProbeLabel {
    /// The `probe=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Observed => "observed",
            Self::NoRecentRequest => "no_recent_request",
            Self::LogUnavailable => "log_unavailable",
            Self::LogFailed => "log_failed",
            Self::FormatUnrecognized => "format_unrecognized",
            Self::TimedOut => "timed_out",
            Self::SpawnFailed => "spawn_failed",
            Self::OutputUnreadable => "output_unreadable",
            Self::RefusedDisabled => "refused_disabled",
            Self::RefusedObserverOff => "refused_observer_off",
            Self::RefusedOutOfBundle => "refused_out_of_bundle",
            Self::RefusedNoExe => "refused_no_exe",
            Self::RefusedNoSubject => "refused_no_subject",
            Self::UnsupportedPlatform => "unsupported_platform",
        }
    }

    /// `true` for a deliberate refusal to read the log, including an
    /// unsupported platform. These labels guarantee nothing was spawned.
    /// [`Self::Pending`] is not a refusal.
    #[must_use]
    pub const fn refused(self) -> bool {
        matches!(
            self,
            Self::RefusedDisabled
                | Self::RefusedObserverOff
                | Self::RefusedOutOfBundle
                | Self::RefusedNoExe
                | Self::RefusedNoSubject
                | Self::UnsupportedPlatform
        )
    }
}

/// One Developer Tools probe: tccd's newest answer for the subject, how many
/// answers the window held, and why.
///
/// It carries no reading of the clock: `answered_at_ms` is tccd's own
/// timestamp, and the slot that dispatches the probe stamps its completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DevToolProbe {
    /// The newest answer, classified.
    pub state: DevToolState,
    /// The observation's progress, refusal, or completed outcome.
    pub label: DevToolProbeLabel,
    /// Answers for the subject in the window. `None` unless the log was read
    /// whole (`observed` or `no_recent_request`).
    pub requests: Option<u32>,
    /// Of those, how many were not `Allowed`: new programs macOS checked
    /// before letting them start.
    pub not_allowed: Option<u32>,
    /// The newest answer's UTC time, in unix milliseconds.
    pub answered_at_ms: Option<i64>,
}

impl DevToolProbe {
    /// No current observation. Neither an answer nor a refusal.
    #[must_use]
    pub const fn pending() -> Self {
        Self::refused(DevToolProbeLabel::Pending)
    }

    /// Nothing learned, for `label`'s reason.
    #[must_use]
    pub const fn refused(label: DevToolProbeLabel) -> Self {
        Self {
            state: DevToolState::Unknown,
            label,
            requests: None,
            not_allowed: None,
            answered_at_ms: None,
        }
    }
}

/// The answer on a platform with no such list: the whole probe off macOS, and
/// compiled into every test build so it is tested everywhere.
#[cfg(any(test, not(target_os = "macos")))]
#[must_use]
pub const fn unsupported_devtool_probe() -> DevToolProbe {
    DevToolProbe::refused(DevToolProbeLabel::UnsupportedPlatform)
}

/// Whether `subject` may be matched against tccd's `Sub:{…}`: a usable bundle
/// id ([`bundle_id_is_usable`]) made only of the characters Apple allows in
/// one (ASCII letters, digits, `.` and `-`), so it can contain neither `}` nor
/// anything else that ends tccd's field.
#[must_use]
pub fn devtool_subject_is_usable(subject: &str) -> bool {
    bundle_id_is_usable(subject)
        && subject
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// One record's answer for `subject`, from tccd's message. Pure.
///
/// `Ok(None)` is another subject's answer. `Err(())` is a message this does not
/// recognise: a subject line must hold exactly one `}Resp:{` (tccd writes one,
/// right after the subject) and exactly one `ReqResult(`, preceded by `}, `
/// and followed by `Auth Right: <value> (<reason>), promptType: <n>,DB
/// Action:<word>[, UpdateVerifierData])` to the END of the message, with both
/// words from tccd's tables, and no truncation marker anywhere. Only `Unknown
/// (None)` is read as `unlisted`; any other `Unknown`, and `Limited`, are not
/// recognised.
///
/// The `}Resp:{` count is what keeps another app's answer from being read as
/// aterm's: an app may name itself `<subject>}Resp:{x` (a bundle id, or a
/// codesign identifier, which `codesign -i` accepted on a scratch copy,
/// 2026-10-07), and its `Sub:{…}` then starts with the subject. tccd's own
/// `}Resp:{` after such a name makes two. A name that closes the Sub early
/// and adds no `}Resp:{` of its own (`<subject>}x`) leaves only tccd's one, so
/// the count passes it: the `Resp:{` that must follow the first `}` is what
/// refuses that name. A program whose id IS the subject is not told apart:
/// its answers are read as aterm's (see the section note).
fn devtool_answer(message: &str, subject: &str) -> Result<Option<DevToolState>, ()> {
    let rest = message.strip_prefix(DEVTOOL_MESSAGE_PREFIX).ok_or(())?;
    let (sub, after) = rest.split_once('}').ok_or(())?;
    if sub != subject {
        return Ok(None);
    }
    if !after.starts_with("Resp:{") {
        return Err(());
    }
    // A record the log cut short: the part that is left can end in a planted
    // answer (measured 2026-10-07 with a 1,016-byte path).
    if message.contains("<decode:") || message.contains("<\u{2026}>") {
        return Err(());
    }
    if message.matches("}Resp:{").count() != 1 || message.matches("ReqResult(").count() != 1 {
        return Err(());
    }
    let (_, result) = message.split_once("}, ReqResult(Auth Right: ").ok_or(())?;
    let (answer, tail) = result.split_once(", promptType: ").ok_or(())?;
    let (value, reason) = answer
        .strip_suffix(')')
        .and_then(|answer| answer.split_once(" ("))
        .ok_or(())?;
    if !TCCD_REASONS.contains(&reason) {
        return Err(());
    }
    let state = match (value, reason) {
        ("Allowed", _) => DevToolState::Allowed,
        ("Denied", _) => DevToolState::Denied,
        ("Unknown", "None") => DevToolState::Unlisted,
        _ => return Err(()),
    };
    let tail = tail.strip_suffix(')').ok_or(())?;
    let tail = tail.strip_suffix(", UpdateVerifierData").unwrap_or(tail);
    let (prompt_type, action) = tail.split_once(",DB Action:").ok_or(())?;
    if prompt_type.is_empty()
        || !prompt_type.bytes().all(|b| b.is_ascii_digit())
        || !TCCD_DB_ACTIONS.contains(&action)
    {
        return Err(());
    }
    Ok(Some(state))
}

/// Days from 1970-01-01 to the civil date `(y, m, d)` in the proleptic
/// Gregorian calendar (H. Hinnant's `days_from_civil`). Pure; the caller has
/// range-checked the fields.
const fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The number of days in month `m` of year `y`.
const fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// A fixed-width run of ASCII digits as a number, or `None`.
fn digits(text: &str, width: usize) -> Option<i64> {
    (text.len() == width && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// `log`'s `ndjson` timestamp (`2026-10-07 13:58:21.820241+0000`) as unix
/// milliseconds, UTC. Pure: the offset is applied by arithmetic, so no
/// time-zone database is read. Anything out of range is `None`.
#[must_use]
pub fn devtool_timestamp_ms(text: &str) -> Option<i64> {
    let (date, time) = text.split_once(' ')?;
    let mut date = date.split('-');
    let (y, m, d) = (
        digits(date.next()?, 4)?,
        digits(date.next()?, 2)?,
        digits(date.next()?, 2)?,
    );
    if date.next().is_some() || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    let split = time.len().checked_sub(5)?;
    let (clock, offset) = (time.get(..split)?, time.get(split..)?);
    let sign = match offset.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let (off_h, off_m) = (digits(offset.get(1..3)?, 2)?, digits(offset.get(3..)?, 2)?);
    if off_h > 23 || off_m > 59 {
        return None;
    }
    let (hms, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut hms = hms.split(':');
    let (hh, mm, ss) = (
        digits(hms.next()?, 2)?,
        digits(hms.next()?, 2)?,
        digits(hms.next()?, 2)?,
    );
    if hms.next().is_some() || hh > 23 || mm > 59 || ss > 59 {
        return None;
    }
    if clock.contains('.')
        && (fraction.is_empty()
            || fraction.len() > 9
            || !fraction.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let millis = fraction
        .bytes()
        .chain(std::iter::repeat(b'0'))
        .take(3)
        .fold(0_i64, |acc, b| acc * 10 + i64::from(b - b'0'));
    let local_s = days_from_civil(y, m, d) * 86_400 + hh * 3_600 + mm * 60 + ss;
    let utc_s = local_s - sign * (off_h * 3_600 + off_m * 60);
    Some(utc_s * 1_000 + millis)
}

/// How old an answer stamped `answered_at_ms` is at `now_ms`, both unix
/// milliseconds. Pure; a stamp in the future is age 0.
#[must_use]
pub const fn devtool_answer_age_ms(answered_at_ms: i64, now_ms: i64) -> u64 {
    let age = now_ms.saturating_sub(answered_at_ms);
    if age < 0 { 0 } else { age.unsigned_abs() }
}

/// Classify one `log show` run for `subject`. Pure.
///
/// * A run that did not exit with code 0 maps to its failure label; a non-zero
///   exit or a signal is [`DevToolProbeLabel::LogFailed`]. It is never
///   [`DevToolProbeLabel::LogUnavailable`], which the store check alone
///   decides, before the spawn.
/// * Every line must be one JSON object: a tccd record (`logEvent`, tccd's
///   image as process and sender, user 0, subsystem `com.apple.TCC`, category
///   `access`, tccd's access format string, a message and a timestamp), or the
///   closing `{"count":<n>,"finished":1}`, which must come last and count the
///   records. A record for `subject` must pass [`devtool_answer`]. Anything
///   else is [`DevToolProbeLabel::FormatUnrecognized`] for the WHOLE probe.
/// * The newest of the subject's answers decides the state; `requests` and
///   `not_allowed` count them all. None in the window is `unknown` with
///   [`DevToolProbeLabel::NoRecentRequest`] and counts of zero.
#[must_use]
pub fn classify_devtool_log(run: &ToolRun, subject: &str) -> DevToolProbe {
    let stdout = match run {
        ToolRun::Exited {
            code: Some(0),
            stdout,
            ..
        } => stdout,
        ToolRun::Exited { .. } => return DevToolProbe::refused(DevToolProbeLabel::LogFailed),
        ToolRun::Absent | ToolRun::SpawnFailed => {
            return DevToolProbe::refused(DevToolProbeLabel::SpawnFailed);
        }
        ToolRun::TimedOut => return DevToolProbe::refused(DevToolProbeLabel::TimedOut),
        ToolRun::Unreadable => return DevToolProbe::refused(DevToolProbeLabel::OutputUnreadable),
    };
    let unrecognized = DevToolProbe::refused(DevToolProbeLabel::FormatUnrecognized);
    let mut records: u64 = 0;
    let mut finished = false;
    let mut requests: u32 = 0;
    let mut not_allowed: u32 = 0;
    // (timestamp, the answer); a later line wins a tie.
    let mut newest: Option<(i64, DevToolState)> = None;
    for line in stdout.lines() {
        if finished {
            return unrecognized;
        }
        let Some(fields) = json::object_line(line) else {
            return unrecognized;
        };
        if let Some(count) = devtool_trailer(&fields) {
            if count != records {
                return unrecognized;
            }
            finished = true;
            continue;
        }
        let Some((message, timestamp)) = tccd_record(&fields) else {
            return unrecognized;
        };
        records += 1;
        let state = match devtool_answer(message, subject) {
            Err(()) => return unrecognized,
            Ok(None) => continue,
            Ok(Some(state)) => state,
        };
        let Some(at) = devtool_timestamp_ms(timestamp) else {
            return unrecognized;
        };
        requests = requests.saturating_add(1);
        if state != DevToolState::Allowed {
            not_allowed = not_allowed.saturating_add(1);
        }
        if newest.is_none_or(|(seen, _)| at >= seen) {
            newest = Some((at, state));
        }
    }
    if !finished {
        return unrecognized;
    }
    match newest {
        None => DevToolProbe {
            requests: Some(0),
            not_allowed: Some(0),
            ..DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest)
        },
        Some((at, state)) => DevToolProbe {
            state,
            label: DevToolProbeLabel::Observed,
            requests: Some(requests),
            not_allowed: Some(not_allowed),
            answered_at_ms: Some(at),
        },
    }
}

/// `log show --style ndjson`'s closing object, `{"count":<n>,"finished":1}`:
/// the record count it reports, or `None` when `fields` is not exactly that.
fn devtool_trailer(fields: &[(String, json::Value)]) -> Option<u64> {
    if fields.len() != 2 || json::uint(fields, "finished")? != 1 {
        return None;
    }
    json::uint(fields, "count")
}

/// A record tccd itself wrote with its access format, as `(message,
/// timestamp)`, or `None`. The image paths are the kernel's account of who
/// logged, so a process that merely calls itself `tccd` is not read.
fn tccd_record(fields: &[(String, json::Value)]) -> Option<(&str, &str)> {
    let is = |key: &str, want: &str| json::str(fields, key) == Some(want);
    let from_tccd = is("eventType", "logEvent")
        && is("processImagePath", TCCD_IMAGE_PATH)
        && is("senderImagePath", TCCD_IMAGE_PATH)
        && is("subsystem", "com.apple.TCC")
        && is("category", "access")
        && is("formatString", TCCD_ACCESS_FORMAT)
        && json::uint(fields, "userID") == Some(0);
    if !from_tccd {
        return None;
    }
    Some((
        json::str(fields, "eventMessage")?,
        json::str(fields, "timestamp")?,
    ))
}

/// The Developer Tools probe, with the executable, the log store's readability
/// and the spawn injected — the seam every refusal is enforced at, and the one
/// a test uses to prove a refusal spawns nothing.
///
/// Refuses, calling neither closure, when the gate is closed
/// ([`DevToolProbeLabel::RefusedDisabled`]), when there is no executable
/// ([`DevToolProbeLabel::RefusedNoExe`]), when it is not inside a `.app`
/// ([`DevToolProbeLabel::RefusedOutOfBundle`], [`probe_fda_with`]'s fence), or
/// when `subject` is not [`devtool_subject_is_usable`]
/// ([`DevToolProbeLabel::RefusedNoSubject`]). Then an unreadable log store is
/// [`DevToolProbeLabel::LogUnavailable`] without a spawn. Otherwise `spawn` is
/// called exactly once, with [`LOG_TOOL`] and [`DEVTOOL_LOG_ARGS`].
#[cfg(any(target_os = "macos", test))]
pub fn probe_devtool_with<R, S>(
    gate: ProbeGate,
    exe: Option<&Path>,
    subject: &str,
    store_readable: R,
    spawn: S,
) -> DevToolProbe
where
    R: FnOnce() -> bool,
    S: FnOnce(&str, &[&str]) -> ToolRun,
{
    if !gate.permits() {
        return DevToolProbe::refused(DevToolProbeLabel::RefusedDisabled);
    }
    let Some(exe) = exe else {
        return DevToolProbe::refused(DevToolProbeLabel::RefusedNoExe);
    };
    // The fence, shared with the Full Disk Access probe: a binary outside a
    // `.app` (a test binary under target/debug/deps above all) spawns nothing.
    if !path_is_in_app_bundle(exe) {
        return DevToolProbe::refused(DevToolProbeLabel::RefusedOutOfBundle);
    }
    if !devtool_subject_is_usable(subject) {
        return DevToolProbe::refused(DevToolProbeLabel::RefusedNoSubject);
    }
    if !store_readable() {
        return DevToolProbe::refused(DevToolProbeLabel::LogUnavailable);
    }
    classify_devtool_log(&spawn(LOG_TOOL, &DEVTOOL_LOG_ARGS), subject)
}

/// Read tccd's Developer Tools answers for `subject`, the running bundle's
/// identifier: at most one `/usr/bin/log show`, within an 8 s ceiling and an
/// 8 MiB output cap.
///
/// Refuses, spawning nothing, when the gate is off, the running executable is
/// not inside a `.app` bundle, or `subject` is unusable. Off macOS it is
/// [`unsupported_devtool_probe`].
#[must_use]
pub fn probe_devtool(gate: ProbeGate, subject: &str) -> DevToolProbe {
    #[cfg(target_os = "macos")]
    {
        let exe = fence_exe();
        probe_devtool_with(
            gate,
            exe.as_deref(),
            subject,
            log_store_readable,
            |program, args| {
                let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
                run_bounded_exit(program, &args, DEVTOOL_CEILING, DEVTOOL_OUTPUT_MAX_BYTES)
            },
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (gate, subject);
        unsupported_devtool_probe()
    }
}

// ---------------------------------------------------------------------------
// Developer Tools list — the switch, read from the system TCC.db
// ---------------------------------------------------------------------------
//
// Owner ruling 2026-10-14 (debugger design §9.1, O18): "fix this everywhere
// and it needs to be in aterm and doctor". tccd's log ([`probe_devtool`])
// answers only after a new program starts, only within its 10-minute window
// and only to an administrator, so a quiet machine or a standard account reads
// `unknown` there. The switch the owner flips is a row of the SYSTEM TCC.db
// ([`SYSTEM_TCC_DB`]): service `kTCCServiceDeveloperTool`, the app's bundle id
// as `client` (`client_type` 0), `auth_value` 2 for on and 0 for off, and its
// `last_modified` in unix seconds (schema read off macOS 26.6.2, 2026-10-14).
//
// [`probe_devtool_list`] reads it without a prompt and without writing: ONE
// `open(…, O_RDONLY)` first, the call [`probe_fda`] makes (a responsible app
// without Full Disk Access gets `EPERM`, and `kTCCServiceSystemPolicyAllFiles`
// cannot prompt), then ONE bounded `/usr/bin/sqlite3` with a constant argv
// ([`DEVTOOL_DB_ARGS`]): read-only, no `~/.sqliterc` (`-init /dev/null`), safe
// mode, no symbolic link, and every client hex-encoded so no app's name can
// forge a row. The subject is matched here, in Rust, never put into an argv.
// sqlite opens the file read-only and takes a reader's lock only; the database
// is in rollback-journal mode (`journal_mode` `delete`), so no `-wal` or
// `-shm` file is created beside it.
//
// The row is the SWITCH, not tccd's decision: it holds the code requirement of
// the copy that was added, and a copy that does not meet it is not answered
// as the switch says. So [`devtool_grant`] lets tccd's own answer decide when
// it is newer than the row's last change, and the switch otherwise.

/// The system TCC database: the switches of Privacy & Security's lists that
/// belong to the Mac rather than to one account, Developer Tools among them.
/// Root-owned; a process can open it only while its responsible app holds Full
/// Disk Access.
pub const SYSTEM_TCC_DB: &str = "/Library/Application Support/com.apple.TCC/TCC.db";

/// The reader, named absolutely: Apple's own `sqlite3`, a platform binary
/// (macOS does not check it before it starts).
pub const SQLITE_TOOL: &str = "/usr/bin/sqlite3";

/// The constant query: every Developer Tools row, its client hex-encoded.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const DEVTOOL_DB_QUERY: &str = "SELECT hex(client), client_type, auth_value, last_modified \
                                FROM access WHERE service = 'kTCCServiceDeveloperTool' \
                                ORDER BY rowid;";

/// The one argv the list probe runs after [`SQLITE_TOOL`]. RUNS NOTHING by
/// itself. `-init /dev/null` because the shell otherwise reads `~/.sqliterc`,
/// which could change the output mode or run commands; `-safe` refuses every
/// command that touches another file; `-bail` exits non-zero on the first
/// error; one `|`-separated line per row, no header.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const DEVTOOL_DB_ARGS: [&str; 13] = [
    "-init",
    "/dev/null",
    "-readonly",
    "-safe",
    "-nofollow",
    "-batch",
    "-bail",
    "-noheader",
    "-list",
    "-separator",
    "|",
    SYSTEM_TCC_DB,
    DEVTOOL_DB_QUERY,
];

/// The read measured 10 ms (2026-10-14, load 14). A lock tccd holds makes
/// sqlite answer `SQLITE_BUSY` at once (no busy timeout is set), so the
/// ceiling bounds only a wedged process.
#[cfg(target_os = "macos")]
const DEVTOOL_DB_CEILING: std::time::Duration = std::time::Duration::from_secs(5);

/// What the Developer Tools list holds for the subject. The `list=` token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DevToolListing {
    /// A row with `auth_value` 2: listed, switched on.
    On,
    /// A row with `auth_value` 0: listed, switched off.
    Off,
    /// The list was read whole and holds no row for the subject.
    Absent,
    /// Not read, or read in a shape this classifier does not recognise. Never
    /// rendered as `absent`.
    #[default]
    Unknown,
}

impl DevToolListing {
    /// The `list=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
            Self::Absent => "absent",
            Self::Unknown => "unknown",
        }
    }
}

/// The list probe's progress or outcome. The `list_probe=` token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DevToolListLabel {
    /// No current result: an asynchronous reader is waiting or refreshing.
    Pending,
    /// The database was read whole.
    Read,
    /// `open` answered `EPERM`: the responsible app does not hold Full Disk
    /// Access, so macOS keeps the database closed to it. Nothing was spawned.
    NoFullDiskAccess,
    /// `open` failed with another errno. Nothing was spawned.
    OpenFailed,
    /// `sqlite3` exited non-zero or by a signal (a schema it does not know, a
    /// lock, a damaged file).
    ToolFailed,
    /// `sqlite3` answered in a shape this classifier does not recognise.
    FormatUnrecognized,
    /// `sqlite3` did not finish within its ceiling.
    TimedOut,
    /// `sqlite3` could not be started.
    SpawnFailed,
    /// Its output was over the cap or not UTF-8, or its exit could not be read.
    OutputUnreadable,
    /// Refused: `[privacy] enabled` or `[privacy] check` is off. Nothing ran.
    RefusedDisabled,
    /// Refused: `[privacy] observer = false`, under which the window reads
    /// neither half of the Developer Tools row. Nothing ran.
    RefusedObserverOff,
    /// Refused: the running executable is not inside a `.app` bundle (the
    /// fence [`probe_fda_with`] keeps). Nothing ran.
    RefusedOutOfBundle,
    /// Refused: the running executable could not be resolved. Nothing ran.
    RefusedNoExe,
    /// Refused: there is no usable bundle identifier to match. Nothing ran.
    RefusedNoSubject,
    /// Not macOS: there is no such list here.
    UnsupportedPlatform,
}

impl DevToolListLabel {
    /// The `list_probe=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Read => "read",
            Self::NoFullDiskAccess => "no_full_disk_access",
            Self::OpenFailed => "open_failed",
            Self::ToolFailed => "sqlite_failed",
            Self::FormatUnrecognized => "format_unrecognized",
            Self::TimedOut => "timed_out",
            Self::SpawnFailed => "spawn_failed",
            Self::OutputUnreadable => "output_unreadable",
            Self::RefusedDisabled => "refused_disabled",
            Self::RefusedObserverOff => "refused_observer_off",
            Self::RefusedOutOfBundle => "refused_out_of_bundle",
            Self::RefusedNoExe => "refused_no_exe",
            Self::RefusedNoSubject => "refused_no_subject",
            Self::UnsupportedPlatform => "unsupported_platform",
        }
    }

    /// `true` for a deliberate refusal, including an unsupported platform:
    /// nothing was opened and nothing spawned. [`Self::Pending`] is not one.
    #[must_use]
    pub const fn refused(self) -> bool {
        matches!(
            self,
            Self::RefusedDisabled
                | Self::RefusedObserverOff
                | Self::RefusedOutOfBundle
                | Self::RefusedNoExe
                | Self::RefusedNoSubject
                | Self::UnsupportedPlatform
        )
    }
}

/// One read of the Developer Tools list for one subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DevToolList {
    /// The subject's row, classified.
    pub listing: DevToolListing,
    /// Why it reads so.
    pub label: DevToolListLabel,
    /// The row's `last_modified`, unix seconds: when the switch last changed.
    /// `None` without a row.
    pub modified_at_s: Option<i64>,
    /// How many apps the list holds, the subject's row included. `None` unless
    /// it was read whole. `Some(0)` is a Mac whose Settings shows no
    /// Developer Tools pane yet (`spctl developer-mode enable-terminal` adds
    /// Terminal to the list, and the pane appears with it).
    pub rows: Option<u32>,
}

impl DevToolList {
    /// No current reading.
    #[must_use]
    pub const fn pending() -> Self {
        Self::refused(DevToolListLabel::Pending)
    }

    /// Nothing learned, for `label`'s reason.
    #[must_use]
    pub const fn refused(label: DevToolListLabel) -> Self {
        Self {
            listing: DevToolListing::Unknown,
            label,
            modified_at_s: None,
            rows: None,
        }
    }
}

/// The list's answer on a platform with no such list: the whole probe off
/// macOS, and compiled into every test build so it is tested everywhere.
#[cfg(any(test, not(target_os = "macos")))]
#[must_use]
pub const fn unsupported_devtool_list() -> DevToolList {
    DevToolList::refused(DevToolListLabel::UnsupportedPlatform)
}

/// One `sqlite3` row, `HEX(client)|client_type|auth_value|last_modified`, as
/// `(client, client_type, auth_value, last_modified)`. `None` for any other
/// shape: four fields, an even run of upper-case hex digits (sqlite's `hex()`),
/// and three decimal integers. A client that is not UTF-8 decodes lossily; it
/// cannot equal a subject made of ASCII ([`devtool_subject_is_usable`]).
fn devtool_db_row(line: &str) -> Option<(String, u32, u32, i64)> {
    let mut fields = line.split('|');
    let (hex, kind, auth, modified) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    if fields.next().is_some() || hex.len() % 2 != 0 {
        return None;
    }
    let decimal = |text: &str| {
        (!text.is_empty() && text.len() <= 18 && text.bytes().all(|b| b.is_ascii_digit()))
            .then(|| text.parse::<i64>().ok())
            .flatten()
    };
    let nibble = |b: u8| match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    };
    let bytes = hex
        .as_bytes()
        .chunks(2)
        .map(|pair| Some(nibble(pair[0])? << 4 | nibble(pair[1])?))
        .collect::<Option<Vec<u8>>>()?;
    Some((
        String::from_utf8_lossy(&bytes).into_owned(),
        u32::try_from(decimal(kind)?).ok()?,
        u32::try_from(decimal(auth)?).ok()?,
        decimal(modified)?,
    ))
}

/// Classify one `sqlite3` run for `subject`. Pure.
///
/// * A run that did not exit with code 0 maps to its failure label.
/// * Every line must be a row [`devtool_db_row`] reads; anything else is
///   [`DevToolListLabel::FormatUnrecognized`] for the WHOLE read.
/// * The subject's row is the one whose client is exactly `subject` with
///   `client_type` 0 (a bundle id). None is [`DevToolListing::Absent`]; more
///   than one, or an `auth_value` other than 2 or 0, is not recognised.
#[must_use]
pub fn classify_devtool_db(run: &ToolRun, subject: &str) -> DevToolList {
    let stdout = match run {
        ToolRun::Exited {
            code: Some(0),
            stdout,
            ..
        } => stdout,
        ToolRun::Exited { .. } => return DevToolList::refused(DevToolListLabel::ToolFailed),
        ToolRun::Absent | ToolRun::SpawnFailed => {
            return DevToolList::refused(DevToolListLabel::SpawnFailed);
        }
        ToolRun::TimedOut => return DevToolList::refused(DevToolListLabel::TimedOut),
        ToolRun::Unreadable => return DevToolList::refused(DevToolListLabel::OutputUnreadable),
    };
    let unrecognized = DevToolList::refused(DevToolListLabel::FormatUnrecognized);
    let mut ours: Option<(u32, i64)> = None;
    let mut rows: u32 = 0;
    for line in stdout.lines() {
        let Some((client, kind, auth, modified)) = devtool_db_row(line) else {
            return unrecognized;
        };
        rows = rows.saturating_add(1);
        if kind != 0 || client != subject {
            continue;
        }
        if ours.replace((auth, modified)).is_some() {
            return unrecognized;
        }
    }
    let Some((auth, modified)) = ours else {
        return DevToolList {
            listing: DevToolListing::Absent,
            label: DevToolListLabel::Read,
            modified_at_s: None,
            rows: Some(rows),
        };
    };
    let listing = match auth {
        2 => DevToolListing::On,
        0 => DevToolListing::Off,
        _ => return unrecognized,
    };
    DevToolList {
        listing,
        label: DevToolListLabel::Read,
        modified_at_s: Some(modified),
        rows: Some(rows),
    }
}

/// The list probe, with the executable, the `open` and the spawn injected —
/// the seam every refusal is enforced at, and the one a test uses to prove a
/// refusal opens and spawns nothing.
///
/// Refuses, calling neither closure, when the gate is closed, when there is no
/// executable, when it is not inside a `.app` ([`probe_fda_with`]'s fence), or
/// when `subject` is not [`devtool_subject_is_usable`]. Then `open_db` is
/// called once with [`SYSTEM_TCC_DB`]: `EPERM` is
/// [`DevToolListLabel::NoFullDiskAccess`] and any other errno
/// [`DevToolListLabel::OpenFailed`], each without a spawn. Otherwise `spawn`
/// is called exactly once, with [`SQLITE_TOOL`] and [`DEVTOOL_DB_ARGS`].
#[cfg(any(target_os = "macos", test))]
pub fn probe_devtool_list_with<O, S>(
    gate: ProbeGate,
    exe: Option<&Path>,
    subject: &str,
    open_db: O,
    spawn: S,
) -> DevToolList
where
    O: FnOnce(&Path) -> ProbeOutcome,
    S: FnOnce(&str, &[&str]) -> ToolRun,
{
    if !gate.permits() {
        return DevToolList::refused(DevToolListLabel::RefusedDisabled);
    }
    let Some(exe) = exe else {
        return DevToolList::refused(DevToolListLabel::RefusedNoExe);
    };
    if !path_is_in_app_bundle(exe) {
        return DevToolList::refused(DevToolListLabel::RefusedOutOfBundle);
    }
    if !devtool_subject_is_usable(subject) {
        return DevToolList::refused(DevToolListLabel::RefusedNoSubject);
    }
    match open_db(Path::new(SYSTEM_TCC_DB)) {
        ProbeOutcome::Ok => classify_devtool_db(&spawn(SQLITE_TOOL, &DEVTOOL_DB_ARGS), subject),
        ProbeOutcome::Errno(ERRNO_EPERM) => {
            DevToolList::refused(DevToolListLabel::NoFullDiskAccess)
        }
        ProbeOutcome::Errno(_) => DevToolList::refused(DevToolListLabel::OpenFailed),
    }
}

/// Read the Developer Tools list's row for `subject`, the running bundle's
/// identifier: one `open` of [`SYSTEM_TCC_DB`] and at most one
/// `/usr/bin/sqlite3`, within a 5 s ceiling and a 1 MiB output cap.
///
/// Refuses, opening and spawning nothing, when the gate is off, the running
/// executable is not inside a `.app` bundle, or `subject` is unusable. Off
/// macOS it is [`unsupported_devtool_list`].
#[must_use]
pub fn probe_devtool_list(gate: ProbeGate, subject: &str) -> DevToolList {
    #[cfg(target_os = "macos")]
    {
        let exe = fence_exe();
        probe_devtool_list_with(
            gate,
            exe.as_deref(),
            subject,
            imp::open_probe,
            |program, args| {
                let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
                run_bounded_exit(program, &args, DEVTOOL_DB_CEILING, TOOL_OUTPUT_MAX_BYTES)
            },
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (gate, subject);
        unsupported_devtool_list()
    }
}

/// Whether the app is on the Developer Tools list, all evidence joined. The
/// `grant=` token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DevToolGrant {
    /// Listed and switched on, or tccd allowed it, with nothing newer saying
    /// otherwise: macOS should not hold the programs it starts for its
    /// first-run check.
    Granted,
    /// Not listed, switched off, or tccd's newer answer did not allow it.
    NotGranted,
    /// Neither the list nor tccd's log could be read. Never reported as fine.
    #[default]
    Unknown,
}

impl DevToolGrant {
    /// The `grant=` token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::NotGranted => "not_granted",
            Self::Unknown => "unknown",
        }
    }
}

/// Join the list's row and tccd's newest answer into one verdict. Pure.
///
/// * The row is the switch NOW, so an absent row decides `not_granted`
///   whatever an older answer said (the owner may just have removed aterm).
/// * A row that is on or off decides unless tccd answered after the row last
///   changed: then tccd's answer, its effective decision for this identity,
///   decides (a copy the row's requirement does not match, or a policy).
/// * Without a read row, tccd's observed answer decides; without either,
///   `unknown`.
#[must_use]
pub fn devtool_grant(list: &DevToolList, log: &DevToolProbe) -> DevToolGrant {
    let tccd = match (log.label, log.state) {
        (DevToolProbeLabel::Observed, DevToolState::Allowed) => {
            Some((log.answered_at_ms, DevToolGrant::Granted))
        }
        (DevToolProbeLabel::Observed, DevToolState::Denied | DevToolState::Unlisted) => {
            Some((log.answered_at_ms, DevToolGrant::NotGranted))
        }
        _ => None,
    };
    let switch = match (list.label, list.listing) {
        (DevToolListLabel::Read, DevToolListing::On) => Some(DevToolGrant::Granted),
        (DevToolListLabel::Read, DevToolListing::Off | DevToolListing::Absent) => {
            Some(DevToolGrant::NotGranted)
        }
        _ => None,
    };
    // An absent row has no time of change: it is the switch as of this read.
    if switch.is_some() && list.listing == DevToolListing::Absent {
        return DevToolGrant::NotGranted;
    }
    match (switch, tccd) {
        (Some(switch), Some((answered_at_ms, verdict))) => {
            let changed_ms = list.modified_at_s.map(|s| s.saturating_mul(1_000));
            match (answered_at_ms, changed_ms) {
                (Some(at), Some(changed)) if at > changed => verdict,
                _ => switch,
            }
        }
        (Some(switch), None) => switch,
        (None, Some((_, verdict))) => verdict,
        (None, None) => DevToolGrant::Unknown,
    }
}

/// `CFBundleIdentifier` of the bundle at `root`, read as `CFBundle` reads it,
/// or `None`: the bundle or its `Info.plist` lies under a protected root (as
/// spelled or once resolved), or names no identifier. No `codesign`.
#[must_use]
pub fn bundle_id_of(root: &Path, protected: &[PathBuf]) -> Option<String> {
    let plist = root.join("Contents").join("Info.plist");
    if !census_may_read(root, protected) || !census_may_read(&plist, protected) {
        return None;
    }
    read_info_plist(&plist).and_then(|info| info.bundle_id)
}

/// A strict reader for one line of `log show --style ndjson`: one JSON value
/// per line (RFC 8259), objects kept as ordered `(key, value)` pairs. It
/// rejects what a strict parser rejects (raw control characters in a string, a
/// lone surrogate, a trailing byte) and, beyond that, a duplicate key at the
/// top level and nesting deeper than 16 levels.
mod json {
    /// One JSON value, as far as the probe reads it: numbers keep their text,
    /// so nothing is rounded, and an array, a boolean and `null` are checked
    /// for shape and then not kept.
    #[derive(Debug)]
    pub(super) enum Value {
        Number(String),
        Str(String),
        Object(Vec<(String, Self)>),
        Other,
    }

    /// The deepest nesting read; `log`'s records nest three levels.
    const MAX_DEPTH: usize = 16;

    /// The fields of a line that is exactly one JSON object with no repeated
    /// key, else `None`.
    pub(super) fn object_line(line: &str) -> Option<Vec<(String, Value)>> {
        let mut reader = Reader {
            bytes: line.as_bytes(),
            at: 0,
        };
        reader.space();
        let value = reader.value(0)?;
        reader.space();
        if reader.at != reader.bytes.len() {
            return None;
        }
        let Value::Object(fields) = value else {
            return None;
        };
        let unique = fields
            .iter()
            .enumerate()
            .all(|(i, (key, _))| fields[..i].iter().all(|(seen, _)| seen != key));
        unique.then_some(fields)
    }

    /// The string field `key`, if present and a string.
    pub(super) fn str<'a>(fields: &'a [(String, Value)], key: &str) -> Option<&'a str> {
        match fields.iter().find(|(k, _)| k == key)? {
            (_, Value::Str(text)) => Some(text),
            _ => None,
        }
    }

    /// The field `key` as a non-negative integer written without a fraction,
    /// an exponent or a leading zero, if present.
    pub(super) fn uint(fields: &[(String, Value)], key: &str) -> Option<u64> {
        match fields.iter().find(|(k, _)| k == key)? {
            (_, Value::Number(text))
                if text.bytes().all(|b| b.is_ascii_digit())
                    && (text == "0" || !text.starts_with('0')) =>
            {
                text.parse().ok()
            }
            _ => None,
        }
    }

    struct Reader<'a> {
        bytes: &'a [u8],
        at: usize,
    }

    impl Reader<'_> {
        fn peek(&self) -> Option<u8> {
            self.bytes.get(self.at).copied()
        }

        fn space(&mut self) {
            while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
                self.at += 1;
            }
        }

        fn eat(&mut self, word: &[u8]) -> Option<()> {
            let end = self.at.checked_add(word.len())?;
            (self.bytes.get(self.at..end)? == word).then(|| self.at = end)
        }

        fn value(&mut self, depth: usize) -> Option<Value> {
            if depth > MAX_DEPTH {
                return None;
            }
            match self.peek()? {
                b'{' => self.object(depth + 1),
                b'[' => self.array(depth + 1),
                b'"' => self.string().map(Value::Str),
                b't' => self.eat(b"true").map(|()| Value::Other),
                b'f' => self.eat(b"false").map(|()| Value::Other),
                b'n' => self.eat(b"null").map(|()| Value::Other),
                b'-' | b'0'..=b'9' => self.number().map(Value::Number),
                _ => None,
            }
        }

        fn object(&mut self, depth: usize) -> Option<Value> {
            self.eat(b"{")?;
            let mut fields = Vec::new();
            self.space();
            if self.eat(b"}").is_some() {
                return Some(Value::Object(fields));
            }
            loop {
                self.space();
                let key = self.string()?;
                self.space();
                self.eat(b":")?;
                self.space();
                let value = self.value(depth)?;
                fields.push((key, value));
                self.space();
                if self.eat(b",").is_some() {
                    continue;
                }
                self.eat(b"}")?;
                return Some(Value::Object(fields));
            }
        }

        fn array(&mut self, depth: usize) -> Option<Value> {
            self.eat(b"[")?;
            self.space();
            if self.eat(b"]").is_some() {
                return Some(Value::Other);
            }
            loop {
                self.space();
                self.value(depth)?;
                self.space();
                if self.eat(b",").is_some() {
                    continue;
                }
                self.eat(b"]")?;
                return Some(Value::Other);
            }
        }

        fn hex4(&mut self) -> Option<u32> {
            let end = self.at.checked_add(4)?;
            let text = std::str::from_utf8(self.bytes.get(self.at..end)?).ok()?;
            if !text.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            self.at = end;
            u32::from_str_radix(text, 16).ok()
        }

        fn string(&mut self) -> Option<String> {
            self.eat(b"\"")?;
            let mut out = String::new();
            loop {
                let start = self.at;
                while let Some(b) = self.peek() {
                    if b == b'"' || b == b'\\' || b < 0x20 {
                        break;
                    }
                    self.at += 1;
                }
                // The input is a `&str` and the run stops only at ASCII, so it
                // is whole UTF-8.
                out.push_str(std::str::from_utf8(self.bytes.get(start..self.at)?).ok()?);
                match self.peek()? {
                    b'"' => {
                        self.at += 1;
                        return Some(out);
                    }
                    b'\\' => {
                        self.at += 1;
                        let escape = self.peek()?;
                        self.at += 1;
                        let ch = match escape {
                            b'"' => '"',
                            b'\\' => '\\',
                            b'/' => '/',
                            b'b' => '\u{8}',
                            b'f' => '\u{c}',
                            b'n' => '\n',
                            b'r' => '\r',
                            b't' => '\t',
                            b'u' => {
                                let unit = self.hex4()?;
                                let code = if (0xD800..0xDC00).contains(&unit) {
                                    self.eat(b"\\u")?;
                                    let low = self.hex4()?;
                                    if !(0xDC00..0xE000).contains(&low) {
                                        return None;
                                    }
                                    0x1_0000 + ((unit - 0xD800) << 10) + (low - 0xDC00)
                                } else {
                                    unit
                                };
                                char::from_u32(code)?
                            }
                            _ => return None,
                        };
                        out.push(ch);
                    }
                    // A raw control character: not JSON.
                    _ => return None,
                }
            }
        }

        fn number(&mut self) -> Option<String> {
            let start = self.at;
            let _ = self.eat(b"-");
            let run = |reader: &mut Self| {
                let from = reader.at;
                while matches!(reader.peek(), Some(b'0'..=b'9')) {
                    reader.at += 1;
                }
                reader.at - from
            };
            let int_start = self.at;
            if run(self) == 0
                || (self.bytes.get(int_start) == Some(&b'0') && self.at - int_start > 1)
            {
                return None;
            }
            if self.eat(b".").is_some() && run(self) == 0 {
                return None;
            }
            if matches!(self.peek(), Some(b'e' | b'E')) {
                self.at += 1;
                if matches!(self.peek(), Some(b'+' | b'-')) {
                    self.at += 1;
                }
                if run(self) == 0 {
                    return None;
                }
            }
            std::str::from_utf8(self.bytes.get(start..self.at)?)
                .ok()
                .map(str::to_owned)
        }
    }
}

// ---------------------------------------------------------------------------
// Debugger access — the owner's change (P1)
// ---------------------------------------------------------------------------
//
// Design §3.4 step 3 (owner ruling O7 = the gesture, 2026-09-29). The one place
// a `DevToolsSecurity -enable`/`-disable` process is constructed is
// [`run_devtools_change`], and it runs only what the owner confirmed in aterm's
// own alert: the GUI's live gesture arm calls it on a detached worker, and
// headless instances and every test wire an inert arm that calls nothing. It
// re-reads `-status` first and spawns ONLY over the change's precondition, so a
// rule changed since the page rendered (an administrator's, an MDM's) is left
// alone. It runs the change UNPRIVILEGED — no `sudo`, no `osascript`, no
// authorization of its own — so macOS should raise its own administrator dialog,
// which aterm never sees or answers. It waits for the child with NO deadline
// (that dialog has shown no timeout) and never kills it. Only a later probe
// says what the rule is: the exit code it returns is evidence for a log line,
// never a claim (`DebuggerAccessChange`'s `ClaimIsObserved`).

/// The most of each stream a change keeps: `DevToolsSecurity` prints one line,
/// and a head is for a log line, never parsed into a verdict.
pub const DEVTOOLS_CHANGE_HEAD_BYTES: usize = 4 * 1024;

/// How long the readers of an EXITED change's pipes are waited for, in 10 ms
/// polls (2 s): a pipe some stranger inherited can outlive the tool (the
/// fd-hygiene note in [`run_bounded_exit`]), and a head is not worth a wedged
/// worker. Readers still running then are left detached, and their head is
/// reported empty.
#[cfg(all(any(target_os = "macos", test), not(target_arch = "wasm32")))]
const CHANGE_READER_GRACE_POLLS: u32 = 200;

/// The kernel's birth record of a process: its start time
/// (`proc_pidinfo(PROC_PIDTBSDINFO)`'s `pbi_start_tvsec`/`pbi_start_tvusec`).
/// A pid plus its birth names one process; a pid alone may be reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessBirth {
    /// Seconds since the epoch.
    pub seconds: u64,
    /// Microseconds within that second.
    pub microseconds: u64,
}

/// The birth record of the process at `pid` right now, or `None` when the
/// kernel gives none (no such process, not ours to read, a record about some
/// other pid). One `proc_pidinfo` — no spawn, no `tccd`, no `WindowServer`.
/// `None` proves nothing either way. Off macOS it is always `None`.
#[must_use]
pub fn process_birth(pid: u32) -> Option<ProcessBirth> {
    #[cfg(target_os = "macos")]
    {
        imp::process_birth(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

/// The `DevToolsSecurity` change child, as the `privacy-debugger-change`
/// marker records it (design §3.4 step 3.3): its pid and its birth, read right
/// after the spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DevtoolsChild {
    /// The child's pid.
    pub pid: u32,
    /// Its birth record, `None` when the kernel gave none at spawn.
    pub birth: Option<ProcessBirth>,
}

impl DevtoolsChild {
    /// Whether the recorded child is still the process at its pid: its birth
    /// was read at spawn and reads the same now. A child whose birth could not
    /// be read at spawn is never proven running, so a marker naming one is
    /// stale (design §3.4 step 6). A child that exited but is not yet reaped
    /// still reads as running; the worker waiting on it reaps it.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.birth.is_some() && process_birth(self.pid) == self.birth
    }
}

/// How one [`run_devtools_change`] ended.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DevtoolsChangeRun {
    /// The fresh `-status` read did not show the change's precondition
    /// ([`DebuggerChange::precondition`]), or was refused: NOTHING was
    /// spawned. `observed` is that read, for the caller's cache.
    NotDue {
        /// The re-read that stopped it.
        observed: DebuggerProbe,
    },
    /// The child ran and exited. `code` is `None` when a signal ended it; each
    /// head is at most [`DEVTOOLS_CHANGE_HEAD_BYTES`] of its stream, read
    /// lossily (empty when its reader could not start or did not finish in
    /// time). What the rule is now, only a probe says.
    Exited {
        /// The exit code, or `None` for a signal.
        code: Option<i32>,
        /// The head of its stdout.
        stdout_head: String,
        /// The head of its stderr.
        stderr_head: String,
    },
    /// There is no program at the path: nothing ran.
    Absent,
    /// It could not be started: nothing ran.
    SpawnFailed,
    /// It was started, but its exit could not be read. It was not killed.
    WaitFailed,
    /// Not macOS: there is no `DevToolsSecurity` here, and nothing ran.
    Unsupported,
}

impl DevtoolsChangeRun {
    /// Whether a change child was started (so macOS may have shown its
    /// dialog, and the rule may have changed).
    #[must_use]
    pub const fn spawned(&self) -> bool {
        matches!(self, Self::Exited { .. } | Self::WaitFailed)
    }
}

/// The change's order, with the re-read and the spawn injected — the seam a
/// test drives without a process, and the one [`run_devtools_change`] runs
/// with the live arms. `recheck` is called exactly once; `spawn` is called at
/// most once, with `change`'s own argv, and only when the re-read shows the
/// change's precondition (design §3.4 step 3.1). A refused re-read reads
/// [`DebuggerMode::Unknown`], which is no precondition, so a closed gate or a
/// binary outside a `.app` spawns nothing here either.
///
/// It constructs no process itself. Private: a shipped caller outside this
/// module has only [`run_devtools_change`], which always re-reads first.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn devtools_change_with<R, S>(change: DebuggerChange, recheck: R, spawn: S) -> DevtoolsChangeRun
where
    R: FnOnce() -> DebuggerProbe,
    S: FnOnce(&[&str]) -> DevtoolsChangeRun,
{
    let observed = recheck();
    if observed.mode != change.precondition() {
        return DevtoolsChangeRun::NotDue { observed };
    }
    spawn(&change.command())
}

/// `devtools_change_with` for aterm-gui's Tier-1 bind, which drives the real
/// order with a scripted read and a stand-in child. Only the `testing`
/// feature (a dev-dependency's, never a shipped build's) compiles it.
#[cfg(feature = "testing")]
#[doc(hidden)]
pub fn devtools_change_with_for_test<R, S>(
    change: DebuggerChange,
    recheck: R,
    spawn: S,
) -> DevtoolsChangeRun
where
    R: FnOnce() -> DebuggerProbe,
    S: FnOnce(&[&str]) -> DevtoolsChangeRun,
{
    devtools_change_with(change, recheck, spawn)
}

/// Run `argv` to its exit with stdin `/dev/null`, keeping at most
/// [`DEVTOOLS_CHANGE_HEAD_BYTES`] of each output stream, and waiting with NO
/// deadline: the child is never killed. `on_spawn` is called once with the
/// child's pid, after the spawn and before the wait — where the caller writes
/// its marker. Not public: [`run_devtools_change`] is the only caller outside
/// tests, which drive it with `/bin/sh` stand-ins.
#[cfg(all(any(target_os = "macos", test), not(target_arch = "wasm32")))]
fn run_to_exit_keeping_heads<F: FnOnce(u32)>(argv: &[&str], on_spawn: F) -> DevtoolsChangeRun {
    use aterm_uds::spawnfd::Stdio;
    /// Read `pipe` to its end, keeping its head. Reading on past the head (and
    /// discarding) keeps a chatty child from blocking on a full pipe.
    fn keep_head(
        pipe: impl std::io::Read + Send + 'static,
    ) -> Option<std::thread::JoinHandle<Vec<u8>>> {
        // `Builder::spawn`: a thread the OS refuses is an empty head, never a
        // panic in the worker. The refused closure drops the pipe with it.
        std::thread::Builder::new()
            .spawn(move || {
                let mut pipe = pipe;
                let mut head = Vec::new();
                let mut chunk = [0_u8; 1024];
                loop {
                    match pipe.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            let room = DEVTOOLS_CHANGE_HEAD_BYTES.saturating_sub(head.len());
                            head.extend_from_slice(&chunk[..n.min(room)]);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => break,
                    }
                }
                head
            })
            .ok()
    }
    let Some((program, flags)) = argv.split_first() else {
        return DevtoolsChangeRun::SpawnFailed;
    };
    let mut child = match aterm_uds::spawnfd::command(program)
        .args(flags)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return DevtoolsChangeRun::Absent;
        }
        Err(_) => return DevtoolsChangeRun::SpawnFailed,
    };
    on_spawn(child.id());
    let out = child.stdout.take().and_then(keep_head);
    let err = child.stderr.take().and_then(keep_head);
    // NO deadline, and never a kill: macOS's administrator dialog has shown no
    // timeout, and the taskport dialog it resembles sat for 12 hours. `wait`
    // retries EINTR itself.
    let Ok(status) = child.wait() else {
        return DevtoolsChangeRun::WaitFailed;
    };
    let mut polls = 0;
    while polls < CHANGE_READER_GRACE_POLLS
        && !(out
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
            && err
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished))
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
        polls += 1;
    }
    let head = |reader: Option<std::thread::JoinHandle<Vec<u8>>>| -> String {
        reader
            .filter(std::thread::JoinHandle::is_finished)
            .and_then(|reader| reader.join().ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    };
    DevtoolsChangeRun::Exited {
        code: status.code(),
        stdout_head: head(out),
        stderr_head: head(err),
    }
}

/// Change the debugger rule the owner's way: THE one place a `DevToolsSecurity`
/// change process is constructed (design §3.4 step 3); the argv builders are
/// private to this module, so no other crate can build one.
///
/// 1. Re-read `-status` through [`probe_debugger`] — behind `gate` and the
///    in-bundle fence, so a test binary, or a closed gate, spawns nothing — and
///    return [`DevtoolsChangeRun::NotDue`] unless it shows
///    [`DebuggerChange::precondition`].
/// 2. Spawn `DebuggerChange::command` UNPRIVILEGED with stdin `/dev/null`;
///    macOS should raise its own administrator dialog (spike D2).
/// 3. Call `on_spawn` with the child's pid and birth, before the wait, so the
///    caller can write the `privacy-debugger-change` marker a successor reads.
/// 4. Wait with NO deadline, never killing the child, and return its exit code
///    and output heads.
///
/// It BLOCKS for as long as macOS's dialog is up — hours, if nobody answers —
/// so it is called only from a detached worker, never the event loop. The
/// child is its own process: an aterm that exits or hands off mid-dialog
/// leaves it running. Off macOS it is [`DevtoolsChangeRun::Unsupported`], and
/// `on_spawn` is never called.
pub fn run_devtools_change<F: FnOnce(DevtoolsChild)>(
    change: DebuggerChange,
    gate: ProbeGate,
    on_spawn: F,
) -> DevtoolsChangeRun {
    #[cfg(target_os = "macos")]
    {
        devtools_change_with(
            change,
            || probe_debugger(gate),
            |argv| {
                run_to_exit_keeping_heads(argv, |pid| {
                    on_spawn(DevtoolsChild {
                        pid,
                        birth: process_birth(pid),
                    });
                })
            },
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (change, gate, on_spawn);
        DevtoolsChangeRun::Unsupported
    }
}

// ---------------------------------------------------------------------------
// Attribution and the posture join
// ---------------------------------------------------------------------------

/// Whether a session's file access is attributed to the live aterm process or
/// to one that has exited.
///
/// This is aterm's own adoption record — the successor's adoption path knows
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

/// Which blocking measurements from design §7 have been made.
///
/// Every field is `false`/`Unknown`: S1, S2 and S4, and the debugger design's
/// D2, D5, D6 and D12 (`docs/DESIGN-macos-debugger-access-2026-09-29.md` §7),
/// are unmeasured. Flipping one is the only way a stronger claim can appear in
/// a report, so an over-claim is a visible code change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent spike's verdict, flipped in its own commit; \
              they are not the states of one machine"
)]
pub struct SpikeEvidence {
    /// §7 S4 — how much a held Full Disk Access grant actually suppresses.
    /// Until this is `true`, no folder is ever reported `covered`.
    pub fda_coverage_measured: bool,
    /// §7 S2: what an adopted-across-a-handoff session's attribution and access
    /// do. Until `true`, an adopted session reports `unknown`, never `denied`.
    /// Measuring it needs a real in-place `update apply` of an installed
    /// Developer-ID `aterm.app` (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md`
    /// §7 S2); then flip it where `aterm-gui`'s `control_privacy.rs` passes
    /// [`Self::UNMEASURED`].
    pub handoff_attribution_measured: bool,
    /// §7 S1 — how far a grant reaches. Stays [`FdaScope::Unknown`].
    pub fda_scope: FdaScope,
    /// Debugger design §7 D2 — what `DevToolsSecurity -enable` does from
    /// aterm's gesture, the app name macOS's dialog shows included.
    pub devtools_enable_measured: bool,
    /// Debugger design §7 D5 — whether developer mode silences lldb's attach.
    /// Until this is `true`, copy says the switch "should stop" the dialog,
    /// never that it "stops" it.
    pub devtools_silences_lldb: bool,
    /// Debugger design §7 D6 — the shape of a cancelled taskport
    /// authorization in authd's log. Until this is `true`, an unrecognised
    /// close reads `unknown`, never `failed`.
    pub taskport_cancel_shape_measured: bool,
    /// Debugger design §7 D12 — whether `sample`, `leaks` and their kin stay
    /// silent with developer mode off. Until this is `true`, help says they
    /// "should not" raise the dialog, never that they "do not".
    pub perf_tools_never_prompt: bool,
    /// Debugger design §7 D13: tccd's Developer Tools answer for a LISTED
    /// identity, switched on and off. Until this is `true`, `denied` reads
    /// "not allowed", never "listed, switched off". [`DevToolState::Denied`]
    /// does not carry tccd's reason, so the flip must also carry it, or a
    /// policy denial would read "switched off".
    pub devtool_answer_shapes_measured: bool,
    /// Debugger design §7 D14: whether being on the Developer Tools list skips
    /// macOS's first-run check of a new program run in aterm. Until this is
    /// `true`, copy says the list "should" stop that check, never that it does.
    pub devtool_skips_first_run_check: bool,
}

impl SpikeEvidence {
    /// Nothing measured — the state of this tree, and what every caller passes
    /// today.
    pub const UNMEASURED: Self = Self {
        fda_coverage_measured: false,
        handoff_attribution_measured: false,
        fda_scope: FdaScope::Unknown,
        devtools_enable_measured: false,
        devtools_silences_lldb: false,
        taskport_cancel_shape_measured: false,
        perf_tools_never_prompt: false,
        devtool_answer_shapes_measured: false,
        devtool_skips_first_run_check: false,
    };
}

impl Default for SpikeEvidence {
    fn default() -> Self {
        Self::UNMEASURED
    }
}

/// Everything the posture join reads. All of it is observed, none of it is
/// inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PostureInputs {
    /// aterm's own adoption record for this session.
    pub adoption: Attribution,
    /// The instance's Full Disk Access state.
    pub fda: FdaState,
    /// The responsibility SPI's corroboration. Informational: it never changes
    /// [`ConsentPosture::attribution`].
    pub responsible: Responsible,
    /// `true` only when aterm itself took an `EPERM(1)` on a path under a
    /// protected root for this session. Never set from a guess.
    pub observed_eperm: bool,
    /// Which spikes have landed.
    pub evidence: SpikeEvidence,
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
    ///   when the grant is held, the session is live, and §7 S4 measured the
    ///   coverage; `Unknown` otherwise.
    /// * An adopted session is `Unknown` and never `Denied` until §7 S2 lands
    ///   (design §3.9 pt 3): aterm reports that the session outlived the
    ///   process that started it, and refuses to assert the TCC consequence.
    /// * `prompt_possible` is `false` only when the grant is held and the
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

    // The debugger-access facts (design §3.2). Declared here rather than in
    // `crates/aterm-libc`, whose cells are generated from the crates.io `libc`
    // and held to it by `libc-oracle` item for item: upstream declares neither
    // `csops` nor the `mbr_*` family, and binds `getgrnam_r` through a
    // `group` struct that cell does not carry. All four are libSystem exports
    // (`csops` in `libsystem_kernel`, the rest in `libsystem_info`), and none
    // takes a `$`-suffixed link name on either Darwin arch.
    unsafe extern "C" {
        /// `int csops(pid_t, unsigned int ops, void *useraddr, size_t usersize)`
        /// — xnu `bsd/sys/codesign.h`; no SDK header declares it.
        fn csops(
            pid: libc::pid_t,
            ops: u32,
            useraddr: *mut libc::c_void,
            usersize: libc::size_t,
        ) -> libc::c_int;
        /// `<membership.h>`: `int mbr_uid_to_uuid(uid_t, uuid_t)`.
        fn mbr_uid_to_uuid(uid: libc::uid_t, uu: *mut u8) -> libc::c_int;
        /// `<membership.h>`: `int mbr_gid_to_uuid(gid_t, uuid_t)`.
        fn mbr_gid_to_uuid(gid: libc::gid_t, uu: *mut u8) -> libc::c_int;
        /// `<membership.h>`: `int mbr_check_membership(const uuid_t user,
        /// const uuid_t group, int *ismember)` — asks the directory, so nested
        /// groups resolve.
        fn mbr_check_membership(
            user: *const u8,
            group: *const u8,
            ismember: *mut libc::c_int,
        ) -> libc::c_int;
        /// `<grp.h>`: the reentrant `getgrnam`. The plain one returns a
        /// static buffer another thread's lookup can overwrite.
        fn getgrnam_r(
            name: *const libc::c_char,
            grp: *mut Group,
            buf: *mut libc::c_char,
            buflen: libc::size_t,
            result: *mut *mut Group,
        ) -> libc::c_int;
    }

    /// `<grp.h>`'s `struct group`, the same on both Darwin arches.
    #[repr(C)]
    #[allow(clippy::struct_field_names, reason = "`<grp.h>`'s own field names")]
    struct Group {
        gr_name: *mut libc::c_char,
        gr_passwd: *mut libc::c_char,
        gr_gid: libc::gid_t,
        gr_mem: *mut *mut libc::c_char,
    }

    /// `uuid_t` — `unsigned char[16]`.
    type Uuid = [u8; 16];

    /// The running image's code-signing status flags, or `None` when `csops`
    /// refuses. A syscall about our own process: no spawn, no dialog.
    pub(super) fn own_cs_status() -> Option<u32> {
        let mut flags: u32 = 0;
        // SAFETY: `getpid` has no preconditions.
        let me = unsafe { libc::getpid() };
        // SAFETY: `flags` is a live, aligned `u32`, and the size passed is its
        // exact size; `CS_OPS_STATUS` writes exactly one `uint32_t`.
        let ret = unsafe {
            csops(
                me,
                super::CS_OPS_STATUS,
                (&raw mut flags).cast::<libc::c_void>(),
                std::mem::size_of::<u32>(),
            )
        };
        (ret == 0).then_some(flags)
    }

    /// The birth record of the process at `pid`, from
    /// `proc_pidinfo(PROC_PIDTBSDINFO)` — the identity half of "is the recorded
    /// child still running", as `aterm-gui`'s handoff reads a candidate's.
    /// `None` for pid 0 or 1, a pid the kernel will not describe, a short
    /// record, or a record about some other pid.
    pub(super) fn process_birth(pid: u32) -> Option<super::ProcessBirth> {
        let pid = libc::pid_t::try_from(pid).ok()?;
        if pid <= 1 {
            return None;
        }
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
        // SAFETY: `info` points at `size` writable bytes of exactly the
        // structure PROC_PIDTBSDINFO fills; libproc returns the number of bytes
        // it wrote.
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        if read != size {
            return None;
        }
        // SAFETY: the exact-size success above initialized the whole record.
        let info = unsafe { info.assume_init() };
        // A record about some OTHER pid would name the wrong process.
        if u32::try_from(pid).ok()? != info.pbi_pid {
            return None;
        }
        Some(super::ProcessBirth {
            seconds: info.pbi_start_tvsec,
            microseconds: info.pbi_start_tvusec,
        })
    }

    /// Whether the running user is a member of the directory group `name`,
    /// nested membership included; `None` when any step cannot answer.
    pub(super) fn in_group(name: &str) -> Option<bool> {
        let gid = group_gid(name)?;
        let mut user: Uuid = [0; 16];
        let mut group: Uuid = [0; 16];
        // SAFETY: `getuid` has no preconditions.
        let uid = unsafe { libc::getuid() };
        // SAFETY: `user` is a live 16-byte buffer, the whole of a `uuid_t`.
        if unsafe { mbr_uid_to_uuid(uid, user.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: as above, for `group`.
        if unsafe { mbr_gid_to_uuid(gid, group.as_mut_ptr()) } != 0 {
            return None;
        }
        let mut is_member: libc::c_int = 0;
        // SAFETY: both are live 16-byte `uuid_t`s, and `is_member` is a live
        // `int` the call writes once.
        let ret =
            unsafe { mbr_check_membership(user.as_ptr(), group.as_ptr(), &raw mut is_member) };
        (ret == 0).then_some(is_member != 0)
    }

    /// The gid of the directory group `name`, through `getgrnam_r`: no gid is
    /// written in this crate. `None` for no such group, or any failure.
    fn group_gid(name: &str) -> Option<libc::gid_t> {
        let name = CString::new(name).ok()?;
        // A group record with a long member list can outgrow a first guess;
        // `ERANGE` doubles the buffer up to a bound, past which it is no answer.
        let mut size = 4096usize;
        while size <= 1 << 20 {
            let mut buf = vec![0 as libc::c_char; size];
            let mut record = Group {
                gr_name: std::ptr::null_mut(),
                gr_passwd: std::ptr::null_mut(),
                gr_gid: 0,
                gr_mem: std::ptr::null_mut(),
            };
            let mut found: *mut Group = std::ptr::null_mut();
            // SAFETY: `name` is NUL-terminated; `record` and `found` are live
            // out-parameters; `buf` is a live allocation of exactly `size`
            // bytes that outlives the call and every pointer written into
            // `record` (only `gr_gid`, a plain integer, is read below).
            let ret = unsafe {
                getgrnam_r(
                    name.as_ptr(),
                    &raw mut record,
                    buf.as_mut_ptr(),
                    size,
                    &raw mut found,
                )
            };
            match ret {
                0 if !found.is_null() => return Some(record.gr_gid),
                libc::ERANGE => size *= 2,
                _ => return None,
            }
        }
        None
    }

    /// No SDK header declares this; LLDB, Qt Creator and Chromium all declare
    /// it themselves. It lives in `/usr/lib/system/libquarantine.dylib`.
    type ResponsibilityGetPid = unsafe extern "C" fn(libc::c_int) -> libc::c_int;

    const RESPONSIBILITY_SYMBOL: &[u8] = b"responsibility_get_pid_responsible_for_pid\0";

    type CfRef = *const libc::c_void;

    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;

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
        fn CFStringGetCString(string: CfRef, buffer: *mut u8, size: isize, encoding: u32) -> u8;
        fn CFRelease(cf: CfRef);
    }

    /// The one probe syscall: open, close, never read.
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

    /// The probe's `open`, close-on-exec from creation (`O_CLOEXEC` is atomic
    /// in `open(2)`), so a child another thread spawns before the close cannot
    /// inherit a handle on `TCC.db`.
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
        #[link(name = "CoreServices", kind = "framework")]
        unsafe extern "C" {
            fn LSCopyApplicationURLsForBundleIdentifier(id: CfRef, error: *mut CfRef) -> CfRef;
        }
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
    /// one large string, which `CoreFoundation` shares but a copy per key would
    /// multiply. A key longer than [`KEY_MAX_UNITS`] is none `CFBundle` reads;
    /// a value longer than [`VALUE_MAX_UNITS`] is no bundle identifier and no
    /// file name. A string that does not come back whole (an embedded NUL) is
    /// unread, so it can never compare equal to a name it is not.
    pub(super) fn parse_info_plist(bytes: &[u8]) -> Option<super::InfoPlist> {
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

    /// The table's paths are not on disk: each spelling is its own bundle.
    fn as_spelled(_: &Path) -> Option<Located> {
        None
    }

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
    /// output. They fail toward the weaker claim: anything unrecognised is
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

        // An ad-hoc signature's requirement is implicit, and codesign prints it
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
            links: Vec::new(),
        };
        assert!(!mk(DEV_ID).conflicts_with(&mk(DEV_ID)));
        assert!(
            mk("designated => cdhash H\"aa\"").conflicts_with(&mk("designated => cdhash H\"bb\""))
        );
        assert!(mk("").conflicts_with(&mk("")));
        assert!(mk("").conflicts_with(&mk(DEV_ID)));
    }

    /// The census, as a table: a Developer-ID install running from `/Apps`,
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
        let c = classify_claimants(
            Some(&running),
            &roots,
            Some(&registered),
            list,
            as_spelled,
            read,
        );
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

    /// Only a complete look may claim absence. An unreadable required root, an
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

        let complete =
            classify_claimants(Some(&running), &roots, Some(&[]), lone, as_spelled, only);
        assert!(complete.sole_claimant() && complete.conflicting().is_empty());

        let no_ls = classify_claimants(Some(&running), &roots, None, lone, as_spelled, only);
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
            as_spelled,
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
            as_spelled,
            |root: &Path| {
                if root == running.as_path() {
                    ours(DEV_ID)
                } else {
                    Candidate::Hole
                }
            },
        );
        assert_eq!(unreadable_bundle.enumeration, Enumeration::Partial);

        let blind = classify_claimants(None, &[], None, lone, as_spelled, |_| Candidate::NotOurs);
        assert_eq!(blind.enumeration, Enumeration::Unavailable);
        assert!(
            blind.conflicting().is_empty(),
            "no reference identity, no claim"
        );
    }

    /// Beside an unstable running copy, a conflicting Developer-ID copy may
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
            as_spelled,
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
            as_spelled,
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

    /// The Trash gesture: a copy is moved only if the owner was shown it, a
    /// fresh census still offers it, it is still the same bundle right before
    /// the move, and nothing runs from it. Every other copy stays with its
    /// reason, and nothing outside the plan is touched.
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
            as_spelled,
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
        // A link to a folder above the root (here the one that holds Documents):
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

    /// The main executable is found by name, with no link: a name that is a
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

    /// A wait for something that must happen is a hang detector, never a latency
    /// budget (AGENTS.md).
    #[cfg(target_os = "macos")]
    const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);

    /// The ceiling bounds the whole run, not only the tool's exit (the product
    /// fd-hygiene sweep of 2026-09-27): a grandchild the tool left holding its
    /// stdout and stderr keeps both readers short of EOF after the tool is gone,
    /// the shape a stranger that inherited a write end takes too. The run gives
    /// up at its ceiling rather than waiting the holder out. The holder lives ten
    /// minutes and the run is held to a minute's [`HANG_DETECTOR`]: a run that
    /// waits the holder out takes all ten, however loaded the machine is.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_pipe_held_open_past_the_tool_cannot_outlast_the_ceiling() {
        let dir =
            std::env::temp_dir().join(format!("aterm-run-bounded-hold-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("holder.pid");
        let script = format!("sleep 600 & echo $! > '{}'", pid_file.display());
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
            took < HANG_DETECTOR,
            "the run outlasted its ceiling waiting on the holder: {took:?}"
        );
    }

    /// The FDA probe's descriptor is close-on-exec from its open, so a child
    /// spawned by another thread cannot carry a handle on `TCC.db` past exec.
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
            links: Vec::new(),
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
            as_spelled,
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
            c.offered_for_trash(true).is_empty(),
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
            as_spelled,
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
            .offered_for_trash(true)
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

    /// The running bundle's parent is listed first and required; the
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

    /// The fence's executable: the exec path when it is bundled (a window an
    /// update renamed under it is judged as before), the kernel's image path
    /// when only that is (a launch through `~/.local/bin/aterm`), and the exec
    /// path otherwise, so a test binary stays outside the fence.
    #[test]
    fn the_fence_judges_the_bundled_image_behind_a_launch_symlink() {
        let exec = PathBuf::from("/Applications/aterm.app/Contents/MacOS/aterm");
        let rolled = PathBuf::from("/Applications/aterm.app.rollback/Contents/MacOS/aterm");
        let link = PathBuf::from("/Users//a/.local/bin/aterm");
        let test_bin = PathBuf::from("/Users//a/aterm/target/debug/deps/aterm_cli-abc");
        let at = |p: &PathBuf| SelfImage::At(p.clone());

        assert_eq!(
            fence_exe_from(Some(link.clone()), &at(&exec)),
            Some(exec.clone())
        );
        assert_eq!(
            fence_exe_from(Some(exec.clone()), &at(&rolled)),
            Some(exec.clone())
        );
        assert_eq!(fence_exe_from(None, &at(&exec)), Some(exec.clone()));
        for image in [at(&test_bin), SelfImage::NoPath, SelfImage::Unreadable] {
            assert_eq!(
                fence_exe_from(Some(test_bin.clone()), &image),
                Some(test_bin.clone())
            );
            assert_eq!(
                fence_exe_from(Some(link.clone()), &image),
                Some(link.clone())
            );
        }
        assert_eq!(fence_exe_from(None, &SelfImage::Unreadable), None);
        // This test binary is outside a bundle by both paths: the fence holds.
        assert!(!fence_exe().is_some_and(|exe| path_is_in_app_bundle(&exe)));
    }

    /// The image anchor as a table: every arm, classified from the kernel's
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
        // A kernel path under another valid `.app` is still a bundle macOS can
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
            // Nested bundles: the innermost enclosing .app wins.
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
            // EACCES is not a TCC denial.
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
        assert_eq!(
            probe_devtool(ProbeGate::on(), "a.b").label,
            DevToolProbeLabel::UnsupportedPlatform
        );
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
        // A sibling whose name merely starts with the root's name is not under it.
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

    /// The warm-up asks for the three folders and not for app-data (whose Allow
    /// macOS forgets when the responsible instance ends), in the order `ALL`
    /// walks them, and every name in it is one `ALL` knows.
    #[test]
    fn the_warmup_default_is_the_folders_without_app_data() {
        assert_eq!(
            Folder::WARMUP_DEFAULT,
            &[Folder::Documents, Folder::Desktop, Folder::Downloads]
        );
        assert!(!Folder::WARMUP_DEFAULT.contains(&Folder::AppData));
        assert!(
            Folder::WARMUP_DEFAULT
                .iter()
                .all(|f| Folder::ALL.contains(f))
        );
        assert_eq!(&Folder::ALL[..3], Folder::WARMUP_DEFAULT);
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

    /// A missing path is a verdict for exactly one item. The three folders are
    /// made at account setup, so their absence is a broken home; the app-data
    /// container may never have been created, and ENOENT means the look was
    /// allowed (a TCC refusal answers EPERM first).
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
        // every app. The builder is structurally incapable of emitting it:
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
        // The subject comes from the running bundle. If an aterm bundle id
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
            // Not observed unstable is not observed unstable: still offered,
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
        // And a denied FDA state alone is not a session denial.
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

    // -----------------------------------------------------------------
    // Debugger access (developer mode)
    // -----------------------------------------------------------------

    /// Every `DevToolsSecurity` argv carries its flag — `-enable` spelled out
    /// even though a bare invocation means it — on an absolute path, and each
    /// direction runs its own.
    #[test]
    fn the_change_builders_always_carry_their_flags() {
        let status = devtools_status_command();
        assert_eq!(status, [DEVTOOLSSECURITY_PATH, "-status"]);
        assert!(
            Path::new(DEVTOOLSSECURITY_PATH).is_absolute(),
            "never resolved through PATH"
        );
        let enable = devtools_enable_command();
        assert_eq!(enable[0], DEVTOOLSSECURITY_PATH);
        assert_eq!(enable[1], "-enable");
        let disable = devtools_disable_command();
        assert_eq!(disable[0], DEVTOOLSSECURITY_PATH);
        assert_eq!(disable[1], "-disable");
        assert_eq!(DebuggerChange::On.command(), enable);
        assert_eq!(DebuggerChange::Off.command(), disable);
        // The revert line a human reads is the argv `Off` runs.
        assert_eq!(DEBUGGER_ACCESS_REVERT, disable.join(" "));
        for change in [DebuggerChange::On, DebuggerChange::Off] {
            assert_ne!(change.precondition(), change.target());
            assert!(!matches!(
                change.precondition(),
                DebuggerMode::Unrecognized | DebuggerMode::Unknown
            ));
            assert_eq!(DebuggerChange::from_token(change.as_str()), Some(change));
        }
        assert_eq!(
            DebuggerChange::On.precondition(),
            DebuggerMode::Disabled,
            "-enable runs only over a rule read as disabled"
        );
        assert_eq!(DebuggerChange::Off.precondition(), DebuggerMode::Enabled);
        assert_eq!(DebuggerChange::from_token(""), None);
        assert_eq!(DebuggerChange::from_token("enable"), None);
    }

    fn probe_reading(mode: DebuggerMode) -> DebuggerProbe {
        DebuggerProbe {
            mode,
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        }
    }

    /// The change spawns only over its precondition, freshly read, and then
    /// spawns exactly its own argv. Nothing here starts a process.
    #[test]
    fn a_change_spawns_only_over_a_fresh_read_of_its_precondition() {
        let never_spawn = |argv: &[&str]| -> DevtoolsChangeRun {
            panic!("spawned {argv:?} over a read that is not the precondition")
        };
        for change in [DebuggerChange::On, DebuggerChange::Off] {
            for mode in [
                DebuggerMode::Enabled,
                DebuggerMode::Disabled,
                DebuggerMode::Unrecognized,
                DebuggerMode::Unknown,
            ] {
                if mode == change.precondition() {
                    continue;
                }
                let observed = probe_reading(mode);
                assert_eq!(
                    devtools_change_with(change, || observed, never_spawn),
                    DevtoolsChangeRun::NotDue { observed },
                    "{change:?} over {mode:?}"
                );
            }
            // A refused re-read (a closed gate, a binary outside a `.app`)
            // is no precondition either.
            for label in [
                DebuggerProbeLabel::RefusedDisabled,
                DebuggerProbeLabel::RefusedOutOfBundle,
                DebuggerProbeLabel::RefusedNoExe,
                DebuggerProbeLabel::UnsupportedPlatform,
            ] {
                let refused = DebuggerProbe::refused(label);
                assert!(matches!(
                    devtools_change_with(change, || refused, never_spawn),
                    DevtoolsChangeRun::NotDue { .. }
                ));
            }
            let mut spawned = Vec::new();
            let run = devtools_change_with(
                change,
                || probe_reading(change.precondition()),
                |argv| {
                    spawned.push(argv.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>());
                    DevtoolsChangeRun::Exited {
                        code: Some(0),
                        stdout_head: String::new(),
                        stderr_head: String::new(),
                    }
                },
            );
            assert!(run.spawned());
            assert_eq!(spawned.len(), 1);
            assert_eq!(spawned[0], change.command());
        }
    }

    /// `spawned` is true exactly where a child was started.
    #[test]
    fn only_a_started_child_counts_as_spawned() {
        let observed = DebuggerProbe::pending();
        assert!(!DevtoolsChangeRun::NotDue { observed }.spawned());
        assert!(!DevtoolsChangeRun::Absent.spawned());
        assert!(!DevtoolsChangeRun::SpawnFailed.spawned());
        assert!(!DevtoolsChangeRun::Unsupported.spawned());
        assert!(DevtoolsChangeRun::WaitFailed.spawned());
    }

    /// The runner, against a `/bin/sh` stand-in (never `DevToolsSecurity`):
    /// the exit code, a head of each stream capped at 4 KiB, stdin closed,
    /// and `on_spawn` told the live child's pid before the wait.
    #[cfg(unix)]
    #[test]
    fn the_change_runner_keeps_the_exit_and_a_head_of_each_stream() {
        let mut seen = None;
        let run = run_to_exit_keeping_heads(
            &[
                "/bin/sh",
                "-c",
                // Reads stdin (it must be /dev/null: `wc -c` says 0), writes
                // 10 000 bytes to stdout and a line to stderr, exits 3.
                "printf 'in=%s\\n' \"$(wc -c | tr -d ' ')\"; \
                 i=0; while [ $i -lt 100 ]; do printf '%0100d' 0; i=$((i+1)); done; \
                 echo nope >&2; exit 3",
            ],
            |pid| seen = Some((pid, process_birth(pid))),
        );
        let (pid, birth) = seen.expect("on_spawn is called");
        assert!(pid > 1);
        if cfg!(target_os = "macos") {
            assert!(birth.is_some(), "the live child has a birth record");
        }
        let DevtoolsChangeRun::Exited {
            code,
            stdout_head,
            stderr_head,
        } = run
        else {
            panic!("{run:?}");
        };
        assert_eq!(code, Some(3));
        assert!(stdout_head.starts_with("in=0\n"), "{stdout_head:?}");
        assert_eq!(stdout_head.len(), DEVTOOLS_CHANGE_HEAD_BYTES);
        assert_eq!(stderr_head, "nope\n");
    }

    /// A child that takes its time is waited for, not killed; a missing
    /// program is `Absent` and never calls `on_spawn`.
    #[cfg(unix)]
    #[test]
    fn the_change_runner_waits_and_names_a_missing_program() {
        let run = run_to_exit_keeping_heads(&["/bin/sh", "-c", "sleep 0.3; echo done"], |_| {});
        assert_eq!(
            run,
            DevtoolsChangeRun::Exited {
                code: Some(0),
                stdout_head: "done\n".to_owned(),
                stderr_head: String::new(),
            }
        );
        let missing = std::env::temp_dir().join(format!(
            "aterm-devtools-missing-{}/DevToolsSecurity-stand-in",
            std::process::id()
        ));
        let missing = missing.to_str().expect("utf-8 temp dir");
        let run =
            run_to_exit_keeping_heads(&[missing, "-enable"], |_| panic!("nothing was spawned"));
        assert_eq!(run, DevtoolsChangeRun::Absent);
        assert_eq!(
            run_to_exit_keeping_heads(&[], |_| panic!("nothing was spawned")),
            DevtoolsChangeRun::SpawnFailed
        );
    }

    /// A recorded child is running only while its pid still carries its birth:
    /// a record with no birth is never proven running, and a mismatched birth
    /// is another process.
    #[test]
    fn a_recorded_child_runs_only_while_its_birth_matches() {
        let me = std::process::id();
        let no_birth = DevtoolsChild {
            pid: me,
            birth: None,
        };
        assert!(!no_birth.is_running());
        let Some(birth) = process_birth(me) else {
            // Off macOS there is no birth record: nothing is ever proven.
            assert!(!cfg!(target_os = "macos"), "our own pid has a birth record");
            return;
        };
        assert!(
            DevtoolsChild {
                pid: me,
                birth: Some(birth)
            }
            .is_running()
        );
        let other = ProcessBirth {
            seconds: birth.seconds.wrapping_add(1),
            ..birth
        };
        assert!(
            !DevtoolsChild {
                pid: me,
                birth: Some(other)
            }
            .is_running()
        );
        assert_eq!(process_birth(0), None);
        assert_eq!(process_birth(1), None);
    }

    /// The wire tokens, pinned: they are the `privacy` row's schema.
    #[test]
    fn the_debugger_tokens_are_the_documented_spellings() {
        let modes = [
            (DebuggerMode::Enabled, "enabled"),
            (DebuggerMode::Disabled, "disabled"),
            (DebuggerMode::Unrecognized, "unrecognized"),
            (DebuggerMode::Unknown, "unknown"),
        ];
        for (mode, token) in modes {
            assert_eq!(mode.as_str(), token);
        }
        let members = [
            (DebuggerMember::Admin, "admin"),
            (DebuggerMember::Developer, "developer"),
            (DebuggerMember::NotMember, "no"),
            (DebuggerMember::Unknown, "unknown"),
        ];
        for (member, token) in members {
            assert_eq!(member.as_str(), token);
        }
        let hardening = [
            (SelfHardening::Hardened, "yes"),
            (SelfHardening::NotHardened, "no"),
            (SelfHardening::Unknown, "unknown"),
        ];
        for (fact, token) in hardening {
            assert_eq!(fact.as_str(), token);
        }
        let labels = [
            (DebuggerProbeLabel::Pending, "pending", false),
            (DebuggerProbeLabel::StatusOk, "status_ok", false),
            (
                DebuggerProbeLabel::StatusUnrecognized,
                "status_unrecognized",
                false,
            ),
            (DebuggerProbeLabel::ToolAbsent, "tool_absent", false),
            (DebuggerProbeLabel::SpawnFailed, "spawn_failed", false),
            (DebuggerProbeLabel::TimedOut, "timed_out", false),
            (
                DebuggerProbeLabel::OutputUnreadable,
                "output_unreadable",
                false,
            ),
            (
                DebuggerProbeLabel::RefusedDisabled,
                "refused_disabled",
                true,
            ),
            (
                DebuggerProbeLabel::RefusedOutOfBundle,
                "refused_out_of_bundle",
                true,
            ),
            (DebuggerProbeLabel::RefusedNoExe, "refused_no_exe", true),
            (
                DebuggerProbeLabel::UnsupportedPlatform,
                "unsupported_platform",
                true,
            ),
        ];
        for (label, token, refused) in labels {
            assert_eq!(label.as_str(), token);
            assert_eq!(label.refused(), refused, "{token}");
        }
        // The defaults are the not-observed answers, never a verdict.
        assert_eq!(DebuggerMode::default(), DebuggerMode::Unknown);
        assert_eq!(DebuggerMember::default(), DebuggerMember::Unknown);
        assert_eq!(SelfHardening::default(), SelfHardening::Unknown);
        let pending = DebuggerProbe::pending();
        assert_eq!(pending.label, DebuggerProbeLabel::Pending);
        assert!(!pending.label.refused());
        assert_eq!(pending.mode, DebuggerMode::Unknown);
        let unsupported = unsupported_debugger_probe();
        assert_eq!(unsupported.label, DebuggerProbeLabel::UnsupportedPlatform);
        assert_eq!(unsupported.member, DebuggerMember::Unknown);
    }

    fn exited(code: Option<i32>, stdout: &str, stderr: &str) -> ToolRun {
        ToolRun::Exited {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    /// `-status`'s answers. The `disabled` line is the one captured live (spike
    /// D1); the `enabled` and unknown-state lines are derived from the binary's
    /// format strings (`Developer mode is%s%s enabled.`, `Developer mode rights
    /// are%s%s in an unknown state.`), so the match tolerates each adverb the
    /// binary's `__cstring` carries (`currently`, `already`, `now`, or none)
    /// and nothing else.
    #[test]
    fn the_status_classifier_reads_only_what_the_tool_says() {
        use DebuggerMode as M;
        use DebuggerProbeLabel as L;
        let ok = L::StatusOk;
        let no = (M::Unknown, L::StatusUnrecognized);
        let cases: &[(ToolRun, (DebuggerMode, DebuggerProbeLabel))] = &[
            // Measured (§1.1, D1): rc 0.
            (
                exited(Some(0), "Developer mode is currently disabled.\n", ""),
                (M::Disabled, ok),
            ),
            // Derived by symmetry from the same format string.
            (
                exited(Some(0), "Developer mode is currently enabled.\n", ""),
                (M::Enabled, ok),
            ),
            (
                exited(Some(0), "Developer mode is already enabled.", ""),
                (M::Enabled, ok),
            ),
            (
                exited(Some(0), "Developer mode is enabled.", ""),
                (M::Enabled, ok),
            ),
            // `now`, the third adverb the binary carries (after a write).
            (
                exited(Some(0), "Developer mode is now enabled.\n", ""),
                (M::Enabled, ok),
            ),
            (
                exited(Some(0), "Developer mode is now disabled.\n", ""),
                (M::Disabled, ok),
            ),
            (
                exited(Some(0), "  Developer mode is currently disabled.\r\n", ""),
                (M::Disabled, ok),
            ),
            // Which stream carries the line is not part of the contract.
            (
                exited(Some(0), "", "Developer mode is currently disabled.\n"),
                (M::Disabled, ok),
            ),
            // `-verbose`-style preamble lines are ignored.
            (
                exited(
                    Some(0),
                    "Getting rights definition for: system.privilege.taskport.debug\n\
                     Developer mode is currently enabled.\n",
                    "",
                ),
                (M::Enabled, ok),
            ),
            // The unknown state is a recognised answer, whatever the code.
            (
                exited(
                    Some(0),
                    "Developer mode rights are currently in an unknown state.\n",
                    "",
                ),
                (M::Unrecognized, ok),
            ),
            (
                exited(
                    Some(1),
                    "Developer mode rights are currently in an unknown state.\n",
                    "",
                ),
                (M::Unrecognized, ok),
            ),
            // enabled/disabled with a code nobody measured: no claim.
            (
                exited(Some(1), "Developer mode is currently disabled.\n", ""),
                no,
            ),
            (
                exited(None, "Developer mode is currently enabled.\n", ""),
                no,
            ),
            // Nothing recognisable is never `disabled`.
            (exited(Some(0), "", ""), no),
            (
                exited(
                    Some(0),
                    "Right definition is already correct; exiting with success.\n",
                    "",
                ),
                no,
            ),
            (exited(Some(0), "Developer mode is not enabled.\n", ""), no),
            (
                exited(Some(0), "Developer mode is currently disabled\n", ""),
                no,
            ),
            (
                exited(Some(0), "developer mode is currently disabled.\n", ""),
                no,
            ),
            (
                exited(Some(0), "Developer mode is newly enabled.\n", ""),
                no,
            ),
            (
                exited(Some(0), "Developer mode is currently disenabled.\n", ""),
                no,
            ),
            // Two answers that disagree are no answer.
            (
                exited(
                    Some(0),
                    "Developer mode is currently enabled.\n",
                    "Developer mode is currently disabled.\n",
                ),
                no,
            ),
            // A run that did not exit keeps its failure.
            (ToolRun::Absent, (M::Unknown, L::ToolAbsent)),
            (ToolRun::SpawnFailed, (M::Unknown, L::SpawnFailed)),
            (ToolRun::TimedOut, (M::Unknown, L::TimedOut)),
            (ToolRun::Unreadable, (M::Unknown, L::OutputUnreadable)),
        ];
        for (run, want) in cases {
            assert_eq!(classify_devtools_status(run), *want, "{run:?}");
        }
        // The same line repeated is one answer, not a conflict.
        assert_eq!(
            classify_devtools_status(&exited(
                Some(0),
                "Developer mode is currently disabled.\n",
                "Developer mode is currently disabled.\n",
            )),
            (M::Disabled, ok)
        );
    }

    /// Membership folds in rule order: `admin` first, and an unanswered
    /// `admin` is unknown, because every admin is also a `_developer`.
    #[test]
    fn membership_names_the_first_rule_that_matches() {
        use DebuggerMember as D;
        let cases = [
            ((Some(true), None), D::Admin),
            ((Some(true), Some(true)), D::Admin),
            ((Some(true), Some(false)), D::Admin),
            ((Some(false), Some(true)), D::Developer),
            ((Some(false), Some(false)), D::NotMember),
            ((Some(false), None), D::Unknown),
            ((None, Some(true)), D::Unknown),
            ((None, Some(false)), D::Unknown),
            ((None, None), D::Unknown),
        ];
        for ((admin, developer), want) in cases {
            assert_eq!(
                classify_membership(admin, developer),
                want,
                "admin={admin:?} developer={developer:?}"
            );
        }
    }

    /// Hardened means the runtime flag without `get-task-allow`. The flags are
    /// what `codesign -dv` prints: the notarized release reads
    /// `flags=0x10000(runtime)` (§1.1), Homebrew's tools `0x2(adhoc)`.
    #[test]
    fn hardening_is_the_runtime_without_get_task_allow() {
        use SelfHardening as H;
        let cases = [
            (None, H::Unknown),
            (Some(0x0001_0000), H::Hardened),
            (Some(0x0001_0000 | 0x0000_0001), H::Hardened),
            (Some(0x0001_0000 | CS_GET_TASK_ALLOW), H::NotHardened),
            (Some(0x0000_0002), H::NotHardened),
            (Some(0x0002_0002), H::NotHardened),
            (Some(CS_GET_TASK_ALLOW), H::NotHardened),
            (Some(0), H::NotHardened),
        ];
        for (flags, want) in cases {
            assert_eq!(classify_cs_status(flags), want, "{flags:x?}");
        }
        assert_eq!(CS_RUNTIME, 0x1_0000);
        assert_eq!(CS_OPS_STATUS, 0);
    }

    /// Every refusal spawns nothing and asks the directory nothing: both
    /// injected closures panic if they are ever called.
    #[test]
    fn the_debugger_probe_refuses_without_spawning() {
        let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
        let never_spawn = |_: &[&str]| -> ToolRun { panic!("a refusal must spawn nothing") };
        let never_ask = || -> DebuggerMember { panic!("a refusal must not query membership") };
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
            let got = probe_debugger_with(gate, Some(&bundled), never_spawn, never_ask);
            assert_eq!(
                got,
                DebuggerProbe::refused(DebuggerProbeLabel::RefusedDisabled)
            );
        }
        let got = probe_debugger_with(ProbeGate::on(), None, never_spawn, never_ask);
        assert_eq!(
            got,
            DebuggerProbe::refused(DebuggerProbeLabel::RefusedNoExe)
        );
        for exe in [
            "/Users//a/aterm/target/debug/deps/aterm_containment-abc123",
            "/Users//a/aterm/target/release/aterm",
            "/Applications/aterm.app/Contents/Resources/aterm",
        ] {
            let got = probe_debugger_with(
                ProbeGate::on(),
                Some(Path::new(exe)),
                never_spawn,
                never_ask,
            );
            assert_eq!(
                got,
                DebuggerProbe::refused(DebuggerProbeLabel::RefusedOutOfBundle),
                "{exe}"
            );
            assert_eq!(got.mode, DebuggerMode::Unknown);
            assert_eq!(got.member, DebuggerMember::Unknown);
        }
    }

    /// Past the refusals the probe spawns exactly the `-status` argv, once, and
    /// reads membership once; a failed run still reports the membership it
    /// read, and never a mode.
    #[test]
    fn the_debugger_probe_spawns_only_the_status_argv() {
        let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
        let mut seen: Vec<Vec<String>> = Vec::new();
        let got = probe_debugger_with(
            ProbeGate::on(),
            Some(&bundled),
            |argv| {
                seen.push(argv.iter().map(|a| (*a).to_string()).collect());
                exited(Some(0), "Developer mode is currently disabled.\n", "")
            },
            || DebuggerMember::Admin,
        );
        assert_eq!(
            got,
            DebuggerProbe {
                mode: DebuggerMode::Disabled,
                member: DebuggerMember::Admin,
                label: DebuggerProbeLabel::StatusOk,
            }
        );
        assert_eq!(
            seen,
            vec![vec![
                DEVTOOLSSECURITY_PATH.to_string(),
                "-status".to_string()
            ]]
        );
        let got = probe_debugger_with(
            ProbeGate::on(),
            Some(&bundled),
            |_| ToolRun::TimedOut,
            || DebuggerMember::Developer,
        );
        assert_eq!(got.mode, DebuggerMode::Unknown);
        assert_eq!(got.member, DebuggerMember::Developer);
        assert_eq!(got.label, DebuggerProbeLabel::TimedOut);
    }

    /// The real entry point refuses from this test binary, which lives under
    /// `target/debug/deps`: the 2026-08-17 fence, end to end. Nothing is
    /// spawned, so `DevToolsSecurity` never runs from a test.
    #[test]
    fn the_real_debugger_probe_refuses_from_a_test_binary() {
        let got = probe_debugger(ProbeGate::on());
        assert!(got.label.refused(), "{got:?}");
        assert_eq!(got.mode, DebuggerMode::Unknown);
        assert_eq!(got.member, DebuggerMember::Unknown);
        #[cfg(target_os = "macos")]
        assert_eq!(got.label, DebuggerProbeLabel::RefusedOutOfBundle);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(got.label, DebuggerProbeLabel::UnsupportedPlatform);
    }

    // -- the Developer Tools list (kTCCServiceDeveloperTool) ----------------

    /// One record exactly as `/usr/bin/log show --info --style ndjson
    /// --timezone UTC` printed it on 2026-10-07 (macOS 26.6.2, build 25G83),
    /// for a `/bin/sh` that started in an aterm tab. Only `bootUUID` is zeroed.
    const CAPTURED_RECORD: &str = r#"{"timezoneName":"","messageType":"Info","eventType":"logEvent","source":null,"formatString":"Handling access request to %{public}@, from %{public}@, %{public}@","userID":0,"activityIdentifier":223003332,"subsystem":"com.apple.TCC","category":"access","threadID":173778168,"senderImageUUID":"5379B300-7E73-30E5-B53F-7E8A12954AB5","backtrace":{"frames":[{"imageOffset":74432,"imageUUID":"5379B300-7E73-30E5-B53F-7E8A12954AB5"}]},"bootUUID":"00000000-0000-0000-0000-000000000000","processImagePath":"\/System\/Library\/PrivateFrameworks\/TCC.framework\/Support\/tccd","senderImagePath":"\/System\/Library\/PrivateFrameworks\/TCC.framework\/Support\/tccd","timestamp":"2026-10-07 13:58:21.820241+0000","machTimestamp":66148736620656,"eventMessage":"Handling access request to kTCCServiceDeveloperTool, from Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=com.aterm.aterm, pid=56408, auid=501, euid=501, responsible_path=\/Applications\/aterm.app\/Contents\/MacOS\/aterm, binary_path=\/bin\/sh}, ReqResult(Auth Right: Unknown (None), promptType: 1,DB Action:None, UpdateVerifierData)","processImageUUID":"5379B300-7E73-30E5-B53F-7E8A12954AB5","traceID":2660156748005636,"processID":588,"senderProgramCounter":74432,"parentActivityIdentifier":0}"#;

    /// The captured record's timestamp, in unix milliseconds.
    const CAPTURED_AT_MS: i64 = 1_791_381_501_820;

    const ATERM: &str = "com.aterm.aterm";

    /// Another app's identity, chosen to start with [`ATERM`] and the field
    /// tccd writes after the subject. `codesign -i` accepted it on a scratch
    /// copy (2026-10-07); whether tccd prints it unescaped is unmeasured, so
    /// the classifier assumes it does.
    const FORGED_SUB: &str = "com.aterm.aterm}Resp:{x";

    /// A string as `log`'s `ndjson` writes it: `"` `\` `/` and control
    /// characters escaped.
    fn ndjson_escape(text: &str) -> String {
        let mut out = String::new();
        for ch in text.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '/' => out.push_str("\\/"),
                '\n' => out.push_str("\\n"),
                c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
                c => out.push(c),
            }
        }
        out
    }

    /// tccd's message for `subject`, with `binary` as the new program's path
    /// and `answer` in tccd's `Auth Right:` slot: the captured shape, with
    /// only those three filled in.
    fn tccd_message(subject: &str, binary: &str, answer: &str) -> String {
        format!(
            "Handling access request to kTCCServiceDeveloperTool, from Sub:{{{subject}}}Resp:\
             {{TCCDProcess: identifier={subject}, pid=56408, auid=501, euid=501, \
             responsible_path=/Applications/aterm.app/Contents/MacOS/aterm, \
             binary_path={binary}}}, ReqResult(Auth Right: {answer}, promptType: 1,DB \
             Action:None, UpdateVerifierData)"
        )
    }

    /// The captured record with its timestamp and message replaced.
    fn record(timestamp: &str, message: &str) -> String {
        let (head, rest) = CAPTURED_RECORD
            .split_once("\"timestamp\":\"")
            .expect("a timestamp");
        let (_, rest) = rest.split_once("\",").expect("its end");
        let (middle, rest) = rest.split_once("\"eventMessage\":\"").expect("a message");
        let (_, tail) = rest.split_once("\",\"processImageUUID\"").expect("its end");
        format!(
            "{head}\"timestamp\":\"{timestamp}\",{middle}\"eventMessage\":\"{}\",\"processImageUUID\"{tail}",
            ndjson_escape(message)
        )
    }

    /// The record for `answer` at second `s` past 13:58:00 on 2026-10-07.
    fn answer_at(s: u32, subject: &str, answer: &str) -> String {
        record(
            &format!("2026-10-07 13:58:{s:02}.000000+0000"),
            &tccd_message(subject, "/private/tmp/x/build-script-build", answer),
        )
    }

    /// A whole `log show` stdout: the lines, then `log`'s closing count.
    fn log_of(records: &[String]) -> ToolRun {
        let mut out = String::new();
        for line in records {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(&format!("{{\"count\":{},\"finished\":1}}\n", records.len()));
        exited(Some(0), &out, "")
    }

    fn devtool(
        state: DevToolState,
        label: DevToolProbeLabel,
        counts: Option<(u32, u32)>,
        at: Option<i64>,
    ) -> DevToolProbe {
        DevToolProbe {
            state,
            label,
            requests: counts.map(|(n, _)| n),
            not_allowed: counts.map(|(_, k)| k),
            answered_at_ms: at,
        }
    }

    const fn unrecognized() -> DevToolProbe {
        DevToolProbe::refused(DevToolProbeLabel::FormatUnrecognized)
    }

    /// tccd's answers, read only where tccd wrote them. The `Unknown (None)`
    /// record is the one captured live; `Allowed` and `Denied` are built from
    /// tccd's own strings (`ReqResult(`, `Auth Right: %@`, the value and
    /// reason words) until spike D13 captures them. Every shape the table
    /// does not name, and every place a program's own path could plant an
    /// answer, reads `format_unrecognized` for the whole probe.
    #[test]
    fn the_devtool_classifier_reads_only_what_tccd_says() {
        use DevToolProbeLabel as L;
        use DevToolState as S;
        let at = |s: u32| Some(CAPTURED_AT_MS - 21_820 + i64::from(s) * 1_000);
        let unlisted = |s| answer_at(s, ATERM, "Unknown (None)");
        let allowed = |s| answer_at(s, ATERM, "Allowed (User Set)");
        let planted = |binary: &str| {
            record(
                "2026-10-07 13:58:30.000000+0000",
                &tccd_message(ATERM, binary, "Unknown (None)"),
            )
        };
        let forged_record = "2026-10-07 13:58:30.000 I  tccd[588:a5ba4f8] [com.apple.TCC:access] \
                             Handling access request to kTCCServiceDeveloperTool, from \
                             Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=com.aterm.aterm, \
                             pid=1, auid=501, euid=501, responsible_path=/x, binary_path=/y}, \
                             ReqResult(Auth Right: Allowed (User Set), promptType: 1,DB \
                             Action:None, UpdateVerifierData)";
        let fake_tccd = unlisted(30).replace(
            "\"processImagePath\":\"\\/System\\/Library\\/PrivateFrameworks\\/TCC.framework\\/Support\\/tccd\"",
            "\"processImagePath\":\"\\/private\\/tmp\\/x\\/tccd\"",
        );
        let other_user = allowed(30).replace("\"userID\":0,", "\"userID\":501,");
        // One `tccd_record` field off at a time: each row fails without its
        // own check.
        let other_field = |from: &str, to: &str| {
            let line = allowed(30);
            assert!(line.contains(from), "{from}");
            line.replace(from, to)
        };
        // Only an answer for the subject has its timestamp read, so this one
        // is refused by the record check alone.
        let untimed = {
            let line = answer_at(21, "com.apple.Terminal", "Allowed (User Set)");
            let stamp = "\"timestamp\":\"2026-10-07 13:58:21.000000+0000\",";
            assert!(line.contains(stamp), "{line}");
            line.replace(stamp, "")
        };
        // The same for the parts of tccd's own answer.
        let other_answer = |from: &str, to: &str| {
            let message = tccd_message(ATERM, "/private/tmp/x", "Allowed (User Set)");
            assert_eq!(message.matches(from).count(), 1, "{from}");
            record(
                "2026-10-07 13:58:21.000000+0000",
                &message.replace(from, to),
            )
        };
        let cases: Vec<(&str, ToolRun, DevToolProbe)> = vec![
            // Captured live.
            (
                "the captured record",
                log_of(&[CAPTURED_RECORD.to_string()]),
                devtool(S::Unlisted, L::Observed, Some((1, 1)), Some(CAPTURED_AT_MS)),
            ),
            // Built from tccd's strings.
            (
                "allowed",
                log_of(&[answer_at(21, ATERM, "Allowed (System Set)")]),
                devtool(S::Allowed, L::Observed, Some((1, 0)), at(21)),
            ),
            (
                "denied",
                log_of(&[answer_at(21, ATERM, "Denied (System Set)")]),
                devtool(S::Denied, L::Observed, Some((1, 1)), at(21)),
            ),
            (
                "another app's answer is not aterm's",
                log_of(&[answer_at(21, "com.apple.Terminal", "Allowed (User Set)")]),
                devtool(S::Unknown, L::NoRecentRequest, Some((0, 0)), None),
            ),
            (
                "a longer bundle id is another subject",
                log_of(&[answer_at(21, "com.aterm.aterm.dev", "Allowed (User Set)")]),
                devtool(S::Unknown, L::NoRecentRequest, Some((0, 0)), None),
            ),
            // The subject match is exact: a name equal to aterm's up to case or
            // surrounding space is another app, and its answer is never aterm's.
            (
                "a bundle id that differs only in case is another app",
                log_of(&[
                    unlisted(10),
                    answer_at(21, "com.aterm.ATERM", "Allowed (User Set)"),
                ]),
                devtool(S::Unlisted, L::Observed, Some((1, 1)), at(10)),
            ),
            (
                "a bundle id with a leading space is another app",
                log_of(&[
                    unlisted(10),
                    answer_at(21, " com.aterm.aterm", "Allowed (User Set)"),
                ]),
                devtool(S::Unlisted, L::Observed, Some((1, 1)), at(10)),
            ),
            (
                "the closing count alone",
                log_of(&[]),
                devtool(S::Unknown, L::NoRecentRequest, Some((0, 0)), None),
            ),
            // The newest answer decides; the counts cover the window.
            (
                "allowed, then unlisted",
                log_of(&[allowed(10), unlisted(20)]),
                devtool(S::Unlisted, L::Observed, Some((2, 1)), at(20)),
            ),
            (
                "unlisted, then allowed",
                log_of(&[unlisted(10), allowed(20)]),
                devtool(S::Allowed, L::Observed, Some((2, 1)), at(20)),
            ),
            (
                "newest by tccd's clock, not by line",
                log_of(&[unlisted(20), allowed(10)]),
                devtool(S::Unlisted, L::Observed, Some((2, 1)), at(20)),
            ),
            (
                "a tie on tccd's clock goes to the later line",
                log_of(&[unlisted(20), allowed(20)]),
                devtool(S::Allowed, L::Observed, Some((2, 1)), at(20)),
            ),
            (
                "a newline in a path is only a path",
                log_of(&[planted("/private/tmp/x/a\nb")]),
                devtool(S::Unlisted, L::Observed, Some((1, 1)), at(30)),
            ),
            // Shapes tccd's table does not hold.
            (
                "limited",
                log_of(&[answer_at(21, ATERM, "Limited (System Set)")]),
                unrecognized(),
            ),
            (
                "unknown for a reason",
                log_of(&[answer_at(21, ATERM, "Unknown (Service Policy)")]),
                unrecognized(),
            ),
            (
                "a bare value",
                log_of(&[answer_at(21, ATERM, "Allowed")]),
                unrecognized(),
            ),
            (
                "an unknown reason",
                log_of(&[answer_at(21, ATERM, "Allowed (Yes)")]),
                unrecognized(),
            ),
            (
                "no promptType",
                log_of(&[record(
                    "2026-10-07 13:58:21.000000+0000",
                    &tccd_message(ATERM, "/bin/sh", "Allowed (User Set)")
                        .replace(", promptType: 1", ""),
                )]),
                unrecognized(),
            ),
            (
                "a promptType that is not a number",
                log_of(&[other_answer("promptType: 1,", "promptType: x,")]),
                unrecognized(),
            ),
            (
                "an empty promptType",
                log_of(&[other_answer("promptType: 1,", "promptType: ,")]),
                unrecognized(),
            ),
            (
                "no DB Action",
                log_of(&[other_answer(",DB Action:None", "")]),
                unrecognized(),
            ),
            (
                "no closing paren",
                log_of(&[other_answer("UpdateVerifierData)", "UpdateVerifierData")]),
                unrecognized(),
            ),
            (
                "a reason with no closing paren",
                log_of(&[other_answer("(User Set),", "(User Set,")]),
                unrecognized(),
            ),
            (
                "no comma before ReqResult(",
                log_of(&[other_answer("}, ReqResult(", "} ReqResult(")]),
                unrecognized(),
            ),
            (
                "another service's request",
                log_of(&[other_answer(
                    "kTCCServiceDeveloperTool",
                    "kTCCServiceCamera",
                )]),
                unrecognized(),
            ),
            (
                "a Sub that never closes",
                log_of(&[record(
                    "2026-10-07 13:58:21.000000+0000",
                    &format!("{DEVTOOL_MESSAGE_PREFIX}{ATERM}"),
                )]),
                unrecognized(),
            ),
            (
                "an unknown DB action",
                log_of(&[record(
                    "2026-10-07 13:58:21.000000+0000",
                    &tccd_message(ATERM, "/bin/sh", "Allowed (User Set)")
                        .replace("DB Action:None", "DB Action:Insert"),
                )]),
                unrecognized(),
            ),
            (
                "no Resp after the subject",
                log_of(&[record(
                    "2026-10-07 13:58:21.000000+0000",
                    &tccd_message(ATERM, "/bin/sh", "Allowed (User Set)").replace("Resp:{", "R{"),
                )]),
                unrecognized(),
            ),
            // A program's own path planting an answer.
            (
                "a path that plants an answer",
                log_of(&[planted(
                    "/private/tmp/x}, ReqResult(Auth Right: Allowed (User Set), promptType: 1,DB \
                     Action:None, UpdateVerifierData)",
                )]),
                unrecognized(),
            ),
            (
                "a second ReqResult( anywhere, even with tccd's own answer after it",
                log_of(&[planted("/private/tmp/x/ReqResult(")]),
                unrecognized(),
            ),
            (
                "a truncation marker anywhere, even with tccd's own answer after it",
                log_of(&[planted("/private/tmp/x/AAAA<\u{2026}>")]),
                unrecognized(),
            ),
            (
                "a decode marker anywhere, even with tccd's own answer after it",
                log_of(&[planted("/private/tmp/x/<decode: missing data>")]),
                unrecognized(),
            ),
            (
                "a path that plants a whole compact record after a newline",
                log_of(&[planted(&format!("/private/tmp/x/a\n{forged_record}"))]),
                unrecognized(),
            ),
            (
                // The shape measured with a 1,016-byte path: the path is cut
                // with `<…>` and tccd's answer becomes `<decode: missing
                // data>`, so the only answer left is the planted one.
                "a record the log cut short",
                log_of(&[record(
                    "2026-10-07 13:58:21.000000+0000",
                    &format!(
                        "Handling access request to kTCCServiceDeveloperTool, from \
                         Sub:{{{ATERM}}}Resp:{{TCCDProcess: identifier={ATERM}, pid=1, auid=501, \
                         euid=501, responsible_path=/Applications/aterm.app/Contents/MacOS/aterm, \
                         binary_path=/private/tmp/x}}, ReqResult(Auth Right: Allowed (User Set), \
                         promptType: 1,DB Action:None, UpdateVerifierData)/AAAA<\u{2026}>, \
                         <decode: missing data>"
                    ),
                )]),
                unrecognized(),
            ),
            // Another app whose own name starts with aterm's subject and
            // tccd's next field: its `Sub:{…}` begins `com.aterm.aterm}`, so
            // only the second `}Resp:{` tells its answer from aterm's.
            (
                "an app named aterm}Resp:{ is not aterm: its unlisted",
                log_of(&[answer_at(21, FORGED_SUB, "Unknown (None)")]),
                unrecognized(),
            ),
            (
                "an app named aterm}Resp:{ is not aterm: its allowed",
                log_of(&[answer_at(21, FORGED_SUB, "Allowed (User Set)")]),
                unrecognized(),
            ),
            (
                "its unlisted after aterm's real allowed",
                log_of(&[allowed(10), answer_at(21, FORGED_SUB, "Unknown (None)")]),
                unrecognized(),
            ),
            (
                "its allowed after aterm's real unlisted",
                log_of(&[
                    unlisted(10),
                    answer_at(21, FORGED_SUB, "Allowed (User Set)"),
                ]),
                unrecognized(),
            ),
            // An app's codesign identifier need not repeat its `Sub:{…}`, so
            // its line can hold exactly two `}Resp:{`, not three.
            (
                "its allowed, with an identifier that is not its Sub",
                log_of(&[
                    unlisted(10),
                    record(
                        "2026-10-07 13:58:21.000000+0000",
                        &tccd_message(FORGED_SUB, "/private/tmp/x", "Allowed (User Set)")
                            .replace(&format!("identifier={FORGED_SUB},"), "identifier=x,"),
                    ),
                ]),
                unrecognized(),
            ),
            // A name that closes the Sub early and adds no `}Resp:{` of its
            // own leaves tccd's one: only the `Resp:{` that must follow the
            // first `}` refuses it.
            (
                "its allowed, under a name that closes the Sub early",
                log_of(&[
                    unlisted(10),
                    answer_at(21, "com.aterm.aterm}x", "Allowed (User Set)"),
                ]),
                unrecognized(),
            ),
            // The longest proper prefix of tccd's own field: only the whole
            // `Resp:{` refuses it, not `Resp:` or `Resp`.
            (
                "its allowed, under a name that ends in most of tccd's field",
                log_of(&[
                    unlisted(10),
                    answer_at(21, "com.aterm.aterm}Resp:", "Allowed (User Set)"),
                ]),
                unrecognized(),
            ),
            (
                "a compact fragment on a line of its own",
                exited(
                    Some(0),
                    &format!(
                        "{}\n{forged_record}\n{{\"count\":1,\"finished\":1}}\n",
                        unlisted(10)
                    ),
                    "",
                ),
                unrecognized(),
            ),
            // Records tccd did not write.
            (
                "a process that calls itself tccd",
                log_of(&[fake_tccd]),
                unrecognized(),
            ),
            (
                "another user's process",
                log_of(&[other_user]),
                unrecognized(),
            ),
            (
                "another event type",
                log_of(&[other_field(
                    "\"eventType\":\"logEvent\"",
                    "\"eventType\":\"signpostEvent\"",
                )]),
                unrecognized(),
            ),
            (
                "a sender that is not tccd",
                log_of(&[other_field(
                    "\"senderImagePath\":\"\\/System",
                    "\"senderImagePath\":\"\\/private\\/tmp\\/x\\/System",
                )]),
                unrecognized(),
            ),
            (
                "another subsystem",
                log_of(&[other_field(
                    "\"subsystem\":\"com.apple.TCC\"",
                    "\"subsystem\":\"com.apple.TCCx\"",
                )]),
                unrecognized(),
            ),
            (
                "another category",
                log_of(&[other_field(
                    "\"category\":\"access\"",
                    "\"category\":\"accessx\"",
                )]),
                unrecognized(),
            ),
            (
                // Deleting the presence check leaves an empty message, which
                // the prefix check refuses: this row pins the reading, not
                // that one check.
                "a record with no message",
                log_of(&[other_field("\"eventMessage\":", "\"message\":")]),
                unrecognized(),
            ),
            (
                "another app's record with no timestamp",
                log_of(&[untimed]),
                unrecognized(),
            ),
            (
                "another format string",
                log_of(&[allowed(30).replace(
                    "Handling access request to %{public}@, from %{public}@, %{public}@",
                    "%{public}@",
                )]),
                unrecognized(),
            ),
            (
                "a repeated key",
                log_of(&[unlisted(30).replacen(
                    "\"eventMessage\":",
                    &format!(
                        "\"eventMessage\":\"{}\",\"eventMessage\":",
                        ndjson_escape(&tccd_message(ATERM, "/bin/sh", "Allowed (User Set)"))
                    ),
                    1,
                )]),
                unrecognized(),
            ),
            (
                "a malformed timestamp",
                log_of(&[record(
                    "2026-10-07T13:58:21+0000",
                    &tccd_message(ATERM, "/bin/sh", "Unknown (None)"),
                )]),
                unrecognized(),
            ),
            // The framing.
            ("empty output", exited(Some(0), "", ""), unrecognized()),
            (
                "no closing count",
                exited(Some(0), &format!("{}\n", allowed(10)), ""),
                unrecognized(),
            ),
            (
                "a count that disagrees",
                exited(
                    Some(0),
                    &format!("{}\n{{\"count\":2,\"finished\":1}}\n", allowed(10)),
                    "",
                ),
                unrecognized(),
            ),
            (
                "a record after the count",
                exited(
                    Some(0),
                    &format!("{{\"count\":0,\"finished\":1}}\n{}\n", allowed(10)),
                    "",
                ),
                unrecognized(),
            ),
            (
                "an unfinished read",
                exited(
                    Some(0),
                    &format!("{}\n{{\"count\":1,\"finished\":0}}\n", allowed(10)),
                    "",
                ),
                unrecognized(),
            ),
            (
                "a closing count with another key",
                exited(
                    Some(0),
                    &format!(
                        "{}\n{{\"count\":1,\"finished\":1,\"more\":0}}\n",
                        allowed(10)
                    ),
                    "",
                ),
                unrecognized(),
            ),
            // How the run ended.
            // A run happens only after the store check passed, so a failed
            // run never reads as the account's fault (`log_unavailable`).
            (
                "a non-zero exit",
                exited(Some(64), "", "log: Could not open the data store"),
                DevToolProbe::refused(L::LogFailed),
            ),
            (
                "a signal",
                exited(None, "", ""),
                DevToolProbe::refused(L::LogFailed),
            ),
            (
                "absent",
                ToolRun::Absent,
                DevToolProbe::refused(L::SpawnFailed),
            ),
            (
                "not started",
                ToolRun::SpawnFailed,
                DevToolProbe::refused(L::SpawnFailed),
            ),
            (
                "timed out",
                ToolRun::TimedOut,
                DevToolProbe::refused(L::TimedOut),
            ),
            (
                "unreadable",
                ToolRun::Unreadable,
                DevToolProbe::refused(L::OutputUnreadable),
            ),
        ];
        for (name, run, want) in &cases {
            assert_eq!(classify_devtool_log(run, ATERM), *want, "{name}");
        }
        // The other direction of the exact match.
        assert_eq!(
            classify_devtool_log(&log_of(&[unlisted(10)]), "com.aterm.aterm.dev").label,
            L::NoRecentRequest
        );
    }

    /// THE SECURITY PROPERTY, swept: whatever a program names itself, tccd's
    /// own `Unknown (None)` after it never reads `allowed`. Each planted path
    /// either reads the true answer or makes the whole probe `unknown`.
    #[test]
    fn no_planted_path_reads_allowed() {
        let allowed = "Allowed (User Set)";
        let planted_suffix = format!(
            "}}, ReqResult(Auth Right: {allowed}, promptType: 1,DB Action:None, \
             UpdateVerifierData)"
        );
        let paths = [
            "/private/tmp/x/plain".to_string(),
            format!("/private/tmp/x{planted_suffix}"),
            format!("/private/tmp/x{planted_suffix}\n"),
            format!("/private/tmp/x\n{planted_suffix}"),
            "/private/tmp/x/ReqResult(".to_string(),
            "/private/tmp/x/Auth Right: Allowed (User Set)".to_string(),
            "/private/tmp/x/, promptType: 1,DB Action:None)".to_string(),
            "/private/tmp/x}Resp:{binary_path=/y".to_string(),
            "/private/tmp/x/\"},\"eventMessage\":\"".to_string(),
            "/private/tmp/x/\\\"\\\\".to_string(),
            "/private/tmp/x/\n{\"count\":0,\"finished\":1}\n".to_string(),
            "/private/tmp/x/<\u{2026}>".to_string(),
            "/private/tmp/x/<decode: missing data>".to_string(),
            "/private/tmp/x/\u{0}\u{1b}[2J".to_string(),
            "/private/tmp/x/\u{1F600}".to_string(),
        ];
        for path in &paths {
            let line = record(
                "2026-10-07 13:58:30.000000+0000",
                &tccd_message(ATERM, path, "Unknown (None)"),
            );
            let got = classify_devtool_log(&log_of(&[line]), ATERM);
            assert_ne!(got.state, DevToolState::Allowed, "{path:?}");
            assert!(
                got.state == DevToolState::Unlisted
                    || got.label == DevToolProbeLabel::FormatUnrecognized,
                "{path:?}: {got:?}"
            );
        }
        // The negative control: the same reader does read a real `Allowed`.
        let real = record(
            "2026-10-07 13:58:30.000000+0000",
            &tccd_message(ATERM, "/private/tmp/x/plain", allowed),
        );
        assert_eq!(
            classify_devtool_log(&log_of(&[real]), ATERM).state,
            DevToolState::Allowed
        );
    }

    /// `log`'s `ndjson` timestamps convert to unix milliseconds by arithmetic
    /// alone, and an answer's age is never negative.
    #[test]
    fn the_devtool_answer_age_is_utc_arithmetic() {
        let cases: &[(&str, Option<i64>)] = &[
            ("1970-01-01 00:00:00.000000+0000", Some(0)),
            ("2026-10-07 13:58:21.820241+0000", Some(CAPTURED_AT_MS)),
            ("2026-10-07 06:58:21.820241-0700", Some(CAPTURED_AT_MS)),
            ("2026-10-07 13:58:21+0000", Some(CAPTURED_AT_MS - 820)),
            ("2026-10-07 13:58:21.8+0000", Some(CAPTURED_AT_MS - 20)),
            ("2024-02-29 00:00:00.000000+0000", Some(1_709_164_800_000)),
            ("2000-02-29 00:00:00.000000+0000", Some(951_782_400_000)),
            ("2025-12-31 23:59:59.999000+0000", Some(1_767_225_599_999)),
            ("2026-01-01 00:00:00.000000+0000", Some(1_767_225_600_000)),
            ("2023-02-29 00:00:00.000000+0000", None),
            ("2100-02-29 00:00:00.000000+0000", None),
            ("2026-13-01 00:00:00.000000+0000", None),
            ("2026-10-32 00:00:00.000000+0000", None),
            ("2026-10-07 24:00:00.000000+0000", None),
            ("2026-10-07 13:60:00.000000+0000", None),
            ("2026-10-07 13:58:60.000000+0000", None),
            ("2026-10-07T13:58:21.820241+0000", None),
            ("2026-10-07 13:58:21.820241", None),
            ("2026-10-07 13:58:21.+0000", None),
            ("2026-10-07 13:58:21.8202410000+0000", None),
            ("2026-10-07 13:58:21.820241Z0000", None),
            ("2026-10-07 13:58:21.820241+2400", None),
            ("26-10-07 13:58:21.820241+0000", None),
            ("", None),
        ];
        for (text, want) in cases {
            assert_eq!(devtool_timestamp_ms(text), *want, "{text:?}");
        }
        assert_eq!(devtool_answer_age_ms(1_000, 5_100), 4_100);
        assert_eq!(devtool_answer_age_ms(5_000, 5_000), 0);
        assert_eq!(devtool_answer_age_ms(9_000, 5_000), 0, "a future stamp");
        assert_eq!(
            devtool_answer_age_ms(i64::MIN, i64::MAX),
            i64::MAX.unsigned_abs(),
            "saturates"
        );
    }

    /// The reader `ndjson` lines go through: strict JSON, one object per line,
    /// no repeated key.
    #[test]
    fn the_ndjson_reader_is_strict() {
        let fields = json::object_line(
            r#"{"a":"x\/y\né😀","n":0,"m":12,"z":null,"t":true,"f":false,"o":{"p":[1,2.5,-3e2,[]]}}"#,
        )
        .expect("well formed");
        assert_eq!(json::str(&fields, "a"), Some("x/y\n\u{e9}\u{1F600}"));
        assert_eq!(json::uint(&fields, "n"), Some(0));
        assert_eq!(json::uint(&fields, "m"), Some(12));
        assert_eq!(json::uint(&fields, "a"), None, "a string is not a number");
        assert_eq!(json::str(&fields, "missing"), None);
        for bad in [
            "",
            "[]",
            "\"x\"",
            "{",
            "{}x",
            "{} {}",
            r#"{"a":1,"a":2}"#,
            r#"{"a":"\ud83d"}"#,
            r#"{"a":"\udc00"}"#,
            "{\"a\":\"\u{1}\"}",
            "{\"a\":\"line\nbreak\"}",
            r#"{"a":"\x"}"#,
            r#"{"a":01}"#,
            r#"{"a":-}"#,
            r#"{"a":1.}"#,
            r#"{"a":1e}"#,
            r#"{"a":tru}"#,
            r#"{"a":[1,]}"#,
            r#"{"a":1,}"#,
            r#"{a:1}"#,
        ] {
            assert!(json::object_line(bad).is_none(), "{bad:?}");
        }
        assert!(json::object_line("{}").is_some());
        let deep = format!("{{\"a\":{}1{}}}", "[".repeat(20), "]".repeat(20));
        assert!(json::object_line(&deep).is_none(), "nesting is bounded");
        let shallow = format!("{{\"a\":{}1{}}}", "[".repeat(3), "]".repeat(3));
        assert!(json::object_line(&shallow).is_some());
        // `uint` reads only a plain non-negative integer.
        for (text, want) in [("7", Some(7)), ("-7", None), ("7.0", None), ("7e0", None)] {
            let fields = json::object_line(&format!("{{\"n\":{text}}}")).expect("a number");
            assert_eq!(json::uint(&fields, "n"), want, "{text}");
        }
    }

    /// Every refusal spawns nothing: the injected spawn panics if it is ever
    /// called, and so does the store check for every refusal ahead of it.
    #[test]
    fn the_devtool_probe_refuses_without_spawning() {
        let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
        let never_spawn =
            |_: &str, _: &[&str]| -> ToolRun { panic!("a refusal must spawn nothing") };
        let never_look = || -> bool { panic!("a refusal ahead of the store check reads nothing") };
        for gate in [
            ProbeGate {
                enabled: false,
                check: true,
            },
            ProbeGate {
                enabled: true,
                check: false,
            },
        ] {
            assert_eq!(
                probe_devtool_with(gate, Some(&bundled), ATERM, never_look, never_spawn),
                DevToolProbe::refused(DevToolProbeLabel::RefusedDisabled)
            );
        }
        assert_eq!(
            probe_devtool_with(ProbeGate::on(), None, ATERM, never_look, never_spawn),
            DevToolProbe::refused(DevToolProbeLabel::RefusedNoExe)
        );
        for exe in [
            "/private/tmp/x/target/debug/deps/aterm_containment-abc123",
            "/private/tmp/x/target/release/aterm",
            "/Applications/aterm.app/Contents/Resources/aterm",
        ] {
            assert_eq!(
                probe_devtool_with(
                    ProbeGate::on(),
                    Some(Path::new(exe)),
                    ATERM,
                    never_look,
                    never_spawn
                ),
                DevToolProbe::refused(DevToolProbeLabel::RefusedOutOfBundle),
                "{exe}"
            );
        }
        for subject in [
            "",
            "  ",
            "-x",
            "a b",
            "com.aterm.aterm}Resp:{",
            "com.aterm.aterm\"",
            "a/b",
            "a\u{0}b",
            "caf\u{e9}",
        ] {
            assert!(!devtool_subject_is_usable(subject), "{subject:?}");
            assert_eq!(
                probe_devtool_with(
                    ProbeGate::on(),
                    Some(&bundled),
                    subject,
                    never_look,
                    never_spawn
                ),
                DevToolProbe::refused(DevToolProbeLabel::RefusedNoSubject),
                "{subject:?}"
            );
        }
        assert_eq!(
            probe_devtool_with(
                ProbeGate::on(),
                Some(&bundled),
                ATERM,
                || false,
                never_spawn
            ),
            DevToolProbe::refused(DevToolProbeLabel::LogUnavailable),
            "not an administrator: the store is not readable"
        );
        // Past the store check a failed run is `log`'s failure, never the
        // account's.
        assert_eq!(
            probe_devtool_with(
                ProbeGate::on(),
                Some(&bundled),
                ATERM,
                || true,
                |_, _| exited(Some(64), "", "log: bad predicate")
            ),
            DevToolProbe::refused(DevToolProbeLabel::LogFailed),
        );
        assert!(devtool_subject_is_usable(ATERM));
        assert!(devtool_subject_is_usable("com.aterm.aterm.dev"));
    }

    /// Past the refusals the probe runs exactly `LOG_TOOL` with the constant
    /// argv, once, and the subject is in no argument: it is matched in Rust.
    #[test]
    fn the_devtool_probe_spawns_only_the_log_argv() {
        let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
        let subject = "com.example.subject";
        let mut seen: Vec<(String, Vec<String>)> = Vec::new();
        let mut looked = 0;
        let got = probe_devtool_with(
            ProbeGate::on(),
            Some(&bundled),
            subject,
            || {
                looked += 1;
                true
            },
            |program, args| {
                seen.push((
                    program.to_string(),
                    args.iter().map(|a| (*a).to_string()).collect(),
                ));
                log_of(&[answer_at(21, subject, "Unknown (None)")])
            },
        );
        assert_eq!(looked, 1);
        assert_eq!(got.state, DevToolState::Unlisted);
        assert_eq!(seen.len(), 1, "exactly one run");
        let (program, args) = &seen[0];
        assert_eq!(program, LOG_TOOL);
        assert_eq!(
            program, "/usr/bin/log",
            "named absolutely: `log` is a zsh builtin"
        );
        assert_eq!(args, &DEVTOOL_LOG_ARGS.map(str::to_string));
        assert!(
            args.iter()
                .all(|a| !a.contains(subject) && !a.contains("example")),
            "the subject never reaches an argv: {args:?}"
        );
        // The window the argv reads is the window the row reports.
        let last = args
            .iter()
            .position(|a| a == "--last")
            .map(|i| args[i + 1].clone());
        assert_eq!(last, Some(format!("{}m", DEVTOOL_WINDOW_S / 60)));
        assert!(args.contains(&"ndjson".to_string()), "the escaped framing");
    }

    /// The real entry point refuses from this test binary, which lives under
    /// `target/debug/deps`: the 2026-08-17 fence, end to end. Nothing runs.
    #[test]
    fn the_real_devtool_probe_refuses_from_a_test_binary() {
        let got = probe_devtool(ProbeGate::on(), ATERM);
        assert!(got.label.refused(), "{got:?}");
        assert_eq!(got.state, DevToolState::Unknown);
        assert_eq!(got.requests, None);
        #[cfg(target_os = "macos")]
        assert_eq!(got.label, DevToolProbeLabel::RefusedOutOfBundle);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(got.label, DevToolProbeLabel::UnsupportedPlatform);
    }

    /// The wire tokens, pinned: they are the `privacy` row's schema.
    #[test]
    fn the_devtool_tokens_are_the_documented_spellings() {
        for (state, token) in [
            (DevToolState::Allowed, "allowed"),
            (DevToolState::Denied, "denied"),
            (DevToolState::Unlisted, "unlisted"),
            (DevToolState::Unknown, "unknown"),
        ] {
            assert_eq!(state.as_str(), token);
        }
        use DevToolProbeLabel as L;
        for (label, token, refused) in [
            (L::Pending, "pending", false),
            (L::Observed, "observed", false),
            (L::NoRecentRequest, "no_recent_request", false),
            (L::LogUnavailable, "log_unavailable", false),
            (L::LogFailed, "log_failed", false),
            (L::FormatUnrecognized, "format_unrecognized", false),
            (L::TimedOut, "timed_out", false),
            (L::SpawnFailed, "spawn_failed", false),
            (L::OutputUnreadable, "output_unreadable", false),
            (L::RefusedDisabled, "refused_disabled", true),
            (L::RefusedObserverOff, "refused_observer_off", true),
            (L::RefusedOutOfBundle, "refused_out_of_bundle", true),
            (L::RefusedNoExe, "refused_no_exe", true),
            (L::RefusedNoSubject, "refused_no_subject", true),
            (L::UnsupportedPlatform, "unsupported_platform", true),
        ] {
            assert_eq!(label.as_str(), token);
            assert_eq!(label.refused(), refused, "{token}");
        }
        assert_eq!(DevToolState::default(), DevToolState::Unknown);
        let pending = DevToolProbe::pending();
        assert_eq!(pending.label, L::Pending);
        assert!(!pending.label.refused());
        assert_eq!(unsupported_devtool_probe().label, L::UnsupportedPlatform);
        assert_eq!(DEVTOOL_WINDOW_S, 600);
        assert!(!SpikeEvidence::UNMEASURED.devtool_answer_shapes_measured);
        assert!(!SpikeEvidence::UNMEASURED.devtool_skips_first_run_check);
    }

    // -- the Developer Tools list's row in the system TCC.db ----------------

    /// `sqlite3`'s line for one row: the client as sqlite's `hex()` spells it.
    fn db_row(client: &str, kind: u32, auth: u32, modified: i64) -> String {
        let hex: String = client.bytes().map(|b| format!("{b:02X}")).collect();
        format!("{hex}|{kind}|{auth}|{modified}")
    }

    fn db_of(rows: &[String]) -> ToolRun {
        let mut stdout = rows.join("\n");
        if !rows.is_empty() {
            stdout.push('\n');
        }
        exited(Some(0), &stdout, "")
    }

    fn listed(listing: DevToolListing, modified_at_s: Option<i64>) -> DevToolList {
        listed_of(listing, modified_at_s, 1)
    }

    fn listed_of(listing: DevToolListing, modified_at_s: Option<i64>, rows: u32) -> DevToolList {
        DevToolList {
            listing,
            label: DevToolListLabel::Read,
            modified_at_s,
            rows: Some(rows),
        }
    }

    /// The rows captured on 2026-10-14 (macOS 26.6.2), before and after the
    /// owner pressed + and switched aterm on: Terminal listed off, then both on.
    #[test]
    fn the_list_classifier_reads_only_the_subjects_bundle_row() {
        let unrecognized = DevToolList::refused(DevToolListLabel::FormatUnrecognized);
        // The captured lines, verbatim.
        let before = "636F6D2E6170706C652E5465726D696E616C|0|0|1781819370\n";
        let after = "636F6D2E6170706C652E5465726D696E616C|0|2|1792020639\n\
                     636F6D2E617465726D2E617465726D|0|2|1792020637\n";
        assert_eq!(
            classify_devtool_db(&exited(Some(0), before, ""), ATERM),
            listed(DevToolListing::Absent, None)
        );
        assert_eq!(
            classify_devtool_db(&exited(Some(0), after, ""), ATERM),
            listed_of(DevToolListing::On, Some(1_792_020_637), 2)
        );
        assert_eq!(
            classify_devtool_db(&exited(Some(0), before, ""), "com.apple.Terminal"),
            listed(DevToolListing::Off, Some(1_781_819_370))
        );
        for (name, rows, want) in [
            (
                "an empty list: no pane yet",
                vec![],
                listed_of(DevToolListing::Absent, None, 0),
            ),
            (
                "switched on",
                vec![db_row(ATERM, 0, 2, 5)],
                listed(DevToolListing::On, Some(5)),
            ),
            (
                "switched off",
                vec![db_row(ATERM, 0, 0, 6)],
                listed(DevToolListing::Off, Some(6)),
            ),
            (
                "another app's row",
                vec![db_row("com.example.other", 0, 2, 1)],
                listed(DevToolListing::Absent, None),
            ),
            (
                "a path client spelled as the id is not the bundle's row",
                vec![db_row(ATERM, 1, 2, 1)],
                listed(DevToolListing::Absent, None),
            ),
            (
                "a longer id that starts with ours",
                vec![db_row("com.aterm.aterm.dev", 0, 2, 1)],
                listed(DevToolListing::Absent, None),
            ),
            (
                "a client that only contains ours",
                vec![db_row("x|0|2|9\ncom.aterm.aterm", 0, 2, 1)],
                listed(DevToolListing::Absent, None),
            ),
            (
                "rows are counted whoever they name",
                vec![db_row("a.b", 0, 0, 1), db_row("/bin/x", 1, 2, 1)],
                listed_of(DevToolListing::Absent, None, 2),
            ),
            (
                "two rows for the subject",
                vec![db_row(ATERM, 0, 2, 1), db_row(ATERM, 0, 0, 2)],
                unrecognized,
            ),
            (
                "an auth value that is neither on nor off",
                vec![db_row(ATERM, 0, 3, 1)],
                unrecognized,
            ),
            ("unknown", vec![db_row(ATERM, 0, 1, 1)], unrecognized),
        ] {
            assert_eq!(classify_devtool_db(&db_of(&rows), ATERM), want, "{name}");
        }
        // Any line that is not a row spoils the whole read: a planted line
        // can force `unknown`, never `on`.
        let ours = db_row(ATERM, 0, 2, 1);
        for bad in [
            "com.aterm.aterm|0|2|1",
            "636f6d2e617465726d2e617465726d|0|2|1",
            "636F6D2E617465726D2E617465726D7|0|2|1",
            "636F6D2E617465726D2E617465726D|0|2",
            "636F6D2E617465726D2E617465726D|0|2|1|x",
            "636F6D2E617465726D2E617465726D|0|2|",
            "636F6D2E617465726D2E617465726D|-1|2|1",
            "636F6D2E617465726D2E617465726D|0|+2|1",
            "636F6D2E617465726D2E617465726D|0|2|1.5",
            "636F6D2E617465726D2E617465726D|0|99999999999999999999|1",
            "Error: in prepare, no such table: access",
            "",
        ] {
            assert_eq!(
                classify_devtool_db(&exited(Some(0), &format!("{ours}\n{bad}\n"), ""), ATERM),
                unrecognized,
                "{bad:?}"
            );
        }
        for (run, label) in [
            (
                exited(Some(1), "", "Error: unable to open database"),
                DevToolListLabel::ToolFailed,
            ),
            (exited(None, &ours, ""), DevToolListLabel::ToolFailed),
            (ToolRun::Absent, DevToolListLabel::SpawnFailed),
            (ToolRun::SpawnFailed, DevToolListLabel::SpawnFailed),
            (ToolRun::TimedOut, DevToolListLabel::TimedOut),
            (ToolRun::Unreadable, DevToolListLabel::OutputUnreadable),
        ] {
            assert_eq!(
                classify_devtool_db(&run, ATERM),
                DevToolList::refused(label),
                "{run:?}"
            );
        }
    }

    /// Every refusal opens and spawns nothing; `EPERM` on the open is the
    /// responsible app's missing Full Disk Access and spawns nothing either.
    #[test]
    fn the_list_probe_refuses_without_opening_or_spawning() {
        let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
        let never_open = |_: &Path| -> ProbeOutcome { panic!("a refusal opens nothing") };
        let never_spawn =
            |_: &str, _: &[&str]| -> ToolRun { panic!("a refusal must spawn nothing") };
        let off = ProbeGate {
            enabled: true,
            check: false,
        };
        let refused = |label| DevToolList::refused(label);
        assert_eq!(
            probe_devtool_list_with(off, Some(&bundled), ATERM, never_open, never_spawn),
            refused(DevToolListLabel::RefusedDisabled)
        );
        assert_eq!(
            probe_devtool_list_with(ProbeGate::on(), None, ATERM, never_open, never_spawn),
            refused(DevToolListLabel::RefusedNoExe)
        );
        assert_eq!(
            probe_devtool_list_with(
                ProbeGate::on(),
                Some(Path::new(
                    "/private/tmp/x/target/debug/deps/aterm_containment-abc"
                )),
                ATERM,
                never_open,
                never_spawn
            ),
            refused(DevToolListLabel::RefusedOutOfBundle)
        );
        assert_eq!(
            probe_devtool_list_with(
                ProbeGate::on(),
                Some(&bundled),
                "com.aterm.aterm|0|2",
                never_open,
                never_spawn
            ),
            refused(DevToolListLabel::RefusedNoSubject)
        );
        for (errno, label) in [
            (ERRNO_EPERM, DevToolListLabel::NoFullDiskAccess),
            (ERRNO_ENOENT, DevToolListLabel::OpenFailed),
            (13, DevToolListLabel::OpenFailed),
        ] {
            assert_eq!(
                probe_devtool_list_with(
                    ProbeGate::on(),
                    Some(&bundled),
                    ATERM,
                    |_| ProbeOutcome::Errno(errno),
                    never_spawn
                ),
                refused(label),
                "errno {errno}"
            );
        }
        for label in [
            DevToolListLabel::RefusedDisabled,
            DevToolListLabel::RefusedObserverOff,
            DevToolListLabel::RefusedOutOfBundle,
            DevToolListLabel::RefusedNoExe,
            DevToolListLabel::RefusedNoSubject,
            DevToolListLabel::UnsupportedPlatform,
        ] {
            assert!(label.refused(), "{label:?}");
        }
        for label in [
            DevToolListLabel::Pending,
            DevToolListLabel::Read,
            DevToolListLabel::NoFullDiskAccess,
            DevToolListLabel::OpenFailed,
        ] {
            assert!(!label.refused(), "{label:?}");
        }
    }

    /// Past the refusals the probe opens the system database once, then runs
    /// exactly `SQLITE_TOOL` with the constant argv: read-only, no rc file,
    /// safe mode, and the subject in no argument.
    #[test]
    fn the_list_probe_opens_once_and_spawns_only_the_sqlite_argv() {
        let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
        let subject = "com.example.subject";
        let mut opened: Vec<PathBuf> = Vec::new();
        let mut seen: Vec<(String, Vec<String>)> = Vec::new();
        let got = probe_devtool_list_with(
            ProbeGate::on(),
            Some(&bundled),
            subject,
            |db| {
                opened.push(db.to_path_buf());
                ProbeOutcome::Ok
            },
            |program, args| {
                seen.push((
                    program.to_string(),
                    args.iter().map(|a| (*a).to_string()).collect(),
                ));
                db_of(&[db_row(subject, 0, 0, 7)])
            },
        );
        assert_eq!(got, listed(DevToolListing::Off, Some(7)));
        assert_eq!(opened, vec![p(SYSTEM_TCC_DB)]);
        assert_eq!(
            SYSTEM_TCC_DB,
            "/Library/Application Support/com.apple.TCC/TCC.db"
        );
        assert_eq!(seen.len(), 1, "exactly one run");
        let (program, args) = &seen[0];
        assert_eq!(program, "/usr/bin/sqlite3");
        assert_eq!(args, &DEVTOOL_DB_ARGS.map(str::to_string));
        for flag in ["-readonly", "-safe", "-nofollow", "-bail"] {
            assert!(args.iter().any(|a| a == flag), "{flag}");
        }
        assert_eq!(
            args.iter().position(|a| a == "-init").map(|i| &args[i + 1]),
            Some(&"/dev/null".to_string()),
            "no ~/.sqliterc"
        );
        assert!(
            args.iter().all(|a| !a.contains("example")),
            "the subject never reaches an argv: {args:?}"
        );
        assert!(
            DEVTOOL_DB_QUERY.contains("hex(client)")
                && DEVTOOL_DB_QUERY.contains("'kTCCServiceDeveloperTool'")
                && DEVTOOL_DB_QUERY.starts_with("SELECT "),
            "a read of the Developer Tools rows only"
        );
    }

    /// The real entry point refuses from this test binary: no open of the
    /// system database, no `sqlite3`.
    #[test]
    fn the_real_list_probe_refuses_from_a_test_binary() {
        let got = probe_devtool_list(ProbeGate::on(), ATERM);
        assert!(got.label.refused(), "{got:?}");
        assert_eq!(got.listing, DevToolListing::Unknown);
        #[cfg(target_os = "macos")]
        assert_eq!(got.label, DevToolListLabel::RefusedOutOfBundle);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(got.label, DevToolListLabel::UnsupportedPlatform);
    }

    /// The join, as a table: the switch decides unless tccd answered after it
    /// last changed; an absent row is the switch now; `unknown` only when
    /// neither source was read.
    #[test]
    fn the_grant_joins_the_switch_and_tccds_answer() {
        use DevToolGrant::{Granted, NotGranted, Unknown};
        let tccd = |state, at_ms: i64| DevToolProbe {
            state,
            label: DevToolProbeLabel::Observed,
            requests: Some(3),
            not_allowed: Some(u32::from(state != DevToolState::Allowed) * 3),
            answered_at_ms: Some(at_ms),
        };
        let quiet = DevToolProbe {
            requests: Some(0),
            not_allowed: Some(0),
            ..DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest)
        };
        let no_log = DevToolProbe::refused(DevToolProbeLabel::LogUnavailable);
        let unread = DevToolList::refused(DevToolListLabel::NoFullDiskAccess);
        let on_at_100 = listed(DevToolListing::On, Some(100));
        let off_at_100 = listed(DevToolListing::Off, Some(100));
        let absent = listed(DevToolListing::Absent, None);
        for (name, list, log, want) in [
            ("on, nothing from tccd", on_at_100, quiet, Granted),
            ("on, tccd unreadable", on_at_100, no_log, Granted),
            ("off", off_at_100, quiet, NotGranted),
            ("absent", absent, quiet, NotGranted),
            // 2026-10-14: switched on at 16:30:37 after 96 unlisted answers.
            (
                "on after tccd's last unlisted answer",
                on_at_100,
                tccd(DevToolState::Unlisted, 99_000),
                Granted,
            ),
            (
                "on, but tccd answered unlisted since: another copy's row",
                on_at_100,
                tccd(DevToolState::Unlisted, 100_001),
                NotGranted,
            ),
            (
                "on, tccd allowed since",
                on_at_100,
                tccd(DevToolState::Allowed, 100_001),
                Granted,
            ),
            (
                "off, tccd allowed since (a policy outranks the switch)",
                off_at_100,
                tccd(DevToolState::Allowed, 100_001),
                Granted,
            ),
            (
                "off after tccd's last allowed answer",
                off_at_100,
                tccd(DevToolState::Allowed, 99_000),
                NotGranted,
            ),
            (
                "the same millisecond: the switch",
                off_at_100,
                tccd(DevToolState::Allowed, 100_000),
                NotGranted,
            ),
            (
                "absent though tccd allowed (just removed)",
                absent,
                tccd(DevToolState::Allowed, i64::MAX),
                NotGranted,
            ),
            (
                "unread, tccd unlisted",
                unread,
                tccd(DevToolState::Unlisted, 1),
                NotGranted,
            ),
            (
                "unread, tccd denied",
                unread,
                tccd(DevToolState::Denied, 1),
                NotGranted,
            ),
            (
                "unread, tccd allowed",
                unread,
                tccd(DevToolState::Allowed, 1),
                Granted,
            ),
            ("unread, tccd quiet", unread, quiet, Unknown),
            ("unread, no log", unread, no_log, Unknown),
            (
                "a list in an unknown shape",
                DevToolList::refused(DevToolListLabel::FormatUnrecognized),
                no_log,
                Unknown,
            ),
            (
                "pending",
                DevToolList::pending(),
                DevToolProbe::pending(),
                Unknown,
            ),
        ] {
            assert_eq!(devtool_grant(&list, &log), want, "{name}");
        }
    }

    /// The wire tokens, pinned: they are the `privacy` row's schema.
    #[test]
    fn the_list_and_grant_tokens_are_the_documented_spellings() {
        for (listing, token) in [
            (DevToolListing::On, "on"),
            (DevToolListing::Off, "off"),
            (DevToolListing::Absent, "absent"),
            (DevToolListing::Unknown, "unknown"),
        ] {
            assert_eq!(listing.as_str(), token);
        }
        for (grant, token) in [
            (DevToolGrant::Granted, "granted"),
            (DevToolGrant::NotGranted, "not_granted"),
            (DevToolGrant::Unknown, "unknown"),
        ] {
            assert_eq!(grant.as_str(), token);
        }
        use DevToolListLabel as L;
        for (label, token) in [
            (L::Pending, "pending"),
            (L::Read, "read"),
            (L::NoFullDiskAccess, "no_full_disk_access"),
            (L::OpenFailed, "open_failed"),
            (L::ToolFailed, "sqlite_failed"),
            (L::FormatUnrecognized, "format_unrecognized"),
            (L::TimedOut, "timed_out"),
            (L::SpawnFailed, "spawn_failed"),
            (L::OutputUnreadable, "output_unreadable"),
            (L::RefusedDisabled, "refused_disabled"),
            (L::RefusedObserverOff, "refused_observer_off"),
            (L::RefusedOutOfBundle, "refused_out_of_bundle"),
            (L::RefusedNoExe, "refused_no_exe"),
            (L::RefusedNoSubject, "refused_no_subject"),
            (L::UnsupportedPlatform, "unsupported_platform"),
        ] {
            assert_eq!(label.as_str(), token);
        }
        assert_eq!(DevToolListing::default(), DevToolListing::Unknown);
        assert_eq!(DevToolGrant::default(), DevToolGrant::Unknown);
        assert_eq!(unsupported_devtool_list().label, L::UnsupportedPlatform);
    }

    /// The bundle id is read off the bundle's own `Info.plist`, and not at all
    /// under a protected root.
    #[test]
    fn the_bundle_id_is_read_off_the_bundle_and_never_under_a_protected_root() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("x.app");
        std::fs::create_dir_all(root.join("Contents")).expect("bundle");
        std::fs::write(
            root.join("Contents").join("Info.plist"),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleIdentifier</key><string>com.example.x</string></dict></plist>\n",
        )
        .expect("plist");
        assert_eq!(bundle_id_of(&root, &[]), Some("com.example.x".to_string()));
        assert_eq!(bundle_id_of(&root, &[dir.path().to_path_buf()]), None);
        assert_eq!(bundle_id_of(&dir.path().join("missing.app"), &[]), None);
    }

    /// `run_bounded_exit` keeps apart what `run_bounded` folds into `None`: an
    /// exit code (a signal's is `None`), a missing program, a run past its
    /// ceiling, and output read in part. `/bin/sh` stands in for the tool. A
    /// run that must finish gets a minute (a hang detector, not a budget).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_bounded_run_keeps_its_outcome_apart() {
        let sh = |script: &str, ceiling_ms: u64| {
            run_bounded_exit(
                "/bin/sh",
                &["-c".as_ref(), script.as_ref()],
                std::time::Duration::from_millis(ceiling_ms),
                TOOL_OUTPUT_MAX_BYTES,
            )
        };
        assert_eq!(
            sh("printf out; printf err >&2; exit 3", 60_000),
            ToolRun::Exited {
                code: Some(3),
                stdout: "out".to_string(),
                stderr: "err".to_string(),
            }
        );
        // SIGKILL, never a core-dump signal (AGENTS.md).
        assert!(matches!(
            sh("kill -KILL $$", 60_000),
            ToolRun::Exited { code: None, .. }
        ));
        assert_eq!(sh("sleep 5", 200), ToolRun::TimedOut);
        assert_eq!(sh("printf '\\377'", 60_000), ToolRun::Unreadable);
        assert_eq!(
            sh(
                &format!("head -c {} /dev/zero", TOOL_OUTPUT_MAX_BYTES + 1),
                60_000
            ),
            ToolRun::Unreadable
        );
        // The cap is the caller's: the same stream reads whole under the
        // Developer Tools read's larger one.
        let over = TOOL_OUTPUT_MAX_BYTES + 1;
        let script = format!("head -c {over} /dev/zero");
        match run_bounded_exit(
            "/bin/sh",
            &["-c".as_ref(), script.as_ref()],
            std::time::Duration::from_secs(60),
            DEVTOOL_OUTPUT_MAX_BYTES,
        ) {
            ToolRun::Exited {
                code: Some(0),
                stdout,
                ..
            } => assert_eq!(u64::try_from(stdout.len()).ok(), Some(over)),
            other => panic!("read whole under the larger cap: {other:?}"),
        }
        let missing =
            std::env::temp_dir().join(format!("aterm-no-such-tool-{}", std::process::id()));
        assert_eq!(
            run_bounded_exit(
                missing.to_str().unwrap(),
                &[],
                std::time::Duration::from_secs(1),
                TOOL_OUTPUT_MAX_BYTES,
            ),
            ToolRun::Absent
        );
        // `run_bounded` is the same run with the outcome folded.
        assert_eq!(
            run_bounded(
                "/bin/sh",
                &["-c".as_ref(), "exit 3".as_ref()],
                CODESIGN_CEILING
            ),
            Some((false, String::new(), String::new()))
        );
    }

    /// A test binary is linker-signed ad hoc with no hardened runtime, so its
    /// own flags read `no`. One `csops` about this process: no spawn, no
    /// dialog.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_test_binary_is_not_hardened() {
        assert_eq!(self_hardening(), SelfHardening::NotHardened);
    }

    /// The membership query is total from a test binary (a directory lookup:
    /// no spawn, no `tccd`, no `WindowServer`), and a group that does not exist
    /// is no answer rather than "not a member".
    #[cfg(target_os = "macos")]
    #[test]
    fn the_membership_query_is_total() {
        let _ = debugger_membership();
        assert_eq!(imp::in_group("aterm-no-such-group-for-tests"), None);
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
    /// point refuses, end to end through the function production calls.
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
    /// call is total — some answer, no panic, no hang.
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
    /// read of an `Info.plist` that is not under a protected root.
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
