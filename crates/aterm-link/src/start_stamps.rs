// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The broker's start, stamped (ATERM-KI-1111-2): when `aterm`'s `main` was entered, when
//! `aterm link broker` was, when its `rest.key` was read and its notes written, when astream's
//! open called its `prepare` (the log opened and locked, before its recovery scan), when the open
//! returned, when the open's aftermath was done (its notes written, the seam armed), when its
//! retention and rate rules were set, when its socket was bound and as it writes `listening`.
//! Each is [`aterm_uds::process::continuous_ns`], the one monotonic clock every process of this
//! machine reads alike, so a launcher that reads it just before its spawn times each phase from
//! its own spawn. The broker writes them on its stderr as one line, [`PREFIX`] and `<phase>=<ns>`
//! words, right after its `listening` line: one clock read a phase and one line a start.
//!
//! A step is named by the phase it ends at and holds everything since the phase before:
//! `key -> locked` is astream's open up to its `prepare` (its options built and checked, the log
//! file opened, on a new log created and its folder synced, and its lock taken), not the lock
//! alone; `locked -> opened` is the rest of that open (`prepare`'s own reads, the recovery scan,
//! the index built and astream's start of the broker over it: its writer threads, the log's
//! wills fired).

use std::sync::OnceLock;

/// What the stamps' line on the broker's stderr begins with.
pub const PREFIX: &str = "aterm link broker: start stamps (continuous clock, ns):";

/// `aterm`'s `main`, entered: what [`main_entered`] read.
pub const MAIN: &str = "main";
/// `aterm link broker`, entered.
pub const BROKER: &str = "broker";
/// Its log's `rest.key` read (`at_rest::prepare`) and its notes written: sealed or not, the log's
/// at-rest state known, and astream's open called next.
pub const KEY: &str = "key";
/// astream's open, the log file opened and locked, enters its `prepare`, before its recovery scan.
pub const LOCKED: &str = "locked";
/// astream's open returned: the scan done, the index built and the broker started over it
/// (astream splits them nowhere aterm can read).
pub const OPENED: &str = "opened";
/// The open's aftermath done: the at-rest key dropped, the open's notes written and the health
/// seam armed, so the step after it is the rules alone.
pub const ARMED: &str = "armed";
/// The retention and rate rules set, and the limit read back.
pub const RULES: &str = "rules";
/// Its socket bound (`serve` returned).
pub const SERVED: &str = "served";
/// Its `listening` line about to be written: the stamp precedes the line, so a launcher's read
/// of its clock once it has the line is never earlier.
pub const LISTENING: &str = "listening";

/// Where a launcher's [`Stamps::steps`] begin: its read of the clock just before its spawn.
pub const SPAWN: &str = "spawn";
/// Where they end: its read of the clock once it has the `listening` line.
pub const SEEN: &str = "seen";

/// The phases in the order a start passes them.
pub const ORDER: [&str; 9] = [
    MAIN, BROKER, KEY, LOCKED, OPENED, ARMED, RULES, SERVED, LISTENING,
];

static MAIN_ENTERED: OnceLock<Option<u64>> = OnceLock::new();

/// Read the clock as `aterm`'s `main` is entered; the first call counts.
pub fn main_entered() {
    let _ = MAIN_ENTERED.get_or_init(aterm_uds::process::continuous_ns);
}

/// A start's stamps, each phase and when, in the order they were taken.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stamps {
    at: Vec<(String, u64)>,
}

impl Stamps {
    /// The stamps of a start entering the broker now: `main`'s (where [`main_entered`] ran) and
    /// the broker's.
    #[must_use]
    pub fn broker_entered() -> Self {
        let mut stamps = Self::default();
        if let Some(Some(ns)) = MAIN_ENTERED.get() {
            stamps.at.push((MAIN.to_string(), *ns));
        }
        stamps.mark(BROKER);
        stamps
    }

    /// Stamp `phase` now; a clock that cannot be read stamps nothing.
    pub fn mark(&mut self, phase: &str) {
        if let Some(ns) = aterm_uds::process::continuous_ns() {
            self.at.push((phase.to_string(), ns));
        }
    }

    /// Stamp `phase` at `ns`, read earlier.
    pub fn mark_at(&mut self, phase: &str, ns: u64) {
        self.at.push((phase.to_string(), ns));
    }

    /// When `phase` was stamped.
    #[must_use]
    pub fn at(&self, phase: &str) -> Option<u64> {
        self.at.iter().find(|(p, _)| p == phase).map(|&(_, ns)| ns)
    }

    /// The stamps' line: [`PREFIX`] and a `<phase>=<ns>` word for each.
    #[must_use]
    pub fn line(&self) -> String {
        use std::fmt::Write as _;
        let mut line = PREFIX.to_string();
        for (phase, ns) in &self.at {
            let _ = write!(line, " {phase}={ns}");
        }
        line
    }

    /// The stamps a [`Stamps::line`] carries; `None` for any other line, or one with a word that
    /// is no `<phase>=<ns>`.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let rest = line.trim_end().strip_prefix(PREFIX)?;
        let at = rest
            .split_whitespace()
            .map(|word| {
                let (phase, ns) = word.split_once('=')?;
                (!phase.is_empty()).then_some(())?;
                Some((phase.to_string(), ns.parse().ok()?))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self { at })
    }

    /// The stamps' line among `text`'s lines (a broker's stderr), if it is there.
    #[must_use]
    pub fn find(text: &str) -> Option<Self> {
        text.lines().find_map(Self::parse)
    }

    /// Each step of the start from `spawn` (a launcher's read of the same clock just before its
    /// spawn) to `seen` (when it read the `listening` line), in [`ORDER`]: `(from, to, ns)`, a
    /// phase the start did not stamp skipped, so a step spans from the last phase stamped. `None`
    /// where a stamp precedes the one before it, which no one clock does.
    #[must_use]
    pub fn steps(&self, spawn: u64, seen: Option<u64>) -> Option<Vec<(String, String, u64)>> {
        let mut steps = Vec::new();
        let mut from = (SPAWN.to_string(), spawn);
        let stamped = ORDER
            .iter()
            .filter_map(|&phase| self.at(phase).map(|ns| (phase.to_string(), ns)))
            .chain(seen.map(|ns| (SEEN.to_string(), ns)));
        for (to, ns) in stamped {
            steps.push((from.0, to.clone(), ns.checked_sub(from.1)?));
            from = (to, ns);
        }
        Some(steps)
    }
}

#[cfg(test)]
#[path = "start_stamps_tests.rs"]
mod tests;
