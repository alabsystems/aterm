// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Error handling for aterm: zero external dependencies.
//!
//! Provides **`#[derive(Error)]`** — generates `Display` and `std::error::Error`
//! impls for enums and structs. Drop-in replacement for `thiserror`. Supports
//! `#[error("...")]`, `#[error(transparent)]`, `#[from]`, and `#[source]`. (The
//! `anyhow`-style half — `Context`, `Result`, `bail!`/`ensure!`/`err!` — had no
//! caller in the workspace and was deleted, 2026-09-25.)
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

#![deny(clippy::all)]
#![deny(unsafe_op_in_unsafe_fn)]

// Re-export the derive macro so users write `use aterm_error::Error;`
pub use aterm_error_derive::Error;

#[cfg(test)]
mod tests;
