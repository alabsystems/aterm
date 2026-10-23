// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! macOS privacy-consent (TCC) and debugger-access posture probes and classifiers.
//!
//! * TCC posture: Full Disk Access ([`FdaState`], [`FdaScope`], [`SpikeEvidence`], [`probe_fda`]),
//!   process responsibility ([`responsible_pid`], [`responsible_pids_detailed`],
//!   [`responsible_app`]), session [`Attribution`], and [`ResetPlan`] ([`tccutil_reset_command`],
//!   [`tccutil_presence`]).
//! * Debugger and Developer Tools posture ([`TASKPORT_DEBUG_RIGHT`]): [`DebuggerMode`]
//!   ([`probe_debugger`], [`run_devtools_change`]), [`DebuggerMember`] ([`debugger_membership`]),
//!   [`SelfHardening`] ([`self_hardening`], [`process_birth`]), and [`devtool_grant`]
//!   ([`DevToolState`], [`DevToolListing`], [`probe_devtool`], [`probe_devtool_list`]).
//! * [`path_is_in_app_bundle`] fences bundle-only probes ([`ProbeLabel::RefusedOutOfBundle`]),
//!   and [`protected_roots`] derives sensitive paths from [`crate::sbpl::PRIVATE_SUBDIRS`].

use std::path::{Component, Path, PathBuf};

/// `EPERM`. A TCC denial is `EPERM`, not `EACCES` ().
pub const ERRNO_EPERM: i32 = 1;

/// `ESRCH` — "no such process".
#[cfg(any(target_os = "macos", test))]
pub const ERRNO_ESRCH: i32 = 3;

/// `ENOENT`. On a TCC-guarded path tccd decides before the lookup.
pub const ERRNO_ENOENT: i32 = 2;

#[cfg(any(target_os = "macos", test))]
const TCC_DB_RELATIVE: &str = "Library/Application Support/com.apple.TCC/TCC.db";

const INFO_PLIST_MAX_BYTES: u64 = 256 * 1024;

/// Whether the responsible app holds Full Disk Access (`kTCCServiceSystemPolicyAllFiles`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FdaState {
    /// The `TCC.db` open succeeded.
    Granted,
    /// The open returned [`ERRNO_EPERM`].
    Denied,
    /// Not measured.
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

/// How far a Full Disk Access grant reaches.
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

/// The observation's progress or outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProbeLabel {
    /// No current result: an asynchronous observer is waiting or refreshing.
    Pending,
    /// The open succeeded.
    OpenOk,
    /// The open returned [`ERRNO_EPERM`].
    OpenEperm,
    /// The open failed with some other errno (carried for the report).
    OpenErrno(i32),
    /// Refused: the running executable is not inside a `.app` bundle.
    RefusedOutOfBundle,
    /// Refused: the running executable could not be resolved.
    RefusedNoExe,
    /// Refused: `[privacy] enabled` or `[privacy] check` is off.
    RefusedDisabled,
    /// Refused: `$HOME` is unset.
    RefusedNoHome,
    /// Refused: the store path contains an interior NUL.
    RefusedBadPath,
    /// Not macOS. There is no TCC here.
    UnsupportedPlatform,
}

impl ProbeLabel {
    /// The `probe=` token.
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

    /// `true` for a refusal (an unsupported platform included).
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

/// One probe result: the state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FdaProbe {
    /// The classified state.
    pub state: FdaState,
    /// The observation's progress, refusal, or completed outcome.
    pub label: ProbeLabel,
}

impl FdaProbe {
    /// No current observation.
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

/// The two `[privacy]` switches that gate the probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProbeGate {
    /// `[privacy] enabled`.
    pub enabled: bool,
    /// `[privacy] check`.
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

    /// `true` when configuration permits a syscall (the in-bundle fence is checked apart).
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

/// Classify one probe syscall.
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

/// The answer on a platform that has no TCC.
#[cfg(any(test, not(target_os = "macos")))]
#[must_use]
pub const fn unsupported_probe() -> FdaProbe {
    FdaProbe::refused(ProbeLabel::UnsupportedPlatform)
}

/// The probe with the executable, the home directory and the syscall injected.
#[cfg(any(target_os = "macos", test))]
pub fn probe_fda_with<F>(
    gate: ProbeGate,
    exe: Option<&Path>,
    home: Option<&Path>,
    mut syscall: F,
) -> FdaProbe
where
    F: FnMut(&Path) -> ProbeOutcome,
{
    if !gate.permits() {
        return FdaProbe::refused(ProbeLabel::RefusedDisabled);
    }
    let Some(exe) = exe else {
        return FdaProbe::refused(ProbeLabel::RefusedNoExe);
    };
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
    match syscall(&db) {
        ProbeOutcome::Errno(ERRNO_ENOENT) => classify_probe(syscall(Path::new(SYSTEM_TCC_DB))),
        outcome => classify_probe(outcome),
    }
}

/// Probe Full Disk Access for the running process.
#[must_use]
pub fn probe_fda(gate: ProbeGate) -> FdaProbe {
    #[cfg(target_os = "macos")]
    {
        let exe = fence_exe();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let child = exe.clone();
        probe_fda_with(gate, exe.as_deref(), home.as_deref(), |db| {
            open_probe_in_child(child.as_deref(), db)
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = gate;
        unsupported_probe()
    }
}

/// The hidden verb `aterm __tcc-open <path>` ([`run_open_probe_verb`]).
pub const OPEN_PROBE_VERB: &str = "__tcc-open";

#[cfg(target_os = "macos")]
const OPEN_PROBE_CEILING: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(target_os = "macos")]
fn open_probe_in_child(exe: Option<&Path>, db: &Path) -> ProbeOutcome {
    open_probe_in_child_bounded(exe, db, OPEN_PROBE_CEILING)
}

#[cfg(target_os = "macos")]
fn open_probe_in_child_bounded(
    exe: Option<&Path>,
    db: &Path,
    ceiling: std::time::Duration,
) -> ProbeOutcome {
    if let Some(program) = exe.and_then(Path::to_str) {
        match run_bounded_exit(
            program,
            &[OPEN_PROBE_VERB.as_ref(), db.as_os_str()],
            ceiling,
            TOOL_OUTPUT_MAX_BYTES,
        ) {
            ToolRun::TimedOut => return ProbeOutcome::Errno(libc::ETIMEDOUT),
            ToolRun::Exited {
                code: Some(0),
                stdout,
                ..
            } => {
                if let Some(outcome) = parse_open_probe_line(&stdout) {
                    return outcome;
                }
            }
            _ => {}
        }
    }
    imp::open_probe(db)
}

/// The one line [`run_open_probe_verb`] prints for `outcome`.
#[must_use]
pub fn open_probe_line(outcome: ProbeOutcome) -> String {
    match outcome {
        ProbeOutcome::Ok => "ok".to_owned(),
        ProbeOutcome::Errno(errno) => format!("errno {errno}"),
    }
}

/// Read back what [`open_probe_line`] printed.
#[must_use]
pub fn parse_open_probe_line(out: &str) -> Option<ProbeOutcome> {
    match out.trim_end_matches('\n').split_once(' ') {
        None if out.trim_end_matches('\n') == "ok" => Some(ProbeOutcome::Ok),
        Some(("errno", errno)) => errno.parse().ok().map(ProbeOutcome::Errno),
        _ => None,
    }
}

/// [`OPEN_PROBE_VERB`]: open the one path named, close it unread, and print [`open_probe_line`].
#[must_use]
pub fn run_open_probe_verb(args: &[std::ffi::OsString]) -> std::process::ExitCode {
    #[cfg(target_os = "macos")]
    if let [db] = args {
        println!("{}", open_probe_line(imp::open_probe(Path::new(db))));
        return std::process::ExitCode::SUCCESS;
    }
    let _ = args;
    eprintln!("aterm {OPEN_PROBE_VERB}: one path, on macOS");
    std::process::ExitCode::FAILURE
}

/// This Mac's hardware UUID (`gethostuuid(2)`), the key of what aterm verified on this Mac.
#[must_use]
pub fn host_uuid() -> Option<String> {
    host_uuid_cell()
        .get_or_init(|| {
            #[cfg(target_os = "macos")]
            {
                imp::host_uuid()
            }
            #[cfg(not(target_os = "macos"))]
            {
                None
            }
        })
        .clone()
}

/// [`host_uuid`] only if something has read it.
#[must_use]
pub fn host_uuid_if_warm() -> Option<&'static str> {
    host_uuid_cell().get().and_then(Option::as_deref)
}

fn host_uuid_cell() -> &'static std::sync::OnceLock<Option<String>> {
    static HOST_UUID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    &HOST_UUID
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn format_uuid(id: &[u8; 16]) -> String {
    use std::fmt::Write as _;
    let hex = id.iter().fold(String::with_capacity(32), |mut hex, byte| {
        let _ = write!(hex, "{byte:02X}");
        hex
    });
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// `true` when `exe` lies inside a `.app` bundle.
#[must_use]
pub fn path_is_in_app_bundle(exe: &Path) -> bool {
    app_bundle_root(exe).is_some()
}

/// The `.app` directory `exe` lives inside, if any (`/Applications/aterm.app/Contents/MacOS/aterm`
/// ⇒ `/Applications/aterm.app`); also the cache key's bundle half.
#[must_use]
pub fn app_bundle_root(exe: &Path) -> Option<PathBuf> {
    bundle_root_where(exe, |dir| {
        Path::new(dir.as_os_str())
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
    })
}

/// The executable the in-bundle fence judges for the fenced probes.
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

/// Whether the bundle macOS keys this process's consent decisions to can still be found.
///
/// ```text
///              current_exe()                 proc_pidpath
/// before       …/A.app/Contents/MacOS/p      …/A.app/Contents/MacOS/p
/// after swap   …/A.app/Contents/MacOS/p      …/A.app.rollback/Contents/MacOS/p
/// after GC     …/A.app/Contents/MacOS/p      rc=0, errno ENOENT
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageAnchor {
    /// The image sits in a `.app` bundle.
    Live,
    /// The image sits in a bundle layout (`…/Contents/MacOS/…`) whose directory
    /// is no longer named `<something>.app` — `aterm.app.rollback` after an apply.
    Displaced,
    /// The image no longer has a path.
    Deleted,
    /// Not a bundle layout: a test binary, `targo run`, a dev build outside a `.app`.
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

    /// Whether this anchor means a held grant cannot be validated against the running code.
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
    /// It has no path: `proc_pidpath` failed with `ENOENT`.
    NoPath,
    /// The lookup failed for another reason.
    Unreadable,
}

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

#[must_use]
fn bundle_layout_root(exe: &Path) -> Option<PathBuf> {
    bundle_root_where(exe, |dir| matches!(dir, Component::Normal(_)))
}

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

#[must_use]
#[cfg(any(target_os = "macos", test))]
fn path_has_interior_nul(path: &Path) -> bool {
    path.as_os_str().as_encoded_bytes().contains(&0)
}

/// Why the responsibility SPI could not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ResponsibleError {
    /// The symbol is not present in this process image.
    SymbolUnavailable,
    /// [`ERRNO_EPERM`].
    NotOurs,
    /// [`ERRNO_ESRCH`].
    Gone,
    /// `-1` with some other errno.
    Errno(i32),
    /// Not macOS.
    Unsupported,
}

/// The `responsible=` corroboration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Responsible {
    /// The responsible pid is this process.
    SelfProcess,
    /// The responsible pid is gone.
    Exited,
    /// The responsible pid is some other live process.
    Other(i32),
    /// The SPI is absent, refused, or answered `-1` for a reason that does not identify anything.
    #[default]
    Unknown,
}

impl Responsible {
    /// The `responsible=` token, or `None` for [`Responsible::Other`].
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
#[must_use]
pub const fn classify_responsible(
    self_pid: i32,
    answer: Result<i32, ResponsibleError>,
) -> Responsible {
    match answer {
        Ok(pid) if pid == self_pid => Responsible::SelfProcess,
        Ok(pid) if pid > 0 => Responsible::Other(pid),
        Err(ResponsibleError::Gone) => Responsible::Exited,
        Ok(_) | Err(_) => Responsible::Unknown,
    }
}

/// Map the SPI's `(return value, errno)` pair to a result.
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
#[must_use]
pub fn responsible_pid(pid: i32) -> Option<i32> {
    responsible_pid_detailed(pid).ok()
}

/// [`responsible_pid`], keeping the reason a `-1` carries.
pub fn responsible_pid_detailed(pid: i32) -> Result<i32, ResponsibleError> {
    responsible_pids_detailed(&[pid])
        .pop()
        .unwrap_or(Err(ResponsibleError::Unsupported))
}

/// [`responsible_pid_detailed`] for each of `pids`, in order.
#[must_use]
pub fn responsible_pids_detailed(pids: &[i32]) -> Vec<Result<i32, ResponsibleError>> {
    #[cfg(target_os = "macos")]
    {
        ask_each(pids, imp::responsibility_spi)
    }
    #[cfg(not(target_os = "macos"))]
    {
        vec![Err(ResponsibleError::Unsupported); pids.len()]
    }
}

#[cfg(any(target_os = "macos", test))]
fn ask_each<F>(
    pids: &[i32],
    resolve: impl FnOnce() -> Option<F>,
) -> Vec<Result<i32, ResponsibleError>>
where
    F: FnMut(i32) -> Result<i32, ResponsibleError>,
{
    if pids.is_empty() {
        return Vec::new();
    }
    match resolve() {
        Some(mut ask) => pids.iter().map(|&pid| ask(pid)).collect(),
        None => vec![Err(ResponsibleError::SymbolUnavailable); pids.len()],
    }
}

/// The app macOS holds responsible for `pid`, resolved far enough to name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsibleApp {
    /// The responsible pid.
    pub pid: i32,
    /// Its executable path, from `proc_pidpath`.
    pub path: PathBuf,
    /// Its display name: `CFBundleDisplayName`, else `CFBundleName`.
    pub display_name: String,
}

/// The `Info.plist` a display name may be read from, or `None` when there is none we may touch.
#[must_use]
pub fn readable_info_plist(exe: &Path, protected: &[PathBuf]) -> Option<PathBuf> {
    let root = app_bundle_root(exe)?;
    if is_under_protected_root(&root, protected) {
        return None;
    }
    Some(root.join("Contents").join("Info.plist"))
}

/// `true` when `path` is `root` or lives under it, for any root.
#[must_use]
pub fn is_under_protected_root(path: &Path, protected: &[PathBuf]) -> bool {
    protected.iter().any(|root| path.starts_with(root))
}

/// The display name for an executable.
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

/// The class of a code-signing designated requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DrClass {
    /// `identifier "…" and anchor apple generic … certificate leaf …`.
    Identity,
    /// The requirement pins a `cdhash`.
    Cdhash,
    /// The code object is not signed at all.
    Unsigned,
    /// Not recognised.
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

    /// `grant_stable=`.
    #[must_use]
    pub const fn grant_stable(self) -> bool {
        matches!(self, Self::Identity)
    }
}

/// Classify a `codesign -d -r-` requirement string (pure: it does not shell out).
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

fn has_cdhash_clause(text: &str) -> bool {
    text.match_indices("cdhash").any(|(at, word)| {
        let joined = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-');
        !text[..at].chars().next_back().is_some_and(joined)
            && text[at + word.len()..].trim_start().starts_with("h\"")
    })
}

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

/// A bounded read of a text plist.
#[must_use]
pub fn read_plist_text(path: &Path) -> Option<String> {
    String::from_utf8(read_plist_bytes(path)?).ok()
}

fn read_plist_bytes(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > INFO_PLIST_MAX_BYTES {
        return None;
    }
    std::fs::read(path).ok()
}

/// The string immediately following one exact XML plist key, non-empty and trimmed.
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
    /// How that requirement is shaped.
    pub dr: DrClass,
    /// `developer-id` | `adhoc` | `unsigned` | `unknown`.
    pub signing: &'static str,
    /// The Team ID, when there is a real one.
    pub team: Option<String>,
}

/// Read one bundle's identity.
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

fn identity_from_report(report: Option<&str>, bundle_id: Option<String>) -> BundleIdentity {
    let dr_text = report.and_then(designated_requirement).unwrap_or_default();
    let signing = report.map_or("unknown", classify_signing);
    BundleIdentity {
        bundle_id,
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

#[cfg(not(target_arch = "wasm32"))]
#[cfg(target_os = "macos")]
fn run_bounded(
    program: &str,
    args: &[&std::ffi::OsStr],
    ceiling: std::time::Duration,
) -> Option<(bool, String, String)> {
    match run_bounded_exit(program, args, ceiling, TOOL_OUTPUT_MAX_BYTES) {
        ToolRun::Exited {
            code,
            stdout,
            stderr,
        } => Some((code == Some(0), stdout, stderr)),
        ToolRun::Absent | ToolRun::SpawnFailed | ToolRun::TimedOut | ToolRun::Unreadable => None,
    }
}

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
    let deadline = std::time::Instant::now() + ceiling;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return ToolRun::TimedOut;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return ToolRun::Unreadable;
            }
        }
    };
    #[cfg(test)]
    let mut deadline = deadline;
    while !(out.is_finished() && err.is_finished()) {
        if std::time::Instant::now() >= deadline {
            return ToolRun::TimedOut;
        }
        #[cfg(test)]
        tests::readers_pending(&mut deadline);
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

#[cfg(target_os = "macos")]
const TOOL_OUTPUT_MAX_BYTES: u64 = 1024 * 1024;

#[cfg(not(target_os = "macos"))]
fn codesign_report(_root: &Path) -> Option<String> {
    None
}

#[cfg(target_os = "macos")]
const CODESIGN_CEILING: std::time::Duration = std::time::Duration::from_secs(10);

fn designated_requirement(report: &str) -> Option<String> {
    report.lines().find_map(|line| {
        let line = line.trim();
        let clause = line.strip_prefix('#').map_or(line, str::trim_start);
        clause
            .starts_with("designated =>")
            .then(|| clause.to_owned())
    })
}

fn team_identifier(report: &str) -> Option<String> {
    report.lines().find_map(|line| {
        let value = line.trim().strip_prefix("TeamIdentifier=")?.trim();
        (!value.is_empty() && value != "not set").then(|| value.to_owned())
    })
}

fn classify_signing(report: &str) -> &'static str {
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

/// At most this many claimant lines on any surface.
pub const MAX_CLAIMANT_ROWS: usize = 8;

/// How much of the candidate space the census got to look at.
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
    /// The links the census found that lead to this bundle.
    pub links: Vec<PathBuf>,
}

impl Claimant {
    fn conflicts_with(&self, other: &Self) -> bool {
        let (mine, theirs) = (self.dr_text.trim(), other.dr_text.trim());
        mine.is_empty() || theirs.is_empty() || mine != theirs
    }
}

/// Whether the Security panel's *Move to Trash* takes one conflicting copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrashOffer {
    /// The button takes it.
    Offered,
    /// Signed with a certificate, as a release is — a Developer ID copy is
    /// never moved, whatever its requirement says — or with a signature that
    /// could not be read, so it is not known to be a build either.
    Signature,
    /// Signed ad hoc, but its requirement does not read as its cdhash.
    Requirement,
    /// A link someone keeps leads to it ([`Claimant::links`]).
    Linked,
    /// The copy running here does not keep its grant across updates.
    RunningUnstable,
    /// This Mac has no trash tool to move it.
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
    /// ones that can reset a grant.
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

    fn running_stable(&self) -> bool {
        self.found.iter().any(|c| c.running && c.dr.grant_stable())
    }

    /// Every conflicting copy the rule ([`TrashOffer`]) lets the move take.
    #[must_use]
    pub fn retirable(&self) -> Vec<&Claimant> {
        let stable = self.running_stable();
        self.conflicting()
            .into_iter()
            .filter(|copy| TrashOffer::of(copy, stable) == TrashOffer::Offered)
            .collect()
    }

    /// The rows every surface lists (the first [`MAX_CLAIMANT_ROWS`] conflicting copies).
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

    /// The copies the Security panel offers to move to the Trash.
    #[must_use]
    pub fn offered_for_trash(&self, trash_tool: bool) -> Vec<&Claimant> {
        self.listed(trash_tool)
            .into_iter()
            .filter_map(|(copy, offer)| (offer == TrashOffer::Offered).then_some(copy))
            .collect()
    }

    /// Whether the census may say nothing else claims the id.
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
    /// A bundle that could not be read.
    Hole,
    /// A bundle claiming the identifier.
    Ours(BundleIdentity),
}

