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

/// A look stores the board it took, whatever the process holds, and every
/// host reads it (the only test that touches the stored board).
#[test]
fn a_look_stores_the_board_it_took() {
    look(NOW, &[]);
    let r = reading_for("any");
    assert_eq!(r.taken_ms, NOW);
    assert_eq!(r.held_slot_last_ms, None);
}

const MIN_MS: i64 = 60_000;

/// [`THREE_ONE_FREE`]'s snapshot at pressure `pressure`, landed at
/// `unix_ms`, stamped by it, its slots read `age_ms` before, its slot holders
/// in the tabs `slot_tabs`.
fn at(
    unix_ms: i64,
    slots: SlotsView,
    pressure: Pressure,
    age_ms: Option<u64>,
    slot_tabs: Option<Vec<u64>>,
) -> Snapshot {
    Snapshot {
        at_ns: u64::try_from(unix_ms).unwrap() * 1_000_000,
        slots_age_ms: age_ms,
        slot_tabs,
        ..snapshot(Some([1.0; 3]), pressure, Some(slots))
    }
}

/// What session `sid`'s host reads at the board's own look (the empty
/// reading before any look).
fn read_at_look(j: &Judged, sid: &str) -> OccupancyReading {
    let at = j.board.as_ref().map_or(0, |b| b.taken_unix_ms);
    j.reading(sid, at)
}

/// One look at `unix_ms` that took `snap`, landed then.
fn step(j: &mut Judged, snap: &Snapshot, unix_ms: i64, sessions: &[(u64, String)]) {
    j.step(
        board_of(snap, Some(unix_ms), Some(Level::Ok), unix_ms),
        Some(snap),
        sessions,
    );
}

/// `hold-room`'s waits are `await sys`'s, parsed with their leading `sys`
/// word as the grammar takes them; without it the grammar refuses them.
#[test]
fn hold_rooms_waits_parse_with_their_sys_word() {
    let slots = machine_wait(HOLD_ROOM_SLOTS).expect("the slot term parses");
    assert_eq!(slots.for_ms, 20 * 60_000);
    assert_eq!(slots.terms.len(), 1);
    let room = machine_wait(HOLD_ROOM_MACHINE).expect("the machine terms parse");
    assert_eq!(room.for_ms, 0);
    assert_eq!(room.terms.len(), 2);
    for text in [HOLD_ROOM_SLOTS, HOLD_ROOM_MACHINE] {
        let bare = text.strip_prefix("sys ").expect("a sys wait");
        assert!(AwaitSys::parse(bare).is_err(), "{bare}");
        assert_eq!(machine_wait(bare), None, "{bare}");
    }
}

/// The slot term holds only over consecutive looks that each saw a usable
/// slot, timed by their snapshots' stamps: twenty minutes of looks, not one
/// look; a look with none free, a stale board, or a slots reading older than
/// the board's cadence starts it over. The same snapshot seen again moves
/// nothing.
#[test]
fn the_slot_term_spans_twenty_minutes_of_the_boards_looks() {
    const NONE_FREE: SlotsView = SlotsView::Read {
        n: 3,
        held: 3,
        free: 0,
        pending: 0,
        unknown: 0,
        discounted: 0,
    };
    let t0 = NOW;
    let mut j = Judged::default();
    for m in [0, 10, 19] {
        step(
            &mut j,
            &at(
                t0 + m * MIN_MS,
                THREE_ONE_FREE,
                Pressure::Normal,
                Some(0),
                None,
            ),
            t0 + m * MIN_MS,
            &[],
        );
        assert_eq!(read_at_look(&j, "a").slots_free_ms, None, "{m} min");
    }
    let s20 = at(
        t0 + 20 * MIN_MS,
        THREE_ONE_FREE,
        Pressure::Normal,
        Some(0),
        None,
    );
    step(&mut j, &s20, t0 + 20 * MIN_MS, &[]);
    assert_eq!(read_at_look(&j, "a").slots_free_ms, Some(20 * 60_000));
    // The same snapshot at the next look: it held no longer.
    j.step(
        board_of(&s20, Some(t0 + 20 * MIN_MS), None, t0 + 21 * MIN_MS),
        Some(&s20),
        &[],
    );
    assert_eq!(read_at_look(&j, "a").slots_free_ms, Some(20 * 60_000));

    // Negative controls: each break starts the span over.
    let fed = |j: &mut Judged, m: i64, slots, age| {
        step(
            j,
            &at(t0 + m * MIN_MS, slots, Pressure::Normal, age, None),
            t0 + m * MIN_MS,
            &[],
        );
        read_at_look(j, "a").slots_free_ms
    };
    // None free at 22: from 23, twenty more minutes.
    assert_eq!(fed(&mut j, 22, NONE_FREE, Some(0)), None);
    assert_eq!(fed(&mut j, 23, THREE_ONE_FREE, Some(0)), None);
    assert_eq!(fed(&mut j, 42, THREE_ONE_FREE, Some(0)), None);
    assert_eq!(fed(&mut j, 43, THREE_ONE_FREE, Some(0)), Some(20 * 60_000));
    // A stale board.
    let late = at(
        t0 + 44 * MIN_MS,
        THREE_ONE_FREE,
        Pressure::Normal,
        Some(0),
        None,
    );
    j.step(
        board_of(&late, Some(t0 + 44 * MIN_MS), None, t0 + 50 * MIN_MS),
        Some(&late),
        &[],
    );
    assert_eq!(read_at_look(&j, "a").slots_free_ms, None);
    assert_eq!(read_at_look(&j, "a").room, None);
    assert_eq!(fed(&mut j, 51, THREE_ONE_FREE, Some(0)), None);
    assert_eq!(fed(&mut j, 70, THREE_ONE_FREE, Some(0)), None);
    // A slots reading past the board's cadence reads as warming; one within
    // it (the reader's at-rest period) speaks for the look.
    let stale_ms = u64::try_from(STALE_AFTER.as_millis()).unwrap();
    assert_eq!(
        fed(&mut j, 71, THREE_ONE_FREE, Some(stale_ms)),
        Some(20 * 60_000)
    );
    assert_eq!(fed(&mut j, 72, THREE_ONE_FREE, Some(stale_ms + 1)), None);
    assert_eq!(fed(&mut j, 92, THREE_ONE_FREE, Some(0)), None);
}

