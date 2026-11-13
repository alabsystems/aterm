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
        "objc: contained {} ({}) in Objective-C method `{}` (containment #{}): {}",
        e.name,
        e.class,
        e.method,
        e.ordinal,
        e.reason
    );
    // One bounded log record cannot carry the collector's full native stack.
    // Keep each captured frame independent so a long reason or early framework
    // frames cannot discard the later application frames.
    if !e.call_stack.is_empty() {
        for (index, frame) in e.call_stack.split(" | ").enumerate() {
            aterm_log::error!(
                "objc: containment #{} stack frame {}: {}",
                e.ordinal,
                index,
                frame
            );
        }
    }
    // A fatal callback aborts as soon as this sink returns, before the writer
    // necessarily runs. Give its diagnostic the same bounded drain as a panic.
    let _ = flush_within(LOG_FLUSH_EXIT_WAIT);
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

/// The Unix event time in a panic report's opening line, including its fractional second.
/// Malformed or unrepresentable timestamps leave the artifact's time unknown.
pub(crate) fn panic_report_timestamp(first: &str) -> Option<Duration> {
    let first = first.strip_prefix("aterm-gui ")?;
    let at = first.split_once(" crashed at unix ")?.1;
    let token = at.split_whitespace().next()?;
    let (seconds, fraction) = token
        .split_once('.')
        .map_or((token, None), |(seconds, fraction)| {
            (seconds, Some(fraction))
        });
    if seconds.is_empty() || !seconds.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let nanos = match fraction {
        None => 0,
        Some(fraction) => {
            if fraction.is_empty()
                || fraction.len() > 9
                || !fraction.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            let digits = u32::try_from(fraction.len()).ok()?;
            fraction
                .parse::<u32>()
                .ok()?
                .checked_mul(10_u32.pow(9 - digits))?
        }
    };
    Some(Duration::new(seconds.parse().ok()?, nanos))
}

/// What a panic report's first line says of the run that filed it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReportHead {
    /// It panicked on the main thread ([`MAIN_THREAD_PANIC`]).
    pub(crate) main_thread: bool,
    /// The start its run's crash marker is named for ([`REPORT_RUN`]); `None` when it names none.
    pub(crate) run: Option<u128>,
}

#[cfg(unix)]
pub(crate) fn open_regular_nonblock_with_meta(
    path: &Path,
) -> std::io::Result<(std::fs::File, std::fs::Metadata)> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok((file, meta))
}

#[cfg(not(unix))]
pub(crate) fn open_regular_nonblock_with_meta(
    path: &Path,
) -> std::io::Result<(std::fs::File, std::fs::Metadata)> {
    let file = std::fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok((file, meta))
}

pub(crate) fn open_regular_nonblock(path: &Path) -> std::io::Result<std::fs::File> {
    open_regular_nonblock_with_meta(path).map(|(file, _)| file)
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
    let Ok(file) = open_regular_nonblock(path) else {
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
    let Ok((file, meta)) = open_regular_nonblock_with_meta(path) else {
        return Vec::new();
    };
    let cap = usize::try_from(meta.len().min(CRASH_HEAD_BYTES).saturating_add(1)).unwrap_or(0);
    let mut buf = Vec::with_capacity(cap);
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
    let pid = body.split('-').next()?;
    (!pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())).then_some(())?;
    pid.parse().ok()
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
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Rotation::Kept;
    };
    if !meta.is_file() {
        return Rotation::Kept;
    }
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
        let rotated = std::fs::symlink_metadata(&path)
            .is_ok_and(|m| m.is_file() && budget.oversized(m.len()))
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
        let on_disk = std::fs::symlink_metadata(&self.path)
            .ok()
            .filter(std::fs::Metadata::is_file)
            .and_then(|m| file_id(&m));
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
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
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
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    f.set_len(0)?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
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
        let now = Instant::now();
        let deadline = now
            .checked_add(within)
            .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 3600));
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
#[path = "logging_tests.rs"]
mod tests;

/// Tier-1 for `aterm_models_misc::log_rotation_model`: two real [`RotatingFile`]s on
/// one path (A, and B which restarts) driven through writes, looks, ticks, rotations,
/// follows, a look that follows and rotates at once, and a write into the older copy.
#[cfg(all(test, unix))]
#[path = "logging_rotation_conformance_tests.rs"]
mod rotation_conformance;
