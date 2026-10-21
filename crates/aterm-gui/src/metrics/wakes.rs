// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE USER-EVENT WAKES BY `Wake` VARIANT (`wakes_by_variant`, responsiveness
//! audit item 2). `event_wakes` counts every loop iteration; nothing said which
//! proxy event drove them.
//!
//! Two halves, split by owner. [`WakeTable`] is the published ledger: one
//! count per slot, written with a single relaxed `fetch_add` and read by the
//! control thread with no lock. [`WakeCensus`] is the UI thread's map from a
//! variant's discriminant to its slot, a field of `App`, so a delivered event
//! costs one map lookup and one atomic add. A variant's name is read once,
//! the first time a census sees it, by [`variant_name`].

use std::fmt;
use std::mem::Discriminant;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Slots in a [`WakeTable`]. `Wake` has about 184 variants; the last slot is
/// reserved for `other`, which counts every variant seen after the rest are
/// claimed.
const WAKE_SLOTS: usize = 256;
const OTHER: usize = WAKE_SLOTS - 1;

/// The process's wake ledger, read by `metrics` and zeroed by `metrics reset`.
pub(crate) static WAKES: WakeTable = WakeTable::new();

/// Counts by slot, and each slot's variant name. A slot is claimed for a
/// name once, in slot order, and keeps it for the life of the table; `reset`
/// zeroes the counts only.
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

    /// The slot that counts `name`: the first slot already holding it, or
    /// the first free one, claimed for it. Every claim scans from slot 0 and
    /// a claimed slot never changes, so two censuses over one table (test
    /// `App`s) share one slot per variant and a name is never listed twice.
    /// Cold: a census calls it once per variant it sees.
    fn slot(&self, name: &str) -> usize {
        for (i, cell) in self.names[..OTHER].iter().enumerate() {
            // Bounded: one short string per variant, for the life of the table.
            if *cell.get_or_init(|| Box::leak(name.into())) == name {
                return i;
            }
        }
        self.names[OTHER].get_or_init(|| "other");
        OTHER
    }

    /// The counted wakes since the last reset, busiest first (ties by name),
    /// zero entries omitted. A slot claimed while this reads may be missed
    /// by this read; its count is the few events in flight.
    #[must_use]
    pub(crate) fn attribution(&self) -> Vec<(&'static str, u64)> {
        let mut out: Vec<(&'static str, u64)> = self
            .names
            .iter()
            .zip(&self.counts)
            .filter_map(|(name, count)| {
                // The name first: a slot's name is set before its first count.
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

/// UI-thread state: which slot of its table each `Wake` variant counts in.
/// Lives in `App`; no static, no lock.
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

/// The leading identifier of `ev`'s `Debug` spelling, which for a derived
/// `Debug` is the variant's name. The writer takes identifier bytes and
/// returns `Err` at the first other byte (` {`, `(`), and the derived impl
/// stops at that error before it formats a field, so the payload is never
/// formatted: a `Wake` can carry a whole document's bytes.
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
    // The `Err` is the writer ending the spelling after the name.
    let _ = fmt::write(&mut name, format_args!("{ev:?}"));
    name.0
}

#[cfg(test)]
mod tests {
    use super::{OTHER, WAKE_SLOTS, WakeCensus, WakeTable, variant_name};
    use crate::{Wake, WindowId};
    use std::cell::Cell;

    fn local_table() -> &'static WakeTable {
        Box::leak(Box::new(WakeTable::new()))
    }

    /// A payload that counts the times its `Debug` runs.
    struct Probe<'a>(&'a Cell<u32>);
    impl std::fmt::Debug for Probe<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.set(self.0.get() + 1);
            f.write_str("probe")
        }
    }

    #[derive(Debug)]
    #[expect(
        dead_code,
        reason = "the payloads are read only through the derived Debug"
    )]
    enum Shapes<'a> {
        Big { first: Probe<'a>, second: Probe<'a> },
        Tup(Probe<'a>, Probe<'a>),
        Unit,
    }

    #[test]
    fn variant_name_never_formats_the_payload() {
        let runs = Cell::new(0);
        let big = Shapes::Big {
            first: Probe(&runs),
            second: Probe(&runs),
        };
        let tup = Shapes::Tup(Probe(&runs), Probe(&runs));
        assert_eq!(variant_name(&big), "Big");
        assert_eq!(variant_name(&tup), "Tup");
        assert_eq!(variant_name(&Shapes::Unit), "Unit");
        assert_eq!(runs.get(), 0, "no payload was formatted");
        let output = Wake::Output {
            session: 7,
            window: WindowId(0),
        };
        assert_eq!(variant_name(&output), "Output");

        // Control: the probes do count when the values are formatted whole.
        let whole = format!("{big:?} {tup:?}");
        assert_eq!(
            whole,
            "Big { first: probe, second: probe } Tup(probe, probe)"
        );
        assert_eq!(runs.get(), 4);
    }

    #[test]
    fn a_census_counts_each_variant_in_its_own_slot_busiest_first() {
        let table = local_table();
        let mut census = WakeCensus::over(table);
        for session in 0..3 {
            census.note(&Wake::Output {
                session,
                window: WindowId(0),
            });
        }
        census.note(&Wake::WarmupResult);
        census.note(&Wake::ControlPrepared);
        assert_eq!(
            table.attribution(),
            [("Output", 3), ("ControlPrepared", 1), ("WarmupResult", 1)]
        );
    }

    #[test]
    fn reset_zeroes_counts_and_keeps_names() {
        let table = local_table();
        let mut census = WakeCensus::over(table);
        census.note(&Wake::WarmupResult);
        census.note(&Wake::ControlPrepared);
        table.reset();
        assert!(table.attribution().is_empty(), "zero entries are omitted");

        census.note(&Wake::ControlPrepared);
        assert_eq!(table.attribution(), [("ControlPrepared", 1)]);
        assert_eq!(table.names[0].get(), Some(&"WarmupResult"), "names survive");
        assert_eq!(table.names[1].get(), Some(&"ControlPrepared"));
        assert_eq!(table.names[2].get(), None, "no slot was claimed again");
    }

    #[test]
    fn two_censuses_over_one_table_share_a_variants_slot() {
        let table = local_table();
        let mut first = WakeCensus::over(table);
        let mut second = WakeCensus::over(table);
        first.note(&Wake::WarmupResult);
        second.note(&Wake::ControlPrepared);
        second.note(&Wake::WarmupResult);
        assert_eq!(
            table.attribution(),
            [("WarmupResult", 2), ("ControlPrepared", 1)]
        );
        assert_eq!(table.names[2].get(), None);
    }

    #[test]
    fn a_full_table_counts_later_variants_as_other() {
        let table = local_table();
        let names: Vec<String> = (0..WAKE_SLOTS + 4).map(|i| format!("V{i}")).collect();
        let slots: Vec<usize> = names.iter().map(|name| table.slot(name)).collect();
        assert_eq!(slots[..OTHER], (0..OTHER).collect::<Vec<_>>());
        assert!(slots[OTHER..].iter().all(|&slot| slot == OTHER));
        assert_eq!(table.names[OTHER].get(), Some(&"other"));
        // Control: a claimed name keeps its own slot once the table is full.
        assert_eq!(table.slot("V0"), 0);
    }
}