/// A BOARD WHOSE LOOKS STOPPED READS STALE ([`Judged::reading`]): a fresh
/// board read up to two disk looks after it was taken reads fresh, and past
/// that stale, however fresh it was when taken (its looks stopped, so what it
/// judged then no longer speaks for now). NEGATIVE CONTROL: the same board
/// read at its own look and at the edge.
#[test]
fn a_board_whose_looks_stopped_reads_stale() {
    let mut j = Judged::default();
    step(
        &mut j,
        &at(NOW, THREE_ONE_FREE, Pressure::Normal, None, None),
        NOW,
        &[],
    );
    let edge = i64::try_from(STALE_AFTER.as_millis()).unwrap();
    for (now, stale) in [(NOW, false), (NOW + edge, false), (NOW + edge + 1, true)] {
        let r = j.reading("a", now);
        assert_eq!((r.taken_ms, r.stale), (NOW, stale), "{now}");
    }
}

/// The machine terms (`avail>=8GiB and mem<critical`) at the last look:
/// unjudged where the pressure is unread.
#[test]
fn the_machine_terms_are_judged_at_each_look() {
    let mut j = Judged::default();
    for (pressure, room) in [
        (Pressure::Normal, Some(true)),
        (Pressure::Warn, Some(true)),
        (Pressure::Critical, Some(false)),
        (Pressure::Unknown, None),
    ] {
        step(
            &mut j,
            &at(NOW, THREE_ONE_FREE, pressure, None, None),
            NOW,
            &[],
        );
        assert_eq!(read_at_look(&j, "a").room, room, "{pressure:?}");
    }
    let mut low = at(NOW + 1, THREE_ONE_FREE, Pressure::Normal, None, None);
    low.avail = Some((8 << 30) - 1);
    step(&mut j, &low, NOW + 1, &[]);
    assert_eq!(read_at_look(&j, "a").room, Some(false));
}

