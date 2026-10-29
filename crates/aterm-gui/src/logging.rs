// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Startup diagnostics: a private file logger for the `aterm_log` facade, plus a crash-reporting panic hook.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

macro_rules! stderr_line {
    ($($arg:tt)*) => {{
        let _ = ::aterm_uds::stdout::stderr_line(::std::format_args!($($arg)*));
    }};
}
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
pub(crate) use stderr_line;

use aterm_log::{LevelFilter, Log, Metadata, Record};

/// Install the panic hook and file logger (at `info`).
pub(crate) fn init(arming: crate::crash_signal::Arming) {
    let Some(dir) = log_dir() else {
        crate::logging::stderr_line!(
            "aterm-gui: {}",
            no_log_dir_reason(
                aterm_types::dev_seam!("ATERM_STATE_HOME").as_deref(),
                aterm_types::dirs::logs_dir(),
            )
        );
        return;
    };
    install_panic_hook(dir.clone());
    prune_harness_faults(&dir, std::process::id());
    crate::crash_signal::install_signal_handlers_as(arming);
    if let Err((path, e)) = install_file_logger(&dir) {
        crate::logging::stderr_line!(
            "aterm-gui: cannot open {}: {e}; logging disabled",
            path.display()
        );
    }
}

/// The file logger alone, for the terminal session (`aterm --session`), whose threads run beside a live shell and so log to `aterm.log`, never stderr.
pub(crate) fn init_session() {
    if let Some(dir) = log_dir()
        && install_file_logger(&dir).is_ok()
    {
        #[cfg(unix)]
        start_writer();
    }
}

fn install_file_logger(dir: &Path) -> Result<(), (PathBuf, std::io::Error)> {
    let level = aterm_types::dev_seam!("ATERM_LOG")
        .and_then(|s| LevelFilter::parse(&s.to_string_lossy()))
        .unwrap_or(LevelFilter::Info);
    if level == LevelFilter::Off {
        return Ok(());
    }
    let path = dir.join("aterm.log");
    let file =
        RotatingFile::open(path.clone(), RotationBudget::ATERM_LOG).map_err(|e| (path, e))?;
    let logger: &'static FileLogger = Box::leak(Box::new(FileLogger::new(file, LOG_QUEUE_BYTES)));
    if aterm_log::set_logger(logger).is_ok() {
        let _ = FILE_LOGGER.set(logger);
        aterm_log::set_max_level(level);
    }
    Ok(())
}

/// Start the installed logger's `aterm-log` writer ([`FileLogger`]); until then each line is appended by the thread that logs it.
pub(crate) fn start_writer() {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let Some(logger) = FILE_LOGGER.get() else {
        return;
    };
    if STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    if let Err(error) = logger.start_writer() {
        aterm_log::warn!(
            "aterm.log: writer thread not spawned ({error}); every line is appended by the \
             thread that logs it"
        );
    }
}

/// Register [`flush_within`] as an `exit(3)` handler, so every `exit` (a
/// return from `main`, `process::exit`, an unwind out of `main`) lands the
/// queue.
#[cfg(unix)]
fn flush_at_exit() {
    extern "C" fn flush_queued_at_exit() {
        if let Some(logger) = FILE_LOGGER.get() {
            logger.unattend();
        }
        let _ = flush_within(LOG_FLUSH_EXIT_WAIT);
    }
    unsafe extern "C" {
        fn atexit(cb: extern "C" fn()) -> libc::c_int;
    }
    // SAFETY: registers a plain `extern "C"` function with no captured state; a failure to register only costs the lines queued at an `exit`.
    unsafe { atexit(flush_queued_at_exit) };
}

/// Route `aterm_objc`'s exception-containment reports into `aterm.log`: an `NSException` raised inside a declared Objective-C method (or block, dispatch trampoline, the `sendEvent:` override) that `aterm_objc::exception` caught.
#[cfg(target_os = "macos")]
pub(crate) fn install_objc_containment_sink() {
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

fn install_panic_hook(dir: PathBuf) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // A panic inside the screen reader's fence is caught and degraded by its
        // caller and warned once per session per location (`reader_guard`): no crash
        // report, no stderr.
        if crate::reader_guard::absorb(info) {
            return;
        }
        let _report = file_panic_report(
            &dir,
            info.location().map_or("", |l| l.file()),
            std::thread::current().name(),
            panic_payload(info),
            info,
            &std::backtrace::Backtrace::force_capture(),
        );
        let _ = flush_within(LOG_FLUSH_EXIT_WAIT);
        prev(info);
    }));
}

fn panic_payload<'a>(info: &'a std::panic::PanicHookInfo<'_>) -> &'a str {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("panicked")
}

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
    if is_harness_thread(thread_name) {
        let path = dir.join(format!("harness-fault-{}.log", std::process::id()));
        match write_harness_fault(&path, thread_name.unwrap_or(""), info, backtrace) {
            Ok(true) => crate::logging::stderr_line!(
                "aterm-gui: supervisor fault on {} — recorded at {}",
                thread_name.unwrap_or("?"),
                path.display()
            ),
            Ok(false) => crate::logging::stderr_line!(
                "aterm-gui: supervisor fault on {} — not recorded ({} is full)",
                thread_name.unwrap_or("?"),
                path.display()
            ),
            Err(e) => crate::logging::stderr_line!(
                "aterm-gui: supervisor fault on {} — not recorded ({}: {e})",
                thread_name.unwrap_or("?"),
                path.display()
            ),
        }
        return None;
    }
    let path = dir.join(format!("crash-{}.log", std::process::id()));
    #[cfg(unix)]
    let run = crate::crash_signal::own_marker_nanos();
    #[cfg(not(unix))]
    let run = None;
    match write_crash_report(&path, thread_name == Some("main"), run, info, backtrace) {
        Ok(()) => {
            crate::logging::stderr_line!("aterm-gui: panic — crash report at {}", path.display());
        }
        Err(e) => crate::logging::stderr_line!(
            "aterm-gui: panic — no crash report ({}: {e})",
            path.display()
        ),
    }
    Some(path)
}

/// [`crate::a11y_backend::is_backend_panic`] where an accessibility tree is compiled
/// in.
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
    crate::a11y_backend::report_failure(payload, location_file);
}

