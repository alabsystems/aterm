// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Startup diagnostics: a private file logger for the `aterm_log` facade,
//! plus a crash-reporting panic hook.
//!
//! Without a logger installed every `aterm_log` record — including the
//! `containment_audit` security denials — is silently discarded. [`init`]
//! installs one writing to `~/Library/Logs/aterm/aterm.log` on macOS
//! (Console.app's convention) and `~/.local/state/aterm/logs/aterm.log`
//! elsewhere (XDG state): a `0600` file in a `0700` dir, same posture as the control
//! socket. The level comes from `$ATERM_LOG` (`off|error|warn|info|debug|
//! trace`), default `info`. Every line carries its time, the writing process's
//! pid and the level. The file is kept small by [`RotatingFile`]: past
//! [`aterm_log::MAX_LOG_BYTES`] it becomes `aterm.log.1` and a fresh one starts,
//! at startup and while running, so the two together stay near 8 MiB.
//!
//! CONTENT SAFETY: terminal cell text, scrollback, and keystrokes are never
//! passed to `aterm_log` anywhere in the tree (call sites log indices, error
//! displays, uids, modes, denied paths — metadata only). Defense in depth on
//! top of that: every record body is run through the engine-side
//! [`aterm_log::sanitize_record_for`] so caller-influenced text (e.g. a denied
//! control-socket path) cannot forge records or smuggle terminal escapes.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// `eprintln!` that CANNOT panic — for every stderr line the windowed app
/// writes at runtime.
///
/// `eprintln!` panics when the write fails ("failed printing to stderr"), and a
/// GUI app's stderr fails in ordinary ways: launched from a terminal that has
/// since closed (EIO on the pty), piped to a reader that went away (EPIPE —
/// Rust ignores SIGPIPE, so the write errors instead), or fd 2 closed outright.
/// Under aterm's `declare_class!` trampoline policy a panic inside any AppKit
/// callback is a process abort, and inside the PANIC HOOK below it is "thread
/// panicked while processing panic. aborting." — which turns a survivable
/// worker-thread panic into a crash of the whole terminal. The 2026-09-02
/// abort audit routed 243 sites in this crate through this macro (and 124 in
/// aterm-gpu through its twin); only cli.rs and the *_conformance.rs harness
/// lanes keep a bare `eprintln!`. It
/// writes and drops the result, exactly what `aterm_objc::abort_on_unwind`
/// already does for the same reason.
macro_rules! stderr_line {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stderr().lock(), $($arg)*);
    }};
}
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
pub(crate) use stderr_line;

use aterm_log::{LevelFilter, Log, Metadata, Record};

/// Install the panic hook and file logger, level from `$ATERM_LOG`.
///
/// Called first thing in `main`, before any thread spawns. Failures are
/// non-fatal (the terminal must still come up): they leave the facade in its
/// discard-everything default and say why on stderr.
pub fn init() {
    let Some(dir) = log_dir() else {
        crate::logging::stderr_line!(
            "aterm-gui: no private log dir (set HOME); logging + crash reports disabled"
        );
        return;
    };
    // Crash reporting is independent of $ATERM_LOG — panics are always worth
    // an artifact, even with routine logging off. Arm BOTH crash paths: the Rust
    // panic hook (unwinds) and the async-signal-safe fatal-signal handler
    // (SIGSEGV/SIGABRT/… which bypass the panic machinery entirely, M6 CRASH-CORE).
    install_panic_hook(dir.clone());
    crate::crash_signal::install_signal_handlers();
    if let Err((path, e)) = install_file_logger(&dir) {
        crate::logging::stderr_line!(
            "aterm-gui: cannot open {}: {e}; logging disabled",
            path.display()
        );
    }
}

/// The file logger alone, for the terminal session (`aterm --session`): its threads run
/// beside a live shell, so what they have to say goes to `aterm.log`, never stderr. No
/// panic hook and no crash-signal handlers — the session's crash story is unchanged — and
/// silent when the log cannot be opened: the session's only other surface is that shell.
pub fn init_session() {
    if let Some(dir) = log_dir() {
        let _ = install_file_logger(&dir);
    }
}

/// Install the `aterm.log` logger in `dir` at `$ATERM_LOG`'s level (default `info`;
/// `off` installs nothing). `Err` names the file that could not be opened.
fn install_file_logger(dir: &Path) -> Result<(), (PathBuf, std::io::Error)> {
    let level = std::env::var("ATERM_LOG")
        .ok()
        .and_then(|s| LevelFilter::parse(&s))
        .unwrap_or(LevelFilter::Info);
    if level == LevelFilter::Off {
        return Ok(());
    }
    let path = dir.join("aterm.log");
    let file =
        RotatingFile::open(path.clone(), RotationBudget::ATERM_LOG).map_err(|e| (path, e))?;
    let logger: &'static FileLogger = Box::leak(Box::new(FileLogger {
        file: Mutex::new(file),
    }));
    if aterm_log::set_logger(logger).is_ok() {
        aterm_log::set_max_level(level);
    }
    Ok(())
}

/// Route `aterm_objc`'s exception-containment reports into `aterm.log`.
///
/// A containment is an `NSException` AppKit raised inside a declared
/// Objective-C method (or a block, a dispatch trampoline, the `sendEvent:`
/// override) that `aterm_objc::exception` caught instead of letting it abort
/// the process. Each one is an ERROR record naming the method, the
/// exception's class, name and reason, and its call stack on one line —
/// and, because the file logger may be off (`ATERM_LOG=off`) or not yet up,
/// the same line still goes to stderr through the crate's default sink.
///
/// Installed right after [`init`], before any window exists, so the first
/// containment of the process is on record. [`crate::App`] reads the count
/// every turn and treats a change as an event (a redraw, the modifier state
/// re-read).
#[cfg(target_os = "macos")]
pub fn install_objc_containment_sink() {
    aterm_objc::exception::set_sink(objc_containment_sink);
}

#[cfg(target_os = "macos")]
fn objc_containment_sink(e: &aterm_objc::ContainedException<'_>) {
    aterm_log::error!(
        "objc: contained {} ({}) in Objective-C method `{}` (containment #{}): {} | stack: {}",
        e.name,
        e.class,
        e.method,
        e.ordinal,
        e.reason,
        e.call_stack
    );
    aterm_objc::exception::default_sink(e);
}

/// Route panics to `crash-<pid>.log` next to the main log: panic message +
/// backtrace + version + timestamp. The default hook only writes stderr,
/// which nobody sees for a windowed app — the crash file is the artifact
/// that survives the window vanishing. Chains to the previous (default)
/// hook and returns, so the unwind itself proceeds unchanged.
///
/// [`file_panic_report`] decides WHICH surface a given panic belongs on: not every
/// panic in this process is a crash of aterm, and one of them is an accessibility
/// outage that the app survives.
fn install_panic_hook(dir: PathBuf) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // The report path is the routing's testable answer; the hook has no further
        // use for it once the artifact is on disk.
        let _report = file_panic_report(
            &dir,
            info.location().map_or("", |l| l.file()),
            std::thread::current().name(),
            panic_payload(info),
            info,
            &std::backtrace::Backtrace::force_capture(),
        );
        prev(info);
    }));
}

