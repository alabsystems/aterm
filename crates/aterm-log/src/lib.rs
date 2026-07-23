// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Minimal logging facade for aterm.
//!
//! Drop-in replacement for the `log` crate with zero external dependencies.
//! Provides five log levels (`error`, `warn`, `info`, `debug`, `trace`) and
//! corresponding macros. A global logger can be installed at program startup;
//! if none is set, log messages are silently discarded.
//!
//! ## Usage
//!
//! ```rust
//! use aterm_log::{info, warn, error, debug, trace};
//!
//! info!("server started on port {}", 8080);
//! warn!("connection pool at {}% capacity", 90);
//! error!("failed to bind: {}", "address in use");
//! debug!("parsed {} bytes", 1024);
//! trace!("entering function");
//! ```
//!
//! ## Installing a logger
//!
//! ```rust
//! use aterm_log::{Log, Level, LevelFilter, Metadata, Record, set_logger, set_max_level};
//!
//! struct StderrLogger;
//!
//! impl Log for StderrLogger {
//!     fn enabled(&self, metadata: &Metadata<'_>) -> bool {
//!         metadata.level() <= Level::Info
//!     }
//!     fn log(&self, record: &Record<'_>) {
//!         if self.enabled(&record.metadata()) {
//!             eprintln!("[{}] {}: {}", record.level(), record.target(), record.args());
//!         }
//!     }
//!     fn flush(&self) {}
//! }
//!
//! static LOGGER: StderrLogger = StderrLogger;
//!
//! set_logger(&LOGGER).expect("logger already set");
//! set_max_level(LevelFilter::Info);
//! ```

#![deny(clippy::all)]
#![deny(unsafe_op_in_unsafe_fn)]
// Under the Trust verifier, register the `trust` tool namespace so the
// `#[cfg_attr(trust_verify, trust::skip)]` opt-out on `__log` resolves; plain
// rustc never sets `trust_verify`, so this is inert off-Trust.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

use std::borrow::Cow;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};

// ── Log levels ──────────────────────────────────────────────────────────────

/// Log severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(usize)]
pub enum Level {
    /// Errors that require attention.
    Error = 1,
    /// Potentially harmful situations.
    Warn = 2,
    /// Informational messages.
    Info = 3,
    /// Detailed debugging information.
    Debug = 4,
    /// Very verbose tracing information.
    Trace = 5,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Error => f.write_str("ERROR"),
            Self::Warn => f.write_str("WARN"),
            Self::Info => f.write_str("INFO"),
            Self::Debug => f.write_str("DEBUG"),
            Self::Trace => f.write_str("TRACE"),
        }
    }
}

/// Filter for log levels. `Off` disables all logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(usize)]
pub enum LevelFilter {
    /// No messages.
    Off = 0,
    /// Only errors.
    Error = 1,
    /// Errors and warnings.
    Warn = 2,
    /// Errors, warnings, and info.
    Info = 3,
    /// Errors, warnings, info, and debug.
    Debug = 4,
    /// All messages.
    Trace = 5,
}

impl LevelFilter {
    /// Parse an `ATERM_LOG`-style level name (`off`, `error`, `warn`, `info`,
    /// `debug`, `trace`), ASCII case-insensitively, ignoring surrounding
    /// whitespace. Returns `None` for anything else so callers choose their
    /// own default.
    ///
    /// ```rust
    /// use aterm_log::LevelFilter;
    /// assert_eq!(LevelFilter::parse("Warn"), Some(LevelFilter::Warn));
    /// assert_eq!(LevelFilter::parse("verbose"), None);
    /// ```
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        // A flat sequence of ASCII-case-insensitive comparisons against string
        // literals. This is exactly a `.iter().find()` over a six-entry name/level
        // table, but written without materializing that `[(&str, LevelFilter); 6]`
        // const: iterating it forces an array-to-slice unsize coercion whose
        // fat-pointer element type carries the `LevelFilter` discriminant-index
        // metadata, which the verifier cannot lower. Each `eq_ignore_ascii_case`
        // lowers to plain byte comparisons with no pointer cast, so behavior is
        // identical (same names, same trimming, `None` otherwise).
        let s = s.trim();
        if s.eq_ignore_ascii_case("off") {
            Some(LevelFilter::Off)
        } else if s.eq_ignore_ascii_case("error") {
            Some(LevelFilter::Error)
        } else if s.eq_ignore_ascii_case("warn") {
            Some(LevelFilter::Warn)
        } else if s.eq_ignore_ascii_case("info") {
            Some(LevelFilter::Info)
        } else if s.eq_ignore_ascii_case("debug") {
            Some(LevelFilter::Debug)
        } else if s.eq_ignore_ascii_case("trace") {
            Some(LevelFilter::Trace)
        } else {
            None
        }
    }
}