#[cfg(not(a11y_tree))]
fn report_accessibility_failure(_payload: &str, _location_file: &str) {}

fn is_harness_thread(thread_name: Option<&str>) -> bool {
    thread_name.is_some_and(|n| n.starts_with(crate::harness_host::THREAD_PREFIX))
}

const HARNESS_FAULT_BYTES: u64 = 256 * 1024;

const HARNESS_FAULT_FULL: &str =
    "later supervisor faults of this run are not recorded: the record is full\n";

fn write_harness_fault(
    path: &Path,
    thread: &str,
    info: &dyn std::fmt::Display,
    backtrace: &dyn std::fmt::Display,
) -> std::io::Result<bool> {
    let ts = aterm_time::since_epoch().unwrap_or_default();
    let mut f = open_append_0600(path)?;
    if f.metadata()?.len() >= HARNESS_FAULT_BYTES {
        return Ok(false);
    }
    writeln!(
        f,
        "aterm-gui {} supervisor fault on {thread} at unix {}.{:03}\n{info}\n\nbacktrace:\n{backtrace}\n",
        crate::build_info::version(),
        ts.as_secs(),
        ts.subsec_millis(),
    )?;
    if f.metadata()?.len() >= HARNESS_FAULT_BYTES {
        f.write_all(HARNESS_FAULT_FULL.as_bytes())?;
    }
    Ok(true)
}

/// Write one `0600` crash report, truncating any prior report from this pid.
pub(crate) fn write_crash_report(
    path: &Path,
    main_thread: bool,
    run: Option<u128>,
    info: &dyn std::fmt::Display,
    backtrace: &dyn std::fmt::Display,
) -> std::io::Result<()> {
    let ts = aterm_time::since_epoch().unwrap_or_default();
    let mut f = create_0600(path)?;
    writeln!(
        f,
        "aterm-gui {} crashed at unix {}.{:03}{}{}\n{info}\n\nbacktrace:\n{backtrace}",
        crate::build_info::version(),
        ts.as_secs(),
        ts.subsec_millis(),
        run.map(|nanos| format!("{REPORT_RUN}{nanos}"))
            .unwrap_or_default(),
        if main_thread { MAIN_THREAD_PANIC } else { "" },
    )
}

/// What names a panic report's run on its first line, before the start its crash marker is named for.
pub(crate) const REPORT_RUN: &str = " in run ";

/// What ends a panic report's first line when the panic was on the main thread: it unwinds out of `main` and ends the process, whose `atexit` removes the run's crash marker, so once another launch has consumed the report (`.seen`) this mark is the only record that the run crashed (read by [`panic_report_head`]).
pub(crate) const MAIN_THREAD_PANIC: &str = " on the main thread";

/// What a panic report's first line says of the run that filed it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReportHead {
    /// It panicked on the main thread ([`MAIN_THREAD_PANIC`]).
    pub(crate) main_thread: bool,
    /// The start its run's crash marker is named for ([`REPORT_RUN`]); `None` when it names none.
    pub(crate) run: Option<u128>,
}

/// The [`ReportHead`] of the panic report at `path`.
#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "read by the crash journal's claim, which is the unix lane's"
    )
)]
pub(crate) fn panic_report_head(path: &Path) -> ReportHead {
    use std::io::{BufRead as _, BufReader, Read as _};
    let Ok(file) = std::fs::File::open(path) else {
        return ReportHead::default();
    };
    let mut first = String::new();
    if BufReader::new(file.take(512))
        .read_line(&mut first)
        .is_err()
    {
        return ReportHead::default();
    }
    let first = first.trim_end();
    ReportHead {
        main_thread: first.ends_with(MAIN_THREAD_PANIC),
        run: first.split(REPORT_RUN).nth(1).and_then(|rest| {
            rest.split(|c: char| !c.is_ascii_digit())
                .next()
                .filter(|digits| !digits.is_empty())?
                .parse()
                .ok()
        }),
    }
}

/// One-shot startup scan for evidence that the previous run died without a clean exit: [`CrashEvidence`] (the artifact and its opening lines, for the crash message), or `None`.
pub(crate) fn take_crash_evidence() -> Option<CrashEvidence> {
    take_crash_evidence_in(&log_dir()?, crate::crash_journal::journal_dir().as_deref())
}

/// What the previous run left behind: the artifact the crash message names and its first lines, so the row can say what happened (`message_reporters::crash_message`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CrashEvidence {
    /// The artifact, absolute, under its consumed (`.seen`) name when the rename succeeded.
    pub(crate) path: PathBuf,
    /// The artifact's first non-empty lines: at most [`CRASH_HEAD_LINES`], from at most [`CRASH_HEAD_BYTES`] of the file.
    pub(crate) head: Vec<String>,
}

impl CrashEvidence {
    /// The line printed to the console.
    pub(crate) fn sentence(&self) -> String {
        format!(
            "aterm closed unexpectedly last time \u{2014} crash log at {}",
            self.path.display()
        )
    }
}

/// The most lines of the artifact the crash message carries.
pub(crate) const CRASH_HEAD_LINES: usize = 8;

/// Most bytes of the artifact read for those lines: the first 1.5 KiB of a report (version, panic, backtrace) holds the cause.
pub(crate) const CRASH_HEAD_BYTES: u64 = 1536;

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
pub(crate) const KEEP_SEEN_CRASH_REPORTS: usize = 8;

/// [`take_crash_evidence`] against an explicit directory (unit-testable).
pub(crate) fn take_crash_evidence_in(dir: &Path, journals: Option<&Path>) -> Option<CrashEvidence> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !(name.starts_with("crash-") && name.ends_with(".log")) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() == 0 {
            continue;
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
    prune_seen_crash_reports(
        dir,
        newest.as_ref().map(|(_, path)| path.as_path()),
        journals,
    );
    let (_, path) = newest?;
    let head = crash_head(&path);
    Some(CrashEvidence { path, head })
}

