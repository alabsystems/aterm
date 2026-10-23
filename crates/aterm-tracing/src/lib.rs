// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `tracing` — aterm's first-party, no-op replacement for the upstream `tracing` facade.

#![no_std]

/// A span — always disabled, therefore zero-sized and empty.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span;

impl Span {
    /// A span that does nothing — here, all of them.
    #[inline]
    #[must_use]
    pub const fn none() -> Self {
        Span
    }

    /// Enter the span, returning a guard.
    #[inline]
    #[must_use]
    pub const fn entered(self) -> EnteredSpan {
        EnteredSpan
    }
}

/// The guard returned by [`Span::entered`].
#[derive(Debug, Default)]
pub struct EnteredSpan;

impl EnteredSpan {
    /// Exit the span, handing back the [`Span`] it was entered from.
    #[inline]
    #[must_use]
    pub const fn exit(self) -> Span {
        Span
    }
}

/// A `TRACE`-level event.
#[macro_export]
macro_rules! trace {
    ($($discarded:tt)*) => {{}};
}

/// A `DEBUG`-level event.
#[macro_export]
macro_rules! debug {
    ($($discarded:tt)*) => {{}};
}

/// An `INFO`-level event.
#[macro_export]
macro_rules! info {
    ($($discarded:tt)*) => {{}};
}

/// A `WARN`-level event.
#[macro_export]
macro_rules! warn {
    ($($discarded:tt)*) => {{}};
}

/// An `ERROR`-level event.
#[macro_export]
macro_rules! error {
    ($($discarded:tt)*) => {{}};
}

/// A `TRACE`-level span.
#[macro_export]
macro_rules! trace_span {
    ($($discarded:tt)*) => {
        $crate::Span::none()
    };
}

/// A `DEBUG`-level span.
#[macro_export]
macro_rules! debug_span {
    ($($discarded:tt)*) => {
        $crate::Span::none()
    };
}

/// An `INFO`-level span.
#[macro_export]
macro_rules! info_span {
    ($($discarded:tt)*) => {
        $crate::Span::none()
    };
}

/// A `WARN`-level span.
#[macro_export]
macro_rules! warn_span {
    ($($discarded:tt)*) => {
        $crate::Span::none()
    };
}

/// An `ERROR`-level span.
#[macro_export]
macro_rules! error_span {
    ($($discarded:tt)*) => {
        $crate::Span::none()
    };
}
