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
//!     fn enabled(&self, metadata: &Metadata) -> bool {
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

// Under the Trust verifier, register the `trust` tool namespace so the
// `#[cfg_attr(trust_verify, trust::skip)]` opt-out on `__log` resolves; plain
// rustc never sets `trust_verify`, so this is inert off-Trust.
#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

use std::borrow::Cow;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};

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

impl Level {
    /// Return the uppercase string representation of this level.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }

    /// Return `true` when this level is `Error` or `Warn`.
    #[must_use]
    pub const fn is_error_or_warn(self) -> bool {
        matches!(self, Self::Error | Self::Warn)
    }

    /// Return the numeric rank (`1 = Error` .. `5 = Trace`).
    #[must_use]
    pub const fn rank(self) -> u8 {
        match self {
            Self::Error => 1,
            Self::Warn => 2,
            Self::Info => 3,
            Self::Debug => 4,
            Self::Trace => 5,
        }
    }
}

/// Encode a compact log record header (`timestamp_ns`, `level_rank`, `line`) into 16 LE bytes.
#[must_use]
pub const fn encode_log_header_le(timestamp_ns: u64, level_rank: u32, line: u32) -> [u8; 16] {
    [
        timestamp_ns as u8,
        (timestamp_ns >> 8) as u8,
        (timestamp_ns >> 16) as u8,
        (timestamp_ns >> 24) as u8,
        (timestamp_ns >> 32) as u8,
        (timestamp_ns >> 40) as u8,
        (timestamp_ns >> 48) as u8,
        (timestamp_ns >> 56) as u8,
        level_rank as u8,
        (level_rank >> 8) as u8,
        (level_rank >> 16) as u8,
        (level_rank >> 24) as u8,
        line as u8,
        (line >> 8) as u8,
        (line >> 16) as u8,
        (line >> 24) as u8,
    ]
}

/// Decode 16 LE bytes into a compact log record header (`timestamp_ns`, `level_rank`, `line`).
#[must_use]
pub const fn decode_log_header_le(bytes: [u8; 16]) -> (u64, u32, u32) {
    let ts = (bytes[0] as u64)
        | ((bytes[1] as u64) << 8)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 24)
        | ((bytes[4] as u64) << 32)
        | ((bytes[5] as u64) << 40)
        | ((bytes[6] as u64) << 48)
        | ((bytes[7] as u64) << 56);
    let lvl = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let line = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (ts, lvl, line)
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
        // An if-chain, not a lookup table: iterating a `[(&str, LevelFilter); 6]`
        // needs an array-to-slice unsize coercion the verifier cannot lower.
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

/// The size one log file may reach before a host rotates it to `<name>.1`
/// (keeping total log size near 8 MiB).
pub const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// Maximum bytes of one sanitized INFO, DEBUG or TRACE record body (see
/// [`sanitize_record`]).
pub const MAX_RECORD_BYTES: usize = 512;

/// Maximum bytes of one sanitized WARN or ERROR record body (see
/// [`sanitize_record_for`]).
pub const MAX_ALERT_RECORD_BYTES: usize = 1024;

// Emits native `ensures` clauses under `cfg(trust_verify)` and a plain fn on
// other compilers. Theorems refer to `trust_import_aterm_log__<fn>`.
#[cfg(trust_verify)]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret
            $($clauses)+
        { $($body)* }
    };
}

#[cfg(not(trust_verify))]
macro_rules! trust_contract_fn {
    (
        $(#[$meta:meta])*
        $vis:vis fn $name:ident($($arg:ident : $arg_ty:ty),* $(,)?) -> $ret:ident
        clauses [$($clauses:tt)+]
        { $($body:tt)* }
    ) => {
        $(#[$meta])*
        $vis fn $name($($arg: $arg_ty),*) -> $ret { $($body)* }
    };
}

#[cfg(trust_verify)]
macro_rules! verify_island {
    ($($tt:tt)*) => {
        $($tt)*
    };
}

#[cfg(not(trust_verify))]
macro_rules! verify_island {
    ($($tt:tt)*) => {};
}

trust_contract_fn! {
    /// Whether a log file of `len` bytes has outgrown [`MAX_LOG_BYTES`] and should
    /// be rotated.
    #[must_use]
    pub fn should_rotate(len: u64) -> bool
    clauses [ensures result == (len > 4194304) by should_rotate_is_threshold]
    {
        len > MAX_LOG_BYTES
    }
}

// Spec `should-rotate-threshold` (SPECS.md): for every length, rotation is due
// exactly past MAX_LOG_BYTES = 4 MiB. Kernel-checked against the compiled body.
verify_island! {
    clean {
        theorem should_rotate_is_threshold (len : UInt64) :
            trust_import_aterm_log__should_rotate len = decide (4194304 < len) := by
          simp only [trust_import_aterm_log__should_rotate]
    }
}

/// The body cap for a record at `level`: [`MAX_ALERT_RECORD_BYTES`] for WARN
/// and ERROR, [`MAX_RECORD_BYTES`] for everything else.
#[must_use]
pub fn record_cap(level: Level) -> usize {
    match level {
        Level::Error | Level::Warn => MAX_ALERT_RECORD_BYTES,
        Level::Info | Level::Debug | Level::Trace => MAX_RECORD_BYTES,
    }
}

/// Sanitize one formatted record body for a line-oriented log file.
///
/// Replaces control characters (C0, DEL, C1) with U+FFFD to prevent record
/// forgery (`\n`) or terminal escape injection (`ESC]…`), and caps length near
/// [`MAX_RECORD_BYTES`] with `…`. Clean short input is returned borrowed.
#[must_use]
pub fn sanitize_record(msg: &str) -> Cow<'_, str> {
    sanitize_capped(msg, MAX_RECORD_BYTES)
}

/// [`sanitize_record`] with the cap for a record at `level` ([`record_cap`]).
#[must_use]
pub fn sanitize_record_for(level: Level, msg: &str) -> Cow<'_, str> {
    sanitize_capped(msg, record_cap(level))
}