/// The panic message as text, for the surfaces that show a sentence rather than the
/// whole `PanicHookInfo` rendering. Both payload shapes the standard library produces
/// (`panic!("literal")` and a formatted `String`) are handled; anything else is a
/// third-party payload type nothing can render, and keeps the neutral word.
fn panic_payload<'a>(info: &'a std::panic::PanicHookInfo<'_>) -> &'a str {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("panicked")
}

/// File one panic on the surface it belongs to, and answer with the crash report's
/// path when it was filed as a crash (`None` when it went to the accessibility
/// surface instead). The panic's parts arrive as arguments so the routing is exercised
/// without unwinding a real thread.
///
/// THE ACCESSIBILITY PUBLISHER'S BACKGROUND THREAD IS THE ONE PANIC THAT IS NOT A
/// CRASH OF ATERM: the process keeps running and every other feature works, so the
/// only thing lost is the tree a screen reader reads (see [`crate::a11y_backend`]).
/// Filing it as `crash-<pid>.log` made the NEXT launch banner "aterm closed
/// unexpectedly last time" for a run that never closed — a false alarm on every single
/// launch, which is exactly how a true one stops being believed. It is reported as the
/// accessibility failure it is instead; the caller still chains to the default hook, so
/// nothing about the panic is swallowed.
fn file_panic_report(
    dir: &Path,
    location_file: &str,
    thread_name: Option<&str>,
    payload: &str,
    info: &dyn std::fmt::Display,
    backtrace: &dyn std::fmt::Display,
) -> Option<PathBuf> {
    if is_accessibility_backend_panic(location_file, thread_name) {
        report_accessibility_failure(payload, location_file);
        return None;
    }
    // THE SUPERVISOR HOST'S THREADS are the other panic that is not a crash:
    // each run is caught, restarted within its budget, and at worst the one
    // session's supervisor goes off (faulted) — the terminal keeps running.
    // Filed as a harness fault record, never as `crash-<pid>.log` (the next
    // launch would banner a crash that never happened).
    if is_harness_thread(thread_name) {
        let path = dir.join(format!("harness-fault-{}.log", std::process::id()));
        let _ = write_harness_fault(&path, thread_name.unwrap_or(""), info, backtrace);
        crate::logging::stderr_line!(
            "aterm-gui: supervisor fault on {} — recorded at {}",
            thread_name.unwrap_or("?"),
            path.display()
        );
        return None;
    }
    // Allocation-light: the small path string and the captured backtrace
    // are the only buffers; the report streams straight to the fd.
    let path = dir.join(format!("crash-{}.log", std::process::id()));
    let _ = write_crash_report(&path, info, backtrace);
    crate::logging::stderr_line!("aterm-gui: panic — crash report at {}", path.display());
    Some(path)
}

/// [`crate::a11y_backend::is_backend_panic`] where an accessibility tree is compiled
/// in. A build without one has no publisher to lose, so every panic is aterm's.
#[cfg(a11y_tree)]
fn is_accessibility_backend_panic(location_file: &str, thread_name: Option<&str>) -> bool {
    crate::a11y_backend::is_backend_panic(location_file, thread_name)
}

#[cfg(not(a11y_tree))]
fn is_accessibility_backend_panic(_location_file: &str, _thread_name: Option<&str>) -> bool {
    false
}

/// Twin of [`is_accessibility_backend_panic`]: the report itself, unreachable in a
/// build with no accessibility tree.
#[cfg(a11y_tree)]
fn report_accessibility_failure(payload: &str, location_file: &str) {
    crate::a11y_backend::report_failure(
        crate::a11y_backend::reason_from_payload(payload),
        location_file,
    );
}

#[cfg(not(a11y_tree))]
fn report_accessibility_failure(_payload: &str, _location_file: &str) {}

/// Whether `thread_name` is one of the supervisor host's threads
/// (`harness_host::THREAD_PREFIX`).
fn is_harness_thread(thread_name: Option<&str>) -> bool {
    thread_name.is_some_and(|n| n.starts_with(crate::harness_host::THREAD_PREFIX))
}

/// Append one harness fault to its `0600` record: every fault of this pid in
/// one file, each with its time, thread, panic and backtrace.
fn write_harness_fault(
    path: &Path,
    thread: &str,
    info: &dyn std::fmt::Display,
    backtrace: &dyn std::fmt::Display,
) -> std::io::Result<()> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let mut f = open_append_0600(path)?;
    writeln!(
        f,
        "aterm-gui {} supervisor fault on {thread} at unix {}.{:03}\n{info}\n\nbacktrace:\n{backtrace}\n",
        aterm_types::version::APP_VERSION,
        ts.as_secs(),
        ts.subsec_millis(),
    )
}

/// Write one `0600` crash report, truncating any prior report from this pid.
fn write_crash_report(
    path: &Path,
    info: &dyn std::fmt::Display,
    backtrace: &dyn std::fmt::Display,
) -> std::io::Result<()> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let mut f = create_0600(path)?;
    writeln!(
        f,
        "aterm-gui {} crashed at unix {}.{:03}\n{info}\n\nbacktrace:\n{backtrace}",
        aterm_types::version::APP_VERSION,
        ts.as_secs(),
        ts.subsec_millis(),
    )
}

/// One-shot startup scan for evidence that the PREVIOUS run died without a clean
/// exit, returning what was found ([`CrashEvidence`]: the report and its
/// opening lines, for the crash message) — or `None` when the last run
/// ended normally. Both crash artifacts live in [`log_dir`] and, until this scan
/// existed, NOTHING ever read either of them, so a crash was completely silent
/// on the next launch (on a GUI-subsystem Explorer launch stderr is null, so the
/// file really is the only trace):
///
///   * `crash-<pid>.log` — the panic hook's report ([`install_panic_hook`]);
///   * `crash-signal-<pid>-<nanos>.log` — a NON-EMPTY fatal-signal/-exception
///     marker (`crate::crash_signal`). Empty ones are clean-run leftovers — the
///     current launch's own freshly-created marker included — which is why the
///     `len() == 0` skip below is correct even though this runs AFTER
///     `install_signal_handlers` created ours (and after its sweep, which only
///     ever removes those same empty markers — ordering against it is moot).
///
/// WHAT IS NOT HERE: a dead accessibility publisher. Its background thread panics
/// without ending the process, so a report filed under `crash-<pid>.log` would have
/// this scan announce "aterm closed unexpectedly" for a run that never closed —
/// every launch, for as long as the accessibility bus stays unreachable.
/// [`file_panic_report`] routes that one class to [`crate::a11y_backend`] instead,
/// which is why nothing in this scan needs to know about it.
///
/// CONSUMING, so the message posts exactly once: every artifact found is renamed
/// to `<name>.seen` — renamed, never deleted, because the message points the user
/// AT the file (`Open log`) and the record must outlive the row's hold. A failed
/// rename keeps the original name and honestly re-posts next launch rather than
/// losing the report. The evidence names the NEWEST artifact (mtime), with its
/// head read in the same pass; older ones are consumed silently. Consumed
/// reports are kept up to [`KEEP_SEEN_CRASH_REPORTS`], newest first, so they
/// cannot pile up without limit; the one the message names is never deleted.
pub(crate) fn take_crash_evidence() -> Option<CrashEvidence> {
    take_crash_evidence_in(&log_dir()?)
}

