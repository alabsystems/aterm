// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! macOS privacy-consent (TCC) and debugger-access posture probes and classifiers.

use std::path::{Component, Path, PathBuf};

/// `EPERM`. A TCC denial is `EPERM`, not `EACCES`.
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

/// The one line [`run_open_probe_verb`] writes to stderr, before it exits 1, when it is not
/// given one path on macOS.
pub const OPEN_PROBE_REFUSAL: &str = "aterm __tcc-open: one path, on macOS";

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
        Some(("errno", errno)) if !errno.starts_with('+') => {
            errno.parse().ok().map(ProbeOutcome::Errno)
        }
        _ => None,
    }
}

/// [`OPEN_PROBE_VERB`]: open the one path named, close it unread, and print [`open_probe_line`].
#[must_use = "return or inspect the command exit status"]
pub fn run_open_probe_verb(args: &[std::ffi::OsString]) -> std::process::ExitCode {
    #[cfg(target_os = "macos")]
    if let [db] = args {
        println!("{}", open_probe_line(imp::open_probe(Path::new(db))));
        return std::process::ExitCode::SUCCESS;
    }
    let _ = args;
    eprintln!("{OPEN_PROBE_REFUSAL}");
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

/// The directory UUID (`GeneratedUID`) of the account this process runs as, from
/// `mbr_uid_to_uuid(3)`: the key of what macOS records per account, such as each account's own
/// Local Network answers. Uppercase and hyphenated, as macOS writes it; `None` off macOS or when
/// the directory names none.
#[must_use]
pub fn user_uuid() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        imp::user_uuid()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
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

/// The `.app` directory `exe` lives inside, if any.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageAnchor {
    /// The image sits in a `.app` bundle.
    Live,
    /// The image sits in a bundle layout whose directory is no longer named `<something>.app`.
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
        Ok(pid) if pid > 0 && pid == self_pid => Responsible::SelfProcess,
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
    use std::io::Read as _;
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .ok()?
    };
    #[cfg(not(unix))]
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > INFO_PLIST_MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(INFO_PLIST_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > INFO_PLIST_MAX_BYTES {
        return None;
    }
    Some(bytes)
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
    identity_from_report(kept_report(root, codesign_report).as_deref(), bundle_id)
}

/// One bundle's signature report, read once: a reader that asks while another reads it waits.
type KeptReport = std::sync::Arc<std::sync::OnceLock<Option<String>>>;

/// The `codesign` reports this process read, by [`signature_key`]: `aterm doctor`'s privacy row
/// and its copy census both read the running bundle's, on two threads at once, and one spawn
/// answers both.
static REPORTS: std::sync::Mutex<Vec<(String, KeptReport)>> = std::sync::Mutex::new(Vec::new());

/// What a bundle's signature report depends on, read without spawning: the bundle's real path
/// and the identity (device, inode, mode, length, modification and status-change times) of its
/// `Info.plist`, its `_CodeSignature/CodeResources` and every file directly in `Contents/MacOS`.
/// A re-sign rewrites the seal and the executables, and a change of mode or owner (an
/// executable codesign can no longer read) moves the status-change time, so either changes the
/// key. `None` when the bundle's path cannot be resolved.
fn signature_key(root: &Path) -> Option<String> {
    use std::fmt::Write as _;
    let real = std::fs::canonicalize(root).ok()?;
    let contents = real.join("Contents");
    let mut files = vec![
        contents.join("Info.plist"),
        contents.join("_CodeSignature").join("CodeResources"),
    ];
    if let Ok(entries) = std::fs::read_dir(contents.join("MacOS")) {
        let mut exes: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        exes.sort();
        files.extend(exes);
    }
    let mut key = real.display().to_string();
    for file in files {
        let _ = write!(key, "\u{1f}{}", file.display());
        match std::fs::symlink_metadata(&file) {
            Ok(meta) => {
                let at = meta
                    .modified()
                    .ok()
                    .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |at| at.as_nanos());
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt as _;
                    let _ = write!(
                        key,
                        " {}:{} {:o} {}.{}",
                        meta.dev(),
                        meta.ino(),
                        meta.mode(),
                        meta.ctime(),
                        meta.ctime_nsec()
                    );
                }
                let _ = write!(key, " {} {at}", meta.len());
            }
            Err(_) => key.push_str(" absent"),
        }
    }
    Some(key)
}

