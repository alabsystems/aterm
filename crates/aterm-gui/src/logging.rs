// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Startup diagnostics: a private file logger for the `aterm_log` facade,
//! plus a crash-reporting panic hook.
//!
//! Without a logger installed every `aterm_log` record — including the
//! `containment_audit` security denials — is silently discarded. [`init`]
//! installs one writing to `~/Library/Logs/aterm/aterm.log` (macOS log
//! convention): a `0600` file in a `0700` dir, same posture as the control
//! socket. The level comes from `$ATERM_LOG` (`off|error|warn|info|debug|
//! trace`), default `info`; rotation-lite truncates the file at startup past
//! [`aterm_log::MAX_LOG_BYTES`].
//!
//! CONTENT SAFETY: terminal cell text, scrollback, and keystrokes are never
//! passed to `aterm_log` anywhere in the tree (call sites log indices, error
//! displays, uids, modes, denied paths — metadata only). Defense in depth on
//! top of that: every record body is run through the engine-side
//! [`aterm_log::sanitize_record`] so caller-influenced text (e.g. a denied
//! control-socket path) cannot forge records or smuggle terminal escapes.

use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use aterm_log::{LevelFilter, Log, Metadata, Record};

/// Install the panic hook and file logger, level from `$ATERM_LOG`.
///
/// Called first thing in `main`, before any thread spawns. Failures are
/// non-fatal (the terminal must still come up): they leave the facade in its
/// discard-everything default and say why on stderr.
pub fn init() {
    let Some(dir) = log_dir() else {
        eprintln!("aterm-gui: no private log dir (set HOME); logging + crash reports disabled");
        return;
    };
    // Crash reporting is independent of $ATERM_LOG — panics are always worth
    // an artifact, even with routine logging off. Arm BOTH crash paths: the Rust
    // panic hook (unwinds) and the async-signal-safe fatal-signal handler
    // (SIGSEGV/SIGABRT/… which bypass the panic machinery entirely, M6 CRASH-CORE).
    install_panic_hook(dir.clone());
    crate::crash_signal::install_signal_handlers();
    let level = std::env::var("ATERM_LOG")
        .ok()
        .and_then(|s| LevelFilter::parse(&s))
        .unwrap_or(LevelFilter::Info);
    if level == LevelFilter::Off {
        return;
    }
    let path = dir.join("aterm.log");
    let file = match open_log_file(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!(
                "aterm-gui: cannot open {}: {e}; logging disabled",
                path.display()
            );
            return;
        }
    };
    let logger: &'static FileLogger = Box::leak(Box::new(FileLogger {
        file: Mutex::new(file),
    }));
    if aterm_log::set_logger(logger).is_ok() {
        aterm_log::set_max_level(level);
    }
}

/// Route panics to `crash-<pid>.log` next to the main log: panic message +
/// backtrace + version + timestamp. The default hook only writes stderr,
/// which nobody sees for a windowed app — the crash file is the artifact
/// that survives the window vanishing. Chains to the previous (default)
/// hook and returns, so the unwind itself proceeds unchanged.
fn install_panic_hook(dir: PathBuf) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Allocation-light: the small path string and the captured backtrace
        // are the only buffers; the report streams straight to the fd.
        let path = dir.join(format!("crash-{}.log", std::process::id()));
        let _ = write_crash_report(&path, info, &std::backtrace::Backtrace::force_capture());
        eprintln!("aterm-gui: panic — crash report at {}", path.display());
        prev(info);
    }));
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
    let mut f = open_0600(path, true)?;
    writeln!(
        f,
        "aterm-gui {} crashed at unix {}.{:03}\n{info}\n\nbacktrace:\n{backtrace}",
        aterm_types::version::APP_VERSION,
        ts.as_secs(),
        ts.subsec_millis(),
    )
}

/// Resolve `~/Library/Logs/aterm`, created `0700` (owner-only, like the
/// control-socket dir — denial records name what a program attempted).
#[cfg(unix)]
pub(crate) fn log_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    let dir = PathBuf::from(home).join("Library/Logs/aterm");
    crate::control_auth::ensure_private_dir(&dir).ok()?;
    Some(dir)
}

/// Windows: `%LOCALAPPDATA%\aterm\logs` — the same per-user base dir the
/// socket seam uses — falling back through the home dir when `LOCALAPPDATA`
/// is unset. The per-user profile dir's default owner-only ACLs are the
/// confidentiality boundary here; POSIX `0700` semantics do not apply.
#[cfg(windows)]
pub(crate) fn log_dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| aterm_types::dirs::home_dir().map(|h| h.join("AppData").join("Local")))?;
    let dir = base.join("aterm").join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Open the log file `0600` for appending, truncating first when it has