/// A session is held at each look a build slot's holder lay in its tab, and
/// keeps that look for [`HELD_KEEP`]; a session whose tab held none, a look
/// with no holders attributed, and a stale board add nothing.
#[test]
fn a_session_is_held_at_the_looks_its_tab_held_a_slot() {
    let sessions = [(7, "a".to_string()), (9, "b".to_string())];
    let mut j = Judged::default();
    step(
        &mut j,
        &at(NOW, THREE_ONE_FREE, Pressure::Normal, None, Some(vec![7])),
        NOW,
        &sessions,
    );
    assert_eq!(read_at_look(&j, "a").held_slot_last_ms, Some(NOW));
    assert_eq!(read_at_look(&j, "b").held_slot_last_ms, None);
    // Later looks with no holder in either tab, or none attributed: "a"
    // keeps its last.
    for tabs in [Some(vec![]), Some(vec![11]), None] {
        step(
            &mut j,
            &at(NOW + MIN_MS, THREE_ONE_FREE, Pressure::Normal, None, tabs),
            NOW + MIN_MS,
            &sessions,
        );
        assert_eq!(read_at_look(&j, "a").held_slot_last_ms, Some(NOW));
        assert_eq!(read_at_look(&j, "b").held_slot_last_ms, None);
    }
    // A stale board's snapshot names "b": not taken.
    let old = at(
        NOW + MIN_MS,
        THREE_ONE_FREE,
        Pressure::Normal,
        None,
        Some(vec![9]),
    );
    j.step(
        board_of(&old, Some(NOW + MIN_MS), None, NOW + 10 * MIN_MS),
        Some(&old),
        &sessions,
    );
    assert_eq!(read_at_look(&j, "b").held_slot_last_ms, None);
    // Kept HELD_KEEP, then forgotten.
    let keep = i64::try_from(HELD_KEEP.as_millis()).unwrap();
    step(
        &mut j,
        &at(NOW + keep, THREE_ONE_FREE, Pressure::Normal, None, None),
        NOW + keep,
        &sessions,
    );
    assert_eq!(read_at_look(&j, "a").held_slot_last_ms, Some(NOW));
    step(
        &mut j,
        &at(NOW + keep + 1, THREE_ONE_FREE, Pressure::Normal, None, None),
        NOW + keep + 1,
        &sessions,
    );
    assert_eq!(read_at_look(&j, "a").held_slot_last_ms, None);
}

/// The board as a session's host reads it: the empty reading before any
/// look; then its facts, the usable slots net of those a dead wrapper's build
/// still occupies, and the pressure in the supervisor's words.
#[test]
fn the_reading_is_the_board_for_one_session() {
    let j = Judged::default();
    assert_eq!(read_at_look(&j, "a"), OccupancyReading::default());
    let mut j = Judged::default();
    let slots = SlotsView::Read {
        n: 4,
        held: 1,
        free: 3,
        pending: 0,
        unknown: 0,
        discounted: 1,
    };
    step(
        &mut j,
        &at(NOW, slots, Pressure::Warn, None, None),
        NOW,
        &[],
    );
    assert_eq!(
        read_at_look(&j, "a"),
        OccupancyReading {
            taken_ms: NOW,
            stale: false,
            avail_mib: Some(96 << 10),
            pressure: Some(MemoryPressure::Warn),
            disk: Some(Level::Ok),
            slots: Some(SlotsSeen { usable: 2, n: 4 }),
            slots_free_ms: None,
            room: Some(true),
            held_slot_last_ms: None,
        }
    );
    // Unread slots are none.
    step(
        &mut j,
        &at(
            NOW,
            SlotsView::Unread(SlotsUnread::Refused),
            Pressure::Critical,
            None,
            None,
        ),
        NOW,
        &[],
    );
    let r = read_at_look(&j, "a");
    assert_eq!(r.slots, None);
    assert_eq!(r.pressure, Some(MemoryPressure::Critical));
}