/// Which bundle one spelling of a candidate names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    /// The bundle directory's volume and file number, links followed.
    pub file: (u64, u64),
    /// The bundle itself: the spelling, or where a link to it resolves.
    pub path: PathBuf,
    /// Whether the spelling is a link to the bundle rather than its own name.
    pub link: bool,
}

/// Locate one spelling ([`Located`]).
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

/// Off unix there is no file number to compare.
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

/// Classify one candidate for `bundle_id`.
#[must_use]
pub fn read_candidate(root: &Path, bundle_id: &str, protected: &[PathBuf]) -> Candidate {
    let plist = root.join("Contents").join("Info.plist");
    if !census_may_read(root, protected) || !census_may_read(&plist, protected) {
        return Candidate::Hole;
    }
    if !std::fs::metadata(root).is_ok_and(|m| m.is_dir()) {
        return Candidate::NotOurs;
    }
    match std::fs::symlink_metadata(&plist) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Candidate::NotOurs,
        Err(_) => return Candidate::Hole,
        Ok(meta) if !meta.is_file() => return Candidate::NotOurs,
        Ok(_) => {}
    }
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

/// Whether the bundle at `claimant.path` is still the copy the census classified.
#[must_use]
pub fn still_claims(claimant: &Claimant, bundle_id: &str) -> bool {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    matches!(
        read_candidate(&claimant.path, bundle_id, &census_fence(home.as_deref())),
        Candidate::Ours(id) if same_identity(&id, claimant)
    )
}

fn same_identity(fresh: &BundleIdentity, claimant: &Claimant) -> bool {
    fresh.dr != DrClass::Unknown
        && fresh.dr == claimant.dr
        && fresh.signing == claimant.signing
        && fresh.dr_text == claimant.dr_text
}

fn list_unless_protected(dir: &Path, protected: &[PathBuf]) -> Listing {
    if census_may_read(dir, protected) {
        list_root(dir)
    } else {
        Listing::Unreadable
    }
}

const BUNDLE_WALK_MAX_ENTRIES: usize = 4096;
const BUNDLE_WALK_MAX_DEPTH: usize = 16;

fn bundle_reaches_protected(root: &Path, protected: &[PathBuf]) -> bool {
    bundle_reaches_protected_on(root, protected, device_of, BUNDLE_WALK_MAX_ENTRIES)
}

fn bundle_reaches_protected_on(
    root: &Path,
    protected: &[PathBuf],
    device: impl Fn(&Path) -> Option<u64>,
    max_entries: usize,
) -> bool {
    let contents = root.join("Contents");
    if !census_may_read(&contents, protected) {
        return true;
    }
    let Ok(bundle) = std::fs::canonicalize(root) else {
        return true;
    };
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
    let Some(volume) = device(&bundle) else {
        return true;
    };
    if !inside.starts_with(&bundle) {
        return true;
    }
    let mut seen = 0usize;
    let Ok(top) = std::fs::read_dir(&bundle) else {
        return true;
    };
    for entry in top {
        let Ok(entry) = entry else {
            return true;
        };
        seen += 1;
        if seen > max_entries {
            return true;
        }
        if entry.file_name() != "Contents" && !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            return true;
        }
    }
    let mut stack = vec![(contents, 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if device(&dir) != Some(volume) {
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
            if seen > max_entries {
                return true;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                return true;
            };
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
    let plist = inside.join("Info.plist");
    match std::fs::symlink_metadata(&plist) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
        Ok(_) => !plist_names_plain_executables(&plist),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct InfoPlist {
    bundle_id: Option<String>,
    #[cfg(target_os = "macos")]
    plain_executables: bool,
}

#[cfg(any(target_os = "macos", test))]
fn plain_file_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(
        (parts.next(), parts.next()),
        (Some(Component::Normal(_)), None)
    ) && !name.chars().any(char::is_control)
}

#[cfg(target_os = "macos")]
fn read_info_plist(plist: &Path) -> Option<InfoPlist> {
    let bytes = read_plist_bytes(plist)?;
    Some(imp::parse_info_plist(&bytes).unwrap_or(InfoPlist {
        bundle_id: None,
        plain_executables: true,
    }))
}

#[cfg(not(target_os = "macos"))]
fn read_info_plist(plist: &Path) -> Option<InfoPlist> {
    let text = read_plist_text(plist)?;
    Some(InfoPlist {
        bundle_id: plist_string(&text, "CFBundleIdentifier").map(str::to_owned),
    })
}

#[cfg(target_os = "macos")]
fn plist_names_plain_executables(plist: &Path) -> bool {
    read_info_plist(plist).is_some_and(|info| info.plain_executables)
}

#[cfg(not(target_os = "macos"))]
fn plist_names_plain_executables(_plist: &Path) -> bool {
    true
}

#[cfg(unix)]
fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).ok().map(|meta| meta.dev())
}

#[cfg(not(unix))]
fn device_of(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|_| 0)
}

fn census_may_read(path: &Path, protected: &[PathBuf]) -> bool {
    !is_under_protected_root(path, protected)
        && !std::fs::canonicalize(path)
            .is_ok_and(|resolved| is_under_protected_root(&resolved, protected))
}

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

#[derive(PartialEq, Eq)]
enum SameBundle {
    File((u64, u64)),
    Spelled(PathBuf),
}

struct Reached {
    same: SameBundle,
    /// The path it is read and reported by.
    path: PathBuf,
    /// Whether `path` is only where a link resolves.
    resolved: bool,
    /// The links that lead to it.
    links: Vec<PathBuf>,
}

/// The census, pure over its sources.
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

/// The census for one identifier, against this machine.
#[must_use]
pub fn claimants_for(bundle_id: &str) -> Claimants {
    let running = running_bundle();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let roots = claimant_search_roots(running.as_deref(), home.as_deref());
    let registered = registered_bundles(bundle_id);
    let protected = census_fence(home.as_deref());
    census_in(
        bundle_id,
        running.as_deref(),
        &roots,
        registered.as_deref(),
        &protected,
    )
}

#[must_use]
fn census_fence(home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = protected_roots_in(home, &[]);
    if let Some(h) = home {
        roots.extend(app_data_roots(h));
        if let Ok(canon) = std::fs::canonicalize(h)
            && canon != h
        {
            for p in protected_roots_in(Some(&canon), &[])
                .into_iter()
                .chain(app_data_roots(&canon))
            {
                if !roots.contains(&p) {
                    roots.push(p);
                }
            }
        }
    }
    roots
}

/// The `kTCCServiceSystemPolicyAppData` parent roots under `home`.
#[must_use]
pub fn app_data_roots(home: &Path) -> Vec<PathBuf> {
    crate::sbpl::APP_DATA_ROOTS
        .iter()
        .map(|sub| home.join(sub))
        .collect()
}

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

fn running_bundle() -> Option<PathBuf> {
    running_bundle_from(self_image(), || {
        std::env::current_exe()
            .ok()
            .and_then(|exe| std::fs::canonicalize(exe).ok())
    })
}

fn running_bundle_from(
    image: SelfImage,
    exec: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    match image {
        SelfImage::At(path) => bundle_layout_root(&path),
        SelfImage::NoPath => None,
        SelfImage::Unreadable => exec().and_then(|exe| bundle_layout_root(&exe)),
    }
}

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
    /// Left in place: a fresh check no longer offers it.
    NotOffered,
    /// Left in place: a process runs from it, or that could not be ruled out.
    InUse,
    /// The move was refused, in the system's words.
    Failed(String),
}

/// Move to the Trash each copy in `plan` (the copies the owner was shown) that a fresh census
/// still offers ([`Claimants::retirable`]), that is still the same bundle right before the move
/// (`unchanged`, see [`still_claims`]), and that no process runs from.
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

/// The protected roots, resolved once, as absolute paths.
#[must_use]
pub fn protected_roots(overrides: &[String]) -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    protected_roots_in(home.as_deref(), overrides)
}

/// [`protected_roots`], with `$HOME` injected.
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

/// The paths under `$HOME`, beyond [`crate::sbpl::PRIVATE_SUBDIRS`].
pub const TCC_PROMPTING_SUBPATHS: &[&str] = &[
    "Library/Containers",
    "Library/Group Containers",
    "Library/CloudStorage",
    "Library/Mobile Documents",
    ".Trash",
];

/// The machine-wide paths whose `stat` can raise a privacy prompt or mount a volume.
pub const TCC_PROMPTING_ABSOLUTE: &[&str] = &["/Volumes", "/System/Volumes/Data/home", "/Network"];

/// Every path whose `stat` can put a privacy dialog on the owner's screen, absolute.
#[must_use]
pub fn tcc_prompting_roots(home: &Path, overrides: &[String]) -> Vec<PathBuf> {
    let mut roots = protected_roots_in(Some(home), &[]);
    for extra in protected_roots_in(Some(home), overrides) {
        if !roots.contains(&extra) {
            roots.push(extra);
        }
    }
    roots.extend(TCC_PROMPTING_SUBPATHS.iter().map(|sub| home.join(sub)));
    roots.extend(TCC_PROMPTING_ABSOLUTE.iter().map(PathBuf::from));
    roots
}

/// The folder that holds every user's home (`/Users` for `/Users//a`).
#[must_use]
pub fn other_homes_root(home: &Path) -> Option<PathBuf> {
    if !home.is_absolute() {
        return None;
    }
    home.parent()
        .filter(|p| p.parent().is_some())
        .map(Path::to_path_buf)
}

/// One promptable per-folder service, named the way `[privacy] warmup_folders` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Folder {
    /// `~/Documents` — `kTCCServiceSystemPolicyDocumentsFolder`.
    Documents,
    /// `~/Desktop` — `kTCCServiceSystemPolicyDesktopFolder`.
    Desktop,
    /// `~/Downloads` — `kTCCServiceSystemPolicyDownloadsFolder`.
    Downloads,
    /// Another application's data.
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

    /// What the warm-up asks for when `[privacy] warmup_folders` is absent.
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

    /// The `tccutil` service name (`SystemPolicyDocumentsFolder`).
    #[must_use]
    pub const fn tcc_service(self) -> &'static str {
        match self {
            Self::Documents => "SystemPolicyDocumentsFolder",
            Self::Desktop => "SystemPolicyDesktopFolder",
            Self::Downloads => "SystemPolicyDownloadsFolder",
            Self::AppData => "SystemPolicyAppData",
        }
    }

    /// Parse a configuration name.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "documents" => Some(Self::Documents),
            "desktop" => Some(Self::Desktop),
            "downloads" => Some(Self::Downloads),
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
            Self::AppData => home
                .join(crate::sbpl::APP_DATA_ROOTS[0])
                .join(APP_DATA_PROBE_BUNDLE),
        }
    }

    /// The display name, so every surface spells it alike and none derives a sentence from
    /// the config token.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Documents => "Documents",
            Self::Desktop => "Desktop",
            Self::Downloads => "Downloads",
            Self::AppData => "Other applications' data",
        }
    }

    /// The noun a sentence about this item uses.
    #[must_use]
    pub const fn noun(self) -> &'static str {
        match self {
            Self::Documents | Self::Desktop | Self::Downloads => "folder",
            Self::AppData => "data",
        }
    }

    /// Whether a missing probe path is a permission answer rather than a failure.
    #[must_use]
    pub const fn absent_means_allowed(self) -> bool {
        match self {
            Self::Documents | Self::Desktop | Self::Downloads => false,
            Self::AppData => true,
        }
    }
}

const APP_DATA_PROBE_BUNDLE: &str = "com.apple.Notes";

/// Resolve configured folder names to `(folder, absolute path)` pairs, in order.
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

/// The one bundle id a `tccutil reset` may name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResetSubject(String);

impl ResetSubject {
    /// Read the subject from the bundle at `root` ([`bundle_id_of`]).
    #[must_use]
    pub fn read(root: &Path, protected: &[PathBuf]) -> Option<Self> {
        Self::from_usable(&bundle_id_of(root, protected)?)
    }

    fn from_usable(bundle_id: &str) -> Option<Self> {
        bundle_id_is_usable(bundle_id).then(|| Self(bundle_id.trim().to_owned()))
    }

    /// The subject every invocation names.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Build the `tccutil reset <service> <bundle id>` argv for one folder.
#[must_use]
pub fn tccutil_reset_command(subject: &ResetSubject, folder: Folder) -> Vec<String> {
    vec![
        String::from("/usr/bin/tccutil"),
        String::from("reset"),
        String::from(folder.tcc_service()),
        String::from(subject.as_str()),
    ]
}

/// The `tccutil` binary, absolute.
#[cfg(any(unix, test))]
pub const TCCUTIL_PATH: &str = "/usr/bin/tccutil";

/// Whether [`TCCUTIL_PATH`] can be run at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TccutilPresence {
    /// A file at [`TCCUTIL_PATH`] carrying an execute bit.
    Executable,
    /// Nothing usable at [`TCCUTIL_PATH`].
    Missing,
    /// Present, but with no execute bit.
    NotExecutable,
    /// Not determined: the path could not be examined.
    #[default]
    Unknown,
}

impl TccutilPresence {
    /// Whether an invocation may be attempted.
    #[must_use]
    pub const fn can_run(self) -> bool {
        matches!(self, Self::Executable)
    }
}

/// Classify a stat of [`TCCUTIL_PATH`] (the caller does the stat).
#[must_use]
#[cfg(any(unix, test))]
pub const fn classify_tccutil(is_file: bool, executable: bool) -> TccutilPresence {
    match (is_file, executable) {
        (false, _) => TccutilPresence::Missing,
        (true, true) => TccutilPresence::Executable,
        (true, false) => TccutilPresence::NotExecutable,
    }
}

/// One `stat` of [`TCCUTIL_PATH`].
#[cfg(unix)]
#[must_use]
pub fn tccutil_presence() -> TccutilPresence {
    tccutil_presence_at(Path::new(TCCUTIL_PATH))
}

#[cfg(unix)]
fn tccutil_presence_at(path: &Path) -> TccutilPresence {
    use std::os::unix::fs::PermissionsExt as _;

    match std::fs::metadata(path) {
        Ok(md) => classify_tccutil(md.is_file(), md.permissions().mode() & 0o111 != 0),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => TccutilPresence::Missing,
        Err(_) => TccutilPresence::Unknown,
    }
}

/// Off unix there is no execute bit to read.
#[cfg(not(unix))]
#[must_use]
pub fn tccutil_presence() -> TccutilPresence {
    TccutilPresence::Unknown
}

/// Whether a string may be used as the `tccutil` subject.
#[must_use]
pub fn bundle_id_is_usable(bundle_id: &str) -> bool {
    let id = bundle_id.trim();
    !id.is_empty()
        && !id.starts_with('-')
        && !id.contains(char::is_whitespace)
        && !id.chars().any(char::is_control)
}

/// What the Settings panel should do about the *Ask again* repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetOffer {
    /// Show the button.
    Offer,
    /// Show no button; say a rebuild, not the owner, invalidated the grant.
    ExplainRebuild,
    /// Show nothing: [`TCCUTIL_PATH`] cannot be run here.
    HideNoTool,
    /// Show nothing: the running bundle id is unknown.
    HideNoBundleId,
}

impl ResetOffer {
    /// Whether the *Ask again* button is rendered at all.
    #[must_use]
    pub const fn shows_button(self) -> bool {
        matches!(self, Self::Offer)
    }
}

/// Everything [`reset_offer`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ResetOfferInputs<'a> {
    /// The designated-requirement class of the running code object ([`classify_dr`]).
    pub dr: DrClass,
    /// Whether [`TCCUTIL_PATH`] can be run ([`tccutil_presence`]).
    pub tccutil: TccutilPresence,
    /// The running bundle's reset subject.
    pub bundle_id: Option<&'a ResetSubject>,
}

/// Decide whether the denied-state repair may be offered.
#[must_use]
pub const fn reset_offer(inputs: ResetOfferInputs<'_>) -> ResetOffer {
    match inputs.dr {
        DrClass::Cdhash | DrClass::Unsigned => ResetOffer::ExplainRebuild,
        DrClass::Identity | DrClass::Unknown => {
            if inputs.tccutil.can_run() {
                match inputs.bundle_id {
                    Some(_) => ResetOffer::Offer,
                    None => ResetOffer::HideNoBundleId,
                }
            } else {
                ResetOffer::HideNoTool
            }
        }
    }
}

/// One owner-initiated *Ask again*.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResetPlan {
    subject: ResetSubject,
    folders: Vec<Folder>,
}

impl ResetPlan {
    /// Build a plan, or `None` for no folder.
    #[must_use]
    pub fn new(subject: &ResetSubject, folders: &[Folder]) -> Option<Self> {
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
            subject: subject.clone(),
            folders: wanted,
        })
    }

    /// Build a plan only when [`reset_offer`] says the repair may be offered.
    #[must_use]
    pub fn for_offer(inputs: ResetOfferInputs<'_>, folders: &[Folder]) -> Option<Self> {
        if !reset_offer(inputs).shows_button() {
            return None;
        }
        Self::new(inputs.bundle_id?, folders)
    }

    /// The subject every invocation names.
    #[must_use]
    pub fn bundle_id(&self) -> &str {
        self.subject.as_str()
    }

    /// The folders, in plan order, each appearing once.
    #[must_use]
    pub fn folders(&self) -> &[Folder] {
        &self.folders
    }

    /// The argv for each folder, in plan order.
    #[must_use]
    pub fn commands(&self) -> Vec<(Folder, Vec<String>)> {
        self.folders
            .iter()
            .map(|folder| (*folder, tccutil_reset_command(&self.subject, *folder)))
            .collect()
    }
}

/// What one `tccutil reset` invocation did to one folder's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetStatus {
    /// Exited 0: this folder's row is gone, so macOS can ask about it again.
    Reset,
    /// Ran and exited nonzero.
    Declined {
        /// The process exit status, or `None` for a signal death.
        code: Option<i32>,
    },
    /// Not run: [`TCCUTIL_PATH`] is missing or not executable.
    ToolAbsent,
}

impl ResetStatus {
    /// Whether this folder's row actually changed.
    #[must_use]
    pub const fn is_reset(self) -> bool {
        matches!(self, Self::Reset)
    }

    /// Classify a finished invocation from `ExitStatus::code()`.
    #[must_use]
    pub const fn from_exit_status(code: Option<i32>) -> Self {
        match code {
            Some(0) => Self::Reset,
            code => Self::Declined { code },
        }
    }
}

/// One folder's independently recorded result.
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

    /// Record an invocation that could not be made because the tool is not there.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResetOutcome {
    /// Every invocation exited 0.
    AllReset,
    /// At least one folder reset and at least one did not.
    Partial,
    /// At least one invocation ran and no folder reset.
    NoneReset,
    /// Nothing ran at all: [`TCCUTIL_PATH`] is missing or not executable.
    ToolAbsent,
    /// There was nothing to attempt.
    NotAttempted,
}

/// Fold one gesture's per-invocation results into one outcome, once, with no retry.
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

/// The folders whose rows actually changed.
#[must_use]
pub fn folders_reset(attempts: &[ResetAttempt]) -> Vec<Folder> {
    attempts
        .iter()
        .filter(|a| a.is_reset())
        .map(|a| a.folder)
        .collect()
}

/// The folders macOS declined, each with the exit status to name in its one line.
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

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const DEVTOOLSSECURITY_PATH: &str = "/usr/sbin/DevToolsSecurity";

/// The one right `DevToolsSecurity` reads and writes ().
pub const TASKPORT_DEBUG_RIGHT: &str = "system.privilege.taskport.debug";

/// The one-line undo every surface that mentions developer mode prints.
pub const DEBUGGER_ACCESS_REVERT: &str = "/usr/sbin/DevToolsSecurity -disable";

#[cfg(target_os = "macos")]
const DEVTOOLS_STATUS_CEILING: std::time::Duration = std::time::Duration::from_secs(2);

/// The directory group `is-admin` names.
pub const ADMIN_GROUP: &str = "admin";

/// The directory group `is-developer` names.
pub const DEVELOPER_GROUP: &str = "_developer";

/// `CS_OPS_STATUS`.
pub const CS_OPS_STATUS: u32 = 0;

/// `CS_GET_TASK_ALLOW`.
pub const CS_GET_TASK_ALLOW: u32 = 0x0000_0004;

