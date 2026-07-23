// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Error handling for aterm: zero external dependencies.
//!
//! Provides three capabilities:
//!
//! 1. **`#[derive(Error)]`** — generates `Display` and `std::error::Error` impls
//!    for enums. Drop-in replacement for `thiserror`. Supports `#[error("...")]`,
//!    `#[error(transparent)]`, `#[from]`, and `#[source]`.
//!
//! 2. **`Context` trait** — adds `.context("msg")` and `.with_context(|| ...)` to
//!    `Result` types for ad-hoc error wrapping. Replacement for `anyhow::Context`.
//!
//! 3. **Ad-hoc error macros** — `bail!`, `ensure!`, and `err!` for binary crates.
//!    Drop-in replacement for `anyhow::bail!`, `anyhow::ensure!`, and `anyhow!()`.
//!
//! ## Library crates
//!
//! ```rust,ignore
//! use aterm_error::Error;
//!
//! #[derive(Debug, Error)]
//! pub enum MyError {
//!     #[error("I/O error: {0}")]
//!     Io(#[from] std::io::Error),
//!
//!     #[error("invalid input: {reason}")]
//!     Invalid { reason: String },
//!
//!     #[error(transparent)]
//!     Other(#[from] SomeOtherError),
//! }
//! ```
//!
//! ## Binary crates
//!
//! ```rust,ignore
//! use aterm_error::{Context, Result, bail, ensure};
//!
//! fn load_config(path: &str) -> Result<Config> {
//!     let data = std::fs::read(path)
//!         .context("failed to read config")?;
//!     ensure!(!data.is_empty(), "config file is empty");
//!     Ok(parse(data)?)
//! }
//! ```

#![deny(clippy::all)]
#![deny(unsafe_op_in_unsafe_fn)]

mod context;

// Re-export the derive macro so users write `use aterm_error::Error;`
pub use aterm_error_derive::Error;

pub use context::{Context, ContextError};

// ============================================================================
// BoxError + Result
// ============================================================================

/// A type-erased error: replacement for `anyhow::Error`.
///
/// Any `E: std::error::Error + Send + Sync + 'static` converts to `BoxError`
/// via the standard `From` impl on `Box<dyn Error>`.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// A result type with `BoxError`: replacement for `anyhow::Result<T>`.
pub type Result<T> = std::result::Result<T, BoxError>;

// ============================================================================
// Ad-hoc error macros
// ============================================================================

/// Create an ad-hoc error from a format string.
///
/// Replacement for `anyhow::anyhow!("msg")` / `anyhow!("fmt", args...)`.
///
/// ```rust,ignore
/// use aterm_error::err;
/// let e = err!("invalid port: {}", port);
/// ```
#[macro_export]
macro_rules! err {
    ($($arg:tt)*) => {
        $crate::_ad_hoc(::std::format!($($arg)*))
    };
}

/// Return early with an ad-hoc error.
///
/// Replacement for `anyhow::bail!("msg")`.
///
/// ```rust,ignore
/// use aterm_error::bail;
/// if name.is_empty() {
///     bail!("name must not be empty");
/// }
/// ```
#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return ::std::result::Result::Err($crate::err!($($arg)*))
    };
}

/// Return early with an ad-hoc error if a condition is not met.
///
/// Replacement for `anyhow::ensure!(condition, "msg")`.
///
/// ```rust,ignore
/// use aterm_error::ensure;
/// ensure!(port > 0, "port must be positive, got {}", port);
/// ```
#[macro_export]
macro_rules! ensure {
    ($cond:expr, $($arg:tt)*) => {
        if !$cond {
            $crate::bail!($($arg)*)
        }
    };
}

/// Internal helper — creates a `BoxError` from a string message.
/// Not part of the public API; used by the `err!` macro.
#[doc(hidden)]
pub fn _ad_hoc(msg: String) -> BoxError {
    // `BoxError::from` (std's blanket `From<E: Error + Send + Sync>` impl,
    // whose body is exactly `Box::new(err)`) instead of `Box::new(..)` with
    // an implicit unsize coercion at the return: the strict gate's VC
    // lowering has no model for a *local* unsize coercion to
    // `Box<dyn Error + ..>` (`CastKind::PointerCoercion::Unsize`), while the
    // identical coercion inside std is scoped out. Same behavior.
    BoxError::from(AdHocError(msg))
}

/// A simple string-based error for ad-hoc error messages.
#[derive(Debug)]
struct AdHocError(String);

impl std::fmt::Display for AdHocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AdHocError {}

#[cfg(test)]
mod tests;