/// A slot's holder counts for the tab whose tree holds it: found by its pid
/// in the sweep, attributed through its parent chain to the nearest tab
/// shell. A holder outside every tab, or not in the sweep, counts for none;
/// with no holders read or no sweep, nothing is attributed. A holder found
/// in a tab counts only while one current start record matches both its
/// sweep identity and the holder's birth. A reused pid or an unknown record
/// cannot carry the sweep's parent chain to a different holder. The current
/// start is asked of the holders found in a tab and of no other.
#[test]
fn a_slot_holder_counts_for_the_tab_whose_tree_holds_it() {
    use aterm_uds::process::{BootClock, ProcessBirth, ProcessStart};

    let proc = |pid, ppid| ProcSample {
        pid,
        start: Some(u64::from(pid)),
        ppid,
        ..ProcSample::default()
    };
    // aterm (100) runs tab 7's shell (200) and tab 9's (300); 210 and 211
    // are builds under tab 7's shell, 400 one outside aterm.
    let procs = [
        proc(100, 1),
        proc(200, 100),
        proc(210, 200),
        proc(211, 210),
        proc(300, 100),
        proc(400, 1),
    ];
    let tab = |key, shell_pid| TabRoot {
        key,
        shell_pid,
        label: String::new(),
        program: String::new(),
    };
    let tabs = [tab(7, 200), tab(9, 300)];
    let read = |pids: &[u32]| SlotsRead::Read {
        counts: SlotCounts::default(),
        holders: pids
            .iter()
            .map(|&pid| Holder { pid, birth_us: 1 })
            .collect(),
    };
    let mut current = |pid: u32| {
        Some(ProcessStart {
            boot: BootClock(u64::from(pid)),
            wall: Some(ProcessBirth {
                seconds: 0,
                microseconds: 1,
            }),
        })
    };
    assert_eq!(
        slot_tabs(&read(&[211, 210]), Some(&procs), 100, &tabs, &mut current),
        Some(vec![7])
    );
    // Negative controls: outside every tab, gone from the sweep, none held.
    assert_eq!(
        slot_tabs(&read(&[400, 999]), Some(&procs), 100, &tabs, &mut current),
        Some(vec![])
    );
    assert_eq!(
        slot_tabs(&read(&[]), Some(&procs), 100, &tabs, &mut current),
        Some(vec![])
    );
    assert_eq!(
        slot_tabs(&read(&[300, 210]), Some(&procs), 100, &tabs, &mut current),
        Some(vec![7, 9])
    );
    assert_eq!(
        slot_tabs(&read(&[210]), None, 100, &tabs, &mut current),
        None
    );
    assert_eq!(
        slot_tabs(
            &SlotsRead::Unread(Unread::Refused),
            Some(&procs),
            100,
            &tabs,
            &mut current
        ),
        None
    );
    // Tab 9's holder gone since, its pid another process's: tab 7's alone.
    let mut asked = Vec::new();
    assert_eq!(
        slot_tabs(
            &read(&[300, 210, 400, 999]),
            Some(&procs),
            100,
            &tabs,
            &mut |pid| {
                asked.push(pid);
                if pid == 300 { None } else { current(pid) }
            }
        ),
        Some(vec![7])
    );
    assert_eq!(asked, [300, 210]);

    // The sweep saw an old member of tab 7. A fresh slot reading proves a
    // new holder at that same pid: its current birth matches, but its boot
    // start differs from the sweep's. It must not inherit the old tab.
    let replacement = Some(ProcessStart {
        boot: BootClock(999),
        wall: Some(ProcessBirth {
            seconds: 0,
            microseconds: 2,
        }),
    });
    let replaced = SlotsRead::Read {
        counts: SlotCounts::default(),
        holders: vec![Holder {
            pid: 210,
            birth_us: 2,
        }],
    };
    assert_eq!(
        slot_tabs(&replaced, Some(&procs), 100, &tabs, &mut |_| replacement),
        Some(vec![]),
        "a current holder cannot use another process's swept parent chain"
    );

    let matching = current(210).expect("known start");
    for unknown in [
        None,
        Some(ProcessStart {
            wall: None,
            ..matching
        }),
        Some(ProcessStart {
            wall: replacement.and_then(|p| p.wall),
            ..matching
        }),
        Some(ProcessStart {
            wall: Some(ProcessBirth {
                seconds: u64::MAX,
                microseconds: 1,
            }),
            ..matching
        }),
    ] {
        assert_eq!(
            slot_tabs(&read(&[210]), Some(&procs), 100, &tabs, &mut |_| unknown),
            Some(vec![]),
            "unknown, mismatched or overflowing wall identity: {unknown:?}"
        );
    }
    let mut unknown = procs.clone();
    unknown[2].start = None;
    assert_eq!(
        slot_tabs(&read(&[210]), Some(&unknown), 100, &tabs, &mut current),
        Some(vec![]),
        "an unknown sweep identity is no match"
    );
    assert_eq!(
        slot_tabs(&read(&[210]), Some(&procs), 100, &tabs, &mut current),
        Some(vec![7]),
        "the same known holder still counts"
    );
}

/// A board looked at `unix_ms` with `free` usable slots of four, its holders
/// in the tabs `slot_tabs`.
fn board_with(j: &mut Judged, unix_ms: i64, free: u16, slot_tabs: Option<Vec<u64>>) {
    let slots = SlotsView::Read {
        n: 4,
        held: 4 - free,
        free,
        pending: 0,
        unknown: 0,
        discounted: 0,
    };
    let sessions = [(7, "a".to_string()), (9, "b".to_string())];
    step(
        j,
        &at(unix_ms, slots, Pressure::Normal, None, slot_tabs),
        unix_ms,
        &sessions,
    );
}

/// THE MACHINE'S PACE: six lines in any hour across every session, the
/// seventh held; a line an hour old no longer counts. A nudge needs no slot.
#[test]
fn the_machine_paces_six_lines_an_hour_across_its_sessions() {
    let mut j = Judged::default();
    board_with(&mut j, NOW, 0, None);
    for n in 0..MACHINE_LINES_PER_HOUR {
        let sid = format!("s-{n}");
        let at = NOW + i64::try_from(n).unwrap() * MIN_MS;
        assert_eq!(j.pace(&sid, RuleId::WaitNudge, NOW, at), PaceGrant::Granted);
    }
    let late = NOW + 10 * MIN_MS;
    assert_eq!(
        j.pace("s-new", RuleId::WaitNudge, NOW, late),
        PaceGrant::Machine
    );
    // CONTROL: an hour after the first line, one more goes.
    assert_eq!(
        j.pace("s-new", RuleId::WaitNudge, NOW, NOW + 60 * MIN_MS),
        PaceGrant::Granted
    );
}