/// outgrown [`aterm_log::MAX_LOG_BYTES`] (rotation-lite).
fn open_log_file(path: &Path) -> std::io::Result<File> {
    let oversized = std::fs::metadata(path).is_ok_and(|m| aterm_log::should_truncate(m.len()));
    open_0600(path, oversized)
}

/// Open `path` at mode `0600`, appending unless `truncate`. Mirrors
/// `snapshot_path::write_private`: restrictive perms BEFORE content lands,
/// and forced even when the file pre-existed (`OpenOptions::mode` only
/// applies on creation).
#[cfg(unix)]
fn open_0600(path: &Path, truncate: bool) -> std::io::Result<File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).mode(0o600);
    if truncate {
        opts.write(true).truncate(true);
    } else {
        opts.append(true);
    }
    let f = opts.open(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(f)
}

/// Non-unix twin of `open_0600`: plain create+append/truncate. There are no
/// POSIX mode bits here — the log lives in the per-user dir under
/// `%LOCALAPPDATA%`, whose default owner-only ACLs are the confidentiality
/// boundary.
#[cfg(not(unix))]
fn open_0600(path: &Path, truncate: bool) -> std::io::Result<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true);
    if truncate {
        opts.write(true).truncate(true);
    } else {
        opts.append(true);
    }
    opts.open(path)
}

/// Writes one sanitized line per record. Idle cost is zero: `aterm_log`
/// gates on the max-level atomic before any record reaches us.
struct FileLogger {
    file: Mutex<File>,
}

impl Log for FileLogger {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true // level gating already happened against the max-level atomic
    }

    fn log(&self, record: &Record<'_>) {
        let body = record.args().to_string();
        let line = format_record(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            record.level(),
            record.target(),
            &body,
        );
        let mut f = self.file.lock().unwrap_or_else(|e| e.into_inner());
        // Single write per record: `File` is unbuffered, so the line is
        // already durable — no interleaved fragments, nothing lost on crash.
        let _ = f.write_all(line.as_bytes());
    }

    fn flush(&self) {
        let _ = self.file.lock().unwrap_or_else(|e| e.into_inner()).flush();
    }
}

/// One log line: epoch-millis timestamp, level, target, sanitized body.
fn format_record(epoch_ms: u128, level: aterm_log::Level, target: &str, body: &str) -> String {
    format!(
        "{}.{:03} {} {}: {}\n",
        epoch_ms / 1000,
        epoch_ms % 1000,
        level,
        target,
        aterm_log::sanitize_record(body)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_auth::ensure_private_dir;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    // The mode-asserting tests are POSIX-only; append/rotation behavior is
    // covered on every platform below.
    #[cfg(unix)]
    #[test]
    fn log_file_is_0600_and_appends_across_opens() {
        let dir = std::env::temp_dir().join(format!("aterm-log-app-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let path = dir.join("aterm.log");
        open_log_file(&path).unwrap().write_all(b"first\n").unwrap();
        open_log_file(&path)
            .unwrap()
            .write_all(b"second\n")
            .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_log_is_truncated_on_open() {
        let dir = std::env::temp_dir().join(format!("aterm-log-rot-{}", std::process::id()));
        ensure_private_dir(&dir).unwrap();
        let path = dir.join("aterm.log");
        let f = open_log_file(&path).unwrap();
        #[cfg(unix)]
        f.set_len(aterm_log::MAX_LOG_BYTES + 1).unwrap(); // sparse: instant
        // Windows twin: the append-only log handle carries FILE_APPEND_DATA but
        // not FILE_WRITE_DATA, so SetEndOfFile on it is Access-denied — inflate
        // through a plain write handle instead (same sparse instant grow).
        #[cfg(windows)]
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(aterm_log::MAX_LOG_BYTES + 1)
            .unwrap();
        drop(f);
        let f = open_log_file(&path).unwrap();
        assert_eq!(f.metadata().unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
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

    #[test]
    fn format_record_sanitizes_and_terminates_line() {
        let line = format_record(
            1_700_000_000_123,
            aterm_log::Level::Warn,
            "containment_audit",
            "DENIED: image write '\x1b]0;x\n'",
        );
        assert_eq!(
            line,
            "1700000000.123 WARN containment_audit: DENIED: image write '\u{fffd}]0;x\u{fffd}'\n"
        );
    }
}