/// `root`'s signature report through `read` ([`codesign_report`]), read once per
/// [`signature_key`] in this process; a bundle whose key cannot be read is read every time.
fn kept_report(root: &Path, read: impl FnOnce(&Path) -> Option<String>) -> Option<String> {
    let Some(key) = signature_key(root) else {
        return read(root);
    };
    let once = {
        let mut reports = REPORTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((_, once)) = reports.iter().find(|(k, _)| *k == key) {
            std::sync::Arc::clone(once)
        } else {
            if reports.len() >= 32 {
                reports.remove(0);
            }
            let once = KeptReport::default();
            reports.push((key, std::sync::Arc::clone(&once)));
            once
        }
    };
    once.get_or_init(|| read(root)).clone()
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
        codesign_ceiling(),
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

/// The bound a test set in place of `CODESIGN_CEILING`, in milliseconds (0: none set).
#[cfg(any(test, debug_assertions))]
static CODESIGN_CEILING_FOR_TEST_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Bound each `codesign` read by `ceiling` instead of the product's 10 s, for this process.
///
/// A test whose bundles `codesign` must read sets the hang detector here: a loaded machine
/// that outlasts the product's ceiling would otherwise read a signed bundle as unknown.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn set_codesign_ceiling_for_test(ceiling: std::time::Duration) {
    let ms = u64::try_from(ceiling.as_millis())
        .unwrap_or(u64::MAX)
        .max(1);
    CODESIGN_CEILING_FOR_TEST_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(target_os = "macos")]
fn codesign_ceiling() -> std::time::Duration {
    #[cfg(any(test, debug_assertions))]
    match CODESIGN_CEILING_FOR_TEST_MS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => {}
        ms => return std::time::Duration::from_millis(ms),
    }
    CODESIGN_CEILING
}

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
    /// Signed with a certificate or with an unreadable signature.
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
    /// The claimants whose requirement differs from the running copy's.
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

/// Move to the Trash each copy in `plan` that a fresh census still offers, that is still the same
/// bundle right before the move (`unchanged`), and that no process runs from.
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

    /// The display name.
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
        String::from(TCCUTIL_PATH),
        String::from("reset"),
        String::from(folder.tcc_service()),
        String::from(subject.as_str()),
    ]
}

/// The `tccutil` binary, absolute.
pub const TCCUTIL_PATH: &str = aterm_uds::os_path!("tccutil");

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

/// The one right `DevToolsSecurity` reads and writes.
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

/// The longest team identifier [`team_requirement`] quotes, in bytes (Apple's are ten).
pub const TEAM_MAX: usize = 32;

/// The code requirement a signature meets when its leaf certificate, under Apple's anchor, names
/// `team`: `anchor apple generic and certificate leaf[subject.OU] = "<team>"`. `None` for a team
/// that is not one to [`TEAM_MAX`] ASCII capitals and digits: no requirement quotes anything else.
#[must_use]
pub fn team_requirement(team: &str) -> Option<String> {
    let shaped = !team.is_empty()
        && team.len() <= TEAM_MAX
        && team
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    shaped.then(|| format!("anchor apple generic and certificate leaf[subject.OU] = \"{team}\""))
}

/// Whether the code process `pid` runs meets [`team_requirement`] for `team` now, as
/// `codesign --verify -R` checks running code: the kernel holds the process valid, and its
/// signature's certificate chain runs from a leaf naming `team` to Apple's anchor. An ad hoc or
/// linker signature has no certificate and meets none. `false` off macOS, for pid 0, for a team
/// no requirement quotes, and when `codesign` cannot say within its ceiling.
#[must_use]
pub fn process_meets_team(pid: u32, team: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        let Some(requirement) = team_requirement(team).filter(|_| pid > 0) else {
            return false;
        };
        let requirement = format!("-R={requirement}");
        let pid = pid.to_string();
        run_bounded(
            "/usr/bin/codesign",
            &["--verify".as_ref(), requirement.as_ref(), pid.as_ref()],
            codesign_ceiling(),
        )
        .is_some_and(|(met, _, _)| met)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, team);
        false
    }
}

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

