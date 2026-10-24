// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WHERE A SLOW STEP'S TIME WENT: a mark at the end of each inner call ([`lap`]), kept only inside
//! a timed run on the same thread ([`within`]), said in one line ([`Laps`]'s `Display`). A mark
//! outside a run costs one thread-local look and keeps nothing.

use std::cell::RefCell;
use std::time::{Duration, Instant};

/// The marks of one timed run, in order: each inner call's name and how long it took since the mark
/// before it (or since the run began).
#[derive(Clone, Debug)]
pub struct Laps {
    began: Instant,
    last: Instant,
    ended: Option<Instant>,
    marks: Vec<(&'static str, Duration)>,
}

impl Laps {
    /// A run that begins now, with no mark yet.
    #[must_use]
    pub fn begin() -> Self {
        let now = Instant::now();
        Self {
            began: now,
            last: now,
            ended: None,
            marks: Vec::new(),
        }
    }

    /// The inner call `name` just ended: the time since the last mark is its.
    pub fn lap(&mut self, name: &'static str) {
        let now = Instant::now();
        self.marks
            .push((name, now.saturating_duration_since(self.last)));
        self.last = now;
    }

    /// The run ended now: its total and its unmarked rest stop growing.
    pub fn end(&mut self) {
        self.ended.get_or_insert_with(Instant::now);
    }

    /// How long the run took (so far, while it has not ended).
    #[must_use]
    pub fn total(&self) -> Duration {
        self.until().saturating_duration_since(self.began)
    }

    /// The marks, in order.
    #[must_use]
    pub fn marks(&self) -> &[(&'static str, Duration)] {
        &self.marks
    }

    /// How long the calls marked `name` took in all (`None`: none was marked).
    #[must_use]
    pub fn took(&self, name: &str) -> Option<Duration> {
        self.marks
            .iter()
            .filter(|(n, _)| *n == name)
            .map(|(_, d)| *d)
            .reduce(|a, b| a.saturating_add(b))
    }

    fn until(&self) -> Instant {
        self.ended.unwrap_or_else(Instant::now)
    }
}

impl std::fmt::Display for Laps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (name, took) in &self.marks {
            write!(f, "{name} {} ms, ", took.as_millis())?;
        }
        let rest = self.until().saturating_duration_since(self.last);
        write!(f, "the rest {} ms", rest.as_millis())
    }
}

thread_local! {
    /// The timed runs open on this thread, innermost last ([`within`]).
    static OPEN: RefCell<Vec<Laps>> = const { RefCell::new(Vec::new()) };
}

/// Run `f` as a timed run: the marks its inner calls take on this thread ([`lap`]) are kept, and
/// handed back, ended, with its result. Runs nest: an inner run's marks are its own.
pub fn within<R>(f: impl FnOnce() -> R) -> (R, Laps) {
    /// Closes the run as `f` returns or unwinds.
    struct Open;
    impl Drop for Open {
        fn drop(&mut self) {
            OPEN.with(|o| o.borrow_mut().pop());
        }
    }
    OPEN.with(|o| o.borrow_mut().push(Laps::begin()));
    let open = Open;
    let out = f();
    let mut laps = OPEN
        .with(|o| o.borrow().last().cloned())
        .unwrap_or_else(Laps::begin);
    drop(open);
    laps.end();
    (out, laps)
}

/// The inner call `name` just ended, marked in the innermost timed run open on this thread
/// ([`within`]); outside one, nothing is kept.
pub fn lap(name: &'static str) {
    OPEN.with(|o| {
        if let Some(laps) = o.borrow_mut().last_mut() {
            laps.lap(name);
        }
    });
}

#[cfg(test)]
#[path = "laps_tests.rs"]
mod tests;
