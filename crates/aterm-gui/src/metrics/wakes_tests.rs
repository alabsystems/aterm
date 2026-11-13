// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{OTHER, WAKE_SLOTS, WakeCensus, WakeTable, variant_name};
use crate::{Wake, WindowId};
use std::cell::Cell;

fn local_table() -> &'static WakeTable {
    Box::leak(Box::new(WakeTable::new()))
}

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
    assert_eq!(table.slot("V0"), 0);
}