/// `CS_RUNTIME` — the hardened runtime, what `codesign` prints as `0x10000(runtime)` (same header).
pub const CS_RUNTIME: u32 = 0x0001_0000;

/// What `DevToolsSecurity -status` says about the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DebuggerMode {
    /// `Developer mode is currently enabled.`.
    Enabled,
    /// `Developer mode is currently disabled.`.
    Disabled,
    /// `Developer mode rights are currently in an unknown state.`.
    Unrecognized,
    /// Not observed: refused, not run, or an answer this classifier does not claim to understand.
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

/// Which developer-mode rule this account would match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DebuggerMember {
    /// In [`ADMIN_GROUP`]: `is-admin`, which needs no authentication.
    Admin,
    /// Not an admin, but in [`DEVELOPER_GROUP`].
    Developer,
    /// In neither.
    NotMember,
    /// Not determined.
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

/// Whether the running aterm is protected from a same-user debugger.
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

/// The debugger probe's progress or outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DebuggerProbeLabel {
    /// No current result: an asynchronous observer is waiting or refreshing.
    Pending,
    /// `-status` exited and said something this classifier recognises.
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
    /// Refused: `[privacy] enabled` or `[privacy] check` is off.
    RefusedDisabled,
    /// Refused: the running executable is not inside a `.app` bundle.
    RefusedOutOfBundle,
    /// Refused: the running executable could not be resolved.
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

    /// `true` for a deliberate refusal to consult the tool, including an unsupported platform.
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

/// One debugger probe: the rule's mode, the account's membership.
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
    /// No current observation.
    #[must_use]
    pub const fn pending() -> Self {
        Self::refused(DebuggerProbeLabel::Pending)
    }

    /// Nothing learned: both facts [`DebuggerMode::Unknown`] / [`DebuggerMember::Unknown`].
    #[must_use]
    pub const fn refused(label: DebuggerProbeLabel) -> Self {
        Self {
            mode: DebuggerMode::Unknown,
            member: DebuggerMember::Unknown,
            label,
        }
    }
}