/// One-shot startup scan for evidence that the installed app's previous run was killed with no fatal signal and no exit path (Force Quit, SIGKILL, jetsam, a watchdog, power loss): an empty `crash-marker-*-app.log` whose owner's lock is free (`crash_signal::markers::dead_app_markers`).
pub(crate) fn take_kill_evidence() -> Option<KillEvidence> {
    #[cfg(unix)]
    {
        let evidence = take_kill_evidence_in(
            &log_dir()?,
            std::process::id(),
            crate::crash_journal::journal_dir().as_deref(),
        )?;
        aterm_log::info!(
            "crash: the last run was killed; marker {}",
            evidence.marker.display()
        );
        Some(evidence)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// What a killed run left behind: its consumed (empty) marker, named in `aterm.log`, and the log whose last lines are what the run said before it went.
#[cfg_attr(
    not(unix),
    allow(
        dead_code,
        reason = "only the unix marker lifecycle finds a killed run"
    )
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KillEvidence {
    /// The consumed marker, absolute.
    pub(crate) marker: PathBuf,
    /// `aterm.log` beside it — the `Open log` target.
    pub(crate) log: PathBuf,
}

impl KillEvidence {
    /// The console's line, the killed twin of [`CrashEvidence::sentence`].
    pub(crate) fn sentence(&self) -> String {
        format!(
            "aterm was stopped last time (no crash signal, no clean exit) \u{2014} its last \
             lines are in {}",
            self.log.display()
        )
    }
}

/// [`take_kill_evidence`] against an explicit directory and pid (unit-testable).
#[cfg(unix)]
pub(crate) fn take_kill_evidence_in(
    dir: &Path,
    own_pid: u32,
    journals: Option<&Path>,
) -> Option<KillEvidence> {
    let marker = consume_dead_markers(
        dir,
        own_pid,
        journals,
        crate::crash_signal::MarkerOwner::App,
    )?;
    Some(KillEvidence {
        marker,
        log: dir.join("aterm.log"),
    })
}

/// The lab desk's kill scan: [`take_kill_evidence`]'s twin for a lab desk (`crash_signal::Arming::Lab`).
#[cfg(unix)]
pub(crate) fn take_lab_kill_evidence() {
    let Some(dir) = log_dir() else {
        return;
    };
    if let Some(marker) = consume_dead_markers(
        &dir,
        std::process::id(),
        crate::crash_journal::journal_dir().as_deref(),
        crate::crash_signal::MarkerOwner::Lab,
    ) {
        aterm_log::info!(
            "crash: the lab desk's last run was killed; marker {}",
            marker.display()
        );
    }
}

/// Consume the dead, empty markers of `owner` in `dir` another process left (`crash_signal::markers::dead_markers_of`): each renamed `.seen`, the consumed reports then pruned ([`prune_seen_crash_reports`]).
#[cfg(unix)]
pub(crate) fn consume_dead_markers(
    dir: &Path,
    own_pid: u32,
    journals: Option<&Path>,
    owner: crate::crash_signal::MarkerOwner,
) -> Option<PathBuf> {
    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for path in crate::crash_signal::markers::dead_markers_of(dir, own_pid, owner) {
        let modified = std::fs::symlink_metadata(&path)
            .and_then(|m| m.modified())
            .unwrap_or(UNIX_EPOCH);
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let seen = path.with_file_name(format!("{name}.seen"));
        let path = if std::fs::rename(&path, &seen).is_ok() {
            seen
        } else {
            path
        };
        if newest.as_ref().is_none_or(|(t, _)| modified >= *t) {
            newest = Some((modified, path));
        }
    }
    prune_seen_crash_reports(
        dir,
        newest.as_ref().map(|(_, path)| path.as_path()),
        journals,
    );
    newest.map(|(_, marker)| marker)
}

fn prune_seen_crash_reports(dir: &Path, named: Option<&Path>, journals: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let waiting = journals.map(waiting_journal_pids).unwrap_or_default();
    let mut seen: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            if !(name.starts_with("crash-") && name.ends_with(".log.seen")) {
                return None;
            }
            if consumed_report_pid(name).is_some_and(|pid| waiting.contains(&pid)) {
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

const KEEP_HARNESS_FAULT_RECORDS: usize = 8;

fn prune_harness_faults(dir: &Path, own_pid: u32) {
    let nanos = aterm_time::since_epoch().unwrap_or_default().as_nanos();
    let _ = std::fs::rename(
        dir.join(format!("harness-fault-{own_pid}.log")),
        dir.join(format!("harness-fault-{own_pid}-{nanos}.log")),
    );
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut records: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            if !(name.starts_with("harness-fault-") && name.ends_with(".log")) {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().unwrap_or(UNIX_EPOCH);
            Some((modified, entry.path()))
        })
        .collect();
    records.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in records.into_iter().skip(KEEP_HARNESS_FAULT_RECORDS) {
        let _ = std::fs::remove_file(&path);
    }
}

fn waiting_journal_pids(dir: &Path) -> Vec<u32> {
    std::fs::read_dir(dir).map_or_else(
        |_| Vec::new(),
        |entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    crate::crash_journal::JournalId::parse(entry.file_name().to_str()?)
                        .map(|id| id.pid)
                })
                .collect()
        },
    )
}

fn consumed_report_pid(name: &str) -> Option<u32> {
    let body = name.strip_prefix("crash-")?.strip_suffix(".log.seen")?;
    let body = body
        .strip_prefix("marker-")
        .or_else(|| body.strip_prefix("signal-"))
        .unwrap_or(body);
    body.split('-').next()?.parse().ok()
}

fn no_log_dir_reason(state_home: Option<&std::ffi::OsStr>, dir: Option<PathBuf>) -> String {
    const LOGS_OFF: &str = "crash reports and aterm.log are off";
    if state_home.is_some_and(|root| !std::path::Path::new(root).is_absolute()) {
        return "ATERM_STATE_HOME is not an absolute path; crash reports, aterm.log, session \
                restore, the crash journal and the cell-metrics cache are off"
            .to_string();
    }
    let Some(dir) = dir else {
        return format!("no home folder found; {LOGS_OFF}");
    };
    #[cfg(unix)]
    let made = crate::control_auth::ensure_private_dir(&dir);
    #[cfg(not(unix))]
    let made = std::fs::create_dir_all(&dir);
    match made {
        Err(e) => format!("cannot use {} ({e}); {LOGS_OFF}", dir.display()),
        Ok(()) => format!("cannot use {}; {LOGS_OFF}", dir.display()),
    }
}

