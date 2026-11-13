// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Minimal logging facade for aterm.
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

#![cfg_attr(trust_verify, feature(register_tool))]
#![cfg_attr(trust_verify, register_tool(trust))]

use std::borrow::Cow;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
    #[inline]
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
    #[inline]
    #[must_use]
    pub const fn is_error_or_warn(self) -> bool {
        matches!(self, Self::Error | Self::Warn)
    }

    /// Return the numeric rank (`1 = Error` .. `5 = Trace`).
    #[inline]
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
#[inline]
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
#[inline]
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

/// Filter for log levels.
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
    /// Parse an `ATERM_LOG`-style level name (`off`, `error`, `warn`, `info`, `debug`, `trace`), ASCII case-insensitively, ignoring surrounding Unicode whitespace.
    ///
    /// ```rust
    /// use aterm_log::LevelFilter;
    /// assert_eq!(LevelFilter::parse("Warn"), Some(LevelFilter::Warn));
    /// assert_eq!(LevelFilter::parse("verbose"), None);
    /// ```
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
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

/// The size one log file may reach before a host rotates it to `<name>.1` (keeping total log size near 8 MiB).
pub const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// Maximum bytes of one sanitized INFO, DEBUG or TRACE record body (see [`sanitize_record`]).
pub const MAX_RECORD_BYTES: usize = 512;

/// Maximum bytes of one sanitized WARN or ERROR record body (see [`sanitize_record_for`]).
pub const MAX_ALERT_RECORD_BYTES: usize = 1024;

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
    /// Whether a log file of `len` bytes has outgrown [`MAX_LOG_BYTES`] and should be rotated.
    #[must_use]
    pub fn should_rotate(len: u64) -> bool
    clauses [ensures result == (len > 4194304) by should_rotate_is_threshold]
    {
        len > MAX_LOG_BYTES
    }
}

verify_island! {
    clean {
        theorem should_rotate_is_threshold (len : UInt64) :
            trust_import_aterm_log__should_rotate len = decide (4194304 < len) := by
          simp only [trust_import_aterm_log__should_rotate]
    }
}

/// The body cap for a record at `level`: [`MAX_ALERT_RECORD_BYTES`] for WARN and ERROR, [`MAX_RECORD_BYTES`] for everything else.
#[inline]
#[must_use]
pub const fn record_cap(level: Level) -> usize {
    match level {
        Level::Error | Level::Warn => MAX_ALERT_RECORD_BYTES,
        Level::Info | Level::Debug | Level::Trace => MAX_RECORD_BYTES,
    }
}

/// Sanitize one formatted record body for a line-oriented log file.
/// Control characters map to U+FFFD, and a trailing `…` is appended only when the sanitized body exceeds [`MAX_RECORD_BYTES`].
#[inline]
#[must_use]
pub fn sanitize_record(msg: &str) -> Cow<'_, str> {
    sanitize_capped(msg, MAX_RECORD_BYTES)
}

