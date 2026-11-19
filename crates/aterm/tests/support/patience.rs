// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The patience of a wait on something a process this test binary started must do: a hang
//! detector, never a latency budget. A [`Patience`] is spent once a reading of the test
//! binary's descendants ([`aterm_uds::deadline::ProcessProgressProbe::read_descendants`], with
//! the binary's own work left out) just taken shows they used no CPU and did no I/O for one
//! whole window ([`Spent::Stalled`]), or once [`aterm_spec::wait::WORKING_GUARD`] passed in all
//! ([`Spent::Guard`]).
//!
//! Descendants, not one child, are the measure: an `aterm ctl` client idles on its socket while
//! the instance the test booted, also a descendant of the binary, does the work it waits for.
//! The wall clock is no measure: a loaded Mac stretches it without bound, and a wall-clock
//! bound named instances hung that were only starved (a landing's `verify land` run with every
//! core busy). A reading that fails, or whose members changed since the last one (a poll's
//! `aterm ctl` started or reaped), is no evidence of a stall: the window opens again at it, as
//! in `aterm_spec::wait`. Every descendant counts, so another test's working child in the same
//! binary holds the window open; only the guard ends such a wait.
//!
//! This is `aterm_spec::wait`'s rule for its private `working`, kept here and not in
//! `aterm-spec`: every crate that dev-depends on `aterm-spec` would join the landing cone of a
//! change to it.
//!
//! A wait this measure cannot judge keeps its wall bound and says why beside it: one whose bound
//! is the point (a timeout it asserts, a "not within" observation, a deadline the product keeps or
//! is handed), and one whose actor is no process this binary started (another machine, a system
//! daemon, another suite's build).

#![allow(dead_code)]

use std::fmt;
use std::time::{Duration, Instant};

use aterm_uds::deadline::ProcessProgressProbe;

/// How often a [`Judge`] takes a reading: each is a sweep of the process table, so not once a
/// poll (as a caller's own poll gap).
const TREE_TICK: Duration = Duration::from_millis(500);

/// Why a [`Patience`] is spent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Spent {
    /// A reading just taken showed no CPU or I/O progress since the window opened, this long
    /// before.
    Stalled(Duration),
    /// The tree kept working, or kept changing its members, for this whole guard.
    Guard(Duration),
}

impl fmt::Display for Spent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stalled(window) => write!(
                f,
                "what this test binary started made no CPU or I/O progress for {window:?}"
            ),
            Self::Guard(guard) => write!(
                f,
                "the wait reached its {guard:?} guard while what this test binary started kept \
                 working"
            ),
        }
    }
}

/// A failure's `expect` prints the sentence [`fmt::Display`] does.
impl fmt::Debug for Spent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// The patience of one wait, started at [`Patience::new`]: ask [`Patience::spent`] once a poll.
pub struct Patience {
    tree: ProcessProgressProbe,
    judge: Judge,
}

impl Patience {
    /// Patience for a wait that names a hang after `window` without progress, from now.
    pub fn new(window: Duration) -> Self {
        Self {
            tree: ProcessProgressProbe::default(),
            judge: Judge::new(window, Instant::now()),
        }
    }

    /// Whether the patience is spent now, and why; reads the tree when a tick has passed since
    /// the last reading.
    pub fn spent(&mut self) -> Option<Spent> {
        let own = std::process::id();
        let tree = &mut self.tree;
        self.judge.at(Instant::now(), || tree.read_descendants(own))
    }
}

/// Polls `probe` every `gap` until it answers, for as long as a [`Patience`] over `window` lasts.
///
/// # Errors
/// Why the patience was spent before `probe` answered.
pub fn until<T>(
    window: Duration,
    gap: Duration,
    mut probe: impl FnMut() -> Option<T>,
) -> Result<T, Spent> {
    let mut patience = Patience::new(window);
    loop {
        if let Some(value) = probe() {
            return Ok(value);
        }
        if let Some(why) = patience.spent() {
            return Err(why);
        }
        std::thread::sleep(gap);
    }
}

/// The judgement of a [`Patience`], over the instants and readings its caller gives it.
pub struct Judge {
    window: Duration,
    tick: Duration,
    began: Instant,
    /// The last reading's progress generation; `None` before one, or where the last one failed.
    seen: Option<u64>,
    /// When the last reading was taken; the start until the first, so a child that ends within a
    /// tick costs no sweep of the process table.
    read_at: Instant,
    /// The window's start: the last reading at which the tree had advanced or did not compare.
    worked_at: Instant,
}

impl Judge {
    /// A judgement of a wait that began at `began` and names a hang after `window` without
    /// progress.
    pub fn new(window: Duration, began: Instant) -> Self {
        Self {
            window,
            tick: TREE_TICK.min(window / 4).max(Duration::from_millis(1)),
            began,
            seen: None,
            read_at: began,
            worked_at: began,
        }
    }

    /// Whether the patience is spent at `at`, and why. `read` is the tree's progress generation
    /// (`None`: unread or incomparable); it is called only once a tick has passed since the last
    /// reading, and only a reading just taken can show a stall: between readings the silence
    /// since the window opened is not all observed.
    pub fn at(&mut self, at: Instant, read: impl FnOnce() -> Option<u64>) -> Option<Spent> {
        if at.saturating_duration_since(self.read_at) >= self.tick {
            self.read_at = at;
            match read() {
                Some(generation) if self.seen == Some(generation) => {}
                reading => {
                    self.seen = reading;
                    self.worked_at = at;
                }
            }
            if at.saturating_duration_since(self.worked_at) >= self.window {
                return Some(Spent::Stalled(self.window));
            }
        }
        (at.saturating_duration_since(self.began) >= aterm_spec::wait::WORKING_GUARD)
            .then_some(Spent::Guard(aterm_spec::wait::WORKING_GUARD))
    }
}
