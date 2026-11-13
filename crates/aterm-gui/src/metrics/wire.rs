// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `metrics` verb's request grammar.

pub(crate) use aterm_control::wire::fields::{
    Cell, Decimal, FieldSink, FlagWords, JsonSink, TextSink,
};

/// Which body a `metrics` reply carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Form {
    /// The one-line summary (`metrics`, `metrics reset`).
    Summary,
    /// The distributions (`metrics percentiles`).
    Percentiles,
}

/// Which sink spells the reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Syntax {
    /// `OK key=value ...`, flags `0`/`1`.
    Text,
    /// `OK 1\n{...}`.
    Json,
}

/// A parsed `metrics` argument (the `--json` flag already taken).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub(crate) form: Form,
    /// Zero the measurement window before reading it.
    pub(crate) reset: bool,
}

impl Request {
    /// `""`, `reset` or `percentiles`.
    #[must_use]
    pub(crate) fn parse(rest: &str) -> Option<Self> {
        let (form, reset) = match rest.trim() {
            "" => (Form::Summary, false),
            "reset" => (Form::Summary, true),
            "percentiles" => (Form::Percentiles, false),
            _ => return None,
        };
        Some(Self { form, reset })
    }
}

/// What a reply reads beside its metrics root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fragments {
    /// The process's other ledgers.
    Process,
    /// None of them.
    #[cfg(test)]
    Inert,
}

/// What one reply reads once and every emitter shares.
#[derive(Clone, Copy)]
pub(crate) struct ReadCtx<'a> {
    /// The ledgers the reply publishes: the process's [`super::METRICS`].
    pub(crate) metrics: &'a super::Metrics,
    /// The process clock ([`super::now_ns`]) at this reply.
    pub(crate) now_ns: u64,
    pub(crate) fragments: Fragments,
}

impl ReadCtx<'static> {
    /// The context of a reply read now, from the process.
    #[must_use]
    pub(crate) fn now() -> Self {
        Self {
            metrics: &super::METRICS,
            now_ns: super::now_ns(),
            fragments: Fragments::Process,
        }
    }
}

impl<'a> ReadCtx<'a> {
    /// A reply that reads `metrics` at `now_ns` and nothing else (tests).
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn inert(metrics: &'a super::Metrics, now_ns: u64) -> Self {
        Self {
            metrics,
            now_ns,
            fragments: Fragments::Inert,
        }
    }

    /// Whether the reply splices the process's other ledgers ([`Fragments::Process`]).
    #[must_use]
    pub(crate) const fn reads_process(&self) -> bool {
        matches!(self.fragments, Fragments::Process)
    }
}

/// Nanoseconds as milliseconds to two places.
#[must_use]
pub(crate) fn ms(ns: u64) -> Decimal {
    Decimal::new(ns as f64 / 1e6, 2)
}

/// Nanoseconds as milliseconds to three places.
#[must_use]
pub(crate) fn ms3(ns: u64) -> Decimal {
    Decimal::new(ns as f64 / 1e6, 3)
}

/// Nanoseconds as milliseconds without discarding nanosecond resolution.
#[must_use]
pub(crate) fn ms6(ns: u64) -> Decimal {
    Decimal::new(ns as f64 / 1e6, 6)
}

/// Nanoseconds as milliseconds to one place: `slow_threshold_ms` and `long_turn_threshold_ms`.
#[must_use]
pub(crate) fn ms1(ns: u64) -> Decimal {
    Decimal::new(ns as f64 / 1e6, 1)
}

/// Microseconds as milliseconds to two places (the echo clock's unit).
#[must_use]
pub(crate) fn us_ms(us: u64) -> Decimal {
    Decimal::new(us as f64 / 1e3, 2)
}

/// Signed microseconds as milliseconds to two places.
#[must_use]
pub(crate) fn signed_us_ms(us: i64) -> Decimal {
    Decimal::new(us as f64 / 1e3, 2)
}

/// A value already in milliseconds, to two places.
#[must_use]
pub(crate) const fn ms_f64(ms: f64) -> Decimal {
    Decimal::new(ms, 2)
}

/// A fragment in the text form, with no head: ` key=value ...`.
#[cfg(test)]
pub(crate) fn render_text(emit: impl FnOnce(&mut dyn FieldSink)) -> String {
    let mut sink = TextSink::new("", FlagWords::ZeroOne);
    emit(&mut sink);
    sink.finish()
}

/// A fragment as one JSON object: `{"key":value,...}`.
#[cfg(test)]
pub(crate) fn render_json(emit: impl FnOnce(&mut dyn FieldSink)) -> String {
    let mut sink = JsonSink::new();
    emit(&mut sink);
    sink.finish()
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;

/// The laws every `metrics` body keeps.
#[cfg(test)]
#[path = "wire_law_tests.rs"]
pub(crate) mod law_tests;