/// [`sanitize_record`] with the cap for a record at `level` ([`record_cap`]).
#[inline]
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
    const OUT_CAPACITY: usize = MAX_ALERT_RECORD_BYTES + 4;
    let cap = if cap > MAX_ALERT_RECORD_BYTES {
        MAX_ALERT_RECORD_BYTES
    } else {
        cap
    };
    let mut out = String::with_capacity(OUT_CAPACITY);
    for c in msg.chars() {
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
    #[inline]
    #[must_use]
    pub const fn level(&self) -> Level {
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
    #[inline]
    #[must_use]
    pub const fn level(&self) -> Level {
        self.level
    }

    /// The target (typically the module path).
    #[inline]
    #[must_use]
    pub const fn target(&self) -> &'a str {
        self.target
    }

    /// The formatted log message.
    #[inline]
    #[must_use]
    pub const fn args(&self) -> &fmt::Arguments<'a> {
        &self.args
    }

    /// Source file, if available.
    #[inline]
    #[must_use]
    pub const fn file(&self) -> Option<&'a str> {
        self.file
    }

    /// Source line number, if available.
    #[inline]
    #[must_use]
    pub const fn line(&self) -> Option<u32> {
        self.line
    }

    /// Build metadata from this record.
    #[inline]
    #[must_use]
    pub const fn metadata(&self) -> Metadata {
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

static MAX_LEVEL: AtomicUsize = AtomicUsize::new(0);
static LOGGER: std::sync::OnceLock<&'static dyn Log> = std::sync::OnceLock::new();
/// Held while [`wait_for_logger`] reads whether a logger takes records, and while
/// [`set_logger`], [`set_max_level`] or [`settle_logger`] wakes it ([`READY`]), so no change
/// goes unseen.
static READY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static READY: std::sync::Condvar = std::sync::Condvar::new();
/// Set by [`settle_logger`]: the process has set up its logging, with a logger or without one.
static SETTLED: AtomicBool = AtomicBool::new(false);
/// How often a [`wait_for_logger`] parked on [`READY`], and how often one woke at its deadline,
/// for the test that must see one woken.
#[cfg(test)]
static PARKS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static TIMEOUTS: AtomicUsize = AtomicUsize::new(0);

/// Set the global maximum log level.
pub fn set_max_level(level: LevelFilter) {
    MAX_LEVEL.store(level as usize, Ordering::Relaxed);
    announce();
}

/// Install a global logger.
/// # Errors
/// Returns `SetLoggerError` if a logger has already been set.
pub fn set_logger(logger: &'static dyn Log) -> Result<(), SetLoggerError> {
    match LOGGER.set(logger) {
        Ok(()) => {
            announce();
            Ok(())
        }
        Err(_) => Err(SetLoggerError(())),
    }
}

/// Say this process has set up its logging, whether or not it installed a logger: each
/// [`wait_for_logger`] then answers at once, so a process that installs none, or sets the level
/// to `Off`, holds no waiter to its deadline.
pub fn settle_logger() {
    SETTLED.store(true, Ordering::Relaxed);
    announce();
}

/// Wake every [`wait_for_logger`]: a logger, the maximum level or [`SETTLED`] changed.
fn announce() {
    let _held = READY_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    READY.notify_all();
}

/// Whether a logger is installed and the maximum level lets a record reach it.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn takes_records() -> bool {
    LOGGER.get().is_some() && MAX_LEVEL.load(Ordering::Relaxed) > LevelFilter::Off as usize
}

/// Wait up to `within` until a logger is installed ([`set_logger`]) and the maximum level lets
/// records reach it ([`set_max_level`]), or until the process has set up its logging without
/// that ([`settle_logger`]); whether records reach a logger. A record made before both is
/// dropped, so a thread that starts before its process installs its logger can wait here first.
/// Absent on wasm32-unknown-unknown, which has no threads to wait for and no std clock.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
#[cfg_attr(trust_verify, trust::skip)]
#[must_use]
pub fn wait_for_logger(within: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now().checked_add(within);
    let mut held = READY_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        if takes_records() {
            return true;
        }
        if SETTLED.load(Ordering::Relaxed) {
            return false;
        }
        let left = match deadline {
            Some(deadline) => {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    return false;
                }
                Some(left)
            }
            None => None,
        };
        #[cfg(test)]
        PARKS.fetch_add(1, Ordering::SeqCst);
        held = match left {
            Some(left) => {
                let (held, waited) = READY
                    .wait_timeout(held, left)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                #[cfg(test)]
                if waited.timed_out() {
                    TIMEOUTS.fetch_add(1, Ordering::SeqCst);
                }
                let _ = waited;
                held
            }
            None => READY
                .wait(held)
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        };
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

/// Log a record.
#[doc(hidden)]
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

pub mod words;

/// Process-global environment mutation serialized through a single lock.
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

    /// Run `body` with `key` set to `value`, holding [`ENV_LOCK`] and restoring the prior state on return or unwind.
    pub fn scoped<T>(
        key: impl AsRef<OsStr>,
        value: impl AsRef<OsStr>,
        body: impl FnOnce() -> T,
    ) -> T {
        scoped_opt(key.as_ref(), Some(value.as_ref()), body)
    }

    /// Run `body` with `key` unset, holding [`ENV_LOCK`] and restoring the prior state on return or unwind.
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
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lib_env_tests.rs"]
mod env_tests;
