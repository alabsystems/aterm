// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The occupancy board: the snapshot's facts as the board copies them, its
//! staleness, and the slot reader's every reading as a snapshot carries it.

use super::*;
use aterm_sysprobe::slots::{Holder, SlotCounts};
use aterm_vitals::model::{CoreView, CpuView, MemSample, MemView};

const NOW: i64 = 1_792_656_000_000;

/// A snapshot of 18 cores, these load averages, 96 GiB available, memory
/// pressure `pressure` and `slots`.
fn snapshot(load: Option<[f64; 3]>, pressure: Pressure, slots: Option<SlotsView>) -> Snapshot {
    Snapshot {
        cpu: Some(CpuView {
            cores: vec![CoreView::default(); 18],
            ..CpuView::default()
        }),
        load,
        avail: Some(96 << 30),
        mem: Some(MemView {
            sample: MemSample {
                pressure,
                ..MemSample::default()
            },
            ..MemView::default()
        }),
        slots,
        ..Snapshot::default()
    }
}

const THREE_ONE_FREE: SlotsView = SlotsView::Read {
    n: 3,
    held: 2,
    free: 1,
    pending: 0,
    unknown: 0,
    discounted: 0,
};

#[test]
fn the_board_copies_the_snapshots_facts_and_says_when_it_is_stale() {
    let s = snapshot(
        Some([2.004, 4.5, 1.0]),
        Pressure::Warn,
        Some(THREE_ONE_FREE),
    );
    let b = board_of(&s, Some(NOW - 1_000), Some(Level::Low), NOW);
    assert_eq!(
        b,
        FactsBoard {
            taken_unix_ms: NOW,
            as_of_unix_ms: NOW - 1_000,
            cores: Some(18),
            load1_x100: Some(200),
            load5_x100: Some(450),
            avail_mib: Some(96 << 10),
            pressure: Some(Pressure::Warn),
            disk: Some(Level::Low),
            slots: Some(THREE_ONE_FREE),
            stale: false,
        }
    );
    // Stale only past two disk looks.
    let edge = i64::try_from(STALE_AFTER.as_millis()).unwrap();
    assert!(!board_of(&s, Some(NOW - edge), None, NOW).stale);
    assert!(board_of(&s, Some(NOW - edge - 1), None, NOW).stale);
    // No snapshot yet: every fact unread, and stale.
    let none = board_of(&Snapshot::default(), None, None, NOW);
    assert_eq!(
        none,
        FactsBoard {
            taken_unix_ms: NOW,
            as_of_unix_ms: 0,
            cores: None,
            load1_x100: None,
            load5_x100: None,
            avail_mib: None,
            pressure: None,
            disk: None,
            slots: None,
            stale: true,
        }
    );
    // An unread pressure and an unjudged volume are none; a load no load is
    // (NaN, negative) is none.
    let odd = snapshot(Some([f64::NAN, -1.0, 0.0]), Pressure::Unknown, None);
    let b = board_of(&odd, Some(NOW), Some(Level::Unknown), NOW);
    assert_eq!(
        (b.load1_x100, b.load5_x100, b.pressure, b.disk, b.slots),
        (None, None, None, None, None)
    );
}

#[test]
fn every_slot_reading_maps_to_the_snapshots_view() {
    let counts = SlotCounts {
        n: 4,
        held: 1,
        free: 2,
        pending: 0,
        unknown: 1,
        discounted: 1,
    };
    assert_eq!(
        slots_view(&SlotsRead::Read {
            counts,
            holders: vec![Holder {
                pid: 99,
                birth_us: 7,
            }],
        }),
        SlotsView::Read {
            n: 4,
            held: 1,
            free: 2,
            pending: 0,
            unknown: 1,
            discounted: 1,
        }
    );
    for (why, view) in [
        (Unread::Unsupported, SlotsUnread::Unsupported),
        (Unread::NotSampled, SlotsUnread::NotSampled),
        (Unread::Refused, SlotsUnread::Refused),
        (Unread::UnreadFormat, SlotsUnread::UnreadFormat),
    ] {
        assert_eq!(slots_view(&SlotsRead::Unread(why)), SlotsView::Unread(view));
    }
}

/// The log follows the slots and the disk word, not the load: a board whose
/// load moved logs nothing new.
#[test]
fn the_board_logs_a_change_of_its_slots_or_disk_word_only() {
    let a = board_of(
        &snapshot(Some([1.0; 3]), Pressure::Normal, Some(THREE_ONE_FREE)),
        Some(NOW),
        Some(Level::Ok),
        NOW,
    );
    assert_eq!(
        change_words(&a),
        "slots=1/3 held=2 free=1 pending=0 unknown=0 discounted=0 disk=ok"
    );
    let loaded = board_of(
        &snapshot(Some([9.0; 3]), Pressure::Warn, Some(THREE_ONE_FREE)),
        Some(NOW),
        Some(Level::Ok),
        NOW,
    );
    assert_eq!(change_words(&a), change_words(&loaded));
    let unread = board_of(
        &snapshot(
            None,
            Pressure::Normal,
            Some(SlotsView::Unread(SlotsUnread::Refused)),
        ),
        Some(NOW),
        None,
        NOW,
    );
    assert_eq!(
        change_words(&unread),
        "slots=unread why=refused disk=unknown"
    );
}

/// A look stores the board it took, whatever the process holds.
#[test]
fn a_look_stores_the_board_it_took() {
    look(NOW);
    assert_eq!(board().map(|b| b.taken_unix_ms), Some(NOW));
}