// ── Host policy helpers ─────────────────────────────────────────────────────
// Pure decisions for hosts that install a file logger (rotation-lite and
// record hygiene). Kept engine-side so they are unit-testable without I/O.

/// Rotation-lite cap: a host truncates its log file at startup once it has
/// grown past this size.
pub const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;

/// Maximum bytes of one sanitized record body (see [`sanitize_record`]).
pub const MAX_RECORD_BYTES: usize = 512;

/// Whether a log file of `len` bytes should be truncated at startup.
#[must_use]
pub fn should_truncate(len: u64) -> bool {
    len > MAX_LOG_BYTES
}

/// Sanitize one formatted record body for a line-oriented log file.
///
/// Log messages embed caller-influenced text (requested paths, error
/// strings); raw control characters in them could forge record boundaries
/// (`\n`) or smuggle terminal escapes (`ESC]…`) to whoever `cat`s the log.
/// Every control character (C0, DEL, C1) is replaced with U+FFFD and the
/// result is capped near [`MAX_RECORD_BYTES`] (with a `…` marker) so a single
/// record cannot balloon the file. Clean short input is returned borrowed.
#[must_use]
pub fn sanitize_record(msg: &str) -> Cow<'_, str> {
    let clean = !msg.chars().any(char::is_control);
    if clean && msg.len() <= MAX_RECORD_BYTES {
        return Cow::Borrowed(msg);
    }
    // Capacity is a single compile-time constant so the verifier can bound the
    // reservation below its per-allocation ceiling: the loop below caps output at
    // `MAX_RECORD_BYTES` and appends at most a 3-byte `…`, so the owned string
    // never exceeds `MAX_RECORD_BYTES + 3`. Reserving `MAX_RECORD_BYTES + 4` is a
    // fixed 516-byte hint that only affects the reservation, not the bytes pushed,
    // so the returned value is byte-identical (a `.min(msg.len())` would only ever
    // under-reserve for tiny inputs — never change output). The `+ 4` is folded in
    // a `const` item so the function body carries no runtime add and the operand
    // reaching `with_capacity` is a plain 516 literal the verifier can bound.
    const OUT_CAPACITY: usize = MAX_RECORD_BYTES + 4;
    let mut out = String::with_capacity(OUT_CAPACITY);
    for c in msg.chars() {
        // Guard on the bytes ACTUALLY pushed. A control char becomes U+FFFD (3
        // bytes), not its own `len_utf8()` (1 byte for C0/DEL), so measuring the
        // original width would let a control char at the boundary push `out` past
        // `MAX_RECORD_BYTES + 3` and silently reallocate past the reservation.
        let (pushed, pushed_len) = if c.is_control() {
            ('\u{FFFD}', '\u{FFFD}'.len_utf8())
        } else {
            (c, c.len_utf8())
        };
        if out.len().saturating_add(pushed_len) > MAX_RECORD_BYTES {
            out.push('…');
            break;
        }
        out.push(pushed);
    }
    Cow::Owned(out)
}

// ── Metadata and Record ─────────────────────────────────────────────────────

/// Metadata about a log record (level and target).
#[derive(Debug)]
pub struct Metadata<'a> {
    level: Level,
    target: &'a str,
}

impl<'a> Metadata<'a> {
    /// The severity level.
    #[must_use]
    pub fn level(&self) -> Level {
        self.level
    }

    /// The target (typically the module path).
    #[must_use]
    pub fn target(&self) -> &'a str {
        self.target
    }
}

/// A single log record.
#[derive(Debug)]
pub struct Record<'a> {
    level: Level,
    target: &'a str,
    args: fmt::Arguments<'a>,
    file: Option<&'a str>,
    line: Option<u32>,
}

impl<'a> Record<'a> {
    /// The severity level.
    #[must_use]
    pub fn level(&self) -> Level {
        self.level
    }

