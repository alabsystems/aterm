// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `Context` trait for adding ad-hoc context to `Result` types.
//!
//! Provides `.context("msg")` and `.with_context(|| ...)` similar to `anyhow`.

use std::fmt;

use crate::BoxError;

/// A wrapper error that carries a context message and an optional source error.
///
/// This is the error type produced by the [`Context`] trait methods.
#[derive(Debug)]
pub struct ContextError {
    msg: String,
    source: Option<BoxError>,
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for ContextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|e| e as &(dyn std::error::Error + 'static))
    }
}

/// Extension trait for adding context to `Result` and `Option` types.
///
/// Works on any `Result<T, E>` where `E: std::error::Error + Send + Sync + 'static`,
/// as well as `Result<T, BoxError>` and `Option<T>`.
pub trait Context<T> {
    /// Wrap the error with a static context message.
    fn context<M: fmt::Display>(self, msg: M) -> crate::Result<T>;

    /// Wrap the error with a lazily-evaluated context message.
    fn with_context<F: FnOnce() -> String>(self, f: F) -> crate::Result<T>;
}

impl<T, E> Context<T> for Result<T, E>
where
    E: Into<BoxError>,
{
    fn context<M: fmt::Display>(self, msg: M) -> crate::Result<T> {
        self.map_err(|e| {
            // `BoxError::from` (std's blanket `From<E: Error + Send + Sync>`
            // impl, whose body is exactly `Box::new(err)`) instead of
            // `Box::new(..) as BoxError`: the strict gate's VC lowering has no
            // model for a *local* unsize coercion to `Box<dyn Error + ..>`
            // (`CastKind::PointerCoercion::Unsize`), while the identical
            // coercion inside std is scoped out. Same behavior.
            BoxError::from(ContextError {
                msg: msg.to_string(),
                source: Some(e.into()),
            })
        })
    }

    fn with_context<F: FnOnce() -> String>(self, f: F) -> crate::Result<T> {
        self.map_err(|e| {
            // Same local-unsize dodge as `context` above.
            BoxError::from(ContextError {
                msg: f(),
                source: Some(e.into()),
            })
        })
    }
}

/// `Context` for `Option<T>` — converts `None` to a `ContextError`.
impl<T> Context<T> for Option<T> {
    fn context<M: fmt::Display>(self, msg: M) -> crate::Result<T> {
        self.ok_or_else(|| {
            // Same local-unsize dodge as `Result::context` above.
            BoxError::from(ContextError {
                msg: msg.to_string(),
                source: None,
            })
        })
    }

    fn with_context<F: FnOnce() -> String>(self, f: F) -> crate::Result<T> {
        self.ok_or_else(|| {
            // Same local-unsize dodge as `Result::context` above.
            BoxError::from(ContextError {
                msg: f(),
                source: None,
            })
        })
    }
}
