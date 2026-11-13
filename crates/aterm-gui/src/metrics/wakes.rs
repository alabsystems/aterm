// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-variant user-event wake counters ([`WakeTable`] and UI-thread [`WakeCensus`]).

use std::fmt;
use std::mem::Discriminant;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

const WAKE_SLOTS: usize = 256;
const OTHER: usize = WAKE_SLOTS - 1;

/// The process's wake ledger, read by `metrics` and zeroed by `metrics reset`.
pub(crate) static WAKES: WakeTable = WakeTable::new();

/// Lock-free per-slot wake counts and once-claimed variant names.
pub(crate) struct WakeTable {
    counts: [AtomicU64; WAKE_SLOTS],
    names: [OnceLock<&'static str>; WAKE_SLOTS],
}

impl WakeTable {
    pub(crate) const fn new() -> Self {
        Self {
            counts: [const { AtomicU64::new(0) }; WAKE_SLOTS],
            names: [const { OnceLock::new() }; WAKE_SLOTS],
        }
    }

    fn slot(&self, name: &str) -> usize {
        for (i, cell) in self.names[..OTHER].iter().enumerate() {
            if *cell.get_or_init(|| Box::leak(name.into())) == name {
                return i;
            }
        }
        self.names[OTHER].get_or_init(|| "other");
        OTHER
    }

    /// Counted wakes since the last reset, sorted busiest first (ties by name), omitting zeroes.
    #[must_use]
    pub(crate) fn attribution(&self) -> Vec<(&'static str, u64)> {
        let mut out: Vec<(&'static str, u64)> = self
            .names
            .iter()
            .zip(&self.counts)
            .filter_map(|(name, count)| {
                let name = *name.get()?;
                let n = count.load(Ordering::Relaxed);
                (n != 0).then_some((name, n))
            })
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        out
    }

    /// Zero every count and keep every claimed name.
    pub(crate) fn reset(&self) {
        for count in &self.counts {
            count.store(0, Ordering::Relaxed);
        }
    }
}

/// UI-thread map from each [`crate::Wake`] discriminant to its [`WakeTable`] slot.
pub(crate) struct WakeCensus {
    table: &'static WakeTable,
    slots: aterm_hash::FxHashMap<Discriminant<crate::Wake>, usize>,
}

impl Default for WakeCensus {
    fn default() -> Self {
        Self::over(&WAKES)
    }
}

impl WakeCensus {
    pub(crate) fn over(table: &'static WakeTable) -> Self {
        Self {
            table,
            slots: aterm_hash::FxHashMap::default(),
        }
    }

    /// Count one delivered proxy event by its variant.
    pub(crate) fn note(&mut self, ev: &crate::Wake) {
        let table = self.table;
        let slot = *self
            .slots
            .entry(std::mem::discriminant(ev))
            .or_insert_with(|| table.slot(&variant_name(ev)));
        table.counts[slot].fetch_add(1, Ordering::Relaxed);
    }
}

fn variant_name(ev: &dyn fmt::Debug) -> String {
    struct LeadingIdent(String);
    impl fmt::Write for LeadingIdent {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let end = s
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(s.len());
            self.0.push_str(&s[..end]);
            if end == s.len() {
                Ok(())
            } else {
                Err(fmt::Error)
            }
        }
    }
    let mut name = LeadingIdent(String::new());
    let _ = fmt::write(&mut name, format_args!("{ev:?}"));
    name.0
}

#[cfg(test)]
#[path = "wakes_tests.rs"]
mod tests;