    /// The target (typically the module path).
    #[must_use]
    pub fn target(&self) -> &'a str {
        self.target
    }

    /// The formatted log message.
    #[must_use]
    pub fn args(&self) -> &fmt::Arguments<'a> {
        &self.args
    }

    /// Source file, if available.
    #[must_use]
    pub fn file(&self) -> Option<&'a str> {
        self.file
    }

    /// Source line number, if available.
    #[must_use]
    pub fn line(&self) -> Option<u32> {
        self.line
    }

    /// Build metadata from this record.
    #[must_use]
    pub fn metadata(&self) -> Metadata<'a> {
        Metadata {
            level: self.level,
            target: self.target,
        }
    }
}

// ── Logger trait ─────────────────────────────────────────────────────────────

/// Trait for logger implementations.
pub trait Log: Send + Sync {
    /// Whether this logger is interested in a record at the given metadata.
    fn enabled(&self, metadata: &Metadata<'_>) -> bool;

    /// Log a record.
    fn log(&self, record: &Record<'_>);

    /// Flush any buffered output.
    fn flush(&self);
}

// ── Global state ─────────────────────────────────────────────────────────────

static MAX_LEVEL: AtomicUsize = AtomicUsize::new(0); // LevelFilter::Off
static LOGGER: std::sync::OnceLock<&'static dyn Log> = std::sync::OnceLock::new();

/// Set the global maximum log level.
pub fn set_max_level(level: LevelFilter) {
    MAX_LEVEL.store(level as usize, Ordering::Relaxed);
}

/// Get the current maximum log level.
#[must_use]
pub fn max_level() -> LevelFilter {
    match MAX_LEVEL.load(Ordering::Relaxed) {
        0 => LevelFilter::Off,
        1 => LevelFilter::Error,
        2 => LevelFilter::Warn,
        3 => LevelFilter::Info,
        4 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    }
}

/// Install a global logger. Returns `Err` if one is already installed.
///
/// # Errors
///
/// Returns `SetLoggerError` if a logger has already been set.
pub fn set_logger(logger: &'static dyn Log) -> Result<(), SetLoggerError> {
    LOGGER.set(logger).map_err(|_| SetLoggerError(()))
}

/// Error returned when `set_logger` is called more than once.
#[derive(Debug)]
pub struct SetLoggerError(());

impl fmt::Display for SetLoggerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("logger already set")
    }
}

impl std::error::Error for SetLoggerError {}

/// Log a record. Called by the macros; not intended for direct use.
#[doc(hidden)]
// Panic-freedom here is inherently unprovable in this crate: the body dispatches
// through `LOGGER: OnceLock<&dyn Log>` (std absent-callee `OnceLock::get`) into a
// runtime-chosen `dyn Log::{enabled,log}` impl. Both the OnceLock read and the
// virtual calls are total in every real logger, but the fail-closed verifier
// cannot see their bodies. The concrete logger impl's safety is established by its
// OWN verification, so this is the textbook `#[trust::skip]` case — and with the
// native-bundle skip-exclusion fix, callers (every `log!` site) demote this to a
// non-fatal expected-absent-callee assumption instead of inheriting a fatal row.
#[cfg_attr(trust_verify, trust::skip)]
pub fn __log(
    level: Level,
    target: &str,
    args: fmt::Arguments<'_>,
    file: Option<&str>,
    line: Option<u32>,
) {
    if (level as usize) > MAX_LEVEL.load(Ordering::Relaxed) {
        return;
    }
    if let Some(logger) = LOGGER.get() {
        let metadata = Metadata { level, target };
        if !logger.enabled(&metadata) {
            return;
        }
        let record = Record {
            level,
            target,
            args,
            file,
            line,
        };
        logger.log(&record);
    }
}

// ── Macros ──────────────────────────────────────────────────────────────────

/// Log at the error level.
#[macro_export]
macro_rules! error {
    ($($arg:tt)*) => {
        $crate::__log(
            $crate::Level::Error,
            ::core::module_path!(),
            ::core::format_args!($($arg)*),
            ::core::option::Option::Some(::core::file!()),
            ::core::option::Option::Some(::core::line!()),
        )
    };
}