/// THE RESERVATION: one free slot reserves for one session's `hold-room`
/// line; the next session waits until the reservation's ten minutes pass,
/// or until the reserving session's tab is seen holding a slot.
#[test]
fn one_free_slot_is_reserved_for_one_line() {
    let mut j = Judged::default();
    board_with(&mut j, NOW, 1, None);
    assert_eq!(j.pace("a", RuleId::HoldRoom, NOW, NOW), PaceGrant::Granted);
    assert_eq!(
        j.pace("b", RuleId::HoldRoom, NOW, NOW + 1_000),
        PaceGrant::Reserved
    );
    // A nudge takes no slot.
    assert_eq!(
        j.pace("b", RuleId::WaitNudge, NOW, NOW + 2_000),
        PaceGrant::Granted
    );
    // Past the reservation's ten minutes (the board looked again), the slot
    // is free for the other.
    let past = NOW + RESERVE_MS;
    board_with(&mut j, past, 1, None);
    assert_eq!(j.pace("b", RuleId::HoldRoom, NOW, past), PaceGrant::Granted);
    // The build it was for took its slot: released at the look that saw it.
    let mut j = Judged::default();
    board_with(&mut j, NOW, 1, None);
    assert_eq!(j.pace("a", RuleId::HoldRoom, NOW, NOW), PaceGrant::Granted);
    board_with(&mut j, NOW + MIN_MS, 1, Some(vec![7]));
    assert_eq!(
        j.pace("b", RuleId::HoldRoom, NOW, NOW + MIN_MS),
        PaceGrant::Granted
    );
    // NEGATIVE CONTROL: a look that sees another tab's holder releases
    // nothing.
    let mut j = Judged::default();
    board_with(&mut j, NOW, 1, None);
    assert_eq!(j.pace("a", RuleId::HoldRoom, NOW, NOW), PaceGrant::Granted);
    board_with(&mut j, NOW + MIN_MS, 1, Some(vec![9]));
    assert_eq!(
        j.pace("b", RuleId::HoldRoom, NOW, NOW + MIN_MS),
        PaceGrant::Reserved
    );
}

/// THE OLDEST HOLD FIRST: a younger hold that asks when a slot frees waits
/// for an older one already waiting; the older takes it. An older hold that
/// stopped asking keeps no one waiting.
#[test]
fn the_oldest_hold_takes_a_freed_slot_first() {
    let older = NOW - 5 * 60 * MIN_MS;
    let younger = NOW - 3 * 60 * MIN_MS;
    let mut j = Judged::default();
    board_with(&mut j, NOW, 0, None);
    assert_eq!(
        j.pace("old", RuleId::HoldRoom, older, NOW),
        PaceGrant::Reserved
    );
    let freed = NOW + MIN_MS;
    board_with(&mut j, freed, 1, None);
    assert_eq!(
        j.pace("young", RuleId::HoldRoom, younger, freed),
        PaceGrant::Reserved,
        "an older hold waits"
    );
    assert_eq!(
        j.pace("old", RuleId::HoldRoom, older, freed + 1_000),
        PaceGrant::Granted
    );
    // CONTROL: the older hold stopped asking; the younger takes the slot.
    let mut j = Judged::default();
    board_with(&mut j, NOW, 0, None);
    assert_eq!(
        j.pace("old", RuleId::HoldRoom, older, NOW),
        PaceGrant::Reserved
    );
    let later = NOW + ASK_KEEP_MS + 1;
    board_with(&mut j, later, 1, None);
    assert_eq!(
        j.pace("young", RuleId::HoldRoom, younger, later),
        PaceGrant::Granted
    );
}

/// A board that no longer speaks for now proves no slot: a slot rule's line
/// waits; the same board fresh lets it go.
#[test]
fn a_stale_board_reserves_no_slot() {
    let stale = NOW + 2 * i64::try_from(STALE_AFTER.as_millis()).unwrap();
    for (at, grant) in [(stale, PaceGrant::Reserved), (NOW, PaceGrant::Granted)] {
        let mut j = Judged::default();
        board_with(&mut j, NOW, 2, None);
        assert_eq!(j.pace("a", RuleId::HoldRoom, NOW, at), grant, "{at}");
    }
}