fn sanitize_capped(msg: &str, cap: usize) -> Cow<'_, str> {
    let mut clean = true;
    for c in msg.chars() {
        if c.is_control() {
            clean = false;
            break;
        }
    }
    if clean && msg.len() <= cap {
        return Cow::Borrowed(msg);
    }
    // Constant capacity so the verifier can bound the reservation (`cap` plus a 3-byte `…`).
    const OUT_CAPACITY: usize = MAX_ALERT_RECORD_BYTES + 4;
    let cap = if cap > MAX_ALERT_RECORD_BYTES {
        MAX_ALERT_RECORD_BYTES
    } else {
        cap
    };
    let mut out = String::with_capacity(OUT_CAPACITY);
    for c in msg.chars() {
        // Count the replacement's 3-byte width so 1-byte control chars cannot overrun `cap`.
        let (pushed, pushed_len) = if c.is_control() {
            ('\u{FFFD}', 3usize)
        } else {
            (c, c.len_utf8())
        };
        if out.len() > cap || cap - out.len() < pushed_len {
            out.push('…');
            break;
        }
        out.push(pushed);
    }
    Cow::Owned(out)
}

/// Metadata about a log record.
#[derive(Debug)]
pub struct Metadata {
    level: Level,
}

impl Metadata {
    /// The severity level.
    #[must_use]
    pub fn level(&self) -> Level {
        self.level
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
    pub fn metadata(&self) -> Metadata {
        Metadata { level: self.level }
    }
}

/// Trait for logger implementations.
pub trait Log: Send + Sync {
    /// Whether this logger is interested in a record at the given metadata.
    fn enabled(&self, metadata: &Metadata) -> bool;

    /// Log a record.
    fn log(&self, record: &Record<'_>);

    /// Flush any buffered output.
    fn flush(&self);
}

static MAX_LEVEL: AtomicUsize = AtomicUsize::new(0); // LevelFilter::Off
static LOGGER: std::sync::OnceLock<&'static dyn Log> = std::sync::OnceLock::new();

/// Set the global maximum log level.
pub fn set_max_level(level: LevelFilter) {
    MAX_LEVEL.store(level as usize, Ordering::Relaxed);
}

/// Install a global logger. Returns `Err` if one is already installed.
///
/// # Errors
///
/// Returns `SetLoggerError` if a logger has already been set.
pub fn set_logger(logger: &'static dyn Log) -> Result<(), SetLoggerError> {
    match LOGGER.set(logger) {
        Ok(()) => Ok(()),
        Err(_) => Err(SetLoggerError(())),
    }
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
// Skipped by the verifier: the body calls `OnceLock::get` and a runtime-chosen
// `dyn Log` impl, neither of which it can see. Each concrete logger is verified
// on its own, and `log!` call sites treat this as an expected absent callee.
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
        let metadata = Metadata { level };
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

/// Process-global environment mutation serialized through a single lock.
///
/// `set_var` and `remove_var` are `unsafe` because they race `getenv` on other
/// threads, and the Trust toolchain denies them via `env_mutation`. Routing all
/// in-process mutations through [`ENV_LOCK`] serializes our own writers and
/// lock-aware readers, though `getenv` in C libraries on other threads remains
/// unsynchronized; callers must still be single-threaded startup, launcher, or
/// test paths.
pub mod env {
    use std::ffi::{OsStr, OsString};
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Acquire [`ENV_LOCK`], recovering from poisoning. Named `env_lock` (not
    /// `lock`) so `aterm-census` registers it in `GUARD_HELPERS`.
    fn env_lock() -> MutexGuard<'static, ()> {
        match ENV_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn remove_locked(key: &OsStr) {
        // SAFETY: caller is a startup, launcher, or test path and holds `ENV_LOCK`.
        #[cfg_attr(
            any(trust_verify, clean_islands),
            allow(
                env_mutation,
                reason = "THE lock-scoped helper the env_mutation lint asks callers to route through; see the module docs for the bound this does and does not provide"
            )
        )]
        unsafe {
            std::env::remove_var(key)
        };
    }

    fn set_locked(key: &OsStr, value: &OsStr) {
        // SAFETY: as `remove_locked` — blessed mutation site, `ENV_LOCK` held.
        #[cfg_attr(
            any(trust_verify, clean_islands),
            allow(
                env_mutation,
                reason = "THE lock-scoped helper the env_mutation lint asks callers to route through; see the module docs for the bound this does and does not provide"
            )
        )]
        unsafe {
            std::env::set_var(key, value)
        };
    }

