// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `log` — aterm's first-party no-op replacement for the upstream `log` facade.
//! Every macro expands to nothing (`{}` or `false`) and [`max_level`] is `Off`.
//!
//! Published under the package name `log` (`crates/aterm-log-shim`) so
//! `[patch.crates-io]` redirects third-party `log` consumers in the dependency
//! graph (`wgpu*`, `naga`, `gpu-allocator`, `gl_generator`) to first-party code.
//!
//! # Why a no-op matches runtime behaviour
//!
//! 1. No logger crate is in the workspace dependency graph on any target cell.
//! 2. Nothing calls `log::set_logger` or `log::set_max_level` (all
//!    `set_max_level` calls in the workspace are on `aterm_log`, a separate
//!    crate).
//! 3. Therefore `log::max_level()` is `LevelFilter::Off` for the entire process
//!    lifetime, and disabled callsites format and evaluate nothing.
//! 4. **Delta on `log!` and `log_enabled!`:** upstream binds `let lvl = $lvl;`
//!    before checking `max_level()`. This crate discards the token tree, so
//!    `$lvl` is not evaluated. Every consumer site passes a pure `Copy` local,
//!    parameter, or constant.
//!
//! # Why this does not delegate to `crates/aterm-log`
//!
//! 1. **`max_level()` gates driver work.** `wgpu-hal` reads `log::max_level()`
//!    to build the `vk::DebugUtilsMessageSeverityFlagsEXT` bitmask and to gate
//!    an EGL config-enumeration loop. `aterm_log::max_level()` defaults to
//!    `Info`; forwarding to it would enable Vulkan validation callbacks per
//!    frame. [`max_level`] therefore reads this crate's own static, which is
//!    never written.
//! 2. **`Display for Level` honours width/alignment.** Upstream uses
//!    `fmt.pad(self.as_str())`, whereas `aterm_log` uses `write_str`.
//! 3. **`FromStr` semantics.** Upstream `FromStr` does not trim whitespace and
//!    rejects `"off"` for `Level` while accepting it for `LevelFilter`, with no
//!    inherent `LevelFilter::parse` method.
//! 4. **`#![no_std]`.** Consumers such as `naga` and `wgpu*` are `#![no_std]`
//!    and do not enable `log/std`.
//!
//! # Surface
//!
//! Only the pure level lattice ([`Level`], [`LevelFilter`],
//! [`ParseLevelError`]), [`max_level`], and the seven macros ([`error!`],
//! [`warn!`], [`info!`], [`debug!`], [`trace!`], [`log!`], [`log_enabled!`])
//! are exported. Logger-registration items (`Log`, `Record`, `Metadata`,
//! `set_logger`, `set_max_level`) are omitted so any consumer attempting to
//! install a `log` logger fails at compile time rather than silently dropping
//! logs. No `Result` or `Error` alias is exported because `gpu-allocator` pairs
//! `use log::*;` with `use crate::result::*;`.

#![no_std]

use core::cmp;
use core::fmt;
use core::str::FromStr;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Level names in discriminant order (`log-0.4.32/src/lib.rs`); the slice index
/// matches the enum discriminant for both [`Level`] and [`LevelFilter`].
static LOG_LEVEL_NAMES: [&str; 6] = ["OFF", "ERROR", "WARN", "INFO", "DEBUG", "TRACE"];

/// `Display` text of [`ParseLevelError`], matching upstream `log`.
static LEVEL_PARSE_ERROR: &str =
    "attempted to convert a string that doesn't match an existing log level";

/// Verbosity level of a log message.
///
/// Discriminants start at 1 while [`LevelFilter`] starts at 0 so that
/// `*self as usize` comparisons align across both types.
#[repr(usize)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Level {
    /// Designates very serious errors.
    Error = 1,
    /// Designates hazardous situations.
    Warn,
    /// Designates useful information.
    Info,
    /// Designates lower priority information.
    Debug,
    /// Designates very low priority, often extremely verbose, information.
    Trace,
}

impl Level {
    /// The most verbose logging level.
    #[must_use]
    #[inline]
    pub const fn max() -> Self {
        Self::Trace
    }

    /// Convert the `Level` to the equivalent [`LevelFilter`].
    #[must_use]
    #[inline]
    pub const fn to_level_filter(&self) -> LevelFilter {
        match self {
            Self::Error => LevelFilter::Error,
            Self::Warn => LevelFilter::Warn,
            Self::Info => LevelFilter::Info,
            Self::Debug => LevelFilter::Debug,
            Self::Trace => LevelFilter::Trace,
        }
    }