/// Which way the owner's gesture changes the debugger rule.
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

macro_rules! devtool_window_minutes {
    ($mins:literal) => {
        /// How far back [`probe_devtool`] reads tccd's answers, in seconds: tccd answers as a new
        /// program starts, and the window's Developer Tools reading is never older than this
        /// (`DEVTOOLS_TTL`), so a longer read finds nothing a reader still needs.
        pub const DEVTOOL_WINDOW_S: u64 = $mins * 60;

        #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
        const DEVTOOL_LAST_ARG: &str = concat!($mins, "m");
    };
}

devtool_window_minutes!(2);

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
    DEVTOOL_LAST_ARG,
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
    /// Not read: the list holds the subject switched on, unchanged since this process
    /// started, and the verdict is left to the switch ([`devtool_grant`]); a row that no
    /// longer matches this copy goes unseen by such a run.
    NotNeeded,
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
            Self::NotNeeded => "not_needed",
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

const DEVTOOL_SERVICE: &str = "kTCCServiceDeveloperTool";

const MAX_TCC_MODIFIED_S: i64 = 1_000_000_000_000_000_000;

#[cfg(any(target_os = "macos", test))]
const DEVTOOL_DB_MAX_BYTES: u64 = 64 * 1024 * 1024;

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
    /// `open` failed with another errno, or the database disappeared before the read.
    OpenFailed,
    /// The database snapshot or `access` schema could not be read.
    ToolFailed,
    /// The `access` rows answered in a shape this classifier does not recognise.
    FormatUnrecognized,
    /// The read did not finish within its ceiling.
    TimedOut,
    /// A helper process could not be started.
    SpawnFailed,
    /// An answer was over the cap or unreadable.
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

struct AccessColumns {
    service: usize,
    client: usize,
    client_type: usize,
    auth_value: usize,
    last_modified: usize,
}

fn access_columns(schema: &aterm_uds::sqlite_read::TableSchema) -> Option<AccessColumns> {
    Some(AccessColumns {
        service: schema.column_index("service")?,
        client: schema.column_index("client")?,
        client_type: schema.column_index("client_type")?,
        auth_value: schema.column_index("auth_value")?,
        last_modified: schema.column_index("last_modified")?,
    })
}