    /// Read `key` and remove it in one critical section under [`ENV_LOCK`].
    #[must_use]
    pub fn take(key: impl AsRef<OsStr>) -> Option<OsString> {
        let key = key.as_ref();
        let _guard = env_lock();
        let value = std::env::var_os(key);
        remove_locked(key);
        value
    }

    /// Remove `key` under [`ENV_LOCK`], discarding any value.
    pub fn unset(key: impl AsRef<OsStr>) {
        let _ = take(key);
    }

    /// Set `key` to `value` under [`ENV_LOCK`].
    pub fn set(key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
        let _guard = env_lock();
        set_locked(key.as_ref(), value.as_ref());
    }

    /// Run `body` with `key` set to `value`, holding [`ENV_LOCK`] and restoring
    /// the prior state on return or unwind. `body` must not re-enter this module.
    pub fn scoped<T>(
        key: impl AsRef<OsStr>,
        value: impl AsRef<OsStr>,
        body: impl FnOnce() -> T,
    ) -> T {
        scoped_opt(key.as_ref(), Some(value.as_ref()), body)
    }

    /// Run `body` with `key` unset, holding [`ENV_LOCK`] and restoring the prior
    /// state on return or unwind. `body` must not re-enter this module.
    pub fn scoped_unset<T>(key: impl AsRef<OsStr>, body: impl FnOnce() -> T) -> T {
        scoped_opt(key.as_ref(), None, body)
    }