    /// The string representation of the `Level` (matching [`fmt::Display`]).
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }

    /// Iterate through all supported logging levels, most severe first.
    pub fn iter() -> impl Iterator<Item = Self> {
        [
            Self::Error,
            Self::Warn,
            Self::Info,
            Self::Debug,
            Self::Trace,
        ]
        .into_iter()
    }

    /// The next-higher-severity `Level`, saturating at [`Level::Trace`].
    #[must_use]
    #[inline]
    pub const fn increment_severity(&self) -> Self {
        match self {
            Self::Error => Self::Warn,
            Self::Warn => Self::Info,
            Self::Info => Self::Debug,
            Self::Debug | Self::Trace => Self::Trace,
        }
    }

    /// The next-lower-severity `Level`, saturating at [`Level::Error`].
    #[must_use]
    #[inline]
    pub const fn decrement_severity(&self) -> Self {
        match self {
            Self::Error | Self::Warn => Self::Error,
            Self::Info => Self::Warn,
            Self::Debug => Self::Info,
            Self::Trace => Self::Debug,
        }
    }
}

impl fmt::Display for Level {
    /// Uses `fmt.pad` so width, alignment, and precision formatters are honoured.
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.pad(self.as_str())
    }
}

impl FromStr for Level {
    type Err = ParseLevelError;

    /// ASCII case-insensitive, no whitespace trimming, and `"OFF"` is rejected
    /// (starts at index 1).
    fn from_str(level: &str) -> Result<Self, Self::Err> {
        for (idx, name) in LOG_LEVEL_NAMES.iter().enumerate().skip(1) {
            if name.eq_ignore_ascii_case(level) {
                return level_from_usize(idx).ok_or(ParseLevelError(()));
            }
        }
        Err(ParseLevelError(()))
    }
}

/// Verbosity level filter of the logger.
///
/// Directly comparable to [`Level`]; discriminants start at 0 so `Off` sorts
/// below every `Level`.
#[repr(usize)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum LevelFilter {
    /// A level lower than all log levels.
    Off,
    /// Corresponds to the `Error` log level.
    Error,
    /// Corresponds to the `Warn` log level.
    Warn,
    /// Corresponds to the `Info` log level.
    Info,
    /// Corresponds to the `Debug` log level.
    Debug,
    /// Corresponds to the `Trace` log level.
    Trace,
}

impl LevelFilter {
    /// The most verbose logging level filter.
    #[must_use]
    #[inline]
    pub const fn max() -> Self {
        Self::Trace
    }

    /// The equivalent [`Level`], or `None` for [`LevelFilter::Off`].
    #[must_use]
    #[inline]
    pub const fn to_level(&self) -> Option<Level> {
        match self {
            Self::Off => None,
            Self::Error => Some(Level::Error),
            Self::Warn => Some(Level::Warn),
            Self::Info => Some(Level::Info),
            Self::Debug => Some(Level::Debug),
            Self::Trace => Some(Level::Trace),
        }
    }

    /// The string representation of the `LevelFilter`.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "OFF",
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }

    /// Iterate through all supported filtering levels, most restrictive first.
    pub fn iter() -> impl Iterator<Item = Self> {
        [
            Self::Off,
            Self::Error,
            Self::Warn,
            Self::Info,
            Self::Debug,
            Self::Trace,
        ]
        .into_iter()
    }

    /// The next-higher-verbosity filter, saturating at [`LevelFilter::Trace`].
    #[must_use]
    #[inline]
    pub const fn increment_severity(&self) -> Self {
        match self {
            Self::Off => Self::Error,
            Self::Error => Self::Warn,
            Self::Warn => Self::Info,
            Self::Info => Self::Debug,
            Self::Debug | Self::Trace => Self::Trace,
        }
    }

    /// The next-lower-verbosity filter, saturating at [`LevelFilter::Off`].
    #[must_use]
    #[inline]
    pub const fn decrement_severity(&self) -> Self {
        match self {
            Self::Off | Self::Error => Self::Off,
            Self::Warn => Self::Error,
            Self::Info => Self::Warn,
            Self::Debug => Self::Info,
            Self::Trace => Self::Debug,
        }
    }
}

impl fmt::Display for LevelFilter {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.pad(self.as_str())
    }
}

impl FromStr for LevelFilter {
    type Err = ParseLevelError;

    /// ASCII case-insensitive, no whitespace trimming, and `"off"` is accepted
    /// (starts at index 0).
    fn from_str(level: &str) -> Result<Self, Self::Err> {
        for (idx, name) in LOG_LEVEL_NAMES.iter().enumerate() {
            if name.eq_ignore_ascii_case(level) {
                return level_filter_from_usize(idx).ok_or(ParseLevelError(()));
            }
        }
        Err(ParseLevelError(()))
    }
}

impl PartialEq<LevelFilter> for Level {
    #[inline]
    fn eq(&self, other: &LevelFilter) -> bool {
        *self as usize == *other as usize
    }
}