fn decode_devtool_row(
    cols: &AccessColumns,
    row: &[aterm_uds::sqlite_read::SqlValue<'_>],
) -> Option<(String, u32, u32, i64)> {
    use aterm_uds::sqlite_read::SqlValue;
    let client = match row.get(cols.client)? {
        SqlValue::Text(s) => s.as_ref().to_owned(),
        SqlValue::Blob(b) => String::from_utf8_lossy(b).into_owned(),
        SqlValue::Null | SqlValue::Int(_) | SqlValue::Float(_) => return None,
    };
    let kind = u32::try_from(row.get(cols.client_type)?.as_int()?).ok()?;
    let auth = u32::try_from(row.get(cols.auth_value)?.as_int()?).ok()?;
    let modified = row
        .get(cols.last_modified)?
        .as_int()
        .filter(|&m| (0..MAX_TCC_MODIFIED_S).contains(&m))?;
    Some((client, kind, auth, modified))
}

fn classify_devtool_rows(
    scanned: &[Option<(String, u32, u32, i64)>],
    subject: &str,
) -> DevToolList {
    let unrecognized = DevToolList::refused(DevToolListLabel::FormatUnrecognized);
    let mut ours: Option<(u32, i64)> = None;
    let mut rows: u32 = 0;
    for entry in scanned {
        let Some((client, kind, auth, modified)) = entry else {
            return unrecognized;
        };
        rows = rows.saturating_add(1);
        if *kind != 0 || client != subject {
            continue;
        }
        if ours.replace((*auth, *modified)).is_some() {
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

/// Classify an in-memory SQLite `TCC.db` snapshot (`main` and optional `wal`) for `subject`.
///
/// Returns `None` when the snapshot is not a readable SQLite 3 database or lacks the `access`
/// table's Developer Tools columns.
#[must_use]
pub fn classify_devtool_db_bytes(
    main: &[u8],
    wal: Option<&[u8]>,
    subject: &str,
) -> Option<DevToolList> {
    let scanned = aterm_uds::sqlite_read::scan_table_with(
        main,
        wal,
        "access",
        access_columns,
        |cols, row| {
            if row.get(cols.service)?.as_str() != Some(DEVTOOL_SERVICE) {
                return None;
            }
            Some(decode_devtool_row(cols, row))
        },
    )?;
    Some(classify_devtool_rows(&scanned, subject))
}

#[cfg(any(target_os = "macos", test))]
const DEVTOOL_DB_READS: usize = 3;

#[cfg(any(target_os = "macos", test))]
const DEVTOOL_DB_PAUSE: std::time::Duration = std::time::Duration::from_millis(10);

/// The list probe with the executable, the database (production: [`SYSTEM_TCC_DB`]), and the
/// in-memory snapshot classifier.
#[cfg(any(target_os = "macos", test))]
pub fn probe_devtool_list_with<O, F>(
    gate: ProbeGate,
    exe: Option<&Path>,
    subject: &str,
    db: &Path,
    open_db: O,
    inspect: F,
) -> DevToolList
where
    O: FnOnce(&Path) -> ProbeOutcome,
    F: FnMut(&[u8], Option<&[u8]>) -> Option<DevToolList>,
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
        ProbeOutcome::Ok => read_devtool_db(db, inspect),
        ProbeOutcome::Errno(ERRNO_EPERM) => {
            DevToolList::refused(DevToolListLabel::NoFullDiskAccess)
        }
        ProbeOutcome::Errno(_) => DevToolList::refused(DevToolListLabel::OpenFailed),
    }
}

#[cfg(any(target_os = "macos", test))]
fn read_devtool_db<F>(db: &Path, mut inspect: F) -> DevToolList
where
    F: FnMut(&[u8], Option<&[u8]>) -> Option<DevToolList>,
{
    use aterm_uds::foreign_db::{Unread, read_bytes};
    for attempt in 0..DEVTOOL_DB_READS {
        if attempt > 0 {
            std::thread::sleep(DEVTOOL_DB_PAUSE);
        }
        match read_bytes(db, DEVTOOL_DB_MAX_BYTES, &mut inspect) {
            Ok(list) => return list,
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
            |main, wal| classify_devtool_db_bytes(main, wal, subject),
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

/// The `DevToolsSecurity` change child, as the `privacy-debugger-change` marker records it.
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
            .map_or_else(String::new, |bytes| {
                String::from_utf8_lossy(&bytes).into_owned()
            })
    };
    DevtoolsChangeRun::Exited {
        code: status.code(),
        stdout_head: head(out),
        stderr_head: head(err),
    }
}

/// Change the debugger rule for a yes: the owner's in aterm's alert, or System State's standing yes.
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

/// Which blocking measurements have been made.
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

    /// The running user's directory UUID, formatted as macOS writes it.
    pub(super) fn user_uuid() -> Option<String> {
        let mut user: Uuid = [0; 16];
        // SAFETY: `getuid` has no preconditions.
        let uid = unsafe { libc::getuid() };
        // SAFETY: `user` is a live 16-byte buffer, the whole of a `uuid_t`.
        if unsafe { mbr_uid_to_uuid(uid, user.as_mut_ptr()) } != 0 {
            return None;
        }
        Some(super::format_uuid(&user))
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
/// # Panics
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
#[path = "consent_tests.rs"]
mod tests;