    fn scoped_opt<T>(key: &OsStr, value: Option<&OsStr>, body: impl FnOnce() -> T) -> T {
        struct Restore {
            key: OsString,
            prev: Option<OsString>,
            _guard: MutexGuard<'static, ()>,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                match self.prev.take() {
                    Some(prev) => set_locked(&self.key, &prev),
                    None => remove_locked(&self.key),
                }
            }
        }

        let guard = env_lock();
        let prev = std::env::var_os(key);
        match value {
            Some(value) => set_locked(key, value),
            None => remove_locked(key),
        }
        let _restore = Restore {
            key: key.to_os_string(),
            prev,
            _guard: guard,
        };
        body()
    }

    #[cfg(test)]
    #[must_use]
    pub(super) fn read(key: impl AsRef<OsStr>) -> Option<OsString> {
        let _guard = env_lock();
        std::env::var_os(key.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Severity order is what `enabled` compares: a filter admits a level at
    /// or below it, and `Off` admits nothing.
    #[test]
    fn levels_and_filters_order_by_verbosity() {
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
        assert!(Level::Debug < Level::Trace);
        assert!(LevelFilter::Off < LevelFilter::Error);
        assert!(LevelFilter::Error < LevelFilter::Warn);
        assert!(LevelFilter::Warn < LevelFilter::Info);
        assert!(LevelFilter::Info < LevelFilter::Debug);
        assert!(LevelFilter::Debug < LevelFilter::Trace);
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
    fn parse_level_filter_takes_names_in_any_case_and_refuses_junk() {
        for (text, want) in [
            ("off", Some(LevelFilter::Off)),
            ("error", Some(LevelFilter::Error)),
            ("warn", Some(LevelFilter::Warn)),
            ("info", Some(LevelFilter::Info)),
            ("debug", Some(LevelFilter::Debug)),
            ("trace", Some(LevelFilter::Trace)),
            ("INFO", Some(LevelFilter::Info)),
            ("  Warn\n", Some(LevelFilter::Warn)),
            ("OfF", Some(LevelFilter::Off)),
            ("", None),
            ("verbose", None),
            ("info,debug", None),
            ("3", None),
        ] {
            assert_eq!(LevelFilter::parse(text), want, "{text:?}");
        }
    }

    #[test]
    fn test_warn_and_error_keep_the_longer_cap() {
        assert_eq!(record_cap(Level::Error), MAX_ALERT_RECORD_BYTES);
        assert_eq!(record_cap(Level::Warn), MAX_ALERT_RECORD_BYTES);
        assert_eq!(record_cap(Level::Info), MAX_RECORD_BYTES);
        assert_eq!(record_cap(Level::Debug), MAX_RECORD_BYTES);
        assert_eq!(record_cap(Level::Trace), MAX_RECORD_BYTES);

        let long = "w".repeat(MAX_ALERT_RECORD_BYTES * 2);
        let warn = sanitize_record_for(Level::Warn, &long);
        assert!(warn.len() > MAX_RECORD_BYTES + '…'.len_utf8());
        assert!(warn.len() <= MAX_ALERT_RECORD_BYTES + '…'.len_utf8());
        assert!(warn.ends_with('…'));
        let info = sanitize_record_for(Level::Info, &long);
        assert_eq!(info, sanitize_record(&long));
        assert!(info.len() <= MAX_RECORD_BYTES + '…'.len_utf8());

        // A WARN body between the two caps is kept whole, and borrowed.
        let mid = "m".repeat(MAX_RECORD_BYTES + 100);
        assert!(matches!(sanitize_record_for(Level::Warn, &mid), Cow::Borrowed(m) if m == mid));
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
        // A 1-byte control char at either cap is pushed as 3-byte U+FFFD.
        for (level, cap) in [
            (Level::Info, MAX_RECORD_BYTES),
            (Level::Error, MAX_ALERT_RECORD_BYTES),
        ] {
            let msg = "a".repeat(cap - 1) + "\u{01}" + "a";
            let out = sanitize_record_for(level, &msg);
            assert!(out.len() <= cap + '…'.len_utf8(), "{level}");
            assert!(!out.chars().any(char::is_control), "{level}");
        }
    }
}

#[cfg(test)]
mod env_tests {
    //! Each test uses a distinct key so parallel tests do not race on values.

    #[test]
    fn take_reads_and_removes_in_one_step() {
        const KEY: &str = "ATERM_LOG_TEST_TAKE";
        super::env::set(KEY, "authority");
        assert_eq!(super::env::read(KEY).as_deref(), Some("authority".as_ref()));
        assert_eq!(super::env::take(KEY).as_deref(), Some("authority".as_ref()));
        assert_eq!(super::env::take(KEY), None);
        assert_eq!(super::env::read(KEY), None);
    }

    #[test]
    fn scoped_restores_the_previous_value_and_the_previous_absence() {
        const KEY: &str = "ATERM_LOG_TEST_SCOPED";
        super::env::set(KEY, "outer");
        let seen = super::env::scoped(KEY, "inner", || std::env::var_os(KEY));
        assert_eq!(
            seen.as_deref(),
            Some("inner".as_ref()),
            "body sees the override"
        );
        assert_eq!(
            super::env::read(KEY).as_deref(),
            Some("outer".as_ref()),
            "and the previous value is back"
        );

        super::env::unset(KEY);
        let seen = super::env::scoped(KEY, "inner", || std::env::var_os(KEY));
        assert_eq!(seen.as_deref(), Some("inner".as_ref()));
        assert_eq!(
            super::env::read(KEY),
            None,
            "restoring an absent key means REMOVING it, not setting it empty"
        );
    }

    #[test]
    fn scoped_unset_hides_a_set_key_for_exactly_the_body() {
        const KEY: &str = "ATERM_LOG_TEST_SCOPED_UNSET";
        super::env::set(KEY, "present");
        let seen = super::env::scoped_unset(KEY, || std::env::var_os(KEY));
        assert_eq!(seen, None, "the body sees the key as absent");
        assert_eq!(super::env::read(KEY).as_deref(), Some("present".as_ref()));
        super::env::unset(KEY);
    }

    /// A panicking body still restores: a leaked override would corrupt every
    /// later test in the binary.
    #[test]
    fn scoped_restores_even_when_the_body_panics() {
        const KEY: &str = "ATERM_LOG_TEST_SCOPED_PANIC";
        super::env::set(KEY, "outer");
        let caught = std::panic::catch_unwind(|| {
            super::env::scoped(KEY, "inner", || panic!("body blew up"));
        });
        assert!(caught.is_err(), "the panic propagates to the caller");
        assert_eq!(
            super::env::read(KEY).as_deref(),
            Some("outer".as_ref()),
            "and the override did not leak past it"
        );
        super::env::unset(KEY);
    }
}