/// Resolve the per-user log dir, created `0700` (denial records name what a program attempted).
#[cfg(unix)]
pub(crate) fn log_dir() -> Option<PathBuf> {
    let dir = aterm_types::dirs::logs_dir()?;
    crate::control_auth::ensure_private_dir(&dir).ok()?;
    Some(dir)
}

/// Windows: `%LOCALAPPDATA%\aterm\logs` ([`aterm_types::dirs::logs_dir`]), falling back through the home dir when `LOCALAPPDATA` is unset.
#[cfg(windows)]
pub(crate) fn log_dir() -> Option<PathBuf> {
    let dir = aterm_types::dirs::logs_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// How big a log file may grow, and how often each writer looks at it on disk.
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
    /// `aterm.log`: [`aterm_log::MAX_LOG_BYTES`] per file, looked at every 64 KiB or 30 s.
    pub(crate) const ATERM_LOG: Self = Self {
        rotate_at: aterm_log::MAX_LOG_BYTES,
        check_bytes: 64 * 1024,
        check_every: Duration::from_secs(30),
    };

    const fn oversized(self, len: u64) -> bool {
        len > self.rotate_at
    }
}

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
    /// `path` names a different file than the caller's: another process rotated it first, so the caller reopens `path` and writes there.
    Moved,
    /// Nothing to do: the file is within the budget, is missing, or another process is rotating it right now.
    Kept,
}

/// Rotate `path` to [`rotated_path`] (replacing the older copy) when larger than `rotate_at`, and, when `ours` names a file, only while `path` is still that file.
pub(crate) fn rotate_if_oversized(path: &Path, rotate_at: u64, ours: FileId) -> Rotation {
    let lock: std::fs::File = match open_lock_0600(&lock_path(path)) {
        Ok(file) => file,
        Err(_) => return Rotation::Kept,
    };
    let Ok(_held) = crate::native_document_host::HeldAdvisoryLock::try_lock(lock) else {
        return Rotation::Kept;
    };
    #[cfg(all(test, unix))]
    crate::parked_fork::fork_if_armed();
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
}