impl PartialOrd<LevelFilter> for Level {
    #[inline]
    fn partial_cmp(&self, other: &LevelFilter) -> Option<cmp::Ordering> {
        Some((*self as usize).cmp(&(*other as usize)))
    }
}

impl PartialEq<Level> for LevelFilter {
    #[inline]
    fn eq(&self, other: &Level) -> bool {
        *self as usize == *other as usize
    }
}

impl PartialOrd<Level> for LevelFilter {
    #[inline]
    fn partial_cmp(&self, other: &Level) -> Option<cmp::Ordering> {
        Some((*self as usize).cmp(&(*other as usize)))
    }
}

/// Error returned by `from_str` when the string does not match any log level.
///
/// Because this crate is `#![no_std]` unconditionally, `core::error::Error` /
/// `std::error::Error` is not implemented.
#[derive(Debug, PartialEq, Eq)]
pub struct ParseLevelError(());

impl fmt::Display for ParseLevelError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.write_str(LEVEL_PARSE_ERROR)
    }
}

#[inline]
const fn level_from_usize(u: usize) -> Option<Level> {
    match u {
        1 => Some(Level::Error),
        2 => Some(Level::Warn),
        3 => Some(Level::Info),
        4 => Some(Level::Debug),
        5 => Some(Level::Trace),
        _ => None,
    }
}

#[inline]
const fn level_filter_from_usize(u: usize) -> Option<LevelFilter> {
    match u {
        0 => Some(LevelFilter::Off),
        1 => Some(LevelFilter::Error),
        2 => Some(LevelFilter::Warn),
        3 => Some(LevelFilter::Info),
        4 => Some(LevelFilter::Debug),
        5 => Some(LevelFilter::Trace),
        _ => None,
    }
}

/// Backing global for [`max_level`]; never written (`set_max_level` is not
/// exported), so loads always return `0` ([`LevelFilter::Off`]).
static MAX_LOG_LEVEL_FILTER: AtomicUsize = AtomicUsize::new(0);

/// Current maximum log level — always [`LevelFilter::Off`].
///
/// Must not delegate to `aterm_log::max_level()`, which defaults to `Info` and
/// would enable `wgpu-hal` Vulkan validation callbacks and EGL config loops.
#[must_use]
pub fn max_level() -> LevelFilter {
    match level_filter_from_usize(MAX_LOG_LEVEL_FILTER.load(Ordering::Relaxed)) {
        Some(filter) => filter,
        None => LevelFilter::Off,
    }
}

// Each macro matches `($($discarded:tt)*)` so bare, `target:`, `logger:`, and
// structured `key = value;` forms are accepted without evaluating arguments,
// and expands to `{}` (valid in both expression and statement position).

/// Log at the error level. Expands to `{}`; arguments are never evaluated.
///
/// ```
/// // Never called: a disabled callsite evaluates nothing, here or upstream.
/// #[allow(dead_code)]
/// fn never_called() -> u32 { unreachable!() }
///
/// log::error!("Root index {} is not bound", never_called());
/// log::error!(target: "sctk", "unknown decoration mode 0x{:x}", never_called());
/// ```
#[macro_export]
macro_rules! error {
    ($($discarded:tt)*) => {{}};
}

/// Log at the warn level. Expands to `{}`; arguments are never evaluated.
#[macro_export]
macro_rules! warn {
    ($($discarded:tt)*) => {{}};
}

/// Log at the info level. Expands to `{}`; arguments are never evaluated.
#[macro_export]
macro_rules! info {
    ($($discarded:tt)*) => {{}};
}

/// Log at the debug level. Expands to `{}`; arguments are never evaluated.
#[macro_export]
macro_rules! debug {
    ($($discarded:tt)*) => {{}};
}

/// Log at the trace level. Expands to `{}`; arguments are never evaluated.
#[macro_export]
macro_rules! trace {
    ($($discarded:tt)*) => {{}};
}

/// Log at a runtime-chosen [`Level`]. Expands to `{}`; arguments (including the
/// level expression) are never evaluated.
///
/// ```
/// use log::{log, Level};
///
/// let level = Level::Warn;
/// assert_eq!(level, Level::Warn);
/// log!(level, "EGL '{}' code 0x{:x}", "eglInitialize", 0x3001);
/// ```
#[macro_export]
macro_rules! log {
    ($($discarded:tt)*) => {{}};
}

/// Whether a callsite at the given [`Level`] would emit. Always `false`.
///
/// ```
/// assert!(!log::log_enabled!(log::Level::Debug));
/// ```
#[macro_export]
macro_rules! log_enabled {
    ($($discarded:tt)*) => {
        false
    };
}
