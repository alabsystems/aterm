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

/// Lock-free per-slot counts and once-claimed names: the wake ledger's variants, and the control
/// listener's requests by peer and verb (`control_listener::note_request`).
pub(crate) struct WakeTable {
    counts: [AtomicU64; WAKE_SLOTS],
    lifetime: [AtomicU64; WAKE_SLOTS],
    names: [OnceLock<&'static str>; WAKE_SLOTS],
}

impl WakeTable {
    pub(crate) const fn new() -> Self {
        Self {
            counts: [const { AtomicU64::new(0) }; WAKE_SLOTS],
            lifetime: [const { AtomicU64::new(0) }; WAKE_SLOTS],
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

    /// Count one event under `name`; a name past the table's 255 counts under `other`.
    pub(crate) fn note(&self, name: &str) {
        self.note_slot(self.slot(name));
    }

    fn note_slot(&self, slot: usize) {
        self.counts[slot].fetch_add(1, Ordering::Relaxed);
        let _ = self.lifetime[slot].try_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            Some(old.saturating_add(1))
        });
    }

    /// The table's lifetime delivered events, independent of `metrics reset`.
    /// The counters are separate observations, not one simultaneous system-wide census.
    pub(crate) fn lifetime(&self) -> (Vec<(&'static str, u64)>, bool) {
        let mut rows: Vec<_> = self
            .names
            .iter()
            .zip(&self.lifetime)
            .filter_map(|(name, count)| {
                let name = *name.get()?;
                let count = count.load(Ordering::Relaxed);
                (count != 0).then_some((name, count))
            })
            .collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let saturated = rows.iter().any(|(_, count)| *count == u64::MAX);
        (rows, saturated)
    }

    /// Counts since the last reset, sorted busiest first (ties by name), omitting zeroes.
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

/// What the census tells wakes apart by: a [`crate::Wake`]'s variant, and beside it a host
/// note's own ([`crate::harness_host::HostNote`]), so a take names the note that woke it.
type CensusKey = (
    Discriminant<crate::Wake>,
    Option<Discriminant<crate::harness_host::HostNote>>,
);

/// UI-thread map from each [`CensusKey`] to its [`WakeTable`] slot.
pub(crate) struct WakeCensus {
    table: &'static WakeTable,
    slots: aterm_hash::FxHashMap<CensusKey, usize>,
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

    /// Count one delivered proxy event by its variant, a host note by its note's
    /// (`HostNote.<note>`).
    pub(crate) fn note(&mut self, ev: &crate::Wake) {
        let table = self.table;
        let slot = *self
            .slots
            .entry(census_key(ev))
            .or_insert_with(|| table.slot(&census_name(ev)));
        table.note_slot(slot);
    }
}

fn census_key(ev: &crate::Wake) -> CensusKey {
    let note = match ev {
        crate::Wake::HostNote(note) => Some(std::mem::discriminant(note)),
        _ => None,
    };
    (std::mem::discriminant(ev), note)
}

/// A wake's census name: its variant's, and a host note's joined to its note's by a `.`, which no
/// identifier holds and the `name:count` pairs of `wakes_by_variant` leave alone.
fn census_name(ev: &crate::Wake) -> String {
    match ev {
        crate::Wake::HostNote(note) => format!("{}.{}", variant_name(ev), variant_name(note)),
        _ => variant_name(ev),
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