/// An append-only log file that rotates itself.
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
    /// Open `path` for appending, `0600`, rotating it first if it is already over the budget (an older copy is kept; nothing is truncated).
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

    /// Append `line` in one write, after a look at the file when one is due.
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

    fn check(&mut self) {
        self.since_check = 0;
        self.last_check = Instant::now();
        let on_disk = std::fs::metadata(&self.path).ok().and_then(|m| file_id(&m));
        if on_disk.is_none() || on_disk != self.id {
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

/// Open `path` at mode `0600` for appending.
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

/// Non-unix twin of `open_append_0600`: plain create+append; the per-user `%LOCALAPPDATA%` dir's owner-only ACLs are the confidentiality boundary.
#[cfg(not(unix))]
pub(crate) fn open_append_0600(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

/// Open the rotation lock ([`lock_path`]) so `File::try_lock` can take it.
#[cfg(unix)]
fn open_lock_0600(path: &Path) -> std::io::Result<File> {
    open_append_0600(path)
}

/// Windows twin of `open_lock_0600` (the doc above says why it differs).
#[cfg(not(unix))]
fn open_lock_0600(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
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

struct FileLogger {
    /// Lines formatted and not yet appended, oldest first. Taken off only
    /// while `file` is held (lock order: `file`, then `pending`).
    pending: Mutex<Pending>,
    /// Signalled when `pending` goes from empty to not.
    ready: Condvar,
    file: Mutex<RotatingFile>,
    /// Queued bytes past which a line is appended by its own thread.
    cap: usize,
}

/// [`FileLogger`]'s queue.
#[derive(Default)]
struct Pending {
    lines: VecDeque<String>,
    bytes: usize,
    /// A writer drains the queue (the `aterm-log` thread spawned).
    attended: bool,
    /// Exit has handed logging back permanently, including before takeover.
    closing: bool,
}

const LOG_QUEUE_BYTES: usize = 256 * 1024;

/// How long a flush at the process's end ([`flush_within`]) waits for the file — the messages log's exit bound (`MESSAGES_FLUSH_EXIT_WAIT`).
pub(crate) const LOG_FLUSH_EXIT_WAIT: Duration = Duration::from_millis(250);

static FILE_LOGGER: OnceLock<&'static FileLogger> = OnceLock::new();

/// Append every line queued for `aterm.log` on the calling thread, waiting no longer than `within` for a writer that is mid-append (law L5: no wait without a deadline).
pub(crate) fn flush_within(within: Duration) -> bool {
    FILE_LOGGER
        .get()
        .is_none_or(|logger| logger.flush_within(within))
}

impl FileLogger {
    fn new(file: RotatingFile, cap: usize) -> Self {
        Self {
            pending: Mutex::new(Pending::default()),
            ready: Condvar::new(),
            file: Mutex::new(file),
            cap,
        }
    }

    fn start_writer(&'static self) -> std::io::Result<()> {
        crate::qos::spawn_lane_thread("aterm-log", crate::qos::Role::Responsive, move || {
            #[cfg(unix)]
            flush_at_exit();
            if self.attend() {
                self.run_writer();
            }
        })?;
        Ok(())
    }

    fn attend(&self) -> bool {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if pending.closing {
            return false;
        }
        pending.attended = true;
        true
    }

    /// From now on every line is appended by the thread that logs it, after
    /// whatever is still queued: the process is ending and its writer may not
    /// get another turn.
    #[cfg(unix)]
    fn unattend(&self) {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        pending.closing = true;
        pending.attended = false;
    }

    fn write_record(&self, level: aterm_log::Level, target: &str, body: &str) {
        let line = format_record(epoch_ms_now(), std::process::id(), level, target, body);
        let line = {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            if pending.attended && pending.bytes.saturating_add(line.len()) <= self.cap {
                let was_empty = pending.lines.is_empty();
                pending.bytes += line.len();
                pending.lines.push_back(line);
                drop(pending);
                if was_empty {
                    self.ready.notify_one();
                }
                return;
            }
            line
        };
        let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        self.append_queued(&mut file, Some(&line));
    }

    fn append_queued(&self, file: &mut RotatingFile, then: Option<&str>) {
        let lines = {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            pending.bytes = 0;
            std::mem::take(&mut pending.lines)
        };
        for line in lines.iter().map(String::as_str).chain(then) {
            file.append(line.as_bytes(), rotation_note);
        }
    }

    fn run_writer(&self) {
        loop {
            {
                let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
                while pending.lines.is_empty() {
                    pending = self
                        .ready
                        .wait(pending)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
            let mut file = self.file.lock().unwrap_or_else(PoisonError::into_inner);
            self.append_queued(&mut file, None);
        }
    }

    fn flush_within(&self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            let mut file = match self.file.try_lock() {
                Ok(file) => file,
                Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return false;
                    }
                    std::thread::sleep(Duration::from_micros(100));
                    continue;
                }
            };
            self.append_queued(&mut file, None);
            file.flush();
            return true;
        }
    }
}

fn rotation_note(older: &Path) -> String {
    let older = older.file_name().map_or_else(
        || older.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    format_record(
        epoch_ms_now(),
        std::process::id(),
        aterm_log::Level::Info,
        module_path!(),
        &format!("log rotated; older lines in {older}"),
    )
}

impl Log for FileLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record<'_>) {
        self.write_record(record.level(), record.target(), &record.args().to_string());
    }

    fn flush(&self) {
        let _ = self.flush_within(LOG_FLUSH_EXIT_WAIT);
    }
}

fn epoch_ms_now() -> u128 {
    aterm_time::since_epoch().unwrap_or_default().as_millis()
}

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

    #[test]
    fn the_logs_sweeps_leave_the_session_journal_alone() {
        let dir = Scratch::new("journal");
        let journal = dir.0.path().join("session-log");
        std::fs::create_dir(&journal).unwrap();
        let seg = journal.join("p1-aa.1.slog");
        std::fs::write(
            &seg,
            b"s1\t*\t#seg\tv=1\tinst=p1-aa\tn=1\tt=1\tbuild=x\tend=\n",
        )
        .unwrap();
        std::fs::write(journal.join("p1-aa.lock"), b"").unwrap();
        for i in 0..20 {
            std::fs::write(dir.0.path().join(format!("crash-{i}.log.seen")), b"x").unwrap();
            std::fs::write(dir.0.path().join(format!("harness-fault-{i}.log")), b"x").unwrap();
        }
        prune_seen_crash_reports(dir.0.path(), None, None);
        prune_harness_faults(dir.0.path(), std::process::id());
        let mut left: Vec<String> = std::fs::read_dir(&journal)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec!["p1-aa.1.slog", "p1-aa.lock"]);
        assert!(std::fs::read(&seg).unwrap().starts_with(b"s1\t*\t#seg"));
        let crash = std::fs::read_dir(dir.0.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("crash-"))
            .count();
        assert!(crash < 20, "{crash}");
    }

    struct Scratch(aterm_tempfile::TempDir);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = aterm_tempfile::Builder::new()
                .prefix(&format!("aterm-log-{tag}-"))
                .tempdir()
                .unwrap();
            ensure_private_dir(dir.path()).unwrap();
            Self(dir)
        }

        fn log(&self) -> PathBuf {
            self.0.path().join("aterm.log")
        }
    }

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

    #[test]
    fn oversized_log_rotates_on_open_and_keeps_the_old_lines() {
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

    #[test]
    fn rotates_while_running_and_keeps_one_older_copy() {
        let dir = Scratch::new("run");
        let path = dir.log();
        let mut f = RotatingFile::open(path.clone(), tiny(40)).unwrap();
        for i in 0..40 {
            f.append(format!("line {i:02}\n").as_bytes(), note);
        }
        let mut names: Vec<String> = std::fs::read_dir(dir.0.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["aterm.log", "aterm.log.1", "aterm.log.lock"]);
        for p in [&path, &rotated_path(&path)] {
            let len = std::fs::metadata(p).unwrap().len();
            assert!(len <= 40 + 8, "{} is {len} bytes", p.display());
        }
        assert!(read(&path).ends_with("line 39\n"));
        assert!(read(&path).starts_with("NOTE aterm.log.1\n"));
    }

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

    #[test]
    fn a_busy_lock_skips_the_rotation_without_waiting() {
        let dir = Scratch::new("busy");
        let path = dir.log();
        let mut f = RotatingFile::open(path.clone(), tiny(4)).unwrap();
        let holder = crate::native_document_host::HeldAdvisoryLock::try_lock(
            open_lock_0600(&lock_path(&path)).unwrap(),
        )
        .unwrap();
        for line in ["one\n", "two\n", "three\n"] {
            f.append(line.as_bytes(), note);
        }
        assert!(!rotated_path(&path).exists());
        assert_eq!(read(&path), "one\ntwo\nthree\n");
        drop(holder);
        f.append(b"four\n", note);
        assert_eq!(read(&rotated_path(&path)), "one\ntwo\nthree\n");
        assert_eq!(read(&path), "NOTE aterm.log.1\nfour\n");
    }

    /// The rotation lock is free the moment a rotation ends, even while a child forked
    /// under it has yet to exec.
    #[cfg(unix)]
    #[test]
    fn a_rotation_frees_its_lock_while_a_child_forked_under_it_has_yet_to_exec() {
        let dir = Scratch::new("fork-window");
        let path = dir.log();
        open_append_0600(&path)
            .unwrap()
            .write_all(b"first\n")
            .unwrap();
        let armed = crate::parked_fork::arm_at(1);
        assert_eq!(rotate_if_oversized(&path, 4, None), Rotation::Rotated);
        let child = armed.child();
        assert!(
            child.still_parked(),
            "forked under the first rotation's lock"
        );

        open_append_0600(&path)
            .unwrap()
            .write_all(b"second\n")
            .unwrap();
        assert_eq!(
            rotate_if_oversized(&path, 4, None),
            Rotation::Rotated,
            "the next rotation takes the lock"
        );
        assert!(child.still_parked(), "…while the child had yet to exec");
        child.release();
        assert_eq!(read(&rotated_path(&path)), "second\n");
    }

    #[test]
    fn rotation_never_loses_or_duplicates_a_line() {
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
        write_crash_report(
            &path,
            false,
            None,
            &"panicked at 'boom', main.rs:7",
            &"0: frame_a",
        )
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let report = std::fs::read_to_string(&path).unwrap();
        assert!(report.contains(crate::build_info::version()));
        assert!(report.contains("panicked at 'boom', main.rs:7"));
        assert!(report.contains("backtrace:\n0: frame_a"));
        assert_eq!(
            panic_report_head(&path),
            ReportHead::default(),
            "a worker's panic is not marked, and a run with no marker names none"
        );
        write_crash_report(
            &path,
            true,
            Some(1_727_000_000_123_456_789),
            &"second",
            &"bt",
        )
        .unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("boom"));
        assert_eq!(
            panic_report_head(&path),
            ReportHead {
                main_thread: true,
                run: Some(1_727_000_000_123_456_789),
            },
            "the main thread's is marked, and the report names its run"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn crash_evidence_returns_the_path_and_the_head_once_and_preserves_the_artifact() {
        let dir = std::env::temp_dir().join(format!("aterm-log-seen-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
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
        let evidence = take_crash_evidence_in(&dir, None).expect("crash evidence must post");
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
        assert!(dir.join("crash-4242.log.seen").exists());
        assert!(dir.join("crash-signal-4242-7.log.seen").exists());
        assert!(!dir.join("crash-4242.log").exists());
        assert!(take_crash_evidence_in(&dir, None).is_none());
        std::fs::write(dir.join("crash-signal-1-1.log"), b"\n\n  \n").unwrap();
        let evidence = take_crash_evidence_in(&dir, None).expect("a non-empty marker posts");
        assert!(evidence.head.is_empty(), "{:?}", evidence.head);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An accessibility publisher panic does not end the process, so it leaves no
    /// crash evidence; its surface is the `a11y.publisher` message on the pre-App
    /// inbox, asserted here.
    #[cfg(a11y_tree)]
    #[test]
    fn an_accessibility_backend_panic_leaves_no_crash_evidence_for_the_next_launch() {
        let _lane = crate::message_inbox::lane_test_guard();
        let _ = crate::message_inbox::take_queued();
        let dir = std::env::temp_dir().join(format!("aterm-log-a11y-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let backend = "crates/aterm-a11y/src/atspi/semantics/node.rs";
        let payload = "called `Result::unwrap()` on an `Err` value: \
                       Handshake(\"Server GUID mismatch: expected a, got b\")";
        let at_before = crate::a11y_backend::at_client_seen();
        assert_eq!(
            file_panic_report(&dir, backend, None, payload, &"panicked", &"bt"),
            None,
            "a dead accessibility publisher is not a crash of aterm"
        );
        let at_after = crate::a11y_backend::at_client_seen();
        assert!(
            take_crash_evidence_in(&dir, None).is_none(),
            "the next launch must not be told aterm closed unexpectedly"
        );
        let queued = crate::message_inbox::take_queued();
        let dead = queued
            .iter()
            .find(|m| {
                m.message.key.as_deref() == Some(crate::message_reporters::KEY_A11Y_PUBLISHER)
            })
            .expect("the inbox holds the a11y message");
        assert!(
            dead.message
                .detail
                .iter()
                .any(|l| l.contains("Server GUID mismatch")),
            "the message names the bus fault: {:?}",
            dead.message
        );
        if at_before {
            assert_eq!(dead.message.hold, aterm_messages::Hold::STANDING);
        } else if !at_after {
            assert_eq!(dead.message.hold, aterm_messages::Hold::LogOnly);
        }

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
        assert!(take_crash_evidence_in(&dir, None).is_some());
        let _ = crate::message_inbox::take_queued();
        let _ = std::fs::remove_dir_all(&dir);
    }

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
            take_crash_evidence_in(&dir, None).is_none(),
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
            Some("aterm-control-listener"),
            "boom",
            &"i",
            &"bt",
        )
        .expect("another thread's panic is a crash");
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn harness_fault_records_are_capped_and_pruned() {
        let dir = Scratch::new("harness-cap");
        let backtrace = "frame\n".repeat(8 * 1024);
        let own = dir
            .0
            .path()
            .join(format!("harness-fault-{}.log", std::process::id()));
        let fault = || {
            file_panic_report(
                dir.0.path(),
                "src/run.rs",
                Some("aterm-harness-s-0123456789abcdef"),
                "boom",
                &"i",
                &backtrace,
            )
        };
        for _ in 0..2 * HARNESS_FAULT_BYTES / backtrace.len() as u64 + 2 {
            fault();
        }
        let record = read(&own);
        assert!(
            (record.len() as u64) < HARNESS_FAULT_BYTES + backtrace.len() as u64 + 1024,
            "{} bytes",
            record.len()
        );
        assert_eq!(record.matches(HARNESS_FAULT_FULL).count(), 1);
        assert!(record.ends_with(HARNESS_FAULT_FULL));

        let base = SystemTime::now() - Duration::from_secs(3600);
        let age = |path: &Path, minutes: u64| {
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(base + Duration::from_secs(minutes * 60))
                .unwrap();
        };
        let others = KEEP_HARNESS_FAULT_RECORDS as u64 + 4;
        for pid in 1..=others {
            let path = dir.0.path().join(format!("harness-fault-{pid}.log"));
            std::fs::write(&path, b"a fault").unwrap();
            age(&path, pid);
        }
        age(&own, others + 1);
        prune_harness_faults(dir.0.path(), std::process::id());
        let aside = format!("harness-fault-{}-", std::process::id());
        let (set_aside, mut left): (Vec<String>, Vec<String>) = std::fs::read_dir(dir.0.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("harness-fault-"))
            .partition(|name| name.starts_with(&aside));
        left.sort();
        let mut expected: Vec<String> = (others + 2 - KEEP_HARNESS_FAULT_RECORDS as u64..=others)
            .map(|pid| format!("harness-fault-{pid}.log"))
            .collect();
        expected.sort();
        assert_eq!(left, expected);
        let [set_aside] = &set_aside[..] else {
            panic!("{set_aside:?}")
        };
        assert_eq!(read(&dir.0.path().join(set_aside)), record);

        fault();
        let fresh = read(&own);
        assert_eq!(fresh.matches("supervisor fault on").count(), 1, "{fresh}");
        assert!(!fresh.contains(HARNESS_FAULT_FULL));

        prune_harness_faults(dir.0.path(), std::process::id());
        let set_aside: Vec<String> = std::fs::read_dir(dir.0.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(&aside))
            .collect();
        assert_eq!(set_aside.len(), 2, "{set_aside:?}");
        assert!(
            set_aside
                .iter()
                .any(|name| read(&dir.0.path().join(name)) == fresh),
            "{set_aside:?}"
        );
        assert!(!own.exists());
    }

    /// The killed run, reported once: an empty, unlocked `app` marker of another pid
    /// is consumed (renamed `.seen`) and named with `aterm.log` beside it.
    #[cfg(unix)]
    #[test]
    fn a_killed_app_run_is_reported_once_and_nothing_else_is() {
        use crate::crash_signal::{MarkerOwner, markers};
        let dir = std::env::temp_dir().join(format!("aterm-log-killed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        ensure_private_dir(&dir).unwrap();
        let own = std::process::id();
        let gone = own.wrapping_add(1_000_000);
        let killed = dir.join(markers::file_name(gone, 5, MarkerOwner::App));
        std::fs::write(&killed, b"").unwrap();
        std::fs::write(
            dir.join(markers::file_name(gone, 6, MarkerOwner::Other)),
            b"",
        )
        .unwrap();
        std::fs::write(dir.join(markers::file_name(own, 7, MarkerOwner::App)), b"").unwrap();
        let (fd, live) =
            markers::create_locked(&dir, &markers::file_name(gone + 1, 8, MarkerOwner::App))
                .unwrap();
        std::fs::write(dir.join("aterm.log"), b"last line before the kill\n").unwrap();

        assert!(
            take_crash_evidence_in(&dir, None).is_none(),
            "control: the crash scan alone never saw a killed run"
        );
        let evidence = take_kill_evidence_in(&dir, own, None).expect("the killed run posts");
        assert_eq!(
            evidence.marker,
            killed.with_file_name(format!(
                "{}.seen",
                killed.file_name().unwrap().to_str().unwrap()
            ))
        );
        assert!(evidence.marker.exists() && !killed.exists());
        assert_eq!(evidence.log, dir.join("aterm.log"));
        assert!(evidence.sentence().contains("killed"));
        assert!(
            take_kill_evidence_in(&dir, own, None).is_none(),
            "consumed: it posts exactly once"
        );
        assert!(live.exists(), "a live owner's marker is not evidence");
        // SAFETY: closes the descriptor `create_locked` returned, once.
        unsafe { libc::close(fd) };
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn queued_logger(path: &Path, cap: usize) -> &'static FileLogger {
        let file = RotatingFile::open(path.to_path_buf(), RotationBudget::ATERM_LOG).unwrap();
        let logger: &'static FileLogger = Box::leak(Box::new(FileLogger::new(file, cap)));
        logger.pending.lock().unwrap().attended = true;
        logger
    }

    fn logged(logger: &FileLogger, n: usize) {
        logger.write_record(aterm_log::Level::Info, "t", &format!("line {n}"));
    }

    fn bodies(path: &Path) -> Vec<String> {
        read(path)
            .lines()
            .map(|l| l.rsplit_once(": ").map_or(l, |(_, body)| body).to_string())
            .collect()
    }

    fn lines(range: std::ops::RangeInclusive<usize>) -> Vec<String> {
        range.map(|n| format!("line {n}")).collect()
    }

    /// Exit can hand logging back before the newly spawned writer takes over.
    #[cfg(unix)]
    #[test]
    fn an_exit_handoff_cannot_be_reversed_by_a_late_writer() {
        let dir = Scratch::new("terminal-handoff");
        let path = dir.log();
        let logger = FileLogger::new(
            RotatingFile::open(path.clone(), RotationBudget::ATERM_LOG).unwrap(),
            LOG_QUEUE_BYTES,
        );
        logged(&logger, 0);
        assert_eq!(bodies(&path), lines(0..=0), "synchronous before takeover");
        assert!(logger.attend(), "an ordinary writer takes the queue over");
        logged(&logger, 1);
        assert_eq!(bodies(&path), lines(0..=0), "the writer's line queued");
        logger.unattend();
        assert!(logger.flush_within(aterm_spec::wait::MUST_HAPPEN));
        logged(&logger, 2);
        assert_eq!(bodies(&path), lines(0..=2), "exit drains and hands back");

        let late_path = dir.0.path().join("late.log");
        let late = FileLogger::new(
            RotatingFile::open(late_path.clone(), RotationBudget::ATERM_LOG).unwrap(),
            LOG_QUEUE_BYTES,
        );
        late.unattend();
        let _ = late.attend();
        logged(&late, 3);
        assert_eq!(
            bodies(&late_path),
            lines(3..=3),
            "EXIT_HANDOFF_MUST_NOT_REATTEND_THE_LOG_QUEUE"
        );
    }

    #[test]
    fn a_record_returns_while_the_file_is_held_and_lands_in_order() {
        let dir = Scratch::new("held");
        let file = RotatingFile::open(dir.log(), RotationBudget::ATERM_LOG).unwrap();
        let logger: &'static FileLogger =
            Box::leak(Box::new(FileLogger::new(file, LOG_QUEUE_BYTES)));
        logger.start_writer().expect("the writer starts");
        let took_over = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(1),
            || logger.pending.lock().unwrap().attended.then_some(()),
        );
        assert!(took_over.is_some(), "the writer took the queue over");
        let held = logger.file.lock().unwrap();
        let (done, returned) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for n in 1..=3 {
                logged(logger, n);
            }
            let _ = done.send(());
        });
        returned
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("three records returned with the file held");
        assert_eq!(read(&dir.log()), "", "nothing reached the held file");
        drop(held);
        assert!(logger.flush_within(aterm_spec::wait::MUST_HAPPEN));
        assert_eq!(bodies(&dir.log()), lines(1..=3));
    }

    #[test]
    fn a_full_queue_is_appended_in_order_by_the_thread_that_overflows_it() {
        let dir = Scratch::new("full");
        let one = format_record(
            epoch_ms_now(),
            std::process::id(),
            aterm_log::Level::Info,
            "t",
            "line 1",
        )
        .len();
        let logger = queued_logger(&dir.log(), 3 * one + 2);
        for n in 1..=3 {
            logged(logger, n);
        }
        assert_eq!(read(&dir.log()), "", "three lines fit the queue");
        logged(logger, 4);
        assert_eq!(
            bodies(&dir.log()),
            lines(1..=4),
            "the overflow lands all, in order"
        );
        logged(logger, 5);
        assert_eq!(bodies(&dir.log()), lines(1..=4), "the queue took the next");
        assert!(logger.flush_within(aterm_spec::wait::MUST_HAPPEN));
        assert_eq!(bodies(&dir.log()), lines(1..=5));
    }

    #[test]
    fn a_flush_is_bounded_and_lands_the_queue() {
        let dir = Scratch::new("flush");
        let logger = queued_logger(&dir.log(), LOG_QUEUE_BYTES);
        logged(logger, 1);
        let held = logger.file.lock().unwrap();
        assert!(
            !logger.flush_within(Duration::from_millis(5)),
            "the file was held"
        );
        drop(held);
        assert_eq!(read(&dir.log()), "", "still queued");
        assert!(logger.flush_within(aterm_spec::wait::MUST_HAPPEN));
        assert_eq!(bodies(&dir.log()), lines(1..=1));
    }

    /// Landed at the end: a process that `exit`s with a line still queued
    /// (its writer never ran) finds it on disk, through the `exit(3)`
    /// handler; and a line logged after that handler ran (here by a handler
    /// registered before it, so run after it) is appended by its own thread,
    /// not queued for the writer the process is losing.
    #[cfg(unix)]
    #[test]
    fn an_exit_lands_the_lines_still_queued() {
        const STAGE: &str = "ATERM_TEST_LOG_EXIT_PATH";
        extern "C" fn logs_after_the_flush() {
            if let Some(logger) = FILE_LOGGER.get() {
                logged(logger, 2);
            }
        }
        unsafe extern "C" {
            fn atexit(cb: extern "C" fn()) -> libc::c_int;
        }
        if let Some(path) = std::env::var_os(STAGE) {
            let logger = queued_logger(Path::new(&path), LOG_QUEUE_BYTES);
            assert!(
                FILE_LOGGER.set(logger).is_ok(),
                "this process's only logger"
            );
            // SAFETY: a plain `extern "C"` function with no captured state.
            assert_eq!(unsafe { atexit(logs_after_the_flush) }, 0, "atexit");
            flush_at_exit();
            logged(logger, 1);
            assert_eq!(read(Path::new(&path)), "", "queued at the exit");
            std::process::exit(0);
        }
        let dir = Scratch::new("exit");
        let mut helper = crate::test_alone::StandIn::this_test();
        helper.command().env(STAGE, dir.log());
        let ran = helper.spawn().selected();
        assert_eq!(ran.status.and_then(|s| s.code()), Some(0), "{}", ran.stderr);
        assert_eq!(bodies(&dir.log()), lines(1..=2));
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

    #[test]
    fn format_record_keeps_more_of_a_warning() {
        let body = "x".repeat(900);
        let warn = format_record(0, 1, aterm_log::Level::Warn, "t", &body);
        assert!(warn.contains(&body), "a 900-byte warning is kept whole");
        let info = format_record(0, 1, aterm_log::Level::Info, "t", &body);
        assert!(info.trim_end().ends_with('…'));
        assert!(info.len() < 600);
    }

    #[test]
    fn crash_evidence_keeps_only_the_newest_shown_reports() {
        let dir = Scratch::new("prune");
        let base = SystemTime::now() - Duration::from_secs(3600);
        for i in 0..12_u64 {
            let p = dir.0.path().join(format!("crash-{i}.log.seen"));
            std::fs::write(&p, b"old report").unwrap();
            File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(base + Duration::from_secs(i * 60))
                .unwrap();
        }
        let fresh = dir.0.path().join("crash-99.log");
        std::fs::write(&fresh, b"panicked").unwrap();
        File::options()
            .write(true)
            .open(&fresh)
            .unwrap()
            .set_modified(base - Duration::from_secs(60))
            .unwrap();
        let evidence = take_crash_evidence_in(dir.0.path(), None).unwrap();
        assert!(evidence.path.ends_with("crash-99.log.seen"), "{evidence:?}");
        assert!(dir.0.path().join("crash-99.log.seen").exists());
        let mut left: Vec<String> = std::fs::read_dir(dir.0.path())
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

/// Tier-1 for `aterm_models_misc::log_rotation_model`: two real [`RotatingFile`]s on
/// one path (A, and B which restarts) driven through writes, looks, ticks, rotations,
/// follows, a look that follows and rotates at once, and a write into the older copy.
#[cfg(all(test, unix))]
mod rotation_conformance {
    use super::*;
    use aterm_models_misc::log_rotation_model;
    use aterm_spec::derive::Model;
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
        h.write('B');
        assert_eq!(h.project()["ga"], 1);
        h.tick();
        h.write('A');
        h.restart_b();
        assert_eq!(h.aged, 3, "the first older copy was retired");
        h.tick();
        h.write('B');
        h.restart_b();
        h.write('A');
        h.write('A');
        assert_eq!(h.project()["gb"], 1);
        h.write('B');
        assert_eq!(h.project()["old"], 3, "a late line joins the older copy");
        h.tick();
        h.write('B');
        h.write('A');
        assert_eq!(h.project()["gb"], 1);
        h.tick();
        h.write('A');
        let aged = h.aged;
        h.write('B');
        assert_eq!((h.project()["gb"], h.project()["ga"]), (0, 1));
        assert!(h.aged > aged, "the look that followed also rotated");
        assert_eq!(h.written, 13);
        let s = h.project();
        assert_eq!(s["live"] + s["old"] + s["aged"], 13, "{s:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_truncating_start_this_replaced_is_refused() {
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