/// Log at the warn level.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        $crate::__log(
            $crate::Level::Warn,
            ::core::module_path!(),
            ::core::format_args!($($arg)*),
            ::core::option::Option::Some(::core::file!()),
            ::core::option::Option::Some(::core::line!()),
        )
    };
}

/// Log at the info level.
#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => {
        $crate::__log(
            $crate::Level::Info,
            ::core::module_path!(),
            ::core::format_args!($($arg)*),
            ::core::option::Option::Some(::core::file!()),
            ::core::option::Option::Some(::core::line!()),
        )
    };
}

/// Log at the debug level.
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        $crate::__log(
            $crate::Level::Debug,
            ::core::module_path!(),
            ::core::format_args!($($arg)*),
            ::core::option::Option::Some(::core::file!()),
            ::core::option::Option::Some(::core::line!()),
        )
    };
}

/// Log at the trace level.
#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {
        $crate::__log(
            $crate::Level::Trace,
            ::core::module_path!(),
            ::core::format_args!($($arg)*),
            ::core::option::Option::Some(::core::file!()),
            ::core::option::Option::Some(::core::line!()),
        )
    };
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Note: since set_logger can only be called once per process and tests
    // run in the same process, we test the internal __log function directly
    // and the macro expansion separately.

    // `max_level` is a process-global singleton. Tests that both MUTATE it and
    // ASSERT on its value race each other under the default parallel test
    // runner (one test's `set_max_level` clobbers another's between its set and
    // its assert). Serialize exactly those tests on this lock. Tests that only
    // set the level without asserting it (so they can't observe a race) don't
    // need it, but taking the lock is harmless. Uses the std Mutex — no new dep.
    // `.unwrap_or_else(|e| e.into_inner())` so a panic in one guarded test
    // (which poisons the lock) doesn't cascade into spurious failures in the
    // others; we only need mutual exclusion, not poison propagation.
    static LEVEL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_level_ordering() {
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
        assert!(Level::Debug < Level::Trace);
    }

    #[test]
    fn test_level_display() {
        assert_eq!(Level::Error.to_string(), "ERROR");
        assert_eq!(Level::Warn.to_string(), "WARN");
        assert_eq!(Level::Info.to_string(), "INFO");
        assert_eq!(Level::Debug.to_string(), "DEBUG");
        assert_eq!(Level::Trace.to_string(), "TRACE");
    }

    #[test]
    fn test_level_filter_ordering() {
        assert!(LevelFilter::Off < LevelFilter::Error);
        assert!(LevelFilter::Error < LevelFilter::Warn);
        assert!(LevelFilter::Warn < LevelFilter::Info);
        assert!(LevelFilter::Info < LevelFilter::Debug);
        assert!(LevelFilter::Debug < LevelFilter::Trace);
    }

    #[test]
    fn test_max_level_default_is_off() {
        let _guard = LEVEL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Verify the round-trip for max_level.
        set_max_level(LevelFilter::Debug);
        assert_eq!(max_level(), LevelFilter::Debug);
        set_max_level(LevelFilter::Off);
        assert_eq!(max_level(), LevelFilter::Off);
    }

    #[test]
    fn test_record_fields() {
        let record = Record {
            level: Level::Info,
            target: "my_module",
            args: format_args!("hello {}", 42),
            file: Some("lib.rs"),
            line: Some(10),
        };
        assert_eq!(record.level(), Level::Info);
        assert_eq!(record.target(), "my_module");
        assert_eq!(record.file(), Some("lib.rs"));
        assert_eq!(record.line(), Some(10));
    }

    #[test]
    fn test_metadata_from_record() {
        let record = Record {
            level: Level::Warn,
            target: "test",
            args: format_args!(""),
            file: None,
            line: None,
        };
        let meta = record.metadata();
        assert_eq!(meta.level(), Level::Warn);
        assert_eq!(meta.target(), "test");
    }

    #[test]
    fn test_log_below_max_level_is_noop() {
        let _guard = LEVEL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // With max level Off, calling __log should not panic even without a logger
        set_max_level(LevelFilter::Off);
        __log(Level::Error, "test", format_args!("boom"), None, None);
        // If we get here without panic, the test passes.
    }

    #[test]
    fn test_set_logger_error_display() {
        let err = SetLoggerError(());
        assert_eq!(err.to_string(), "logger already set");
    }

    #[test]
    fn test_parse_level_filter_names() {
        assert_eq!(LevelFilter::parse("off"), Some(LevelFilter::Off));
        assert_eq!(LevelFilter::parse("error"), Some(LevelFilter::Error));
        assert_eq!(LevelFilter::parse("warn"), Some(LevelFilter::Warn));
        assert_eq!(LevelFilter::parse("info"), Some(LevelFilter::Info));
        assert_eq!(LevelFilter::parse("debug"), Some(LevelFilter::Debug));
        assert_eq!(LevelFilter::parse("trace"), Some(LevelFilter::Trace));
    }

    #[test]
    fn test_parse_level_filter_case_and_whitespace() {
        assert_eq!(LevelFilter::parse("INFO"), Some(LevelFilter::Info));
        assert_eq!(LevelFilter::parse("  Warn\n"), Some(LevelFilter::Warn));
        assert_eq!(LevelFilter::parse("OfF"), Some(LevelFilter::Off));
    }

    #[test]
    fn test_parse_level_filter_rejects_junk() {
        assert_eq!(LevelFilter::parse(""), None);
        assert_eq!(LevelFilter::parse("verbose"), None);
        assert_eq!(LevelFilter::parse("info,debug"), None);
        assert_eq!(LevelFilter::parse("3"), None);
    }

    #[test]
    fn test_should_truncate_threshold() {
        assert!(!should_truncate(0));
        assert!(!should_truncate(MAX_LOG_BYTES));
        assert!(should_truncate(MAX_LOG_BYTES + 1));
        assert!(should_truncate(u64::MAX));
    }

    #[test]
    fn test_sanitize_record_clean_input_is_borrowed() {
        let msg = "DENIED: control_socket::auth in Standard mode";
        assert!(matches!(sanitize_record(msg), Cow::Borrowed(m) if m == msg));
    }

    #[test]
    fn test_sanitize_record_replaces_control_characters() {
        // ESC (escape injection), newline (record forgery), DEL, C1 CSI.
        let msg = "path '\x1b]0;evil\x07\nFAKE\u{7f}\u{9b}'";
        let out = sanitize_record(msg);
        assert!(!out.chars().any(char::is_control));
        assert_eq!(
            out,
            "path '\u{fffd}]0;evil\u{fffd}\u{fffd}FAKE\u{fffd}\u{fffd}'"
        );
    }

    #[test]
    fn test_sanitize_record_caps_length() {
        let long = "a".repeat(MAX_RECORD_BYTES * 2);
        let out = sanitize_record(&long);
        assert!(out.len() <= MAX_RECORD_BYTES + '…'.len_utf8());
        assert!(out.ends_with('…'));
    }

    #[test]
    fn test_sanitize_record_cap_respects_char_boundaries() {
        // Multibyte chars straddling the cap must not split a code point.
        let long = "é".repeat(MAX_RECORD_BYTES); // 2 bytes each
        let out = sanitize_record(&long);
        assert!(out.len() <= MAX_RECORD_BYTES + '…'.len_utf8());
        assert!(out.ends_with('…'));
        assert!(out.trim_end_matches('…').chars().all(|c| c == 'é'));
    }

    #[test]
    fn test_sanitize_record_control_char_at_boundary_stays_within_reservation() {
        // A C0 control char is 1 byte but is pushed as U+FFFD (3 bytes). With the
        // cap guard measuring the ORIGINAL width, a control char landing at the
        // byte boundary would push `out` to MAX_RECORD_BYTES + 5 (past the reserved
        // MAX_RECORD_BYTES + 4) and silently reallocate. Guarding on the pushed
        // width keeps the owned string within MAX_RECORD_BYTES + '…'.len_utf8().
        let msg = "a".repeat(MAX_RECORD_BYTES - 1) + "\u{01}" + "a";
        let out = sanitize_record(&msg);
        assert!(out.len() <= MAX_RECORD_BYTES + '…'.len_utf8());
    }

    #[test]
    fn test_macros_compile() {
        let _guard = LEVEL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Verify all macros expand without errors.
        // Without a logger installed, these are noops.
        set_max_level(LevelFilter::Trace);
        error!("e {}", 1);
        warn!("w {}", 2);
        info!("i {}", 3);
        debug!("d {}", 4);
        trace!("t {}", 5);
    }
}