/// What the previous run left behind: the artifact the crash message names
/// and the first lines of it, so the row can say WHAT happened before anyone
/// opens the file (`message_reporters::crash_message`, design R2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CrashEvidence {
    /// The artifact, absolute, under its consumed (`.seen`) name when the
    /// rename succeeded.
    pub(crate) path: PathBuf,
    /// The artifact's first non-empty lines: at most [`CRASH_HEAD_LINES`],
    /// from at most [`CRASH_HEAD_BYTES`] of the file.
    pub(crate) head: Vec<String>,
}

impl CrashEvidence {
    /// The console's line — the sentence the banner carried, kept verbatim so
    /// a console launch reads what it always read.
    pub(crate) fn sentence(&self) -> String {
        format!(
            "aterm closed unexpectedly last time \u{2014} crash log at {}",
            self.path.display()
        )
    }
}

/// The most lines of the artifact the crash message carries.
pub(crate) const CRASH_HEAD_LINES: usize = 8;

/// The most bytes of the artifact read for those lines: a report is a
/// version line, the panic and a backtrace, and the first 1.5 KiB holds what
/// says why; the read is BOUNDED (`Read::take`) because a file in the log
/// dir is not a file this process wrote — see the urandom incident.
pub(crate) const CRASH_HEAD_BYTES: u64 = 1536;

/// The first [`CRASH_HEAD_LINES`] non-empty lines of `path`, from its first
/// [`CRASH_HEAD_BYTES`]; empty when it cannot be read. A last line the byte
/// cap cut mid-way is kept as it is: this is an excerpt, and the message's
/// own line cap trims it again.
fn crash_head(path: &Path) -> Vec<String> {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut buf = Vec::new();
    if file.take(CRASH_HEAD_BYTES).read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    String::from_utf8_lossy(&buf)
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .take(CRASH_HEAD_LINES)
        .map(str::to_owned)
        .collect()
}

/// How many consumed (`.seen`) crash reports [`take_crash_evidence`] keeps.
const KEEP_SEEN_CRASH_REPORTS: usize = 8;

/// [`take_crash_evidence`] against an explicit directory (unit-testable).
fn take_crash_evidence_in(dir: &Path) -> Option<CrashEvidence> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // Matches BOTH artifact families ("crash-signal-…" shares the prefix);
        // `aterm.log` and already-consumed "….log.seen" files do not match.
        if !(name.starts_with("crash-") && name.ends_with(".log")) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() == 0 {
            continue; // a clean-run signal marker, not crash evidence
        }
        let path = entry.path();
        let seen = path.with_file_name(format!("{name}.seen"));
        let path = if std::fs::rename(&path, &seen).is_ok() {
            seen
        } else {
            path
        };
        let modified = meta.modified().unwrap_or(UNIX_EPOCH);
        if newest.as_ref().is_none_or(|(t, _)| modified >= *t) {
            newest = Some((modified, path));
        }
    }
    prune_seen_crash_reports(dir, newest.as_ref().map(|(_, path)| path.as_path()));
    let (_, path) = newest?;
    let head = crash_head(&path);
    Some(CrashEvidence { path, head })
}