/// The answer on a platform with no `DevToolsSecurity`.
#[cfg(any(test, not(target_os = "macos")))]
#[must_use]
pub const fn unsupported_debugger_probe() -> DebuggerProbe {
    DebuggerProbe::refused(DebuggerProbeLabel::UnsupportedPlatform)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const fn devtools_status_command() -> [&'static str; 2] {
    [DEVTOOLSSECURITY_PATH, "-status"]
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const fn devtools_enable_command() -> [&'static str; 2] {
    [DEVTOOLSSECURITY_PATH, "-enable"]
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const fn devtools_disable_command() -> [&'static str; 2] {
    [DEVTOOLSSECURITY_PATH, "-disable"]
}

/// Which way the owner's gesture changes the debugger rule ().
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DebuggerChange {
    /// *Allow debugger access…*.
    On,
    /// *Turn off debugger access…*.
    Off,
}

impl DebuggerChange {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    const fn command(self) -> [&'static str; 2] {
        match self {
            Self::On => devtools_enable_command(),
            Self::Off => devtools_disable_command(),
        }
    }

    /// The mode a fresh `-status` read must show for the change to run.
    #[must_use]
    pub const fn precondition(self) -> DebuggerMode {
        match self {
            Self::On => DebuggerMode::Disabled,
            Self::Off => DebuggerMode::Enabled,
        }
    }

    /// The mode the change sets, when it lands.
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

    /// Read a direction token back ([`Self::as_str`]).
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
    /// It outlasted the ceiling and was killed.
    TimedOut,
    /// A stream was over the cap or not UTF-8, or the exit could not be read.
    Unreadable,
}

const DEVTOOLS_ADVERBS: [&str; 4] = ["", "currently", "already", "now"];

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

/// Classify one `DevToolsSecurity -status` run, reading lines from both streams.
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

/// Fold the two directory answers (`None`: the directory could not answer) in rule order.
#[must_use]
pub const fn classify_membership(admin: Option<bool>, developer: Option<bool>) -> DebuggerMember {
    match (admin, developer) {
        (Some(true), _) => DebuggerMember::Admin,
        (Some(false), Some(true)) => DebuggerMember::Developer,
        (Some(false), Some(false)) => DebuggerMember::NotMember,
        (Some(false), None) | (None, _) => DebuggerMember::Unknown,
    }
}

/// The running user's developer-mode membership.
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

/// Classify the running image's code-signing status flags.
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

/// The debugger probe with the executable, the spawn and the membership query injected.
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

/// Probe the debugger rule for the running process.
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

/// The unified log's tool, absolute.
pub const LOG_TOOL: &str = "/usr/bin/log";

/// The directory whose readability decides whether this account can read the unified log at all.
pub const DIAGNOSTICS_DIR: &str = "/var/db/diagnostics";

/// Whether this account can read the unified log's backing store.
#[cfg(target_os = "macos")]
#[must_use]
pub fn log_store_readable() -> bool {
    std::fs::read_dir(DIAGNOSTICS_DIR).is_ok()
}

/// How far back [`probe_devtool`] reads tccd's answers, in seconds.
pub const DEVTOOL_WINDOW_S: u64 = 600;

/// Where a person adds aterm to the Developer Tools list, in words.
pub const DEVTOOLS_SETTINGS_WORDS: &str =
    "System Settings \u{25b8} Privacy & Security \u{25b8} Developer Tools";

/// The Developer Tools list through the modern Settings extension.
pub const DEVTOOLS_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_DevTools";

/// The same pane through its legacy pane id, which System Settings maps to the extension.
pub const DEVTOOLS_SETTINGS_URL_LEGACY: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_DevTools";

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const DEVTOOL_PREDICATE: &str = "process == \"tccd\" AND eventMessage BEGINSWITH \"Handling access \
                                 request to kTCCServiceDeveloperTool, from Sub:{\"";

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

#[cfg(target_os = "macos")]
const DEVTOOL_CEILING: std::time::Duration = std::time::Duration::from_secs(8);

#[cfg(target_os = "macos")]
const DEVTOOL_OUTPUT_MAX_BYTES: u64 = 8 * 1024 * 1024;

const TCCD_IMAGE_PATH: &str = "/System/Library/PrivateFrameworks/TCC.framework/Support/tccd";

const TCCD_ACCESS_FORMAT: &str =
    "Handling access request to %{public}@, from %{public}@, %{public}@";

const DEVTOOL_MESSAGE_PREFIX: &str =
    "Handling access request to kTCCServiceDeveloperTool, from Sub:{";

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

const TCCD_DB_ACTIONS: [&str; 3] = ["None", "Update", "Retry"];

/// What tccd's newest Developer Tools answer for aterm's identity said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DevToolState {
    /// `Auth Right: Allowed (<reason>)`.
    Allowed,
    /// `Auth Right: Denied (<reason>)`, for any reason in tccd's table.
    Denied,
    /// `Auth Right: Unknown (None)`.
    Unlisted,
    /// No answer could be read.
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

/// The Developer Tools probe's progress or outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DevToolProbeLabel {
    /// No current result: an asynchronous observer is waiting or refreshing.
    Pending,
    /// Tccd's log was read whole and held at least one answer for the subject.
    Observed,
    /// Tccd's log was read whole and held no answer for the subject in the window.
    NoRecentRequest,
    /// The log store could not be read.
    LogUnavailable,
    /// `log` ran on an account that can read the store, and exited non-zero or by a signal.
    LogFailed,
    /// The log answered in a shape this classifier does not recognise.
    FormatUnrecognized,
    /// `log` did not finish within its ceiling.
    TimedOut,
    /// `log` could not be started.
    SpawnFailed,
    /// Its output was over the cap or not UTF-8, or its exit could not be read.
    OutputUnreadable,
    /// Refused: `[privacy] enabled` or `[privacy] check` is off.
    RefusedDisabled,
    /// Refused: `[privacy] observer = false`, which stops aterm reading tccd's log.
    RefusedObserverOff,
    /// Refused: the running executable is not inside a `.app` bundle.
    RefusedOutOfBundle,
    /// Refused: the running executable could not be resolved.
    RefusedNoExe,
    /// Refused: there is no usable bundle identifier to match.
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

    /// `true` for a deliberate refusal to read the log, including an unsupported platform.
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

/// One Developer Tools probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DevToolProbe {
    /// The newest answer, classified.
    pub state: DevToolState,
    /// The observation's progress, refusal, or completed outcome.
    pub label: DevToolProbeLabel,
    /// Answers for the subject in the window.
    pub requests: Option<u32>,
    /// Of those, how many were not `Allowed`.
    pub not_allowed: Option<u32>,
    /// The newest answer's UTC time, in unix milliseconds.
    pub answered_at_ms: Option<i64>,
}

impl DevToolProbe {
    /// No current observation.
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

/// The answer on a platform with no such list.
#[cfg(any(test, not(target_os = "macos")))]
#[must_use]
pub const fn unsupported_devtool_probe() -> DevToolProbe {
    DevToolProbe::refused(DevToolProbeLabel::UnsupportedPlatform)
}

/// Whether `subject` may be matched against tccd's `Sub:{…}`.
#[must_use]
pub fn devtool_subject_is_usable(subject: &str) -> bool {
    bundle_id_is_usable(subject)
        && subject
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

fn devtool_answer(message: &str, subject: &str) -> Result<Option<DevToolState>, ()> {
    let rest = message.strip_prefix(DEVTOOL_MESSAGE_PREFIX).ok_or(())?;
    let (sub, after) = rest.split_once('}').ok_or(())?;
    if sub != subject {
        return Ok(None);
    }
    if !after.starts_with("Resp:{") {
        return Err(());
    }
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

const fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

const fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn digits(text: &str, width: usize) -> Option<i64> {
    (text.len() == width && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// `log`'s `ndjson` timestamp as unix milliseconds, UTC.
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

/// How old an answer stamped `answered_at_ms` is at `now_ms`, both unix milliseconds.
#[must_use]
pub const fn devtool_answer_age_ms(answered_at_ms: i64, now_ms: i64) -> u64 {
    let age = now_ms.saturating_sub(answered_at_ms);
    if age < 0 { 0 } else { age.unsigned_abs() }
}

/// Classify one `log show` run for `subject`.
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

fn devtool_trailer(fields: &[(String, json::Value)]) -> Option<u64> {
    if fields.len() != 2 || json::uint(fields, "finished")? != 1 {
        return None;
    }
    json::uint(fields, "count")
}

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

/// The Developer Tools probe with the executable.
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

/// Read tccd's Developer Tools answers for `subject`, the running bundle's identifier.
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

/// The system TCC database.
pub const SYSTEM_TCC_DB: &str = "/Library/Application Support/com.apple.TCC/TCC.db";

/// The reader, named absolutely.
pub const SQLITE_TOOL: &str = "/usr/bin/sqlite3";

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const DEVTOOL_DB_QUERY: &str = "SELECT hex(client), client_type, auth_value, last_modified \
                                FROM access WHERE service = 'kTCCServiceDeveloperTool' \
                                ORDER BY rowid;";

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const DEVTOOL_DB_OPTIONS: [&str; 11] = [
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
];

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn devtool_db_args(target: &str) -> Vec<&str> {
    DEVTOOL_DB_OPTIONS
        .iter()
        .copied()
        .chain([target, DEVTOOL_DB_QUERY])
        .collect()
}

#[cfg(target_os = "macos")]
const DEVTOOL_DB_CEILING: std::time::Duration = std::time::Duration::from_secs(5);

/// What the Developer Tools list holds for the subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DevToolListing {
    /// A row with `auth_value` 2.
    On,
    /// A row with `auth_value` 0.
    Off,
    /// The list was read whole and holds no row for the subject.
    Absent,
    /// Not read, or read in a shape this classifier does not recognise.
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

/// The list probe's progress or outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DevToolListLabel {
    /// No current result: an asynchronous reader is waiting or refreshing.
    Pending,
    /// The database was read whole.
    Read,
    /// `open` answered `EPERM`.
    NoFullDiskAccess,
    /// `open` failed with another errno.
    OpenFailed,
    /// `sqlite3` exited non-zero or by a signal.
    ToolFailed,
    /// `sqlite3` answered in a shape this classifier does not recognise.
    FormatUnrecognized,
    /// `sqlite3` did not finish within its ceiling.
    TimedOut,
    /// `sqlite3` could not be started.
    SpawnFailed,
    /// Its output was over the cap or not UTF-8, or its exit could not be read.
    OutputUnreadable,
    /// Refused: `[privacy] enabled` or `[privacy] check` is off.
    RefusedDisabled,
    /// Refused: `[privacy] observer = false`.
    RefusedObserverOff,
    /// Refused: the running executable is not inside a `.app` bundle.
    RefusedOutOfBundle,
    /// Refused: the running executable could not be resolved.
    RefusedNoExe,
    /// Refused: there is no usable bundle identifier to match.
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

    /// `true` for a deliberate refusal, including an unsupported platform.
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
    /// The row's `last_modified`, unix seconds.
    pub modified_at_s: Option<i64>,
    /// How many apps the list holds, the subject's row included.
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

/// The list's answer on a platform with no such list.
#[cfg(any(test, not(target_os = "macos")))]
#[must_use]
pub const fn unsupported_devtool_list() -> DevToolList {
    DevToolList::refused(DevToolListLabel::UnsupportedPlatform)
}

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

/// Classify one `sqlite3` run for `subject`.
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

#[cfg(any(target_os = "macos", test))]
const DEVTOOL_DB_READS: usize = 3;

#[cfg(any(target_os = "macos", test))]
const DEVTOOL_DB_PAUSE: std::time::Duration = std::time::Duration::from_millis(10);

/// The list probe with the executable, the database (production: [`SYSTEM_TCC_DB`]).
#[cfg(any(target_os = "macos", test))]
pub fn probe_devtool_list_with<O, S>(
    gate: ProbeGate,
    exe: Option<&Path>,
    subject: &str,
    db: &Path,
    open_db: O,
    spawn: S,
) -> DevToolList
where
    O: FnOnce(&Path) -> ProbeOutcome,
    S: FnMut(&str, &[&str]) -> ToolRun,
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
    match open_db(db) {
        ProbeOutcome::Ok => read_devtool_db(db, subject, spawn),
        ProbeOutcome::Errno(ERRNO_EPERM) => {
            DevToolList::refused(DevToolListLabel::NoFullDiskAccess)
        }
        ProbeOutcome::Errno(_) => DevToolList::refused(DevToolListLabel::OpenFailed),
    }
}

#[cfg(any(target_os = "macos", test))]
fn read_devtool_db<S>(db: &Path, subject: &str, mut spawn: S) -> DevToolList
where
    S: FnMut(&str, &[&str]) -> ToolRun,
{
    use aterm_uds::foreign_db::{Unread, read};
    for attempt in 0..DEVTOOL_DB_READS {
        if attempt > 0 {
            std::thread::sleep(DEVTOOL_DB_PAUSE);
        }
        match read(db, |target| {
            Some(spawn(SQLITE_TOOL, &devtool_db_args(target)))
        }) {
            Ok(run) => return classify_devtool_db(&run, subject),
            Err(Unread::Changed | Unread::Writing) => {}
            Err(Unread::Missing) => return DevToolList::refused(DevToolListLabel::OpenFailed),
            Err(Unread::Failed) => break,
        }
    }
    DevToolList::refused(DevToolListLabel::ToolFailed)
}

/// Read the Developer Tools list's row for `subject`, the running bundle's identifier.
#[must_use]
pub fn probe_devtool_list(gate: ProbeGate, subject: &str) -> DevToolList {
    #[cfg(target_os = "macos")]
    {
        let exe = fence_exe();
        probe_devtool_list_with(
            gate,
            exe.as_deref(),
            subject,
            Path::new(SYSTEM_TCC_DB),
            |db| open_probe_in_child(exe.as_deref(), db),
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

/// Whether the app is on the Developer Tools list, all evidence joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DevToolGrant {
    /// Listed and switched on, or tccd allowed it, with nothing newer saying otherwise.
    Granted,
    /// Not listed, switched off, or tccd's newer answer did not allow it.
    NotGranted,
    /// Neither the list nor tccd's log could be read.
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

/// Join the list's row and tccd's newest answer into one verdict.
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

/// `CFBundleIdentifier` of the bundle at `root`, read as `CFBundle` reads it, or `None`.
#[must_use]
pub fn bundle_id_of(root: &Path, protected: &[PathBuf]) -> Option<String> {
    let plist = root.join("Contents").join("Info.plist");
    if !census_may_read(root, protected) || !census_may_read(&plist, protected) {
        return None;
    }
    read_info_plist(&plist).and_then(|info| info.bundle_id)
}

mod json {
    /// One JSON value, as far as the probe reads it.
    #[derive(Debug)]
    pub(super) enum Value {
        Number(String),
        Str(String),
        Object(Vec<(String, Self)>),
        Other,
    }

    const MAX_DEPTH: usize = 16;

    /// The fields of a line that is exactly one JSON object with no repeated key, else `None`.
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

    /// The field `key` as a non-negative integer written without a fraction.
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

/// The most of each stream a change keeps.
pub const DEVTOOLS_CHANGE_HEAD_BYTES: usize = 4 * 1024;

#[cfg(all(any(target_os = "macos", all(test, unix)), not(target_arch = "wasm32")))]
const CHANGE_READER_GRACE_POLLS: u32 = 200;

/// The kernel's birth record of a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessBirth {
    /// Seconds since the epoch.
    pub seconds: u64,
    /// Microseconds within that second.
    pub microseconds: u64,
}

/// The birth record of the process at `pid` right now, or `None` when the kernel gives none.
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

/// The `DevToolsSecurity` change child, as the `privacy-debugger-change` marker records it (.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DevtoolsChild {
    /// The child's pid.
    pub pid: u32,
    /// Its birth record, `None` when the kernel gave none at spawn.
    pub birth: Option<ProcessBirth>,
}

impl DevtoolsChild {
    /// Whether the recorded child is still the process at its pid.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.birth.is_some() && process_birth(self.pid) == self.birth
    }
}

/// How one [`run_devtools_change`] ended.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DevtoolsChangeRun {
    /// The fresh `-status` read did not show the change's precondition.
    NotDue {
        /// The re-read that stopped it.
        observed: DebuggerProbe,
    },
    /// The child ran and exited.
    Exited {
        /// The exit code, or `None` for a signal.
        code: Option<i32>,
        /// The head of its stdout.
        stdout_head: String,
        /// The head of its stderr.
        stderr_head: String,
    },
    /// There is no program at the path.
    Absent,
    /// It could not be started.
    SpawnFailed,
    /// It was started, but its exit could not be read.
    WaitFailed,
    /// Not macOS: there is no `DevToolsSecurity` here, and nothing ran.
    Unsupported,
}

impl DevtoolsChangeRun {
    /// Whether a change child was started.
    #[must_use]
    pub const fn spawned(&self) -> bool {
        matches!(self, Self::Exited { .. } | Self::WaitFailed)
    }
}

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

/// `devtools_change_with` for aterm-gui's Tier-1 bind.
#[cfg(debug_assertions)]
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

#[cfg(all(any(target_os = "macos", all(test, unix)), not(target_arch = "wasm32")))]
fn run_to_exit_keeping_heads<F: FnOnce(u32)>(argv: &[&str], on_spawn: F) -> DevtoolsChangeRun {
    use aterm_uds::spawnfd::Stdio;
    fn keep_head(
        pipe: impl std::io::Read + Send + 'static,
    ) -> Option<std::thread::JoinHandle<Vec<u8>>> {
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
        #[cfg(test)]
        tests::reader_grace_poll(&mut polls, &|| {
            out.as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
                && err
                    .as_ref()
                    .is_none_or(std::thread::JoinHandle::is_finished)
        });
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

/// Change the debugger rule the owner's way.
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

/// Whether a session's file access is attributed to the live aterm process or to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Attribution {
    /// Spawned by the process that is running now.
    Live,
    /// Adopted across an in-place apply.
    Adopted,
    /// No adoption record.
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
    /// Full Disk Access is held, the session is live, and the service was measured to be covered.
    Covered,
    /// Aterm itself observed an `EPERM(1)` for this session.
    Denied,
    /// Everything else.
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

/// Which blocking measurements from (and the debugger design's ) have been made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one independent spike's verdict, flipped in its own commit; \
              they are not the states of one machine"
)]
pub struct SpikeEvidence {
    /// S4 — how much a held Full Disk Access grant actually suppresses.
    pub fda_coverage_measured: bool,
    /// S2: what an adopted-across-a-handoff session's attribution and access.
    pub handoff_attribution_measured: bool,
    /// S1 — how far a grant reaches.
    pub fda_scope: FdaScope,
    /// Debugger D2 — what `DevToolsSecurity -enable` does from aterm's gesture.
    pub devtools_enable_measured: bool,
    /// Debugger D5 — whether developer mode silences lldb's attach.
    pub devtools_silences_lldb: bool,
    /// Debugger D6 — the shape of a cancelled taskport authorization in authd's log.
    pub taskport_cancel_shape_measured: bool,
    /// Debugger D12 — whether `sample`, `leaks` and their kin stay silent with developer mode.
    pub perf_tools_never_prompt: bool,
    /// Debugger D13: tccd's Developer Tools answer for a listed identity, switched.
    pub devtool_answer_shapes_measured: bool,
    /// Debugger D14: whether being on the Developer Tools list skips macOS's first-run check.
    pub devtool_skips_first_run_check: bool,
}

impl SpikeEvidence {
    /// What has been measured, and what every production caller passes.
    pub const MEASURED: Self = Self {
        devtools_enable_measured: true,
        devtools_silences_lldb: true,
        taskport_cancel_shape_measured: true,
        perf_tools_never_prompt: true,
        ..Self::UNMEASURED
    };

    /// Nothing measured: the zero state, and the hedged copy's baseline.
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

/// Everything the posture join reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PostureInputs {
    /// Aterm's own adoption record for this session.
    pub adoption: Attribution,
    /// The instance's Full Disk Access state.
    pub fda: FdaState,
    /// The responsibility SPI's corroboration.
    pub responsible: Responsible,
    /// `true` only when aterm itself took an `EPERM(1)` on a path under a protected root.
    pub observed_eperm: bool,
    /// Which spikes have landed.
    pub evidence: SpikeEvidence,
}

/// The joined posture for one session.
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
    /// How far the grant reaches.
    pub fda_scope: FdaScope,
    /// `true` when a macOS consent dialog remains possible for this session.
    pub prompt_possible: bool,
}

impl ConsentPosture {
    /// Join the observations.
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

#[cfg(target_os = "macos")]
mod imp {
    use super::{
        ProbeOutcome, ResponsibleApp, ResponsibleError, SelfImage, display_name, read_plist_text,
        readable_info_plist, responsible_answer,
    };
    use std::ffi::{CString, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    unsafe extern "C" {
        fn proc_pidpath(
            pid: libc::c_int,
            buffer: *mut libc::c_void,
            buffersize: u32,
        ) -> libc::c_int;
    }

    const PROC_PIDPATHINFO_MAXSIZE: usize = 4 * 1024;

    unsafe extern "C" {
        fn gethostuuid(id: *mut u8, wait: *const libc::timespec) -> libc::c_int;
    }

    /// This Mac's hardware UUID, upper-case and hyphenated.
    pub(super) fn host_uuid() -> Option<String> {
        let mut id = [0u8; 16];
        let wait = libc::timespec {
            tv_sec: 1,
            tv_nsec: 0,
        };
        // SAFETY: `id` is the 16 bytes of a `uuid_t` and outlives the call; `wait` is a
        // valid `timespec` read during the call only.
        let rc = unsafe { gethostuuid(id.as_mut_ptr(), &raw const wait) };
        (rc == 0).then(|| super::format_uuid(&id))
    }

    unsafe extern "C" {
        fn csops(
            pid: libc::pid_t,
            ops: u32,
            useraddr: *mut libc::c_void,
            usersize: libc::size_t,
        ) -> libc::c_int;
        fn mbr_uid_to_uuid(uid: libc::uid_t, uu: *mut u8) -> libc::c_int;
        fn mbr_gid_to_uuid(gid: libc::gid_t, uu: *mut u8) -> libc::c_int;
        fn mbr_check_membership(
            user: *const u8,
            group: *const u8,
            ismember: *mut libc::c_int,
        ) -> libc::c_int;
        fn getgrnam_r(
            name: *const libc::c_char,
            grp: *mut Group,
            buf: *mut libc::c_char,
            buflen: libc::size_t,
            result: *mut *mut Group,
        ) -> libc::c_int;
    }

    #[repr(C)]
    #[allow(clippy::struct_field_names, reason = "`<grp.h>`'s own field names")]
    struct Group {
        gr_name: *mut libc::c_char,
        gr_passwd: *mut libc::c_char,
        gr_gid: libc::gid_t,
        gr_mem: *mut *mut libc::c_char,
    }

    type Uuid = [u8; 16];

    /// The running image's code-signing status flags, or `None` when `csops` refuses.
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

    /// The birth record of the process at `pid`, from `proc_pidinfo(PROC_PIDTBSDINFO)`.
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
        if u32::try_from(pid).ok()? != info.pbi_pid {
            return None;
        }
        Some(super::ProcessBirth {
            seconds: info.pbi_start_tvsec,
            microseconds: info.pbi_start_tvusec,
        })
    }

    /// Whether the running user is a member of the directory group `name`.
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

    fn group_gid(name: &str) -> Option<libc::gid_t> {
        let name = CString::new(name).ok()?;
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

    type ResponsibilityGetPid = unsafe extern "C" fn(libc::c_int) -> libc::c_int;

    const RESPONSIBILITY_SYMBOL: &[u8] = b"responsibility_get_pid_responsible_for_pid\0";

    type CfRef = *const libc::c_void;

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
                drop(fd);
                ProbeOutcome::Ok
            }
            Err(errno) => ProbeOutcome::Errno(errno),
        }
    }

    /// The probe's `open`, close-on-exec from creation (`O_CLOEXEC` is atomic in `open(2)`).
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

    fn errno() -> i32 {
        // SAFETY: `__error()` returns a valid pointer to this thread's errno.
        unsafe { *libc::__error() }
    }

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

    /// The responsibility SPI resolved, as a question about one pid at a time.
    pub(super) fn responsibility_spi() -> Option<impl FnMut(i32) -> Result<i32, ResponsibleError>> {
        let func = responsibility_symbol()?;
        Some(move |pid: i32| {
            // SAFETY: `__error()` returns a valid pointer to this thread's errno;
            // cleared so a stale value cannot be read back as this call's.
            unsafe {
                *libc::__error() = 0;
            }
            // SAFETY: `func` has the signature dyld resolved it to, and the argument is a
            // plain `pid_t`.
            let ret = unsafe { func(pid) };
            responsible_answer(ret, errno())
        })
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

    /// [`SelfImage`] from `proc_pidpath` on our own pid.
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

    fn pid_path(pid: i32) -> Result<PathBuf, i32> {
        let mut buf = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
        // SAFETY: `__error()` returns a valid pointer to this thread's errno.
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

    /// Every bundle `LaunchServices` has registered for `bundle_id`.
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

    /// `bytes` (a bundle's `Info.plist`) parsed by `CFBundle`'s reader.
    pub(super) fn parse_info_plist(bytes: &[u8]) -> Option<super::InfoPlist> {
        const IMMUTABLE: usize = 0;
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

    const KEY_MAX_UNITS: isize = 128;
    const VALUE_MAX_UNITS: isize = 1024;
}

/// Explicit mounted-volume acceptance for the census boundary.
///
/// # Errors
/// Returns the setup failure when a disk image cannot be created or attached.
///
/// Returns the setup failure when a disk image cannot be created or attached.
///
/// # Panics
/// If detaching fails or the mounted/detached policy judgments are incorrect.
///
/// Panics if detaching fails or the mounted/detached policy judgments are incorrect.
#[cfg(all(debug_assertions, target_os = "macos"))]
pub fn a_real_volume_mounted_in_the_bundle_is_not_crossed(dir: &Path) -> std::io::Result<()> {
    struct Detach {
        point: PathBuf,
        attached: bool,
    }
    impl Detach {
        fn now(&mut self) -> std::io::Result<std::process::Output> {
            let out = aterm_uds::spawnfd::command("/usr/bin/hdiutil")
                .args(["detach", "-force"])
                .arg(&self.point)
                .output()?;
            if out.status.success() {
                self.attached = false;
            }
            Ok(out)
        }
    }
    impl Drop for Detach {
        fn drop(&mut self) {
            if self.attached {
                let _ = self.now();
            }
        }
    }
    let bundle = dir.join("m.app");
    let mount = bundle.join("Contents/Vol");
    std::fs::create_dir_all(&mount)?;
    std::fs::write(
        bundle.join("Contents/Info.plist"),
        "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
    )?;
    let image = dir.join("vol.dmg");
    let hdiutil = |args: &[&std::ffi::OsStr]| -> std::io::Result<()> {
        let out = aterm_uds::spawnfd::command("/usr/bin/hdiutil")
            .args(args)
            .output()?;
        if out.status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!("hdiutil {args:?}: {out:?}")))
        }
    };
    hdiutil(&[
        "create".as_ref(),
        "-size".as_ref(),
        "1m".as_ref(),
        "-fs".as_ref(),
        "HFS+".as_ref(),
        image.as_os_str(),
    ])?;
    let mut attached = Detach {
        point: mount.clone(),
        attached: true,
    };
    hdiutil(&[
        "attach".as_ref(),
        "-nobrowse".as_ref(),
        "-readonly".as_ref(),
        "-mountpoint".as_ref(),
        mount.as_os_str(),
        image.as_os_str(),
    ])?;
    let refused = bundle_reaches_protected(&bundle, &[]);
    let detached = attached
        .now()
        .expect("detach the successfully attached volume");
    assert!(detached.status.success(), "hdiutil detach: {detached:?}");
    assert!(refused, "a volume mounted inside the bundle");
    assert!(
        !bundle_reaches_protected(&bundle, &[]),
        "the control: the same bundle, detached"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    type ReadersAct = Box<dyn FnOnce(&mut std::time::Instant)>;

    #[cfg(target_os = "macos")]
    thread_local! {
        static READERS_PENDING: std::cell::RefCell<std::collections::VecDeque<ReadersAct>> =
            const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
    }

    #[cfg(target_os = "macos")]
    pub(super) fn readers_pending(deadline: &mut std::time::Instant) {
        if let Some(act) = READERS_PENDING.with(|acts| acts.borrow_mut().pop_front()) {
            act(deadline);
        }
    }

    #[cfg(unix)]
    type GracePollAct = (u32, Box<dyn FnOnce(&mut u32, &dyn Fn() -> bool)>);

    #[cfg(unix)]
    thread_local! {
        static GRACE_POLL: std::cell::RefCell<Option<GracePollAct>> =
            const { std::cell::RefCell::new(None) };
    }

    #[cfg(unix)]
    pub(super) fn reader_grace_poll(poll: &mut u32, readers_done: &dyn Fn() -> bool) {
        let act = GRACE_POLL.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.as_ref().is_some_and(|(at, _)| *at == *poll) {
                slot.take().map(|(_, act)| act)
            } else {
                None
            }
        });
        if let Some(act) = act {
            act(poll, readers_done);
        }
    }

    #[cfg(target_os = "macos")]
    const GATED_GRANDCHILD: &str =
        "exec 3<>\"$1\"; (read line <&3; printf 'late%s' \"$line\") & printf early";

    #[cfg(target_os = "macos")]
    const HANG_DETECTOR_RELEASE: &str = "+released-by-the-hang-detector";

    #[cfg(target_os = "macos")]
    fn fifo_gate(dir: &Path) -> PathBuf {
        let gate = dir.join("gate");
        let fifo = std::ffi::CString::new(gate.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `fifo` is a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        gate
    }

    #[cfg(target_os = "macos")]
    fn release_gate(gate: &Path) -> std::io::Result<()> {
        release_gate_saying(gate, "")
    }

    #[cfg(target_os = "macos")]
    fn release_gate_saying(gate: &Path, said: &str) -> std::io::Result<()> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(gate)
            .and_then(|mut gate| gate.write_all(format!("{said}\n").as_bytes()))
    }

    #[cfg(target_os = "macos")]
    fn gate_hang_detector(gate: &Path) -> std::sync::mpsc::Sender<()> {
        let (stand_down, detector) = std::sync::mpsc::channel::<()>();
        let gate = gate.to_path_buf();
        std::thread::spawn(move || {
            if detector.recv_timeout(HANG_DETECTOR)
                == Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            {
                let _ = release_gate_saying(&gate, HANG_DETECTOR_RELEASE);
            }
        });
        stand_down
    }

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn the_bundle_root_is_lexical() {
        let (app, dev, inner) = (
            "/A/x.app",
            "/A/x (dev).app",
            "/A/O.app/Contents/MacOS/I.app",
        );
        let cases: &[(&str, Option<&str>, Option<&str>)] = &[
            ("/A/x.app/Contents/MacOS/x", Some(app), Some(app)),
            ("/A/x (dev).app/Contents/MacOS/x", Some(dev), Some(dev)),
            ("/A/x.app/Contents/MacOS/helpers/x", Some(app), Some(app)),
            (
                "/A/O.app/Contents/MacOS/I.app/Contents/MacOS/x",
                Some(inner),
                Some(inner),
            ),
            (
                "sub/x.app/Contents/MacOS/x",
                Some("sub/x.app"),
                Some("sub/x.app"),
            ),
            (
                "/A/x.app.rollback/Contents/MacOS/x",
                None,
                Some("/A/x.app.rollback"),
            ),
            ("/A/.app/Contents/MacOS/x", None, Some("/A/.app")),
            ("rel/Contents/MacOS/x", None, Some("rel")),
            ("/Contents/MacOS/x", None, None),
            ("/A/target/debug/deps/aterm_containment-abc123", None, None),
            ("/usr/local/bin/aterm", None, None),
            (app, None, None),
            ("/A/x.app/Contents/MacOS", None, None),
            ("/A/x.app/Contents/Resources/x", None, None),
            ("/A/x.app/Other/MacOS/x", None, None),
            (
                "/A/X.APP/Contents/MacOS/x",
                Some("/A/X.APP"),
                Some("/A/X.APP"),
            ),
        ];
        for &(exe, bundle, layout) in cases {
            let exe_path = Path::new(exe);
            assert_eq!(app_bundle_root(exe_path), bundle.map(p), "{exe}");
            assert_eq!(path_is_in_app_bundle(exe_path), bundle.is_some(), "{exe}");
            assert_eq!(bundle_layout_root(exe_path), layout.map(p), "{exe}");
        }
    }

    const DEV_ID: &str = "designated => identifier \"x\" and anchor apple generic and \
                          certificate leaf[subject.OU] = A66A9P66Z7";

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
        assert_eq!(team_identifier("TeamIdentifier=\n"), None, "an empty id");
        for (report, signing) in [
            (
                "/x: code object is not signed at all\nIdentifier=x\n",
                "unknown",
            ),
            ("not signed at all\n", "unknown"),
            (
                "Executable=/A/Signature=adhoc/x\nTeamIdentifier=T\n",
                "developer-id",
            ),
        ] {
            assert_eq!(classify_signing(report), signing, "{report:?}");
        }
    }

    #[test]
    fn the_plist_reader_is_exact_and_xml_only() {
        let xml = "<plist><dict>\
                   <key>CFBundleIdentifier</key><string>com.aterm.aterm.dev</string>\
                   <key>CFBundleNameSuffix</key><string>nope</string>\
                   </dict></plist>";
        assert_eq!(
            plist_string(xml, "CFBundleIdentifier"),
            Some("com.aterm.aterm.dev")
        );
        assert_eq!(plist_string(xml, "CFBundleName"), None);
        assert_eq!(
            plist_string("<key>A</key><key>B</key><string>v</string>", "A"),
            None
        );
        assert_eq!(plist_string("bplist00\u{0}\u{1}", "A"), None);
        assert_eq!(
            plist_string("<key> A </key>\n\t<string> v </string>", "A"),
            Some("v"),
            "whitespace around the key, before its string and inside it"
        );
        #[cfg(unix)]
        assert_eq!(read_plist_bytes(Path::new("/dev/null")), None, "a device");
        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let sized = |name: &str, len: u64| {
            let path = tmp.path().join(name);
            std::fs::File::create(&path).unwrap().set_len(len).unwrap();
            path
        };
        assert!(read_plist_bytes(&sized("at", INFO_PLIST_MAX_BYTES)).is_some());
        assert_eq!(
            read_plist_bytes(&sized("over", INFO_PLIST_MAX_BYTES + 1)),
            None
        );
    }

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
        assert!(mk("").conflicts_with(&mk("")));
        assert!(!mk(DEV_ID).conflicts_with(&mk(&format!(" {DEV_ID}\n"))));
        assert!(mk(" ").conflicts_with(&mk(" ")));
        let fresh = |dr, signing, dr_text: &str| BundleIdentity {
            bundle_id: Some("x".to_string()),
            dr_text: dr_text.to_string(),
            dr,
            signing,
            team: None,
        };
        let unsigned = Claimant {
            dr: DrClass::Unsigned,
            signing: "unsigned",
            ..mk("")
        };
        let unread = Claimant {
            dr: DrClass::Unknown,
            signing: "unknown",
            ..mk("")
        };
        for (read, copy, same) in [
            (fresh(DrClass::Unsigned, "unsigned", ""), &unsigned, true),
            (fresh(DrClass::Unknown, "unknown", ""), &unread, false),
            (fresh(DrClass::Cdhash, "unsigned", ""), &unsigned, false),
            (fresh(DrClass::Unsigned, "adhoc", ""), &unsigned, false),
        ] {
            assert_eq!(same_identity(&read, copy), same, "{read:?}");
        }
    }

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
                    apps.join("x-plain"),
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
            Some("x-plain") => ours("designated => cdhash H\"cc\""),
            _ => Candidate::NotOurs,
        };
        let trashed = PathBuf::from("/Users//u/.Trash/x.app");
        let volume_trash = PathBuf::from("/Volumes/V/.Trashes/501/x.app");
        let registered = [
            far.clone(),
            apps.join("x.app"),
            trashed.clone(),
            volume_trash.clone(),
        ];
        let c = classify_claimants(
            Some(&running),
            &roots,
            Some(&registered),
            list,
            as_spelled,
            read,
        );
        for trash in [&trashed, &volume_trash] {
            assert!(
                !c.found.iter().any(|x| x.path == *trash),
                "a copy in the Trash is not a claimant, though LaunchServices still lists it"
            );
        }

        assert_eq!(
            c.enumeration,
            Enumeration::Complete,
            "a missing /Applications is empty"
        );
        assert_eq!(c.found.len(), 5, "{:?}", c.found);
        assert!(c.found[0].running && c.found[0].path == running);
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
        let nothing_listed = classify_claimants(
            Some(&running),
            &[(apps.clone(), true)],
            None,
            |_| Listing::Unreadable,
            as_spelled,
            only,
        );
        assert_eq!(
            nothing_listed.enumeration,
            Enumeration::Partial,
            "nothing listed, but the running copy was read"
        );
    }

    #[test]
    fn a_bundle_is_reported_once_under_its_own_name() {
        let (link, own, resolved) = (p("/Links/x.app"), p("/Apps/x.app"), p("/real/x.app"));
        let firmlink = p("/Data/Apps/x.app");
        let locate = |spelled: &Path| {
            let via_link = spelled == Path::new("/Links/x.app");
            let named =
                spelled == Path::new("/Apps/x.app") || spelled == Path::new("/Data/Apps/x.app");
            (via_link || named).then(|| Located {
                file: (1, 1),
                path: if via_link {
                    p("/real/x.app")
                } else {
                    spelled.to_path_buf()
                },
                link: via_link,
            })
        };
        let c = classify_claimants(
            Some(&link),
            &[(p("/Links"), true), (p("/Apps"), false)],
            Some(&[link.clone(), firmlink]),
            |dir: &Path| Listing::Entries(vec![dir.join("x.app")]),
            locate,
            |_: &Path| ours(DEV_ID),
        );
        assert_eq!(c.found.len(), 1, "{:?}", c.found);
        let copy = &c.found[0];
        assert_eq!(copy.path, own, "not {resolved:?}");
        assert_eq!(copy.links, [link]);
        assert!(copy.running);
    }

    #[test]
    fn only_an_ad_hoc_or_unsigned_copy_is_retirable() {
        let running = PathBuf::from("/A/x.app");
        let [pinned, adhoc, unsigned, unread] = [
            "x-pinned.app",
            "x-adhoc.app",
            "x-unsigned.app",
            "x-unread.app",
        ]
        .map(|name| PathBuf::from("/A").join(name));
        let entries = vec![
            running.clone(),
            pinned.clone(),
            adhoc.clone(),
            unsigned.clone(),
            unread.clone(),
        ];
        let c = classify_claimants(
            Some(&running),
            &[(PathBuf::from("/A"), true)],
            Some(&[]),
            |_| Listing::Entries(entries.clone()),
            as_spelled,
            |root: &Path| {
                let report = if root == running.as_path() {
                    return ours(DEV_ID);
                } else if root == pinned.as_path() {
                    return Candidate::Ours(BundleIdentity {
                        bundle_id: Some("x".to_string()),
                        dr_text: "cdhash H\"aa\"".to_string(),
                        dr: DrClass::Cdhash,
                        signing: "developer-id",
                        team: Some("T".to_string()),
                    });
                } else if root == adhoc.as_path() {
                    return ours("designated => cdhash H\"bb\"");
                } else if root == unsigned.as_path() {
                    Some("/A/x-unsigned.app: code object is not signed at all\n")
                } else {
                    None
                };
                Candidate::Ours(identity_from_report(report, Some("x".to_string())))
            },
        );
        assert_eq!(c.conflicting().len(), 4);
        let retirable: Vec<&Path> = c.retirable().iter().map(|c| c.path.as_path()).collect();
        assert_eq!(retirable, vec![adhoc.as_path(), unsigned.as_path()]);
        use TrashOffer::{Linked, NoTool, Offered, Requirement, RunningUnstable, Signature};
        let offers = |trash_tool| -> Vec<TrashOffer> {
            c.listed(trash_tool)
                .into_iter()
                .map(|(_, offer)| offer)
                .collect()
        };
        assert_eq!(offers(true), [Offered, Signature, Signature, Offered]);
        assert_eq!(offers(false), [NoTool, Signature, Signature, NoTool]);

        let none: &[&str] = &[];
        for (signing, dr, links, stable, want) in [
            ("adhoc", DrClass::Cdhash, none, true, Offered),
            ("unsigned", DrClass::Unsigned, none, true, Offered),
            ("adhoc", DrClass::Identity, none, true, Requirement),
            ("adhoc", DrClass::Unknown, none, true, Requirement),
            ("developer-id", DrClass::Cdhash, none, true, Signature),
            ("developer-id", DrClass::Unsigned, none, true, Signature),
            ("unsigned", DrClass::Unknown, none, true, Signature),
            ("unknown", DrClass::Cdhash, none, true, Signature),
            (
                "adhoc",
                DrClass::Cdhash,
                &["/Applications/x-dev.app"],
                true,
                Linked,
            ),
            ("adhoc", DrClass::Cdhash, none, false, RunningUnstable),
        ] {
            let copy = Claimant {
                path: PathBuf::from("/A/x-copy.app"),
                dr,
                dr_text: "r".to_string(),
                signing,
                team: None,
                running: false,
                links: links.iter().map(PathBuf::from).collect(),
            };
            assert_eq!(
                TrashOffer::of(&copy, stable),
                want,
                "{signing} {dr:?} {links:?}"
            );
        }

        let claimant = |name: &str, dr_text: &str, running: bool| Claimant {
            path: PathBuf::from("/A").join(name),
            dr: classify_dr(dr_text),
            dr_text: dr_text.to_string(),
            signing: if dr_text.contains("cdhash") {
                "adhoc"
            } else {
                "developer-id"
            },
            team: None,
            running,
            links: Vec::new(),
        };
        let census = |found| Claimants {
            found,
            enumeration: Enumeration::Complete,
        };
        let paths = |copies: Vec<&Claimant>| -> Vec<PathBuf> {
            copies.into_iter().map(|c| c.path.clone()).collect()
        };
        let (aa, bb) = (
            "designated => cdhash H\"aa\"",
            "designated => cdhash H\"bb\"",
        );
        let unread_running = census(vec![
            claimant("x.app", "", true),
            claimant("y.app", DEV_ID, false),
        ]);
        assert_eq!(paths(unread_running.conflicting()), [p("/A/y.app")]);
        let twin = census(vec![
            claimant("x.app", DEV_ID, true),
            claimant("y.app", DEV_ID, false),
        ]);
        assert!(twin.conflicting().is_empty());
        let headless = census(vec![
            claimant("x.app", DEV_ID, false),
            claimant("y.app", aa, false),
        ]);
        assert!(headless.conflicting().is_empty());
        let unstable = census(vec![
            claimant("x.app", aa, true),
            claimant("y.app", DEV_ID, false),
            claimant("z.app", bb, false),
        ]);
        assert!(
            unstable.retirable().is_empty(),
            "a stable copy that is not running does not steady the running one"
        );
    }

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

        let link = apps.join("x.app");
        std::os::unix::fs::symlink(&hidden, &link).unwrap();
        assert!(is_under_protected_root(&hidden, &protected));
        assert!(
            !is_under_protected_root(&link, &protected),
            "not as spelled"
        );
        assert!(!census_may_read(&link, &protected), "but once resolved");
        assert_eq!(read_candidate(&link, "x", &protected), Candidate::Hole);

        let shell = apps.join("y.app");
        std::fs::create_dir_all(&shell).unwrap();
        std::os::unix::fs::symlink(hidden.join("Contents"), shell.join("Contents")).unwrap();
        assert!(
            census_may_read(&shell, &protected),
            "the bundle itself is outside"
        );
        assert_eq!(read_candidate(&shell, "x", &protected), Candidate::Hole);
        assert_eq!(bundle_identity(&shell, &protected), None);

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
        assert_eq!(bundle_id_of(&other, &protected), None);
        let bare = apps.join("b.app");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(documents.join("no-plist")).unwrap();
        std::os::unix::fs::symlink(documents.join("no-plist"), bare.join("Contents")).unwrap();
        assert!(bundle_reaches_protected(&bare, &protected));
        assert_eq!(bundle_identity(&bare, &protected), None);
        let into = apps.join("n.app");
        std::os::unix::fs::symlink(documents.join("no-plist"), &into).unwrap();
        assert_eq!(read_candidate(&into, "x", &protected), Candidate::Hole);
        let q = documents.join("q.app");
        std::fs::create_dir_all(&q).unwrap();
        std::os::unix::fs::symlink(&away, q.join("Contents")).unwrap();
        let q_link = apps.join("q.app");
        std::os::unix::fs::symlink(&q, &q_link).unwrap();
        assert_eq!(bundle_id_of(&q_link, &protected), None);
        assert!(bundle_reaches_protected(&hidden, &protected));
        let cyc = apps.join("c.app");
        std::fs::create_dir_all(cyc.join("Contents/MacOS")).unwrap();
        std::fs::write(cyc.join("Contents/MacOS/aterm"), b"").unwrap();
        std::os::unix::fs::symlink("aterm", cyc.join("Contents/MacOS/atpkg")).unwrap();
        std::os::unix::fs::symlink(cyc.join("Contents"), cyc.join("Contents/loop")).unwrap();
        assert!(!bundle_reaches_protected(&cyc, &protected));
        assert!(
            bundle_identity(&cyc, &protected).is_some(),
            "the control: vouched for, so read"
        );
        let alias = dir.join("apps-alias");
        std::os::unix::fs::symlink(&apps, &alias).unwrap();
        let spelled = alias.join("c.app");
        assert_eq!(
            bundle_identity(&spelled, &[spelled.join("Contents/Info.plist")]),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

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

        let p = apps.join("p.app");
        lay(&p);
        std::fs::create_dir_all(p.join("Contents/Private")).unwrap();
        std::fs::write(p.join("Contents/Private/secret.txt"), b"").unwrap();
        assert!(bundle_reaches_protected(&p, &[p.join("Contents/Private")]));
        assert!(
            !bundle_reaches_protected(&p, &[]),
            "the control: nothing protected inside it"
        );
        assert!(bundle_reaches_protected(&p, &[p.join("Contents/Absent")]));
        let alias = dir.join("alias");
        std::os::unix::fs::symlink(&apps, &alias).unwrap();
        assert!(bundle_reaches_protected(
            &p,
            &[alias.join("p.app/Contents/Private")]
        ));

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
        let forged = apps.join("c\nSignature=adhoc\n.app");
        lay(&forged);
        assert!(bundle_reaches_protected(&forged, &[]));
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
        let carriage = apps.join("r\r.app");
        lay(&carriage);
        assert!(bundle_reaches_protected(&carriage, &[]));
        let next_line = apps.join(format!("e{}.app", char::from(0x85_u8)));
        lay(&next_line);
        assert!(
            bundle_reaches_protected(&next_line, &[]),
            "a control character outside ASCII"
        );
        let plain = dir.join("plain.app");
        lay(&plain);
        let crooked_link = apps.join("n\n.app");
        std::os::unix::fs::symlink(&plain, &crooked_link).unwrap();
        assert!(bundle_reaches_protected(&crooked_link, &[]));
        let plain_link = apps.join("plain-link.app");
        std::os::unix::fs::symlink(&forged, &plain_link).unwrap();
        assert!(bundle_reaches_protected(&plain_link, &[]));
        assert!(bundle_reaches_protected(&apps.join("gone.app"), &[]));
        let hollow = apps.join("hollow.app");
        std::fs::create_dir_all(&hollow).unwrap();
        assert!(bundle_reaches_protected(&hollow, &[]));
        #[cfg(target_os = "macos")]
        {
            let folder_plist = apps.join("fp.app");
            std::fs::create_dir_all(folder_plist.join("Contents/Info.plist")).unwrap();
            assert!(bundle_reaches_protected(&folder_plist, &[]));
        }
        if std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&apps).unwrap()) != 0 {
            use std::os::unix::fs::PermissionsExt as _;
            let shut = apps.join("shut.app");
            std::fs::create_dir_all(shut.join("Contents")).unwrap();
            std::fs::write(shut.join("Contents/Info.plist"), b"").unwrap();
            let mode = |bits| {
                let mode = std::fs::Permissions::from_mode(bits);
                std::fs::set_permissions(shut.join("Contents"), mode).unwrap();
            };
            mode(0o444);
            let refused = bundle_reaches_protected(&shut, &[]);
            mode(0o755);
            assert!(
                refused,
                "a listable Contents whose plist cannot be looked at"
            );
            for (name, shut, bits) in [
                ("sealed.app", ".", 0o100),
                ("closed.app", "Contents/Resources", 0o000),
            ] {
                let app = apps.join(name);
                lay(&app);
                std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
                let set = |bits| {
                    let mode = std::fs::Permissions::from_mode(bits);
                    std::fs::set_permissions(app.join(shut), mode).unwrap();
                };
                set(bits);
                let refused = bundle_reaches_protected(&app, &[]);
                set(0o755);
                assert!(refused, "{name}");
                assert!(!bundle_reaches_protected(&app, &[]), "the control: {name}");
            }
        }
        let nested = apps.join("nested.app");
        lay(&nested);
        std::fs::create_dir_all(nested.join("Contents/Resources")).unwrap();
        std::fs::write(nested.join("Contents/Resources/Info-macos.plist"), b"").unwrap();
        std::fs::write(nested.join("Contents/version.plist"), b"").unwrap();
        std::fs::write(nested.join("Contents/Info.txt"), b"").unwrap();
        assert!(!bundle_reaches_protected(&nested, &[]));
        let deep = apps.join("deep.app");
        lay(&deep);
        let sixteen = (0..16).fold(deep.join("Contents"), |at, _| at.join("d"));
        std::fs::create_dir_all(&sixteen).unwrap();
        assert!(!bundle_reaches_protected(&deep, &[]));
        std::fs::create_dir_all(sixteen.join("d")).unwrap();
        assert!(bundle_reaches_protected(&deep, &[]));
        let wide = apps.join("wide.app");
        std::fs::create_dir_all(wide.join("Contents")).unwrap();
        let full = apps.join("full.app");
        std::fs::create_dir_all(full.join("Contents")).unwrap();
        for name in ["a", "b"] {
            std::fs::write(wide.join(name), b"").unwrap();
            std::fs::write(full.join("Contents").join(name), b"").unwrap();
        }
        assert!(!bundle_reaches_protected_on(&wide, &[], device_of, 3));
        assert!(!bundle_reaches_protected_on(&full, &[], device_of, 3));
        std::fs::write(wide.join("c"), b"").unwrap();
        std::fs::write(full.join("Contents/c"), b"").unwrap();
        assert!(bundle_reaches_protected_on(&wide, &[], device_of, 3));
        assert!(bundle_reaches_protected_on(&full, &[], device_of, 3));
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        assert!(refused("e18.app", &dict(&exe(&"a".repeat(2000)))));
        assert!(!refused(
            "e19.app",
            &dict(&format!("<key>{}Executable</key>{abs}", "x".repeat(200)))
        ));
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
        assert!(!refused("e10.app", &dict(&exe("x"))), "a plain name");
        assert!(
            !refused(
                "k.app",
                &dict(&format!("<key>NSHumanReadableCopyright</key>{abs}"))
            ),
            "a key naming no executable may hold a path"
        );
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
        let unusable = dir.join("v3.app");
        std::fs::create_dir_all(unusable.join("Contents/MacOS")).unwrap();
        std::fs::write(
            unusable.join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>x</string>\
             <key>CFBundleIdentifier-macos</key><integer>1</integer></dict></plist>",
        )
        .unwrap();
        assert_eq!(read_candidate(&unusable, "x", &[]), Candidate::NotOurs);
        let json = dir.join("j.app");
        std::fs::create_dir_all(json.join("Contents/MacOS")).unwrap();
        std::fs::write(
            json.join("Contents/Info.plist"),
            "{\"CFBundleIdentifier\": \"x\", \"CFBundlePackageType\": \"APPL\"}",
        )
        .unwrap();
        assert_eq!(read_candidate(&json, "x", &[]), Candidate::NotOurs);
        let nul = binary("n.app", "x");
        let mut bplist = b"bplist00\xd1\x01\x02\x5f\x10\x12CFBundleIdentifier\x56x\0junk".to_vec();
        bplist.extend_from_slice(&[8, 11, 32, 0, 0, 0, 0, 0, 0, 1, 1]);
        bplist.extend_from_slice(&3u64.to_be_bytes());
        bplist.extend_from_slice(&0u64.to_be_bytes());
        bplist.extend_from_slice(&39u64.to_be_bytes());
        std::fs::write(nul.join("Contents/Info.plist"), &bplist).unwrap();
        assert_eq!(read_candidate(&nul, "x", &[]), Candidate::NotOurs);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    const HANG_DETECTOR: std::time::Duration = std::time::Duration::from_secs(60);

    #[cfg(target_os = "macos")]
    #[test]
    fn a_pipe_held_open_past_the_tool_cannot_outlast_the_ceiling() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let gate = fifo_gate(dir.path());
        let _detector = gate_hang_detector(&gate);
        let armed = gate.clone();
        READERS_PENDING.with(|acts| {
            let mut acts = acts.borrow_mut();
            acts.push_back(Box::new(|deadline| *deadline = std::time::Instant::now()));
            acts.push_back(Box::new(move |_| release_gate(&armed).unwrap()));
        });
        let run = run_bounded_exit(
            "/bin/sh",
            &[
                "-c".as_ref(),
                GATED_GRANDCHILD.as_ref(),
                "sh".as_ref(),
                gate.as_os_str(),
            ],
            HANG_DETECTOR,
            TOOL_OUTPUT_MAX_BYTES,
        );
        let unrun = READERS_PENDING.with(|acts| acts.borrow_mut().drain(..).count());
        let held = release_gate(&gate);
        assert_eq!(run, ToolRun::TimedOut);
        assert_eq!(unrun, 1, "the wait ended at the poll past its ceiling");
        assert!(held.is_ok(), "the grandchild held the pipes: {held:?}");
    }

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

    #[test]
    fn the_open_probe_child_line_reads_back_as_its_outcome() {
        for outcome in [
            ProbeOutcome::Ok,
            ProbeOutcome::Errno(ERRNO_EPERM),
            ProbeOutcome::Errno(ERRNO_ENOENT),
            ProbeOutcome::Errno(13),
        ] {
            let printed = format!("{}\n", open_probe_line(outcome));
            assert_eq!(
                parse_open_probe_line(&printed),
                Some(outcome),
                "{printed:?}"
            );
        }
        for garbage in [
            "",
            "\n",
            "OK\n",
            "okay\n",
            "errno\n",
            "errno x\n",
            "errno 1 2\n",
            "error 13\n",
            "ok \n",
            "ok\r\n",
        ] {
            assert_eq!(parse_open_probe_line(garbage), None, "{garbage:?}");
        }
        let missing = std::env::temp_dir().join(format!("aterm-open-verb-{}", std::process::id()));
        for args in [
            vec![],
            vec![missing.clone().into_os_string(), missing.into_os_string()],
        ] {
            assert_eq!(
                format!("{:?}", run_open_probe_verb(&args)),
                format!("{:?}", std::process::ExitCode::FAILURE),
                "the verb wants one path"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_child_that_cannot_answer_leaves_the_open_to_this_process() {
        let scratch = aterm_tempfile::tempdir().expect("owned probe scratch");
        let path = scratch.path().join("open-child");
        std::fs::write(&path, b"").unwrap();
        let missing = Path::new("/nonexistent/aterm.app/Contents/MacOS/aterm");
        for exe in [None, Some(missing), Some(Path::new("/usr/bin/false"))] {
            assert_eq!(open_probe_in_child(exe, &path), ProbeOutcome::Ok, "{exe:?}");
        }
        let stand_in = |name: &str, code: u8| {
            use std::os::unix::fs::PermissionsExt as _;
            let exe = scratch.path().join(name);
            std::fs::write(&exe, format!("#!/bin/sh\necho 'errno 13'\nexit {code}\n")).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            exe
        };
        let (answers, fails) = (stand_in("answers", 0), stand_in("fails", 1));
        assert_eq!(
            open_probe_in_child(Some(&answers), &path),
            ProbeOutcome::Errno(13)
        );
        assert_eq!(open_probe_in_child(Some(&fails), &path), ProbeOutcome::Ok);
        let hangs = {
            use std::os::unix::fs::PermissionsExt as _;
            let exe = scratch.path().join("hangs");
            std::fs::write(&exe, "#!/bin/sh\nexec /bin/sleep 10\n").unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            exe
        };
        assert_eq!(
            open_probe_in_child_bounded(Some(&hangs), &path, std::time::Duration::from_millis(50),),
            ProbeOutcome::Errno(libc::ETIMEDOUT)
        );
        let folder = scratch.path().to_str().expect("utf-8 scratch");
        assert_eq!(
            run_bounded_exit(folder, &[], HANG_DETECTOR, TOOL_OUTPUT_MAX_BYTES),
            ToolRun::SpawnFailed,
            "a folder is there but runs nothing"
        );
        std::fs::remove_file(&path).expect("remove only this probe fixture");
        assert_eq!(
            open_probe_in_child(Some(missing), &path),
            ProbeOutcome::Errno(libc::ENOENT)
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_walk_does_not_cross_into_another_volume() {
        let scratch = aterm_tempfile::tempdir().expect("owned walk scratch");
        let bundle = scratch.path().canonicalize().unwrap().join("m.app");
        let mount = bundle.join("Contents/Vol");
        std::fs::create_dir_all(mount.join("inside")).unwrap();
        std::fs::write(
            bundle.join("Contents/Info.plist"),
            "<plist><dict><key>CFBundleIdentifier</key><string>x</string></dict></plist>",
        )
        .unwrap();
        let volume = device_of(&bundle).expect("the bundle's volume");
        let mounted = |path: &Path| {
            device_of(path).map(|dev| {
                if path.starts_with(&mount) {
                    dev + 1
                } else {
                    dev
                }
            })
        };
        assert!(
            bundle_reaches_protected_on(&bundle, &[], mounted, BUNDLE_WALK_MAX_ENTRIES),
            "a volume mounted inside the bundle"
        );
        assert!(
            !bundle_reaches_protected_on(
                &bundle,
                &[],
                |_: &Path| Some(volume),
                BUNDLE_WALK_MAX_ENTRIES
            ),
            "the control: every folder on the bundle's volume"
        );
        assert!(bundle_reaches_protected_on(
            &bundle,
            &[],
            |_: &Path| None,
            BUNDLE_WALK_MAX_ENTRIES
        ));
    }

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

    #[test]
    fn unsigned_is_the_whole_report_not_a_phrase_in_it() {
        let planted = "Executable=/A/x: code object is not signed at all/x\n\
                       Identifier=x\nSignature=adhoc\n\
                       # designated => cdhash H\"aa\"\n";
        assert_eq!(classify_signing(planted), "adhoc");
        assert_ne!(
            identity_from_report(Some(planted), None).dr,
            DrClass::Unsigned
        );
        let quoted = "Identifier=x\nTeamIdentifier=T\n\
                      designated => identifier \"code object is not signed at all\" and \
                      anchor apple generic and certificate leaf[subject.OU] = \"T\"\n";
        assert_eq!(classify_signing(quoted), "developer-id");
        assert_eq!(
            identity_from_report(Some(quoted), None).dr,
            DrClass::Unknown
        );
    }

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

    #[test]
    fn the_census_never_reads_a_copy_inside_another_apps_container() {
        let scratch = aterm_tempfile::tempdir().expect("owned census scratch");
        let home = scratch.path().canonicalize().unwrap().join("home");
        let container_app =
            home.join("Library/Containers/com.other.App/Data/Applications/aterm.app");
        let group_app = home.join("Library/Group Containers/group.other/aterm.app");
        let normal_app = home.join("Applications/aterm.app");
        for bundle in [&container_app, &group_app, &normal_app] {
            let contents = bundle.join("Contents");
            std::fs::create_dir_all(&contents).unwrap();
            std::fs::write(
                contents.join("Info.plist"),
                "<plist><dict><key>CFBundleIdentifier</key><string>dev.aterm</string></dict></plist>",
            )
            .unwrap();
        }
        let protected = census_fence(Some(&home));
        assert!(
            protected.contains(&home.join("Library/Containers"))
                && protected.contains(&home.join("Library/Group Containers")),
            "census_fence must include both App Data roots: {protected:?}"
        );
        assert_eq!(
            read_candidate(&container_app, "dev.aterm", &protected),
            Candidate::Hole,
            "a bundle under ~/Library/Containers must not be read"
        );
        assert_eq!(
            read_candidate(&group_app, "dev.aterm", &protected),
            Candidate::Hole,
            "a bundle under ~/Library/Group Containers must not be read"
        );
        let absent_container = home.join("Library/Containers/com.apple.Notes/Data/aterm.app");
        assert_eq!(
            read_candidate(&absent_container, "dev.aterm", &protected),
            Candidate::Hole,
            "an absent path under ~/Library/Containers is still fenced as a hole"
        );
        #[cfg(unix)]
        {
            let link_in_apps = home.join("Applications/linked-aterm.app");
            std::os::unix::fs::symlink(&container_app, &link_in_apps).unwrap();
            assert_eq!(
                read_candidate(&link_in_apps, "dev.aterm", &protected),
                Candidate::Hole,
                "a symlink in ~/Applications resolving into ~/Library/Containers must not be read"
            );
            let link_home = scratch.path().canonicalize().unwrap().join("home-link");
            std::os::unix::fs::symlink(&home, &link_home).unwrap();
            let linked_fence = census_fence(Some(&link_home));
            assert_eq!(
                read_candidate(&container_app, "dev.aterm", &linked_fence),
                Candidate::Hole,
                "canonical container path must be fenced even when HOME is a symlink"
            );
        }
        assert!(
            matches!(
                read_candidate(&normal_app, "dev.aterm", &protected),
                Candidate::Ours(_)
            ),
            "control bundle in ~/Applications must still be read"
        );
        assert!(
            Folder::AppData
                .path(&home)
                .starts_with(&app_data_roots(&home)[0]),
            "Folder::AppData probe path must share APP_DATA_ROOTS[0]"
        );
    }

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
        assert_eq!(c.listed(true).len(), MAX_CLAIMANT_ROWS);
        assert_eq!(c.retirable().len(), 1, "the ad-hoc copy is retirable");
        assert!(
            c.offered_for_trash(true).is_empty(),
            "but it is past the listed rows, so it is not offered"
        );
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
            "aterm.app.",
            "",
        ] {
            assert!(!looks_like_bundle(Path::new(no)), "{no}");
        }
    }

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
        assert_eq!(
            list_root(&tmp.path().join("archive.app.zip")),
            Listing::Unreadable,
            "a file is not a missing folder"
        );
        let at = |name: &str| tmp.path().join(name);
        std::fs::create_dir_all(at("flat.app")).unwrap();
        std::fs::write(at("flat.app/Contents"), b"").unwrap();
        std::fs::create_dir_all(at("huge.app/Contents")).unwrap();
        std::fs::File::create(at("huge.app/Contents/Info.plist"))
            .unwrap()
            .set_len(INFO_PLIST_MAX_BYTES + 1)
            .unwrap();
        std::fs::create_dir_all(at("folder.app/Contents/Info.plist")).unwrap();
        lay("upper.app", "X");
        for (name, want) in [
            ("flat.app", Candidate::Hole),
            ("huge.app", Candidate::Hole),
            ("folder.app", Candidate::NotOurs),
            ("upper.app", Candidate::NotOurs),
        ] {
            assert_eq!(read_candidate(&at(name), "x", &[]), want, "{name}");
        }
        #[cfg(unix)]
        {
            std::fs::create_dir_all(at("plain-folder")).unwrap();
            std::os::unix::fs::symlink(at("plain-folder"), at("named.app")).unwrap();
            assert_eq!(locate_candidate(&at("named.app")), None);
            std::os::unix::fs::symlink(at("mine.app"), at("mine-link.app")).unwrap();
            assert!(locate_candidate(&at("mine-link.app")).is_some_and(|found| found.link));
        }
    }

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
        let other = PathBuf::from("/Applications/aterm-new.app/Contents/MacOS/aterm");
        assert_eq!(
            fence_exe_from(Some(exec.clone()), &at(&other)),
            Some(exec.clone()),
            "both bundled: the exec path still wins"
        );
        let never = || -> Option<PathBuf> { panic!("the kernel answered") };
        let (app, rollback) = (
            p("/Applications/aterm.app"),
            p("/Applications/aterm.app.rollback"),
        );
        assert_eq!(running_bundle_from(at(&rolled), never), Some(rollback));
        assert_eq!(running_bundle_from(at(&exec), never), Some(app.clone()));
        assert_eq!(running_bundle_from(at(&test_bin), never), None);
        assert_eq!(running_bundle_from(SelfImage::NoPath, never), None);
        assert_eq!(
            running_bundle_from(SelfImage::Unreadable, || Some(exec.clone())),
            Some(app)
        );
        assert_eq!(running_bundle_from(SelfImage::Unreadable, || None), None);
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
    }

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
        assert_eq!(
            classify_image_anchor(Some(exec), &at(rolled)),
            ImageAnchor::Displaced
        );
        let other = Path::new("/Applications/aterm-new.app/Contents/MacOS/aterm");
        assert_eq!(
            classify_image_anchor(Some(exec), &at(other)),
            ImageAnchor::Live
        );
        assert_eq!(
            classify_image_anchor(Some(unbundled), &at(unbundled)),
            ImageAnchor::NotBundled
        );

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
    }

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
        assert_eq!(
            probe_fda_with(
                ProbeGate::on(),
                Some(&bundled),
                Some(&p("/Users//a\0b")),
                boom
            )
            .label,
            ProbeLabel::RefusedBadPath,
            "a home no C API can take"
        );
        for gate in [off, no_check] {
            assert_eq!(
                probe_fda_with(gate, Some(&bundled), Some(&home), boom).state,
                FdaState::Unknown
            );
        }
    }

    #[test]
    fn a_host_uuid_is_spelled_8_4_4_4_12() {
        let id = [
            0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ];
        assert_eq!(format_uuid(&id), "12345678-9ABC-DEF0-0123-456789ABCDEF");
    }

    #[test]
    fn probe_opens_only_the_user_store_when_it_answers() {
        let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
        let home = p("/Users//a");
        let user = p("/Users//a/Library/Application Support/com.apple.TCC/TCC.db");
        for (outcome, state) in [
            (ProbeOutcome::Ok, FdaState::Granted),
            (ProbeOutcome::Errno(ERRNO_EPERM), FdaState::Denied),
            (ProbeOutcome::Errno(13), FdaState::Unknown),
        ] {
            let mut seen = Vec::new();
            let got = probe_fda_with(ProbeGate::on(), Some(&bundled), Some(&home), |db| {
                seen.push(db.to_path_buf());
                outcome
            });
            assert_eq!(got.state, state, "{outcome:?}");
            assert_eq!(seen, vec![user.clone()], "{outcome:?}");
        }
    }

    #[test]
    fn probe_falls_back_to_the_system_store_when_the_user_store_is_absent() {
        let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
        let home = p("/Users//a");
        let user = p("/Users//a/Library/Application Support/com.apple.TCC/TCC.db");
        for (system, state, label) in [
            (ProbeOutcome::Ok, FdaState::Granted, ProbeLabel::OpenOk),
            (
                ProbeOutcome::Errno(ERRNO_EPERM),
                FdaState::Denied,
                ProbeLabel::OpenEperm,
            ),
            (
                ProbeOutcome::Errno(ERRNO_ENOENT),
                FdaState::Unknown,
                ProbeLabel::OpenErrno(ERRNO_ENOENT),
            ),
        ] {
            let mut seen = Vec::new();
            let got = probe_fda_with(ProbeGate::on(), Some(&bundled), Some(&home), |db| {
                seen.push(db.to_path_buf());
                if db == user {
                    ProbeOutcome::Errno(ERRNO_ENOENT)
                } else {
                    system
                }
            });
            assert_eq!((got.state, got.label), (state, label), "{system:?}");
            assert_eq!(seen, vec![user.clone(), p(SYSTEM_TCC_DB)], "{system:?}");
        }
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
            (
                ProbeOutcome::Errno(13),
                FdaState::Unknown,
                ProbeLabel::OpenErrno(13),
            ),
        ];
        for (outcome, state, label) in cases {
            let got = classify_probe(outcome);
            assert_eq!(got.state, state, "state for {outcome:?}");
            assert_eq!(got.label, label, "label for {outcome:?}");
        }
    }

    #[test]
    fn responsible_pids_detailed_resolves_once() {
        let resolutions = std::cell::Cell::new(0_usize);
        let asked = std::cell::RefCell::new(Vec::new());
        let resolve = || {
            resolutions.set(resolutions.get() + 1);
            Some(|pid: i32| {
                asked.borrow_mut().push(pid);
                if pid == 3 {
                    Err(ResponsibleError::Gone)
                } else {
                    Ok(pid * 10)
                }
            })
        };
        assert_eq!(
            ask_each(&[1, 2, 3], resolve),
            [Ok(10), Ok(20), Err(ResponsibleError::Gone)]
        );
        assert_eq!(resolutions.get(), 1, "the SPI is resolved once per slice");
        assert_eq!(
            *asked.borrow(),
            [1, 2, 3],
            "and asked once per pid, in order"
        );

        let absent = || {
            resolutions.set(resolutions.get() + 1);
            None::<fn(i32) -> Result<i32, ResponsibleError>>
        };
        assert_eq!(
            ask_each(&[1, 2, 3], absent),
            [Err(ResponsibleError::SymbolUnavailable); 3]
        );
        assert_eq!(resolutions.get(), 2);

        assert!(ask_each(&[], resolve).is_empty());
        assert_eq!(resolutions.get(), 2, "an empty slice resolves nothing");
        assert_eq!(asked.borrow().len(), 3, "the absent symbol asked nothing");
    }

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
        assert_eq!(classify_responsible(me, Ok(0)), Responsible::Unknown);
        assert_eq!(classify_responsible(me, Ok(-1)), Responsible::Unknown);
    }

    #[test]
    fn info_plist_under_a_protected_root_is_refused_and_falls_back_to_the_basename() {
        let home = p("/Users//a");
        let protected = protected_roots_in(Some(&home), &[]);
        assert!(
            protected.contains(&p("/Users//a/Documents")),
            "PRIVATE_SUBDIRS must supply ~/Documents"
        );

        let evil = p("/Users//a/Documents/Sketchy.app/Contents/MacOS/Sketchy");
        assert_eq!(readable_info_plist(&evil, &protected), None);
        assert_eq!(display_name(&evil, None), "Sketchy");

        let ok = p("/Applications/iTerm.app/Contents/MacOS/iTerm2");
        assert_eq!(
            readable_info_plist(&ok, &protected),
            Some(p("/Applications/iTerm.app/Contents/Info.plist"))
        );

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

        let empty = "<plist><dict><key>CFBundleDisplayName</key><string></string></dict></plist>";
        assert_eq!(display_name(&exe, Some(empty)), "aterm");
        let wrong = "<plist><dict><key>CFBundleDisplayName</key><true/><key>CFBundleName</key><string>ok</string></dict></plist>";
        assert_eq!(display_name(&exe, Some(wrong)), "ok");
    }

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
            (
                "identifier \"com.aterm.aterm\" and cdhash H\"abc\"",
                DrClass::Cdhash,
            ),
            ("code object is not signed at all", DrClass::Unsigned),
            ("", DrClass::Unknown),
            ("anchor apple", DrClass::Unknown),
            ("identifier \"com.aterm.aterm\"", DrClass::Unknown),
            (
                "identifier \"com.cdhash.x\" and anchor apple generic and \
                 certificate leaf[subject.OU] = \"CDHASH\"",
                DrClass::Identity,
            ),
            (
                "identifier \"x cdhash H\" and anchor apple generic",
                DrClass::Identity,
            ),
            ("identifier \"anchor apple\"", DrClass::Unknown),
            (
                "designated => identifier mycdhash and anchor apple generic and \
                 certificate leaf[subject.OU] = A66A9P66Z7",
                DrClass::Identity,
            ),
            ("identifier cdhash", DrClass::Unknown),
            ("(cdhash H\"abc\")", DrClass::Cdhash),
            (
                "identifier \"a\\\" and anchor apple generic\"",
                DrClass::Unknown,
            ),
            (
                "Executable=/A/we\"ird\ndesignated => cdhash H\"aa\"",
                DrClass::Cdhash,
            ),
            ("x: not signed at all", DrClass::Unsigned),
            ("code object is not signed", DrClass::Unsigned),
            (
                "identifier\"x\" and anchor apple generic",
                DrClass::Identity,
            ),
            (
                "identifier \"x\" and certificate root = H\"ab\"",
                DrClass::Identity,
            ),
            (
                "identifier \"x\" and certificate 1[subject.OU] = \"T\"",
                DrClass::Identity,
            ),
            ("identifier xcdhash H\"ab\"", DrClass::Unknown),
            ("identifier my_cdhash H\"ab\"", DrClass::Unknown),
        ];
        for (input, want) in cases {
            assert_eq!(classify_dr(input), want, "classifying {input:?}");
        }
        for (class, stable) in [
            (DrClass::Identity, true),
            (DrClass::Cdhash, false),
            (DrClass::Unsigned, false),
            (DrClass::Unknown, false),
        ] {
            assert_eq!(class.grant_stable(), stable, "{class:?}");
        }
    }

    #[test]
    fn empty_overrides_resolve_to_private_subdirs() {
        let home = p("/Users//a");
        let roots = protected_roots_in(Some(&home), &[]);
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
        let volumes = if cfg!(windows) {
            "C:/Volumes"
        } else {
            "/Volumes"
        };
        let overrides = [
            String::from("~/Documents"),
            String::from(volumes),
            String::from("~"),
            String::from("  ~/Library/CloudStorage  "),
            String::from("relative/path"),
            String::new(),
        ];
        let roots = protected_roots_in(Some(&home), &overrides);
        assert_eq!(
            roots,
            vec![
                p("/Users//a/Documents"),
                p(volumes),
                p("/Users//a"),
                p("/Users//a/Library/CloudStorage"),
            ]
        );
        assert!(protected_roots_in(None, &[]).is_empty());
        assert_eq!(protected_roots_in(None, &overrides), vec![p(volumes)]);
    }

    #[test]
    fn tcc_prompting_roots_hold_the_private_set_and_what_it_omits() {
        let home = p("/Users//a");
        let roots = tcc_prompting_roots(&home, &[]);
        for sub in crate::sbpl::PRIVATE_SUBDIRS {
            assert!(roots.contains(&home.join(sub)), "{sub}");
        }
        for sub in TCC_PROMPTING_SUBPATHS {
            assert!(roots.contains(&home.join(sub)), "{sub}");
        }
        for abs in TCC_PROMPTING_ABSOLUTE {
            assert!(roots.contains(&p(abs)), "{abs}");
        }
        let narrowed = tcc_prompting_roots(&home, &[String::from("~/Projects/secret")]);
        assert!(narrowed.contains(&home.join("Documents")));
        assert!(narrowed.contains(&p("/Users//a/Projects/secret")));
        assert_eq!(narrowed.len(), roots.len() + 1);
    }

    #[cfg(unix)]
    #[test]
    fn other_homes_sit_beside_this_one() {
        assert_eq!(other_homes_root(Path::new("/Users//a")), Some(p("/Users")));
        assert_eq!(
            other_homes_root(Path::new("/System/Volumes/Data/Users//a")),
            Some(p("/System/Volumes/Data/Users"))
        );
        assert_eq!(other_homes_root(Path::new("/root")), None);
        assert_eq!(other_homes_root(Path::new("relative/a")), None);
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
        assert!(!is_under_protected_root(
            Path::new("/Users//a/DocumentsOld/x"),
            &roots
        ));
        assert!(!is_under_protected_root(Path::new("/Users//a"), &roots));
        assert!(!is_under_protected_root(Path::new("/Applications"), &[]));
    }

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
            assert_eq!(folder.absent_means_allowed(), folder == Folder::AppData);
            assert_eq!(Folder::parse(name), Some(folder));
            assert_eq!(Folder::parse(&name.to_uppercase()), Some(folder));
        }
        for spelling in ["app_data", "appdata", " App_Data "] {
            assert_eq!(
                Folder::parse(spelling),
                Some(Folder::AppData),
                "{spelling:?}"
            );
        }
        for unknown in ["pictures", "", "app data", "apps"] {
            assert_eq!(Folder::parse(unknown), None, "{unknown:?}");
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
            (false, true, gone, false),
        ];
        for (is_file, executable, want, can_run) in cases {
            let got = classify_tccutil(is_file, executable);
            assert_eq!(got, want, "is_file={is_file} executable={executable}");
            assert_eq!(got.can_run(), can_run);
        }
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
        for bad in ["", "   ", "-not-a-flag"] {
            assert!(ResetSubject::from_usable(bad).is_none(), "{bad:?}");
        }
        let subject = ResetSubject::from_usable("a.b.c").expect("usable");
        for folder in Folder::ALL {
            let argv = tccutil_reset_command(&subject, *folder);
            assert_eq!(argv.len(), 4, "{folder:?}");
            assert_eq!(argv[0], TCCUTIL_PATH);
            assert_eq!(argv[1], "reset");
            assert_eq!(argv[2], folder.tcc_service());
            assert_eq!(argv[3], "a.b.c");
        }
    }

    #[test]
    fn a_reset_subject_is_read_from_a_bundles_plist() {
        let scratch = aterm_tempfile::tempdir().expect("subject scratch");
        let root = scratch.path().join("Subject.app");
        let contents = root.join("Contents");
        std::fs::create_dir_all(&contents).expect("bundle contents");
        let plist = |id: &str| {
            format!(
                "<plist><dict><key>CFBundleIdentifier</key><string>{id}</string></dict></plist>"
            )
        };
        std::fs::write(contents.join("Info.plist"), plist(" a.b.c ")).expect("plist");
        let subject = ResetSubject::read(&root, &[]).expect("a subject");
        assert_eq!(subject.as_str(), "a.b.c", "the subject is trimmed once");
        std::fs::write(contents.join("Info.plist"), plist("-not-a-flag")).expect("plist");
        assert_eq!(ResetSubject::read(&root, &[]), None);
    }

    #[test]
    fn a_plan_is_one_command_per_distinct_folder_from_one_subject() {
        let subject = ResetSubject::from_usable(" a.b.c ").expect("usable");
        let plan = ResetPlan::new(
            &subject,
            &[
                Folder::Downloads,
                Folder::Documents,
                Folder::Downloads,
                Folder::Documents,
            ],
        )
        .expect("plan builds");
        assert_eq!(plan.bundle_id(), "a.b.c", "the subject is trimmed once");
        assert_eq!(plan.folders(), &[Folder::Downloads, Folder::Documents]);
        let commands = plan.commands();
        assert_eq!(
            commands,
            vec![
                (
                    Folder::Downloads,
                    tccutil_reset_command(&subject, Folder::Downloads)
                ),
                (
                    Folder::Documents,
                    tccutil_reset_command(&subject, Folder::Documents)
                ),
            ]
        );
        assert!(ResetPlan::new(&subject, &[]).is_none());
    }

    fn offer_inputs<'a>(
        dr: DrClass,
        tccutil: TccutilPresence,
        bundle_id: Option<&'a ResetSubject>,
    ) -> ResetOfferInputs<'a> {
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
        let subject = ResetSubject::from_usable("a.b.c").expect("usable");
        let id = Some(&subject);
        let cases = [
            (DrClass::Identity, run, id, ResetOffer::Offer),
            (DrClass::Unknown, run, id, ResetOffer::Offer),
            (DrClass::Cdhash, run, id, ResetOffer::ExplainRebuild),
            (DrClass::Unsigned, run, id, ResetOffer::ExplainRebuild),
            (DrClass::Cdhash, gone, id, ResetOffer::ExplainRebuild),
            (DrClass::Cdhash, run, None, ResetOffer::ExplainRebuild),
            (DrClass::Identity, gone, id, ResetOffer::HideNoTool),
            (DrClass::Identity, notx, id, ResetOffer::HideNoTool),
            (DrClass::Identity, unk, id, ResetOffer::HideNoTool),
            (DrClass::Identity, run, None, ResetOffer::HideNoBundleId),
        ];
        for (dr, tool, bundle, want) in cases {
            let got = reset_offer(offer_inputs(dr, tool, bundle));
            assert_eq!(got, want, "dr={dr:?} tool={tool:?} bundle={bundle:?}");
            let offered = want == ResetOffer::Offer;
            assert_eq!(got.shows_button(), offered);
            assert_eq!(
                ResetPlan::for_offer(offer_inputs(dr, tool, bundle), Folder::ALL).is_some(),
                offered,
                "dr={dr:?} tool={tool:?} bundle={bundle:?}"
            );
        }
        assert_eq!(
            reset_offer(ResetOfferInputs::default()),
            ResetOffer::HideNoTool
        );
    }

    #[test]
    fn one_invocations_status_is_read_from_its_exit_alone() {
        assert_eq!(ResetStatus::from_exit_status(Some(0)), ResetStatus::Reset);
        assert_eq!(
            ResetStatus::from_exit_status(Some(64)),
            ResetStatus::Declined { code: Some(64) }
        );
        assert_eq!(
            ResetStatus::from_exit_status(None),
            ResetStatus::Declined { code: None }
        );
    }

    #[test]
    fn the_reset_outcome_is_a_table() {
        let ok = |f: Folder| ResetAttempt::new(f, ResetStatus::Reset);
        let bad = |f: Folder, c: i32| ResetAttempt::from_exit_status(f, Some(c));
        let gone = ResetAttempt::tool_absent;
        let cases: [(&[ResetAttempt], ResetOutcome); 7] = [
            (
                &[
                    ok(Folder::Documents),
                    ok(Folder::Desktop),
                    ok(Folder::Downloads),
                ],
                ResetOutcome::AllReset,
            ),
            (
                &[
                    ok(Folder::Documents),
                    bad(Folder::Desktop, 1),
                    ok(Folder::Downloads),
                ],
                ResetOutcome::Partial,
            ),
            (
                &[
                    bad(Folder::Documents, 1),
                    bad(Folder::Desktop, 1),
                    bad(Folder::Downloads, 70),
                ],
                ResetOutcome::NoneReset,
            ),
            (
                &[
                    gone(Folder::Documents),
                    gone(Folder::Desktop),
                    gone(Folder::Downloads),
                ],
                ResetOutcome::ToolAbsent,
            ),
            (
                &[ok(Folder::Documents), gone(Folder::Desktop)],
                ResetOutcome::Partial,
            ),
            (
                &[bad(Folder::Documents, 1), gone(Folder::Desktop)],
                ResetOutcome::NoneReset,
            ),
            (&[], ResetOutcome::NotAttempted),
        ];
        for (attempts, want) in cases {
            assert_eq!(reset_outcome(attempts), want, "{attempts:?}");
        }
    }

    #[test]
    fn only_a_reset_folder_is_warmed_and_only_an_asked_one_declined() {
        let attempts = [
            ResetAttempt::new(Folder::Documents, ResetStatus::Reset),
            ResetAttempt::from_exit_status(Folder::Desktop, Some(1)),
            ResetAttempt::from_exit_status(Folder::Downloads, None),
            ResetAttempt::tool_absent(Folder::AppData),
        ];
        assert_eq!(folders_reset(&attempts), vec![Folder::Documents]);
        assert_eq!(
            folders_declined(&attempts),
            vec![(Folder::Desktop, Some(1)), (Folder::Downloads, None)]
        );
    }

    fn inputs(adoption: Attribution, fda: FdaState) -> PostureInputs {
        PostureInputs {
            adoption,
            fda,
            ..PostureInputs::default()
        }
    }

    #[test]
    fn posture_join_is_a_table_under_todays_evidence() {
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
        for evidence in [SpikeEvidence::UNMEASURED, SpikeEvidence::MEASURED] {
            for (adoption, fda) in cases {
                let got = ConsentPosture::join(PostureInputs {
                    evidence,
                    ..inputs(adoption, fda)
                });
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
    }

    #[test]
    fn an_observed_eperm_is_the_only_route_to_denied() {
        for fda in [FdaState::Granted, FdaState::Denied, FdaState::Unknown] {
            let mut i = inputs(Attribution::Live, fda);
            i.observed_eperm = true;
            assert_eq!(ConsentPosture::join(i).fs_consent, FsConsent::Denied);
        }
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

        i.evidence.handoff_attribution_measured = true;
        assert_eq!(ConsentPosture::join(i).fs_consent, FsConsent::Denied);
    }

    #[test]
    fn covered_requires_grant_plus_live_plus_measured_coverage() {
        let measured = SpikeEvidence {
            fda_coverage_measured: true,
            fda_scope: FdaScope::ThisProcess,
            ..SpikeEvidence::UNMEASURED
        };
        let mut i = inputs(Attribution::Live, FdaState::Granted);
        i.evidence = measured;
        let got = ConsentPosture::join(i);
        assert_eq!(got.fs_consent, FsConsent::Covered);
        assert!(!got.prompt_possible);
        assert_eq!(
            got.fda_scope,
            FdaScope::ThisProcess,
            "carried from the evidence"
        );

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
    }

    #[test]
    fn the_wire_tokens_are_the_documented_spellings() {
        let tokens = [
            (FdaState::Granted.as_str(), "granted"),
            (FdaState::Denied.as_str(), "denied"),
            (FdaState::Unknown.as_str(), "unknown"),
            (Attribution::Live.as_str(), "live"),
            (Attribution::Adopted.as_str(), "adopted"),
            (Attribution::Unknown.as_str(), "unknown"),
            (FsConsent::Covered.as_str(), "covered"),
            (FsConsent::Denied.as_str(), "denied"),
            (FsConsent::Unknown.as_str(), "unknown"),
            (FdaScope::Unknown.as_str(), "unknown"),
            (FdaScope::ThisProcess.as_str(), "this_process"),
            (FdaScope::NewProcesses.as_str(), "new_processes"),
            (DrClass::Identity.as_str(), "identity"),
            (DrClass::Cdhash.as_str(), "cdhash"),
            (DrClass::Unsigned.as_str(), "unsigned"),
            (DrClass::Unknown.as_str(), "unknown"),
            (ImageAnchor::Live.as_str(), "live"),
            (ImageAnchor::Displaced.as_str(), "displaced"),
            (ImageAnchor::Deleted.as_str(), "deleted"),
            (ImageAnchor::NotBundled.as_str(), "not-bundled"),
            (ImageAnchor::Unknown.as_str(), "unknown"),
            (DebuggerMode::Enabled.as_str(), "enabled"),
            (DebuggerMode::Disabled.as_str(), "disabled"),
            (DebuggerMode::Unrecognized.as_str(), "unrecognized"),
            (DebuggerMode::Unknown.as_str(), "unknown"),
            (DebuggerMember::Admin.as_str(), "admin"),
            (DebuggerMember::Developer.as_str(), "developer"),
            (DebuggerMember::NotMember.as_str(), "no"),
            (DebuggerMember::Unknown.as_str(), "unknown"),
            (SelfHardening::Hardened.as_str(), "yes"),
            (SelfHardening::NotHardened.as_str(), "no"),
            (SelfHardening::Unknown.as_str(), "unknown"),
            (DevToolState::Allowed.as_str(), "allowed"),
            (DevToolState::Denied.as_str(), "denied"),
            (DevToolState::Unlisted.as_str(), "unlisted"),
            (DevToolState::Unknown.as_str(), "unknown"),
            (DevToolListing::On.as_str(), "on"),
            (DevToolListing::Off.as_str(), "off"),
            (DevToolListing::Absent.as_str(), "absent"),
            (DevToolListing::Unknown.as_str(), "unknown"),
            (DevToolGrant::Granted.as_str(), "granted"),
            (DevToolGrant::NotGranted.as_str(), "not_granted"),
            (DevToolGrant::Unknown.as_str(), "unknown"),
        ];
        for (got, want) in tokens {
            assert_eq!(got, want);
        }

        use ProbeLabel as F;
        for (label, token, refused) in [
            (F::Pending, "pending", false),
            (F::OpenOk, "open_ok", false),
            (F::OpenEperm, "open_eperm", false),
            (F::OpenErrno(9), "open_errno", false),
            (F::RefusedOutOfBundle, "refused_out_of_bundle", true),
            (F::RefusedNoExe, "refused_no_exe", true),
            (F::RefusedDisabled, "refused_disabled", true),
            (F::RefusedNoHome, "refused_no_home", true),
            (F::RefusedBadPath, "refused_bad_path", true),
            (F::UnsupportedPlatform, "unsupported_platform", true),
        ] {
            assert_eq!((label.as_str(), label.refused()), (token, refused));
        }
        use DebuggerProbeLabel as D;
        for (label, token, refused) in [
            (D::Pending, "pending", false),
            (D::StatusOk, "status_ok", false),
            (D::StatusUnrecognized, "status_unrecognized", false),
            (D::ToolAbsent, "tool_absent", false),
            (D::SpawnFailed, "spawn_failed", false),
            (D::TimedOut, "timed_out", false),
            (D::OutputUnreadable, "output_unreadable", false),
            (D::RefusedDisabled, "refused_disabled", true),
            (D::RefusedOutOfBundle, "refused_out_of_bundle", true),
            (D::RefusedNoExe, "refused_no_exe", true),
            (D::UnsupportedPlatform, "unsupported_platform", true),
        ] {
            assert_eq!((label.as_str(), label.refused()), (token, refused));
        }
        use DevToolProbeLabel as T;
        for (label, token, refused) in [
            (T::Pending, "pending", false),
            (T::Observed, "observed", false),
            (T::NoRecentRequest, "no_recent_request", false),
            (T::LogUnavailable, "log_unavailable", false),
            (T::LogFailed, "log_failed", false),
            (T::FormatUnrecognized, "format_unrecognized", false),
            (T::TimedOut, "timed_out", false),
            (T::SpawnFailed, "spawn_failed", false),
            (T::OutputUnreadable, "output_unreadable", false),
            (T::RefusedDisabled, "refused_disabled", true),
            (T::RefusedObserverOff, "refused_observer_off", true),
            (T::RefusedOutOfBundle, "refused_out_of_bundle", true),
            (T::RefusedNoExe, "refused_no_exe", true),
            (T::RefusedNoSubject, "refused_no_subject", true),
            (T::UnsupportedPlatform, "unsupported_platform", true),
        ] {
            assert_eq!((label.as_str(), label.refused()), (token, refused));
        }
        use DevToolListLabel as L;
        for (label, token, refused) in [
            (L::Pending, "pending", false),
            (L::Read, "read", false),
            (L::NoFullDiskAccess, "no_full_disk_access", false),
            (L::OpenFailed, "open_failed", false),
            (L::ToolFailed, "sqlite_failed", false),
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
            assert_eq!((label.as_str(), label.refused()), (token, refused));
        }

        let unobserved = [
            FdaState::default() == FdaState::Unknown,
            Attribution::default() == Attribution::Unknown,
            FsConsent::default() == FsConsent::Unknown,
            DebuggerMode::default() == DebuggerMode::Unknown,
            DebuggerMember::default() == DebuggerMember::Unknown,
            SelfHardening::default() == SelfHardening::Unknown,
            DevToolState::default() == DevToolState::Unknown,
            DevToolListing::default() == DevToolListing::Unknown,
            DevToolGrant::default() == DevToolGrant::Unknown,
        ];
        assert_eq!(unobserved, [true; 9], "a default that is a verdict");
    }

    #[test]
    fn the_change_builders_always_carry_their_flags() {
        assert!(
            Path::new(DEVTOOLSSECURITY_PATH).has_root(),
            "never resolved through PATH"
        );
        assert_eq!(
            DebuggerChange::On.command(),
            [DEVTOOLSSECURITY_PATH, "-enable"]
        );
        let disable = DebuggerChange::Off.command();
        assert_eq!(disable, [DEVTOOLSSECURITY_PATH, "-disable"]);
        assert_eq!(DEBUGGER_ACCESS_REVERT, disable.join(" "));
        assert_eq!(DebuggerChange::On.precondition(), DebuggerMode::Disabled);
        assert_eq!(DebuggerChange::Off.precondition(), DebuggerMode::Enabled);
        assert_eq!(DebuggerChange::On.target(), DebuggerMode::Enabled);
        assert_eq!(DebuggerChange::Off.target(), DebuggerMode::Disabled);
        for change in [DebuggerChange::On, DebuggerChange::Off] {
            assert_eq!(DebuggerChange::from_token(change.as_str()), Some(change));
        }
        assert_eq!(DebuggerChange::from_token("enable"), None);
    }

    fn probe_reading(mode: DebuggerMode) -> DebuggerProbe {
        DebuggerProbe {
            mode,
            member: DebuggerMember::Admin,
            label: DebuggerProbeLabel::StatusOk,
        }
    }

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

    #[cfg(unix)]
    #[test]
    fn the_change_runner_keeps_the_exit_and_a_head_of_each_stream() {
        let mut seen = None;
        let run = run_to_exit_keeping_heads(
            &[
                "/bin/sh",
                "-c",
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

    #[cfg(target_os = "macos")]
    #[test]
    fn a_change_reader_that_lags_the_exit_still_reaches_the_head() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let gate = fifo_gate(dir.path());
        let _detector = gate_hang_detector(&gate);
        let armed = gate.clone();
        GRACE_POLL.with(|slot| {
            let act = move |_: &mut u32, readers_done: &dyn Fn() -> bool| {
                release_gate(&armed).unwrap();
                let released = std::time::Instant::now();
                while !readers_done() {
                    assert!(
                        released.elapsed() < HANG_DETECTOR,
                        "the readers never ended"
                    );
                    std::thread::yield_now();
                }
            };
            *slot.borrow_mut() = Some((1, Box::new(act)));
        });
        let run = run_to_exit_keeping_heads(
            &[
                "/bin/sh",
                "-c",
                GATED_GRANDCHILD,
                "sh",
                gate.to_str().unwrap(),
            ],
            |_| {},
        );
        let _ = release_gate(&gate);
        assert_eq!(
            run,
            DevtoolsChangeRun::Exited {
                code: Some(0),
                stdout_head: "earlylate".to_string(),
                stderr_head: String::new(),
            }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_change_reader_still_running_past_the_grace_leaves_an_empty_head() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let gate = fifo_gate(dir.path());
        let _detector = gate_hang_detector(&gate);
        GRACE_POLL.with(|slot| {
            let act = |polls: &mut u32, readers_done: &dyn Fn() -> bool| {
                assert!(!readers_done(), "the grandchild holds both pipes");
                *polls = CHANGE_READER_GRACE_POLLS - 1;
                GRACE_POLL.with(|slot| {
                    *slot.borrow_mut() = Some((CHANGE_READER_GRACE_POLLS, Box::new(|_, _| {})));
                });
            };
            *slot.borrow_mut() = Some((0, Box::new(act)));
        });
        let run = run_to_exit_keeping_heads(
            &[
                "/bin/sh",
                "-c",
                GATED_GRANDCHILD,
                "sh",
                gate.to_str().unwrap(),
            ],
            |_| {},
        );
        let armed = GRACE_POLL.with(|slot| slot.borrow_mut().take().map(|(at, _)| at));
        let held = release_gate(&gate);
        assert_eq!(
            armed,
            Some(CHANGE_READER_GRACE_POLLS),
            "the first grace poll ran its act (it left Some(0) armed), and no poll ran past \
             the last (it would have taken the act, None)"
        );
        assert_eq!(
            run,
            DevtoolsChangeRun::Exited {
                code: Some(0),
                stdout_head: String::new(),
                stderr_head: String::new(),
            }
        );
        assert!(
            held.is_ok(),
            "the grandchild still held the pipes when the run returned: {held:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_change_runner_names_a_missing_program() {
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
        let folder = std::env::temp_dir();
        let folder = folder.to_str().expect("utf-8 temp dir");
        assert_eq!(
            run_to_exit_keeping_heads(&[folder], |_| panic!("nothing was spawned")),
            DevtoolsChangeRun::SpawnFailed,
            "a folder is there but runs nothing"
        );
        assert!(
            DevtoolsChangeRun::WaitFailed.spawned(),
            "a child whose wait failed was still started"
        );
    }

    #[test]
    fn a_recorded_child_runs_only_while_its_birth_matches() {
        let me = std::process::id();
        let no_birth = DevtoolsChild {
            pid: me,
            birth: None,
        };
        assert!(!no_birth.is_running());
        assert!(
            !DevtoolsChild {
                pid: 0,
                birth: None
            }
            .is_running(),
            "no birth matches no birth: a pid the kernel will not describe is not running"
        );
        let Some(birth) = process_birth(me) else {
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

    fn exited(code: Option<i32>, stdout: &str, stderr: &str) -> ToolRun {
        ToolRun::Exited {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn the_status_classifier_reads_only_what_the_tool_says() {
        use DebuggerMode as M;
        use DebuggerProbeLabel as L;
        let ok = L::StatusOk;
        let no = (M::Unknown, L::StatusUnrecognized);
        let cases: &[(ToolRun, (DebuggerMode, DebuggerProbeLabel))] = &[
            (
                exited(Some(0), "Developer mode is currently disabled.\n", ""),
                (M::Disabled, ok),
            ),
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
            (
                exited(Some(0), "Developer mode is now enabled.\n", ""),
                (M::Enabled, ok),
            ),
            (
                exited(Some(0), "  Developer mode is currently disabled.\r\n", ""),
                (M::Disabled, ok),
            ),
            (
                exited(Some(0), "", "Developer mode is currently disabled.\n"),
                (M::Disabled, ok),
            ),
            (
                exited(
                    Some(0),
                    "Getting rights definition for: system.privilege.taskport.debug\n\
                     Developer mode is currently enabled.\n",
                    "",
                ),
                (M::Enabled, ok),
            ),
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
            (
                exited(Some(1), "Developer mode is currently disabled.\n", ""),
                no,
            ),
            (
                exited(None, "Developer mode is currently enabled.\n", ""),
                no,
            ),
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
                exited(
                    Some(0),
                    "Developer mode rights are newly in an unknown state.\n",
                    "",
                ),
                no,
            ),
            (
                exited(Some(0), "Developer mode is currently disenabled.\n", ""),
                no,
            ),
            (
                exited(
                    Some(0),
                    "Developer mode is currently enabled.\n",
                    "Developer mode is currently disabled.\n",
                ),
                no,
            ),
            (ToolRun::Absent, (M::Unknown, L::ToolAbsent)),
            (ToolRun::SpawnFailed, (M::Unknown, L::SpawnFailed)),
            (ToolRun::TimedOut, (M::Unknown, L::TimedOut)),
            (ToolRun::Unreadable, (M::Unknown, L::OutputUnreadable)),
        ];
        for (run, want) in cases {
            assert_eq!(classify_devtools_status(run), *want, "{run:?}");
        }
        assert_eq!(
            classify_devtools_status(&exited(
                Some(0),
                "Developer mode is currently disabled.\n",
                "Developer mode is currently disabled.\n",
            )),
            (M::Disabled, ok)
        );
    }

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
        let deps = p("/Users//a/aterm/target/debug/deps/aterm_containment-abc123");
        let got = probe_debugger_with(ProbeGate::on(), Some(&deps), never_spawn, never_ask);
        assert_eq!(
            got,
            DebuggerProbe::refused(DebuggerProbeLabel::RefusedOutOfBundle)
        );
        assert_eq!(
            (got.mode, got.member),
            (DebuggerMode::Unknown, DebuggerMember::Unknown)
        );
    }

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

    const CAPTURED_RECORD: &str = r#"{"timezoneName":"","messageType":"Info","eventType":"logEvent","source":null,"formatString":"Handling access request to %{public}@, from %{public}@, %{public}@","userID":0,"activityIdentifier":223003332,"subsystem":"com.apple.TCC","category":"access","threadID":173778168,"senderImageUUID":"5379B300-7E73-30E5-B53F-7E8A12954AB5","backtrace":{"frames":[{"imageOffset":74432,"imageUUID":"5379B300-7E73-30E5-B53F-7E8A12954AB5"}]},"bootUUID":"00000000-0000-0000-0000-000000000000","processImagePath":"\/System\/Library\/PrivateFrameworks\/TCC.framework\/Support\/tccd","senderImagePath":"\/System\/Library\/PrivateFrameworks\/TCC.framework\/Support\/tccd","timestamp":"2026-10-07 13:58:21.820241+0000","machTimestamp":66148736620656,"eventMessage":"Handling access request to kTCCServiceDeveloperTool, from Sub:{com.aterm.aterm}Resp:{TCCDProcess: identifier=com.aterm.aterm, pid=56408, auid=501, euid=501, responsible_path=\/Applications\/aterm.app\/Contents\/MacOS\/aterm, binary_path=\/bin\/sh}, ReqResult(Auth Right: Unknown (None), promptType: 1,DB Action:None, UpdateVerifierData)","processImageUUID":"5379B300-7E73-30E5-B53F-7E8A12954AB5","traceID":2660156748005636,"processID":588,"senderProgramCounter":74432,"parentActivityIdentifier":0}"#;

    const CAPTURED_AT_MS: i64 = 1_791_381_501_820;

    const ATERM: &str = "com.aterm.aterm";

    const FORGED_SUB: &str = "com.aterm.aterm}Resp:{x";

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

    fn tccd_message(subject: &str, binary: &str, answer: &str) -> String {
        format!(
            "Handling access request to kTCCServiceDeveloperTool, from Sub:{{{subject}}}Resp:\
             {{TCCDProcess: identifier={subject}, pid=56408, auid=501, euid=501, \
             responsible_path=/Applications/aterm.app/Contents/MacOS/aterm, \
             binary_path={binary}}}, ReqResult(Auth Right: {answer}, promptType: 1,DB \
             Action:None, UpdateVerifierData)"
        )
    }

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

    fn answer_at(s: u32, subject: &str, answer: &str) -> String {
        record(
            &format!("2026-10-07 13:58:{s:02}.000000+0000"),
            &tccd_message(subject, "/private/tmp/x/build-script-build", answer),
        )
    }

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
        let other_field = |from: &str, to: &str| {
            let line = allowed(30);
            assert!(line.contains(from), "{from}");
            line.replace(from, to)
        };
        let untimed = {
            let line = answer_at(21, "com.apple.Terminal", "Allowed (User Set)");
            let stamp = "\"timestamp\":\"2026-10-07 13:58:21.000000+0000\",";
            assert!(line.contains(stamp), "{line}");
            line.replace(stamp, "")
        };
        let other_answer = |from: &str, to: &str| {
            let message = tccd_message(ATERM, "/private/tmp/x", "Allowed (User Set)");
            assert_eq!(message.matches(from).count(), 1, "{from}");
            record(
                "2026-10-07 13:58:21.000000+0000",
                &message.replace(from, to),
            )
        };
        let cases: Vec<(&str, ToolRun, DevToolProbe)> = vec![
            (
                "the captured record",
                log_of(&[CAPTURED_RECORD.to_string()]),
                devtool(S::Unlisted, L::Observed, Some((1, 1)), Some(CAPTURED_AT_MS)),
            ),
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
            (
                "its allowed, under a name that closes the Sub early",
                log_of(&[
                    unlisted(10),
                    answer_at(21, "com.aterm.aterm}x", "Allowed (User Set)"),
                ]),
                unrecognized(),
            ),
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
            (
                "a record tccd did not write, left out of the count",
                exited(
                    Some(0),
                    &format!(
                        "{fake_tccd}\n{}\n{{\"count\":1,\"finished\":1}}\n",
                        unlisted(10)
                    ),
                    "",
                ),
                unrecognized(),
            ),
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
        assert_eq!(
            classify_devtool_log(&log_of(&[unlisted(10)]), "com.aterm.aterm.dev").label,
            L::NoRecentRequest
        );
    }

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
        let real = record(
            "2026-10-07 13:58:30.000000+0000",
            &tccd_message(ATERM, "/private/tmp/x/plain", allowed),
        );
        assert_eq!(
            classify_devtool_log(&log_of(&[real]), ATERM).state,
            DevToolState::Allowed
        );
    }

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
            ("2026-10-00 00:00:00.000000+0000", None),
            ("2026-10-07-01 13:58:21.820241+0000", None),
            ("2026-+1-07 13:58:21.820241+0000", None),
            ("2026-10-07 13:58:21:00.820241+0000", None),
            ("2026-10-07 13:58:21.8a+0000", None),
            ("2026-10-07 13:58:21.820241+0060", None),
        ];
        for (text, want) in cases {
            assert_eq!(devtool_timestamp_ms(text), *want, "{text:?}");
        }
        let midnight = |y: i64, m: i64, d: i64| {
            devtool_timestamp_ms(&format!("{y}-{m:02}-{d:02} 00:00:00+0000"))
        };
        for y in [2000, 2023, 2024, 2100] {
            for m in 1..=12 {
                let last = (28..=31)
                    .rev()
                    .find(|&d| midnight(y, m, d).is_some())
                    .expect("every month has a 28th");
                let next = if m == 12 {
                    midnight(y + 1, 1, 1)
                } else {
                    midnight(y, m + 1, 1)
                };
                assert_eq!(
                    midnight(y, m, last).map(|ms| ms + 86_400_000),
                    next,
                    "{y}-{m:02}-{last}"
                );
            }
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

    #[test]
    fn the_ndjson_reader_is_strict() {
        let fields = json::object_line(
            r#"{"a":"x\/y\né😀","n":0,"m":12,"z":null,"t":true,"f":false,"o":{"p":[1,2.5,-3e2,4E+1,5e-1,[]]}}"#,
        )
        .expect("well formed");
        assert_eq!(json::str(&fields, "a"), Some("x/y\n\u{e9}\u{1F600}"));
        assert_eq!(json::uint(&fields, "n"), Some(0));
        assert_eq!(json::uint(&fields, "m"), Some(12));
        assert_eq!(json::uint(&fields, "a"), None, "a string is not a number");
        assert_eq!(json::str(&fields, "missing"), None);
        let u = |hex: &str| format!("{}u{hex}", '\u{5c}');
        let escaped = format!("{{\"a\":\"{}{}{}\"}}", u("00e9"), u("d83d"), u("de00"));
        let escaped = json::object_line(&escaped).expect("escapes");
        assert_eq!(json::str(&escaped, "a"), Some("\u{e9}\u{1F600}"));
        let unpaired = format!("{{\"a\":\"{}{}\"}}", u("d83d"), u("0041"));
        assert!(json::object_line(&unpaired).is_none(), "{unpaired}");
        for bad in [
            r#"{"a":"\u+0e9"}"#,
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
        let nested =
            |arrays: usize| format!("{{\"a\":{}1{}}}", "[".repeat(arrays), "]".repeat(arrays));
        assert!(json::object_line(&nested(15)).is_some());
        assert!(json::object_line(&nested(16)).is_none());
        for (text, want) in [("7", Some(7)), ("-7", None), ("7.0", None), ("7e0", None)] {
            let fields = json::object_line(&format!("{{\"n\":{text}}}")).expect("a number");
            assert_eq!(json::uint(&fields, "n"), want, "{text}");
        }
    }

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
        let deps = p("/private/tmp/x/target/debug/deps/aterm_containment-abc123");
        assert_eq!(
            probe_devtool_with(ProbeGate::on(), Some(&deps), ATERM, never_look, never_spawn),
            DevToolProbe::refused(DevToolProbeLabel::RefusedOutOfBundle)
        );
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
        let last = args
            .iter()
            .position(|a| a == "--last")
            .map(|i| args[i + 1].clone());
        assert_eq!(last, Some(format!("{}m", DEVTOOL_WINDOW_S / 60)));
        assert!(args.contains(&"ndjson".to_string()), "the escaped framing");
    }

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

    #[test]
    fn the_list_classifier_reads_only_the_subjects_bundle_row() {
        let unrecognized = DevToolList::refused(DevToolListLabel::FormatUnrecognized);
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
                "ours in another case is another id",
                vec![db_row(&ATERM.to_ascii_uppercase(), 0, 2, 1)],
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
            "612e62|0|2|1",
            "612E62|0|2|1|x",
            "612E62|+0|2|1",
            "612E62|4294967296|2|1",
            "612E62|0|2|1000000000000000000",
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

    #[test]
    fn the_list_probe_refuses_without_opening_or_spawning() {
        let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
        let tcc = Path::new(SYSTEM_TCC_DB);
        let never_open = |_: &Path| -> ProbeOutcome { panic!("a refusal opens nothing") };
        let never_spawn =
            |_: &str, _: &[&str]| -> ToolRun { panic!("a refusal must spawn nothing") };
        let off = ProbeGate {
            enabled: true,
            check: false,
        };
        let refused = |label| DevToolList::refused(label);
        assert_eq!(
            probe_devtool_list_with(off, Some(&bundled), ATERM, tcc, never_open, never_spawn),
            refused(DevToolListLabel::RefusedDisabled)
        );
        assert_eq!(
            probe_devtool_list_with(ProbeGate::on(), None, ATERM, tcc, never_open, never_spawn),
            refused(DevToolListLabel::RefusedNoExe)
        );
        assert_eq!(
            probe_devtool_list_with(
                ProbeGate::on(),
                Some(Path::new(
                    "/private/tmp/x/target/debug/deps/aterm_containment-abc"
                )),
                ATERM,
                tcc,
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
                tcc,
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
                    tcc,
                    |_| ProbeOutcome::Errno(errno),
                    never_spawn
                ),
                refused(label),
                "errno {errno}"
            );
        }
    }

    #[test]
    fn the_list_probe_opens_once_and_spawns_only_the_sqlite_argv() {
        let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
        let subject = "com.example.subject";
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("TCC.db");
        std::fs::write(&db, b"SQLite format 3\0").expect("db");
        let mut opened: Vec<PathBuf> = Vec::new();
        let mut seen: Vec<(String, Vec<String>)> = Vec::new();
        let got = probe_devtool_list_with(
            ProbeGate::on(),
            Some(&bundled),
            subject,
            &db,
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
        assert_eq!(opened, vec![db.clone()]);
        assert_eq!(
            SYSTEM_TCC_DB,
            "/Library/Application Support/com.apple.TCC/TCC.db"
        );
        assert_eq!(seen.len(), 1, "exactly one run");
        let (program, args) = &seen[0];
        assert_eq!(program, "/usr/bin/sqlite3");
        let target = aterm_uds::foreign_db::immutable_uri(&db);
        assert!(target.ends_with("/TCC.db?immutable=1"), "{target}");
        let expected: Vec<String> = devtool_db_args(&target)
            .into_iter()
            .map(str::to_string)
            .collect();
        assert_eq!(args, &expected);
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

    #[test]
    fn a_list_read_that_meets_a_write_is_read_again() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("TCC.db");
        std::fs::write(&db, b"SQLite format 3\0").expect("db");
        let subject = "com.example.subject";
        let write = || {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&db)
                .expect("open");
            std::io::Write::write_all(&mut f, b"x").expect("write");
        };
        let mut runs = 0;
        let got = read_devtool_db(&db, subject, |_, _| {
            runs += 1;
            write();
            db_of(&[db_row(subject, 0, 2, 9)])
        });
        assert_eq!(got, DevToolList::refused(DevToolListLabel::ToolFailed));
        assert_eq!(
            runs, DEVTOOL_DB_READS,
            "read again while each read met a write"
        );
        let mut runs = 0;
        let got = read_devtool_db(&db, subject, |_, _| {
            runs += 1;
            if runs == 1 {
                write();
            }
            db_of(&[db_row(subject, 0, 2, 9)])
        });
        assert_eq!(got, listed(DevToolListing::On, Some(9)));
        assert_eq!(runs, 2);
    }

    #[cfg(all(target_os = "macos", debug_assertions))]
    #[test]
    fn the_list_read_never_locks_out_a_writer_with_no_busy_timeout() {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let db = std::fs::canonicalize(dir.path())
            .expect("canonical")
            .join("TCC.db");
        let subject = "com.example.subject";
        let sqlite = |sql: &str| {
            aterm_uds::spawnfd::command(SQLITE_TOOL)
                .arg(&db)
                .arg(sql)
                .output()
                .expect("sqlite3 runs")
        };
        let made = sqlite(
            "CREATE TABLE access (service TEXT NOT NULL, client TEXT NOT NULL, \
             client_type INTEGER NOT NULL, auth_value INTEGER NOT NULL, csreq BLOB, \
             last_modified INTEGER NOT NULL DEFAULT 0); \
             INSERT INTO access VALUES ('kTCCServiceDeveloperTool', 'com.example.subject', \
             0, 0, NULL, 7); \
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 20000) \
             INSERT INTO access SELECT 'kTCCServiceAccessibility', 'com.example.app' || i, \
             0, 2, randomblob(1000), 0 FROM n;",
        );
        assert!(made.status.success(), "{made:?}");
        let bundled = p("/Applications/aterm.app/Contents/MacOS/aterm");
        let switch_on = "BEGIN; UPDATE access SET auth_value = 2, last_modified = 8 \
                         WHERE service = 'kTCCServiceDeveloperTool'; COMMIT;";
        let (read, caught) = aterm_uds::foreign_db::witness::while_read(
            &db,
            0x4000_0000,
            512,
            || {
                probe_devtool_list_with(
                    ProbeGate::on(),
                    Some(&bundled),
                    subject,
                    &db,
                    |_| ProbeOutcome::Ok,
                    |program, args| {
                        let args: Vec<&std::ffi::OsStr> =
                            args.iter().map(std::ffi::OsStr::new).collect();
                        run_bounded_exit(program, &args, DEVTOOL_DB_CEILING, TOOL_OUTPUT_MAX_BYTES)
                    },
                )
            },
            || sqlite(switch_on),
        );
        let caught = caught.map(|(pid, out)| {
            (
                pid,
                out.status,
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        });
        assert_eq!(caught, None, "the read held a lock a writer met");
        assert_eq!(read, listed(DevToolListing::Off, Some(7)));
        let wrote = sqlite(switch_on);
        assert!(wrote.status.success(), "{wrote:?}");
        let after = sqlite(
            "SELECT auth_value, last_modified FROM access \
             WHERE service = 'kTCCServiceDeveloperTool';",
        );
        assert_eq!(String::from_utf8_lossy(&after.stdout), "2|8\n");
    }

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
            (
                "a row not read, whatever its listing",
                DevToolList {
                    listing: DevToolListing::On,
                    ..DevToolList::refused(DevToolListLabel::ToolFailed)
                },
                quiet,
                Unknown,
            ),
            (
                "an absent listing not read",
                DevToolList {
                    listing: DevToolListing::Absent,
                    ..DevToolList::refused(DevToolListLabel::ToolFailed)
                },
                tccd(DevToolState::Allowed, 1),
                Granted,
            ),
            (
                "an answer not observed, whatever its state",
                unread,
                DevToolProbe {
                    state: DevToolState::Allowed,
                    ..quiet
                },
                Unknown,
            ),
            (
                "absent, though stamped before tccd allowed",
                listed(DevToolListing::Absent, Some(100)),
                tccd(DevToolState::Allowed, 100_001),
                NotGranted,
            ),
        ] {
            assert_eq!(devtool_grant(&list, &log), want, "{name}");
        }
    }

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
        match sh(
            &format!("head -c {TOOL_OUTPUT_MAX_BYTES} /dev/zero"),
            60_000,
        ) {
            ToolRun::Exited {
                code: Some(0),
                stdout,
                ..
            } => assert_eq!(
                u64::try_from(stdout.len()).ok(),
                Some(TOOL_OUTPUT_MAX_BYTES)
            ),
            other => panic!("a report at the cap: {other:?}"),
        }
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
        assert_eq!(
            run_bounded(
                "/bin/sh",
                &["-c".as_ref(), "exit 3".as_ref()],
                CODESIGN_CEILING
            ),
            Some((false, String::new(), String::new()))
        );

        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let gate = fifo_gate(dir.path());
        let _detector = gate_hang_detector(&gate);
        let armed = gate.clone();
        READERS_PENDING.with(|acts| {
            acts.borrow_mut()
                .push_back(Box::new(move |_| release_gate(&armed).unwrap()));
        });
        let run = run_bounded_exit(
            "/bin/sh",
            &[
                "-c".as_ref(),
                GATED_GRANDCHILD.as_ref(),
                "sh".as_ref(),
                gate.as_os_str(),
            ],
            HANG_DETECTOR,
            TOOL_OUTPUT_MAX_BYTES,
        );
        let _ = release_gate(&gate);
        assert_eq!(
            run,
            ToolRun::Exited {
                code: Some(0),
                stdout: "earlylate".to_string(),
                stderr: String::new(),
            }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_test_binary_is_not_hardened() {
        assert_eq!(self_hardening(), SelfHardening::NotHardened);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_membership_query_is_total() {
        let _ = debugger_membership();
        assert_eq!(imp::in_group("aterm-no-such-group-for-tests"), None);
        assert_eq!(
            imp::in_group("_www"),
            Some(false),
            "no account is the web server's"
        );
        // SAFETY: `getgid` has no preconditions.
        if unsafe { libc::getgid() } == 20 {
            assert_eq!(
                imp::in_group("staff"),
                Some(true),
                "the account's own group"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_unregistered_id_is_an_empty_answer_and_a_malformed_one_none() {
        assert_eq!(
            imp::registered_bundles("com.aterm.test.no-such-app"),
            Some(Vec::new())
        );
        assert_eq!(imp::registered_bundles(""), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn errno_constants_match_the_platform() {
        assert_eq!(ERRNO_EPERM, libc::EPERM);
        assert_eq!(ERRNO_ESRCH, libc::ESRCH);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_real_probes_refuse_from_a_test_binary() {
        let fda = probe_fda(ProbeGate::on());
        assert_eq!(
            (fda.state, fda.label),
            (FdaState::Unknown, ProbeLabel::RefusedOutOfBundle)
        );
        assert_eq!(
            probe_debugger(ProbeGate::on()),
            DebuggerProbe::refused(DebuggerProbeLabel::RefusedOutOfBundle)
        );
        assert_eq!(
            probe_devtool(ProbeGate::on(), ATERM),
            DevToolProbe::refused(DevToolProbeLabel::RefusedOutOfBundle)
        );
        assert_eq!(
            probe_devtool_list(ProbeGate::on(), ATERM),
            DevToolList::refused(DevToolListLabel::RefusedOutOfBundle)
        );
    }

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
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = aterm_tempfile::tempdir().expect("tempdir");
        let plain = scratch.path().join("tccutil");
        std::fs::write(&plain, b"").unwrap();
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
        let group_x = scratch.path().join("group-x");
        std::fs::write(&group_x, b"").unwrap();
        std::fs::set_permissions(&group_x, std::fs::Permissions::from_mode(0o610)).unwrap();
        for (path, want) in [
            (plain.clone(), TccutilPresence::NotExecutable),
            (group_x, TccutilPresence::Executable),
            (scratch.path().join("gone"), TccutilPresence::Missing),
            (plain.join("x"), TccutilPresence::Unknown),
        ] {
            assert_eq!(tccutil_presence_at(&path), want, "{}", path.display());
        }
    }
}