/// Delete consumed crash reports (`crash-….log.seen`) beyond the newest
/// [`KEEP_SEEN_CRASH_REPORTS`] by mtime. `named` — the report the message is about
/// to point at — is never deleted, whatever its mtime says.
fn prune_seen_crash_reports(dir: &Path, named: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut seen: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            if !(name.starts_with("crash-") && name.ends_with(".log.seen")) {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().unwrap_or(UNIX_EPOCH);
            Some((modified, entry.path()))
        })
        .collect();
    seen.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in seen.into_iter().skip(KEEP_SEEN_CRASH_REPORTS) {
        if Some(path.as_path()) != named {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Resolve the per-user log dir, created `0700` (owner-only, like the
/// control-socket dir — denial records name what a program attempted). WHERE is
/// [`aterm_types::dirs::logs_dir`]'s one rule — `~/Library/Logs/aterm` on macOS
/// (Console.app's convention), the XDG state dir's `aterm/logs` elsewhere — which
/// atpkg's `packages.log` follows too, so the two logs sit side by side.
#[cfg(unix)]
pub(crate) fn log_dir() -> Option<PathBuf> {
    let dir = aterm_types::dirs::logs_dir()?;
    crate::control_auth::ensure_private_dir(&dir).ok()?;
    Some(dir)
}

/// Windows: `%LOCALAPPDATA%\aterm\logs` ([`aterm_types::dirs::logs_dir`]) — the same
/// per-user base dir the socket seam uses — falling back through the home dir when
/// `LOCALAPPDATA` is unset. The per-user profile dir's default owner-only ACLs are the
/// confidentiality boundary here; POSIX `0700` semantics do not apply.
#[cfg(windows)]
pub(crate) fn log_dir() -> Option<PathBuf> {
    let dir = aterm_types::dirs::logs_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// How big a log file may grow, and how often each writer looks at it on disk.
///
/// Between two looks a writer appends to the file it has open, so the check
/// cadence bounds how far past `rotate_at` one writer can run (`check_bytes`)
/// and how long it can keep writing into a file another process has already
/// rotated away (`check_every`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct RotationBudget {
    /// A file larger than this is rotated to `<name>.1`.
    pub(crate) rotate_at: u64,
    /// Look again once this many bytes have been written since the last look.
    pub(crate) check_bytes: u64,
    /// …or once this long has passed since it.
    pub(crate) check_every: Duration,
}

impl RotationBudget {
    /// `aterm.log`: [`aterm_log::MAX_LOG_BYTES`] per file, looked at every
    /// 64 KiB or 30 s. While the log fills more slowly than one file per 30 s,
    /// no writer falls two files behind, so no line is written into a deleted
    /// copy (`aterm_spec::derive::log_rotation_model` proves it).
    pub(crate) const ATERM_LOG: Self = Self {
        rotate_at: aterm_log::MAX_LOG_BYTES,
        check_bytes: 64 * 1024,
        check_every: Duration::from_secs(30),
    };

    fn oversized(self, len: u64) -> bool {
        len > self.rotate_at
    }
}

/// `(device, inode)` of an open file or a path: how a writer tells that the
/// path now names a different file than the one it holds. `None` where the
/// platform cannot say, which makes every look reopen the path.
type FileId = Option<(u64, u64)>;

#[cfg(unix)]
fn file_id(meta: &std::fs::Metadata) -> FileId {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(_meta: &std::fs::Metadata) -> FileId {
    None
}

/// The one older copy of `path` rotation keeps: `aterm.log` → `aterm.log.1`.
pub(crate) fn rotated_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// The sibling lock file that makes one rotation at a time across processes:
/// `aterm.log` → `aterm.log.lock`. It stays on disk, empty, `0600`.
fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// What [`rotate_if_oversized`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rotation {
    /// `path` was renamed to [`rotated_path`]; the caller opens a fresh one.
    Rotated,
    /// `path` names a different file than the caller's: another process
    /// rotated it first, so the caller reopens `path` and writes there.
    Moved,
    /// Nothing to do: the file is within the budget, is missing, or another
    /// process is rotating it right now.
    Kept,
}

/// Rotate `path` to [`rotated_path`] (replacing the older copy) when it is
/// larger than `rotate_at` — and, when `ours` names a file, only while `path`
/// is still that file.
///
/// Every aterm process that logs appends to the same file, so the rename
/// happens under the sibling [`lock_path`], taken WITHOUT waiting: loggers run
/// on the main thread, and a busy lock means another process is rotating, so
/// this one skips and the next look tries again. (A child being spawned while
/// the lock was held also keeps it until it execs; that too costs only the
/// retry.) Under the lock the file is looked at again, because another writer
/// may have rotated it between the caller's look and the lock. Nothing here
/// syncs to disk.
pub(crate) fn rotate_if_oversized(path: &Path, rotate_at: u64, ours: FileId) -> Rotation {
    // The ascription is the lock-order census's evidence that this is a
    // cross-process file lock, not an in-process mutex.
    let lock: std::fs::File = match open_append_0600(&lock_path(path)) {
        Ok(file) => file,
        Err(_) => return Rotation::Kept,
    };
    if lock.try_lock().is_err() {
        return Rotation::Kept;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return Rotation::Kept;
    };
    if ours.is_some() && file_id(&meta) != ours {
        return Rotation::Moved;
    }
    if meta.len() <= rotate_at {
        return Rotation::Kept;
    }
    match std::fs::rename(path, rotated_path(path)) {
        Ok(()) => Rotation::Rotated,
        Err(_) => Rotation::Kept,
    }
    // `lock` drops here, after the rename: that is the release.
}

/// An append-only log file that rotates itself. Opened with `O_APPEND`, so
/// every aterm process can write the same file without overwriting another's
/// lines; each append is ONE write. Before an append, when a look is due (see
/// [`RotationBudget`]), the writer follows another process's rotation or
/// rotates the file itself once it is too big.
pub(crate) struct RotatingFile {
    path: PathBuf,
    budget: RotationBudget,
    file: File,
    id: FileId,
    since_check: u64,
    last_check: Instant,
    /// This writer started a fresh file and has not yet said so in it.
    rotated: bool,
}

impl RotatingFile {
    /// Open `path` for appending, `0600`, rotating it first if it is already
    /// over the budget (an older copy is kept; nothing is truncated).
    pub(crate) fn open(path: PathBuf, budget: RotationBudget) -> std::io::Result<Self> {
        let rotated = std::fs::metadata(&path).is_ok_and(|m| budget.oversized(m.len()))
            && rotate_if_oversized(&path, budget.rotate_at, None) == Rotation::Rotated;
        let file = open_append_0600(&path)?;
        let id = file.metadata().ok().and_then(|m| file_id(&m));
        Ok(Self {
            path,
            budget,
            file,
            id,
            since_check: 0,
            last_check: Instant::now(),
            rotated,
        })
    }

    /// Append `line` in ONE write. When a look at the file is due it comes
    /// first; and when this writer has just started a fresh file (at open or
    /// in that look), `note` is written ahead of `line` to say where the older
    /// lines went — it is given the older copy's path.
    pub(crate) fn append(&mut self, line: &[u8], note: impl FnOnce(&Path) -> String) {
        if self.check_due() {
            self.check();
        }
        if std::mem::take(&mut self.rotated) {
            self.write(note(&rotated_path(&self.path)).as_bytes());
        }
        self.write(line);
    }

    fn write(&mut self, bytes: &[u8]) {
        let _ = self.file.write_all(bytes);
        self.since_check = self.since_check.saturating_add(bytes.len() as u64);
    }

    fn check_due(&self) -> bool {
        self.since_check >= self.budget.check_bytes
            || self.last_check.elapsed() >= self.budget.check_every
    }

    /// Look at the file on disk: follow another process's rotation, then rotate
    /// the file this writer now holds if it has outgrown the budget.
    fn check(&mut self) {
        self.since_check = 0;
        self.last_check = Instant::now();
        let on_disk = std::fs::metadata(&self.path).ok().and_then(|m| file_id(&m));
        if on_disk.is_none() || on_disk != self.id {
            // Rotated by another process, or deleted: write where the path
            // points now.
            self.reopen();
        }
        if !self
            .file
            .metadata()
            .is_ok_and(|m| self.budget.oversized(m.len()))
        {
            return;
        }
        match rotate_if_oversized(&self.path, self.budget.rotate_at, self.id) {
            Rotation::Rotated => {
                self.reopen();
                self.rotated = true;
            }
            Rotation::Moved => self.reopen(),
            Rotation::Kept => {}
        }
    }

    /// Reopen `path`. On failure the old handle stays: a line in the older
    /// copy beats a line nowhere.
    fn reopen(&mut self) {
        if let Ok(file) = open_append_0600(&self.path) {
            self.id = file.metadata().ok().and_then(|m| file_id(&m));
            self.file = file;
        }
    }

    fn flush(&mut self) {
        let _ = self.file.flush();
    }
}

/// Open `path` at mode `0600` for appending (`O_APPEND`: every write lands at
/// the end, whoever else has it open). Mirrors `snapshot_path::write_private`:
/// restrictive perms BEFORE content lands, and forced even when the file
/// pre-existed (`OpenOptions::mode` only applies on creation).
#[cfg(unix)]
pub(crate) fn open_append_0600(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(f)
}

/// Non-unix twin of `open_append_0600`: plain create+append. There are no
/// POSIX mode bits here — the log lives in the per-user dir under
/// `%LOCALAPPDATA%`, whose default owner-only ACLs are the confidentiality
/// boundary.
#[cfg(not(unix))]
pub(crate) fn open_append_0600(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

/// Create (or replace) `path` at mode `0600` — a crash report, never a log,
/// which is always appended to.
#[cfg(unix)]
fn create_0600(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(f)
}

/// Non-unix twin of `create_0600` (see `open_append_0600` on permissions).
#[cfg(not(unix))]
fn create_0600(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

/// Writes one sanitized line per record. Idle cost is zero: `aterm_log`
/// gates on the max-level atomic before any record reaches us.
struct FileLogger {
    file: Mutex<RotatingFile>,
}

impl FileLogger {
    /// Format one record and append it: the whole of [`Log::log`] but the
    /// `Record`, which only `aterm_log` can build.
    fn write_record(&self, level: aterm_log::Level, target: &str, body: &str) {
        let epoch_ms = epoch_ms_now();
        let pid = std::process::id();
        let line = format_record(epoch_ms, pid, level, target, body);
        let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
        f.append(line.as_bytes(), |older| {
            let older = older.file_name().map_or_else(
                || older.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            format_record(
                epoch_ms,
                pid,
                aterm_log::Level::Info,
                module_path!(),
                &format!("log rotated; older lines in {older}"),
            )
        });
    }
}

impl Log for FileLogger {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true // level gating already happened against the max-level atomic
    }

    fn log(&self, record: &Record<'_>) {
        // Single write per record: `File` is unbuffered, so the line is
        // already durable — no interleaved fragments, nothing lost on crash.
        self.write_record(record.level(), record.target(), &record.args().to_string());
    }

    fn flush(&self) {
        self.file.lock().unwrap_or_else(|e| e.into_inner()).flush();
    }
}

fn epoch_ms_now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// One log line: epoch-millis timestamp (UTC), the writing process's pid (the
/// GUI, every `aterm --session` and an update's successor share this file),
/// level, target, sanitized body — `1790191963.664 2061 INFO aterm_gui: …`.
fn format_record(
    epoch_ms: u128,
    pid: u32,
    level: aterm_log::Level,
    target: &str,
    body: &str,
) -> String {
    format!(
        "{}.{:03} {} {} {}: {}\n",
        epoch_ms / 1000,
        epoch_ms % 1000,
        pid,
        level,
        target,
        aterm_log::sanitize_record_for(level, body)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_auth::ensure_private_dir;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// Serializes the tests that rotate, here and in `rotation_conformance`
    /// (which spawns `ty`): a process forked while a lock file is open holds
    /// that lock until it execs, which would turn a rotation a test expects
    /// into a skip.
    pub(super) fn serial() -> std::sync::MutexGuard<'static, ()> {
        static ROTATION_TESTS: Mutex<()> = Mutex::new(());
        ROTATION_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A private scratch dir per test, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("aterm-log-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            ensure_private_dir(&dir).unwrap();
            Self(dir)
        }

        fn log(&self) -> PathBuf {
            self.0.join("aterm.log")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A budget small enough to rotate in a test: `rotate_at` bytes per file,
    /// a look before every write after the first.
    fn tiny(rotate_at: u64) -> RotationBudget {
        RotationBudget {
            rotate_at,
            check_bytes: 1,
            check_every: Duration::from_secs(30),
        }
    }

    fn note(older: &Path) -> String {
        format!("NOTE {}\n", older.file_name().unwrap().to_string_lossy())
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    // The mode-asserting tests are POSIX-only; append/rotation behavior is
    // covered on every platform below.
    #[cfg(unix)]
    #[test]
    fn log_file_is_0600_and_appends_across_opens() {
        let dir = Scratch::new("app");
        let path = dir.log();
        RotatingFile::open(path.clone(), RotationBudget::ATERM_LOG)
            .unwrap()
            .append(b"first\n", note);
        RotatingFile::open(path.clone(), RotationBudget::ATERM_LOG)
            .unwrap()
            .append(b"second\n", note);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(read(&path), "first\nsecond\n");
    }

    /// The `aterm.log` budget is the engine's policy, not a second copy of it.
    #[test]
    fn the_aterm_log_budget_rotates_where_the_engine_says() {
        let budget = RotationBudget::ATERM_LOG;
        for len in [0, aterm_log::MAX_LOG_BYTES, aterm_log::MAX_LOG_BYTES + 1] {
            assert_eq!(
                budget.oversized(len),
                aterm_log::should_rotate(len),
                "{len}"
            );
        }
    }

    /// An oversized file at startup is ROTATED, never truncated: the lead-up to
    /// a crash is exactly what the relaunch after it needs, and another process
    /// may still be appending to it.
    #[test]
    fn oversized_log_rotates_on_open_and_keeps_the_old_lines() {
        let _serial = serial();

        let dir = Scratch::new("rot");
        let path = dir.log();
        open_append_0600(&path)
            .unwrap()
            .write_all(b"before the crash\n")
            .unwrap();
        let mut f = RotatingFile::open(path.clone(), tiny(4)).unwrap();
        assert_eq!(read(&rotated_path(&path)), "before the crash\n");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        f.append(b"after\n", note);
        assert_eq!(read(&path), "NOTE aterm.log.1\nafter\n");
        #[cfg(unix)]
        for p in [rotated_path(&path), lock_path(&path)] {
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", p.display());
        }
    }

    /// While running, a writer that fills the file rotates it itself, and only
    /// ONE older copy is ever kept.
    #[test]
    fn rotates_while_running_and_keeps_one_older_copy() {
        let _serial = serial();

        let dir = Scratch::new("run");
        let path = dir.log();
        let mut f = RotatingFile::open(path.clone(), tiny(40)).unwrap();
        for i in 0..40 {
            f.append(format!("line {i:02}\n").as_bytes(), note);
        }
        let mut names: Vec<String> = std::fs::read_dir(&dir.0)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["aterm.log", "aterm.log.1", "aterm.log.lock"]);
        // A look comes before every write after the first here, so a file ends
        // one line past the cap at most.
        for p in [&path, &rotated_path(&path)] {
            let len = std::fs::metadata(p).unwrap().len();
            assert!(len <= 40 + 8, "{} is {len} bytes", p.display());
        }
        assert!(read(&path).ends_with("line 39\n"));
        assert!(read(&path).starts_with("NOTE aterm.log.1\n"));
    }

    /// A second writer on the same path follows the first one's rotation at
    /// its next look instead of writing into the older copy for ever.
    #[test]
    fn a_second_writer_follows_the_rotation() {
        let _serial = serial();

        let dir = Scratch::new("follow");
        let path = dir.log();
        let mut a = RotatingFile::open(path.clone(), tiny(20)).unwrap();
        let mut b = RotatingFile::open(path.clone(), tiny(20)).unwrap();
        a.append(b"a1 fills the file\n", note);
        a.append(b"a2 past\n", note); // past the cap: A's next look rotates
        a.append(b"a3\n", note);
        assert_eq!(read(&rotated_path(&path)), "a1 fills the file\na2 past\n");
        b.append(b"b1\n", note); // B's first write since its open: no look yet
        b.append(b"b2\n", note); // B looks, sees another file, follows
        assert_eq!(
            read(&rotated_path(&path)),
            "a1 fills the file\na2 past\nb1\n"
        );
        assert_eq!(read(&path), "NOTE aterm.log.1\na3\nb2\n");
    }

    /// A deleted log is recreated at the next look, not written into the void.
    #[test]
    fn a_deleted_log_is_recreated_at_the_next_look() {
        let dir = Scratch::new("gone");
        let path = dir.log();
        let mut f = RotatingFile::open(path.clone(), tiny(1024)).unwrap();
        f.append(b"one\n", note);
        std::fs::remove_file(&path).unwrap();
        f.append(b"two\n", note);
        assert_eq!(read(&path), "two\n");
    }

    /// The lock is taken without waiting: while another process holds it, a
    /// writer skips the rotation and keeps appending, and the next look after
    /// the lock is free rotates.
    #[test]
    fn a_busy_lock_skips_the_rotation_without_waiting() {
        let _serial = serial();

        let dir = Scratch::new("busy");
        let path = dir.log();
        let mut f = RotatingFile::open(path.clone(), tiny(4)).unwrap();
        let holder = open_append_0600(&lock_path(&path)).unwrap();
        holder.try_lock().unwrap();
        for line in ["one\n", "two\n", "three\n"] {
            f.append(line.as_bytes(), note);
        }
        assert!(!rotated_path(&path).exists());
        assert_eq!(read(&path), "one\ntwo\nthree\n");
        drop(holder);
        // The next look rotates — or, if a child spawned elsewhere in this
        // process still holds the lock until it execs, a later one does.
        for n in 0.. {
            assert!(n < 200, "no rotation after the lock was released");
            f.append(format!("four {n}\n").as_bytes(), note);
            if rotated_path(&path).exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(read(&rotated_path(&path)).starts_with("one\ntwo\nthree\n"));
        assert!(read(&path).starts_with("NOTE aterm.log.1\nfour "));
    }

    /// Two writers interleaving across several rotations: every line appears
    /// exactly once across the two files until its file is retired, and the
    /// retired lines are exactly the oldest ones.
    #[test]
    fn rotation_never_loses_or_duplicates_a_line() {
        let _serial = serial();

        let dir = Scratch::new("once");
        let path = dir.log();
        let mut a = RotatingFile::open(path.clone(), tiny(64)).unwrap();
        let mut b = RotatingFile::open(path.clone(), tiny(64)).unwrap();
        let mut written = Vec::new();
        for i in 0..60 {
            let (w, name) = if i % 3 == 0 {
                (&mut b, "b")
            } else {
                (&mut a, "a")
            };
            let line = format!("{name}{i:03}");
            w.append(format!("{line}\n").as_bytes(), note);
            written.push(line);
        }
        let kept: Vec<String> = [rotated_path(&path), path.clone()]
            .iter()
            .flat_map(|p| read(p).lines().map(str::to_owned).collect::<Vec<_>>())
            .filter(|l| !l.starts_with("NOTE"))
            .collect();
        let mut sorted = kept.clone();
        sorted.sort_by_key(|l| l[1..].parse::<u32>().unwrap());
        sorted.dedup();
        assert_eq!(sorted.len(), kept.len(), "a line was duplicated: {kept:?}");
        // What survives is a suffix of what was written: nothing in the middle
        // went missing.
        let first = written.iter().position(|l| *l == sorted[0]).unwrap();
        assert_eq!(sorted, written[first..], "a line went missing");
        assert!(first > 0, "the test must rotate at least twice");
    }

    #[cfg(unix)]
    #[test]
    fn crash_report_carries_version_message_and_backtrace() {
        let dir = std::env::temp_dir().join(format!("aterm-log-crash-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let path = dir.join("crash-1.log");
        write_crash_report(&path, &"panicked at 'boom', main.rs:7", &"0: frame_a").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let report = std::fs::read_to_string(&path).unwrap();
        assert!(report.contains(aterm_types::version::APP_VERSION));
        assert!(report.contains("panicked at 'boom', main.rs:7"));
        assert!(report.contains("backtrace:\n0: frame_a"));
        // A later report from the same pid replaces, not appends.
        write_crash_report(&path, &"second", &"bt").unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("boom"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The scan returns the artifact's path and its head, once, and preserves
    /// the artifact under its consumed name: the crash message names the path
    /// in its detail and `Open log` opens it, so the file must outlive the row.
    #[test]
    fn crash_evidence_returns_the_path_and_the_head_once_and_preserves_the_artifact() {
        let dir = std::env::temp_dir().join(format!("aterm-log-seen-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        // A previous run's panic report, a NON-empty signal marker, an EMPTY
        // marker (a clean run's leftover — must be ignored), and the ordinary
        // log file (wrong prefix — must be ignored). The report is the newest.
        std::fs::write(dir.join("crash-signal-4242-7.log"), b"fatal signal 11").unwrap();
        std::fs::write(dir.join("crash-signal-9999-8.log"), b"").unwrap();
        std::fs::write(dir.join("aterm.log"), b"routine line").unwrap();
        let mut report = String::from("aterm-gui 0.1.0 crashed at unix 1.000\n");
        report.push_str("panicked at 'boom', main.rs:7\n\nbacktrace:\n");
        for frame in 0..40 {
            report.push_str(&format!("{frame}: {}\n", "f".repeat(60)));
        }
        std::fs::write(dir.join("crash-4242.log"), report.as_bytes()).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(dir.join("crash-4242.log"))
            .unwrap()
            .set_modified(later)
            .unwrap();
        let evidence = take_crash_evidence_in(&dir).expect("crash evidence must post");
        assert!(evidence.path.is_absolute());
        assert_eq!(
            evidence.path,
            dir.join("crash-4242.log.seen"),
            "the newest artifact, under its consumed (renamed) name"
        );
        assert!(
            evidence
                .sentence()
                .starts_with("aterm closed unexpectedly last time")
        );
        assert!(evidence.sentence().contains(".log.seen"));
        // The head: the first non-empty lines (the blank line before the
        // backtrace is skipped), at most CRASH_HEAD_LINES of them, from at
        // most CRASH_HEAD_BYTES of the file.
        assert_eq!(evidence.head.len(), CRASH_HEAD_LINES);
        assert_eq!(evidence.head[0], "aterm-gui 0.1.0 crashed at unix 1.000");
        assert_eq!(evidence.head[1], "panicked at 'boom', main.rs:7");
        assert_eq!(evidence.head[2], "backtrace:");
        assert!(evidence.head[3].starts_with("0: "));
        let bytes: usize = evidence.head.iter().map(|l| l.len() + 1).sum();
        assert!(
            bytes <= usize::try_from(CRASH_HEAD_BYTES).unwrap(),
            "the head is read within the byte cap: {bytes}"
        );
        // Consumed = renamed, never deleted: both non-empty artifacts survive
        // under `.seen` names, so the user can still open what the row named.
        assert!(dir.join("crash-4242.log.seen").exists());
        assert!(dir.join("crash-signal-4242-7.log.seen").exists());
        assert!(!dir.join("crash-4242.log").exists());
        // One-shot: a second scan (the next launch) finds nothing to post —
        // the empty marker and aterm.log were never candidates.
        assert!(take_crash_evidence_in(&dir).is_none());
        // A marker with no readable head still posts: the path is the message.
        std::fs::write(dir.join("crash-signal-1-1.log"), b"\n\n  \n").unwrap();
        let evidence = take_crash_evidence_in(&dir).expect("a non-empty marker posts");
        assert!(evidence.head.is_empty(), "{:?}", evidence.head);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The accessibility publisher's background thread panics without ending the
    /// process, so filing it as a crash makes the NEXT launch claim aterm closed
    /// unexpectedly — on every launch, for as long as the accessibility bus stays
    /// unreachable. It must leave no crash evidence behind; the surface it does get
    /// is the `a11y.publisher` message on the pre-App inbox, asserted here.
    #[cfg(a11y_tree)]
    #[test]
    fn an_accessibility_backend_panic_leaves_no_crash_evidence_for_the_next_launch() {
        let _lane = crate::message_inbox::lane_test_guard();
        let _ = crate::message_inbox::take_queued();
        let dir = std::env::temp_dir().join(format!("aterm-log-a11y-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let backend = "/r/accesskit_unix-0.22.0/src/context.rs";
        let payload = "called `Result::unwrap()` on an `Err` value: \
                       Handshake(\"Server GUID mismatch: expected a, got b\")";
        // The AT-client latch is process-wide and never lowered, and a parallel
        // test's attach edge may raise it: read it on both sides of the report.
        let at_before = crate::a11y_backend::at_client_seen();
        assert_eq!(
            file_panic_report(&dir, backend, None, payload, &"panicked", &"bt"),
            None,
            "a dead accessibility publisher is not a crash of aterm"
        );
        let at_after = crate::a11y_backend::at_client_seen();
        assert!(
            take_crash_evidence_in(&dir).is_none(),
            "the next launch must not be told aterm closed unexpectedly"
        );
        let queued = crate::message_inbox::take_queued();
        let dead = queued
            .iter()
            .find(|m| {
                m.message.key.as_deref() == Some(crate::message_reporters::KEY_A11Y_PUBLISHER)
            })
            .expect("the inbox holds the a11y message");
        assert_eq!(
            dead.message.title, "Screen reader access lost",
            "the window must say the tree is gone: {:?}",
            dead.message
        );
        assert_eq!(
            dead.message.detail[0], "restart aterm to retry",
            "the retry leads, alone on its line: {:?}",
            dead.message
        );
        assert!(
            dead.message.detail[1].contains("Server GUID mismatch"),
            "and must name the bus fault: {:?}",
            dead.message
        );
        // On the glass only when a screen reader had attached; a record otherwise.
        if at_before {
            assert_eq!(dead.message.hold, aterm_messages::Hold::Standing);
        } else if !at_after {
            assert_eq!(dead.message.hold, aterm_messages::Hold::LogOnly);
        }

        // The SAME dir, an ordinary panic: still a crash report, still posted.
        let path = file_panic_report(
            &dir,
            "crates/aterm-gui/src/app.rs",
            None,
            "boom",
            &"i",
            &"b",
        )
        .expect("an ordinary panic is still filed as a crash");
        assert!(path.exists());
        assert!(take_crash_evidence_in(&dir).is_some());
        let _ = crate::message_inbox::take_queued();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A supervisor worker's panic is caught and restarted (`harness_host`):
    /// the process keeps running, so it is a harness fault record, never
    /// crash evidence for the next launch. NEGATIVE CONTROL: the same panic on
    /// another thread is still a crash.
    #[test]
    fn a_supervisor_thread_panic_is_a_fault_record_not_a_crash() {
        let dir = std::env::temp_dir().join(format!("aterm-log-harness-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let thread = "aterm-harness-s-0123456789abcdef";
        for _ in 0..2 {
            assert_eq!(
                file_panic_report(&dir, "src/run.rs", Some(thread), "boom", &"i", &"bt"),
                None
            );
        }
        assert!(
            take_crash_evidence_in(&dir).is_none(),
            "not a crash of aterm"
        );
        let record =
            std::fs::read_to_string(dir.join(format!("harness-fault-{}.log", std::process::id())))
                .expect("the fault record");
        assert_eq!(
            record
                .matches("supervisor fault on aterm-harness-s-")
                .count(),
            2
        );
        let path = file_panic_report(
            &dir,
            "src/run.rs",
            Some("aterm-operator-observer"),
            "boom",
            &"i",
            &"bt",
        )
        .expect("another thread's panic is a crash");
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_evidence_absent_after_a_clean_run() {
        let dir = std::env::temp_dir().join(format!("aterm-log-clean-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        // A clean run leaves exactly an empty marker + the routine log.
        std::fs::write(dir.join("crash-signal-1234-5.log"), b"").unwrap();
        std::fs::write(dir.join("aterm.log"), b"routine line").unwrap();
        assert!(take_crash_evidence_in(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn format_record_sanitizes_and_terminates_line() {
        let line = format_record(
            1_700_000_000_123,
            2061,
            aterm_log::Level::Warn,
            "containment_audit",
            "DENIED: image write '\x1b]0;x\n'",
        );
        assert_eq!(
            line,
            "1700000000.123 2061 WARN containment_audit: DENIED: image write '\u{fffd}]0;x\u{fffd}'\n"
        );
    }

    /// A WARN or ERROR keeps up to 1 KiB of its text; INFO stays at 512 bytes.
    #[test]
    fn format_record_keeps_more_of_a_warning() {
        let body = "x".repeat(900);
        let warn = format_record(0, 1, aterm_log::Level::Warn, "t", &body);
        assert!(warn.contains(&body), "a 900-byte warning is kept whole");
        let info = format_record(0, 1, aterm_log::Level::Info, "t", &body);
        assert!(info.trim_end().ends_with('…'));
        assert!(info.len() < 600);
    }

    /// Shown crash reports are kept, newest first, up to the limit — and the
    /// one the message names survives even when its mtime says it is old.
    #[test]
    fn crash_evidence_keeps_only_the_newest_shown_reports() {
        let dir = Scratch::new("prune");
        let base = SystemTime::now() - Duration::from_secs(3600);
        for i in 0..12_u64 {
            let p = dir.0.join(format!("crash-{i}.log.seen"));
            std::fs::write(&p, b"old report").unwrap();
            File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(base + Duration::from_secs(i * 60))
                .unwrap();
        }
        // This launch's evidence, stamped OLDER than every shown report.
        let fresh = dir.0.join("crash-99.log");
        std::fs::write(&fresh, b"panicked").unwrap();
        File::options()
            .write(true)
            .open(&fresh)
            .unwrap()
            .set_modified(base - Duration::from_secs(60))
            .unwrap();
        let evidence = take_crash_evidence_in(&dir.0).unwrap();
        assert!(evidence.path.ends_with("crash-99.log.seen"), "{evidence:?}");
        assert!(dir.0.join("crash-99.log.seen").exists());
        let mut left: Vec<String> = std::fs::read_dir(&dir.0)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let mut expected: Vec<String> = (4..12).map(|i| format!("crash-{i}.log.seen")).collect();
        expected.push("crash-99.log.seen".into());
        expected.sort();
        assert_eq!(left, expected);
    }
}

/// Tier-1 for `aterm_spec::derive::log_rotation_model`: two real
/// [`RotatingFile`]s on one path — writer A, and writer B, which restarts —
/// driven through writes, looks, ticks, rotations, follows, a look that follows
/// and rotates at once, and a write into the older copy. After every step the files on disk are projected onto the model
/// (lines per file, which file each writer holds, whether its look is due) and
/// the step must be exactly one the model allows; every look-due answer the
/// real code gives must match the model's guard. The negative control replays
/// the truncating start this replaced and must be refused.
///
/// The model counts lines, so the budget is in whole 4-byte lines (`Cap = 1`
/// line per file, a look after every line) and the rotation note is empty.
#[cfg(all(test, unix))]
mod rotation_conformance {
    use super::*;
    use aterm_spec::derive::{Model, log_rotation_model};
    use aterm_spec::interp::State;
    use aterm_spec::verify::validate_transition_tiered;

    const LINE: u64 = 4;
    const OVERRIDES: &[(&str, i64)] = &[("MaxLines", 14)];

    fn budget() -> RotationBudget {
        RotationBudget {
            rotate_at: LINE,
            check_bytes: LINE,
            check_every: Duration::from_secs(30),
        }
    }

    fn no_note(_: &Path) -> String {
        String::new()
    }

    fn lines(path: &Path) -> i64 {
        std::fs::metadata(path).map_or(0, |m| (m.len() / LINE) as i64)
    }

    fn id_at(path: &Path) -> FileId {
        std::fs::metadata(path).ok().and_then(|m| file_id(&m))
    }

    /// The real files plus the two things the files cannot show: how many
    /// lines were retired with a replaced older copy, and time since the last
    /// rotation (the model's environment clock).
    struct Harness {
        model: Model,
        path: PathBuf,
        a: RotatingFile,
        b: RotatingFile,
        written: i64,
        aged: i64,
        er: i64,
        older: (FileId, i64),
        steps: usize,
    }

    impl Harness {
        fn handle(&self, w: &RotatingFile) -> i64 {
            if w.id.is_some() && w.id == id_at(&self.path) {
                0
            } else if w.id.is_some() && w.id == id_at(&rotated_path(&self.path)) {
                1
            } else {
                2
            }
        }

        fn project(&self) -> State {
            let due_by_bytes = |w: &RotatingFile| i64::from(w.since_check >= w.budget.check_bytes);
            let due_by_time =
                |w: &RotatingFile| i64::from(w.last_check.elapsed() >= w.budget.check_every);
            let mut s = self.model.init_state();
            s.insert("live", lines(&self.path));
            s.insert("old", lines(&rotated_path(&self.path)));
            s.insert("aged", self.aged);
            s.insert("written", self.written);
            s.insert("ga", self.handle(&self.a));
            s.insert("gb", self.handle(&self.b));
            s.insert("na", due_by_bytes(&self.a));
            s.insert("nb", due_by_bytes(&self.b));
            s.insert("ea", due_by_time(&self.a));
            s.insert("eb", due_by_time(&self.b));
            s.insert("er", self.er);
            s
        }

        /// Run one real step and hold it to the model's `action`.
        fn step(&mut self, action: &'static str, run: impl FnOnce(&mut Self)) {
            let before = self.project();
            assert_eq!(
                self.a.check_due(),
                self.model.action_enabled("CheckA", &before),
                "A's look-due answer departs from the model at {before:?}"
            );
            assert_eq!(
                self.b.check_due(),
                self.model.action_enabled("CheckB", &before),
                "B's look-due answer departs from the model at {before:?}"
            );
            assert!(
                self.model.action_enabled(action, &before),
                "step {}: the script asks for {action}, which the model does not allow at {before:?}",
                self.steps
            );
            run(self);
            // An older copy that is not the one we saw before was replaced:
            // the lines in the one we saw are retired, and a rotation happened.
            let older_now = id_at(&rotated_path(&self.path));
            if older_now != self.older.0 {
                self.aged += self.older.1;
                self.er = 0;
            }
            self.older = (older_now, lines(&rotated_path(&self.path)));
            let after = self.project();
            let label = format!("log rotation step {} ({action})", self.steps);
            let (conforms, evidence) = validate_transition_tiered(
                &self.model,
                OVERRIDES,
                &before,
                &after,
                Some(action),
                &label,
            );
            assert!(conforms, "{label}: {before:?} -> {after:?}\n{evidence}");
            for inv in &self.model.invariants {
                assert!(
                    self.model.check_invariant(inv.name, &after),
                    "{label}: violates {}: {after:?}",
                    inv.name
                );
            }
            self.steps += 1;
        }

        /// What `append` does, as the model's two steps: the look when one is
        /// due, then the line.
        fn write(&mut self, writer: char) {
            let (check, write) = if writer == 'A' {
                ("CheckA", "WriteA")
            } else {
                ("CheckB", "WriteB")
            };
            let due = if writer == 'A' {
                self.a.check_due()
            } else {
                self.b.check_due()
            };
            if due {
                self.step(check, |h| {
                    if writer == 'A' {
                        h.a.check()
                    } else {
                        h.b.check()
                    }
                });
            }
            let line = format!("{writer}{:02}\n", self.written);
            assert_eq!(line.len() as u64, LINE);
            self.step(write, |h| {
                let w = if writer == 'A' { &mut h.a } else { &mut h.b };
                assert!(!w.check_due(), "the look was taken just above");
                w.append(line.as_bytes(), no_note);
                h.written += 1;
            });
        }

        fn tick(&mut self) {
            self.step("Tick", |h| {
                for w in [&mut h.a, &mut h.b] {
                    w.last_check = w
                        .last_check
                        .checked_sub(w.budget.check_every)
                        .expect("the monotonic clock is older than 30 s");
                }
                h.er = 1;
            });
        }

        fn restart_b(&mut self) {
            self.step("RestartB", |h| {
                h.b = RotatingFile::open(h.path.clone(), budget()).unwrap();
            });
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aterm-logconf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::control_auth::ensure_private_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn two_real_writers_conform_through_rotation_follow_and_restart() {
        let _serial = super::tests::serial();

        let dir = scratch("trace");
        let path = dir.join("aterm.log");
        let model = aterm_spec::interp::with_consts(&log_rotation_model(), OVERRIDES);
        let a = RotatingFile::open(path.clone(), budget()).unwrap();
        let b = RotatingFile::open(path.clone(), budget()).unwrap();
        let mut h = Harness {
            model,
            path: path.clone(),
            a,
            b,
            written: 0,
            aged: 0,
            er: 1,
            older: (None, 0),
            steps: 0,
        };
        h.write('B');
        h.write('B');
        h.write('A');
        h.write('B'); // B's look rotates; A now holds the older copy
        assert_eq!(h.project()["ga"], 1);
        h.tick();
        h.write('A'); // A's look follows to the new file
        h.restart_b(); // an oversized file at start is rotated, not truncated
        assert_eq!(h.aged, 3, "the first older copy was retired");
        h.tick();
        h.write('B');
        h.restart_b(); // within the budget: plain append
        h.write('A'); // follows
        h.write('A'); // rotates; the fresh B now holds the older copy
        assert_eq!(h.project()["gb"], 1);
        h.write('B'); // B's first line since its start lands in the older copy
        assert_eq!(h.project()["old"], 3, "a late line joins the older copy");
        h.tick();
        h.write('B'); // follows
        h.write('A'); // rotates again; B now holds the older copy
        assert_eq!(h.project()["gb"], 1);
        h.tick();
        h.write('A'); // within the window after a tick: past the cap again
        let aged = h.aged;
        h.write('B'); // B follows onto the full file and rotates it in one look
        assert_eq!((h.project()["gb"], h.project()["ga"]), (0, 1));
        assert!(h.aged > aged, "the look that followed also rotated");
        assert_eq!(h.written, 13);
        let s = h.project();
        assert_eq!(s["live"] + s["old"] + s["aged"], 13, "{s:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The historical start: an oversized file was TRUNCATED, erasing the lines
    /// other writers had put there. Replayed on real files, the model must
    /// refuse it as a restart and name the lost lines.
    #[test]
    fn the_truncating_start_this_replaced_is_refused() {
        let _serial = super::tests::serial();

        let dir = scratch("neg");
        let path = dir.join("aterm.log");
        let model = aterm_spec::interp::with_consts(&log_rotation_model(), OVERRIDES);
        let a = RotatingFile::open(path.clone(), budget()).unwrap();
        let b = RotatingFile::open(path.clone(), budget()).unwrap();
        let mut h = Harness {
            model,
            path: path.clone(),
            a,
            b,
            written: 0,
            aged: 0,
            er: 1,
            older: (None, 0),
            steps: 0,
        };
        h.write('A');
        h.write('B');
        let before = h.project();
        assert!(before["live"] > 1, "the file is over the cap: {before:?}");
        // The old `open_log_file`: truncate when oversized.
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
                .unwrap(),
        );
        h.b = RotatingFile::open(path.clone(), budget()).unwrap();
        let after = h.project();
        let (conforms, _) = validate_transition_tiered(
            &h.model,
            OVERRIDES,
            &before,
            &after,
            Some("RestartB"),
            "log rotation negative control",
        );
        assert!(
            !conforms,
            "a truncating start conformed: {before:?} -> {after:?}"
        );
        assert!(
            !h.model.check_invariant("EveryLineOnce", &after),
            "{after:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
