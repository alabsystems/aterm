// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Occupancy board snapshot, staleness, and slot-reader tests.

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
    let edge = i64::try_from(STALE_AFTER.as_millis()).unwrap();
    assert!(!board_of(&s, Some(NOW - edge), None, NOW).stale);
    assert!(board_of(&s, Some(NOW - edge - 1), None, NOW).stale);
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
/// host reads it (the only test that touches the stored board). A poke's
/// look, which takes no ring row.
#[test]
fn a_look_stores_the_board_it_took() {
    look(NOW, &[], Look::Poke);
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
    j.step(
        board_of(&s20, Some(t0 + 20 * MIN_MS), None, t0 + 21 * MIN_MS),
        Some(&s20),
        &[],
    );
    assert_eq!(read_at_look(&j, "a").slots_free_ms, Some(20 * 60_000));

    let fed = |j: &mut Judged, m: i64, slots, age| {
        step(
            j,
            &at(t0 + m * MIN_MS, slots, Pressure::Normal, age, None),
            t0 + m * MIN_MS,
            &[],
        );
        read_at_look(j, "a").slots_free_ms
    };
    assert_eq!(fed(&mut j, 22, NONE_FREE, Some(0)), None);
    assert_eq!(fed(&mut j, 23, THREE_ONE_FREE, Some(0)), None);
    assert_eq!(fed(&mut j, 42, THREE_ONE_FREE, Some(0)), None);
    assert_eq!(fed(&mut j, 43, THREE_ONE_FREE, Some(0)), Some(20 * 60_000));
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
        tab: key,
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
    assert_eq!(
        j.pace("b", RuleId::WaitNudge, NOW, NOW + 2_000),
        PaceGrant::Granted
    );
    let past = NOW + RESERVE_MS;
    board_with(&mut j, past, 1, None);
    assert_eq!(j.pace("b", RuleId::HoldRoom, NOW, past), PaceGrant::Granted);
    let mut j = Judged::default();
    board_with(&mut j, NOW, 1, None);
    assert_eq!(j.pace("a", RuleId::HoldRoom, NOW, NOW), PaceGrant::Granted);
    board_with(&mut j, NOW + MIN_MS, 1, Some(vec![7]));
    assert_eq!(
        j.pace("b", RuleId::HoldRoom, NOW, NOW + MIN_MS),
        PaceGrant::Granted
    );
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

// SURFACING (slice 1e): the told word, the ready proxy, the slot-minutes idle while ready, the
// status fields and the `occupancy` verb's replies.

use aterm_agent::supervise::occupancy::rules::Guards;
use aterm_agent::supervise::occupancy::{SaidHold, SaidRule, SaidVerdict};
use aterm_agent::supervise::{Autopilot, Guard};

const HOUR_MS: i64 = 60 * MIN_MS;

/// A word told under `mode` with a hold begun at `since` (none for `None`), each rule `verdicts`,
/// `guard` holding at its break (a brake where it is neither a person's nor a draft's).
fn said(
    mode: Occupancy,
    since: Option<i64>,
    verdicts: [SaidVerdict; 2],
    guard: Option<Guard>,
) -> OccupancySaid {
    OccupancySaid {
        mode,
        hold: since.map(|since_ms| SaidHold {
            since_ms,
            declared: false,
        }),
        ready: false,
        guards: Some(Guards {
            person: guard == Some(Guard::Person),
            draft: guard == Some(Guard::Draft),
            brake: guard.filter(|g| !matches!(g, Guard::Person | Guard::Draft)),
            ..Guards::clear(Autopilot::Cruise)
        }),
        rules: [
            SaidRule {
                rule: RuleId::WaitNudge,
                verdict: verdicts[0],
                at_ms: NOW,
            },
            SaidRule {
                rule: RuleId::HoldRoom,
                verdict: verdicts[1],
                at_ms: NOW,
            },
        ],
        at_ms: NOW,
        reading: OccupancyReading::default(),
    }
}

const QUIET: [SaidVerdict; 2] = [SaidVerdict::Quiet, SaidVerdict::Quiet];

/// Thirty-one looks a minute apart from [`NOW`], one slot of three free and holders in tab 7
/// (session `a`'s) and tab 9 (session `b`'s), the machine with room: what each look's row says.
fn thirty_minutes(j: &mut Judged, sessions: &[(u64, String)]) -> Vec<RingRow> {
    (0..=30)
        .map(|m| {
            let t = NOW + m * MIN_MS;
            let snap = at(
                t,
                THREE_ONE_FREE,
                Pressure::Normal,
                Some(0),
                Some(vec![7, 9]),
            );
            step(j, &snap, t, sessions);
            j.look_row(sessions, t)
        })
        .collect()
}

/// THE READY PROXY AND THE SLOT-MINUTES IDLE WHILE READY: a told hold past two hours in a
/// session whose tab held a slot reads ready once the board's slot term spans its twenty
/// minutes, and each look from then counts one slot-minute (one usable slot, one ready
/// session). NEGATIVE CONTROLS: a hold under two hours (in a tab that held a slot too, so the
/// two hours are all it lacks), a closed hold, and a session nothing was told of are never
/// ready, and with none ready no minute counts.
#[test]
fn a_told_hold_past_two_hours_is_ready_while_a_slot_stands_free() {
    let sessions = [
        (7, "a".to_string()),
        (9, "b".to_string()),
        (11, "c".to_string()),
    ];
    let mut j = Judged::default();
    j.tell(
        "a",
        Some(said(
            Occupancy::Observe,
            Some(NOW - 3 * HOUR_MS),
            QUIET,
            Some(Guard::Person),
        )),
    );
    j.tell(
        "b",
        Some(said(
            Occupancy::Observe,
            Some(NOW - 10 * MIN_MS),
            QUIET,
            None,
        )),
    );
    let rows = thirty_minutes(&mut j, &sessions);
    let ready = |row: &RingRow, sid: &str| row.session(sid).is_some_and(|s| s.ready);
    let first = rows
        .iter()
        .position(|row| ready(row, "a"))
        .expect("a reads ready");
    assert!(
        rows[first].at >= NOW + 20 * MIN_MS,
        "the slot term's twenty minutes"
    );
    assert!(rows[first..].iter().all(|row| ready(row, "a")));
    assert!(
        rows.iter()
            .all(|row| row.session("b").and_then(|s| s.held_ms) == Some(row.at)),
        "b's tab holds a slot at every look: only its hold's age keeps it from ready"
    );
    assert!(rows.iter().all(|row| !ready(row, "b") && !ready(row, "c")));
    assert_eq!(
        rows[0].session("a").and_then(|s| s.guard.as_deref()),
        Some("person"),
        "the told guard is the row's"
    );
    assert_eq!(rows[0].session("a").and_then(|s| s.held_ms), Some(NOW));
    let looks = u64::try_from(rows.len() - first).unwrap();
    assert_eq!(j.ready_now(), 1);
    assert_eq!(j.idle_ready_min(NOW + 30 * MIN_MS), looks);

    // A closed hold is no longer ready, and the minutes stop.
    j.tell("a", Some(said(Occupancy::Observe, None, QUIET, None)));
    let t = NOW + 31 * MIN_MS;
    step(
        &mut j,
        &at(t, THREE_ONE_FREE, Pressure::Normal, Some(0), Some(vec![7])),
        t,
        &sessions,
    );
    let row = j.look_row(&sessions, t);
    assert!(!ready(&row, "a"));
    assert_eq!(j.ready_now(), 0);
    assert_eq!(j.idle_ready_min(t), looks, "no minute added");
}

/// The rows a predecessor's ring left are taken once, the newest day of them.
#[test]
fn the_ring_seeds_the_rows_once() {
    let row = |at: i64| RingRow {
        at,
        ..RingRow::default()
    };
    let mut j = Judged::default();
    let total = ring::RING_ROWS + 5;
    j.seed_rows(
        (0..total)
            .map(|i| row(NOW + i64::try_from(i).unwrap()))
            .collect(),
    );
    assert_eq!(j.rows.len(), ring::RING_ROWS);
    assert_eq!(j.rows.front().map(|r| r.at), Some(NOW + 5));
    j.seed_rows(vec![row(0)]);
    assert_eq!(j.rows.len(), ring::RING_ROWS, "taken once");
}

/// The status fields name the owner's mode, the session's told hold, the sessions ready, the
/// board's slots and the idle minutes: asserted by key, not by the whole line.
#[test]
fn the_status_fields_say_the_mode_the_hold_and_the_slots() {
    let fields = |words: &str| -> BTreeMap<String, String> {
        words
            .split(' ')
            .filter_map(|w| w.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    let keys = |words: &str| -> Vec<String> {
        words
            .split(' ')
            .filter_map(|w| w.split_once('=').map(|(k, _)| k.to_string()))
            .collect()
    };
    let none = status_words(Occupancy::Observe, None, Some("a"), NOW);
    assert_eq!(
        keys(&none),
        [
            "occupancy",
            "occupancy_hold",
            "ready",
            "slots",
            "idle_ready_min"
        ]
    );
    let f = fields(&none);
    assert_eq!(f["occupancy"], "observe");
    assert_eq!(f["occupancy_hold"], "none");
    assert_eq!(f["slots"], "-");
    assert_eq!(f["ready"], "0");

    let sessions = [(7, "a".to_string())];
    let mut j = Judged::default();
    j.tell(
        "a",
        Some(said(
            Occupancy::Prompt,
            Some(NOW - 3 * HOUR_MS),
            QUIET,
            None,
        )),
    );
    let _ = thirty_minutes(&mut j, &sessions);
    let now = NOW + 30 * MIN_MS;
    let words = status_words(Occupancy::Prompt, Some(&j), Some("a"), now);
    assert_eq!(keys(&words), keys(&none), "one field set");
    let f = fields(&words);
    assert_eq!(f["occupancy"], "prompt");
    assert_eq!(f["occupancy_hold"], "undeclared");
    assert_eq!(f["ready"], "1");
    assert_eq!(f["slots"], "1/3");
    assert_eq!(f["idle_ready_min"], j.idle_ready_min(now).to_string());
    // Another session's hold is its own.
    let other = fields(&status_words(Occupancy::Prompt, Some(&j), Some("z"), now));
    assert_eq!(other["occupancy_hold"], "none");
    assert_eq!(
        fields(&status_words(Occupancy::Off, Some(&j), None, now))["occupancy"],
        "off"
    );
}

/// The reply's `OK <n>` and its lines.
fn reply_lines(reply: &str) -> Vec<String> {
    let mut lines = reply.lines();
    let head = lines.next().expect("a head");
    let n: usize = head
        .strip_prefix("OK ")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("an OK head: {reply}"));
    let lines: Vec<String> = lines.map(str::to_string).collect();
    assert_eq!(lines.len(), n, "{reply}");
    lines
}

/// `occupancy why` against a scripted word: the head line names the mode, a line each rule,
/// and the guards' line; with nothing told, the mode and `unsaid`.
#[test]
fn why_answers_the_told_word_or_unsaid() {
    let s = said(
        Occupancy::Observe,
        Some(NOW - 45 * MIN_MS),
        [SaidVerdict::Would, SaidVerdict::Held(Guard::Draft)],
        Some(Guard::Draft),
    );
    let lines = reply_lines(&why_reply(Occupancy::Observe, Some(&s), NOW, 0));
    assert_eq!(lines.len(), 4, "{lines:#?}");
    assert!(
        lines[0].starts_with("occupancy=observe hold undeclared "),
        "{lines:#?}"
    );
    assert!(
        lines[1].starts_with("wait-nudge@v1 rev=builtin would "),
        "{lines:#?}"
    );
    assert!(
        lines[2].starts_with("hold-room@v1 rev=builtin held:draft "),
        "{lines:#?}"
    );
    assert!(lines[3].trim_start().starts_with("guards: "), "{lines:#?}");
    assert!(lines[3].contains("draft ✗"), "{lines:#?}");
    assert_eq!(
        lines[3].matches('✗').count(),
        1,
        "draft alone holds: {lines:#?}"
    );
    let unsaid = reply_lines(&why_reply(Occupancy::Prompt, None, NOW, 0));
    assert_eq!(unsaid, ["occupancy=prompt unsaid"]);
}

/// A `fact-hold@v1` ledger row of session `sid` at `ts` with `reason`, as the loop books it.
fn hold_row(ts: i64, sid: &str, reason: &str) -> String {
    format!(
        "{{\"ts\":{ts},\"sid\":\"{sid}\",\"rule_id\":\"fact-hold@v1\",\"decision\":\"waiting\",\
         \"command\":\"\",\"reason\":\"{reason}\"}}"
    )
}

/// `occupancy check --replay` over the board's own rows and the session's ledger: the hold the
/// ledger opened three hours back gives the nudge at the first look and `hold-room` once the
/// slot term spans its twenty minutes. NEGATIVE CONTROL: a ledger with no hold rows gives no
/// line of either.
#[test]
fn check_replay_runs_the_decider_over_the_rows_and_the_ledger() {
    let sessions = [(7, "a".to_string())];
    let mut j = Judged::default();
    let rows = thirty_minutes(&mut j, &sessions);
    let since = NOW - 3 * HOUR_MS;
    let ledger = format!(
        "{}\n{}\n",
        hold_row(since + 10 * MIN_MS, "a", "hold=open held_ms=600000"),
        hold_row(
            since + 20 * MIN_MS,
            "b",
            "hold=closed why=point held_ms=600000"
        ),
    );
    let window = (NOW, NOW + 30 * MIN_MS);
    let lines = reply_lines(&replay_reply("a", &rows, &ledger, window, 0));
    assert!(
        lines[0].contains(&format!("looks={}", rows.len())),
        "{lines:#?}"
    );
    assert!(lines[0].ends_with("holds=1"), "{lines:#?}");
    let changes: Vec<&str> = lines
        .iter()
        .filter_map(|l| l.split_once(' ').map(|(_, rest)| rest))
        .filter(|rest| rest.ends_with(" would"))
        .collect();
    assert_eq!(
        changes,
        ["wait-nudge@v1 would", "hold-room@v1 would"],
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.ends_with("first would never")),
        "{lines:#?}"
    );

    let none = reply_lines(&replay_reply("a", &rows, "", window, 0));
    assert!(none[0].ends_with("holds=0"), "{none:#?}");
    assert_eq!(
        none.iter()
            .filter(|l| l.ends_with("first would never"))
            .count(),
        2,
        "{none:#?}"
    );
    assert!(!none.iter().any(|l| l.ends_with(" would")), "{none:#?}");
}

/// The verb's forms: `why` (also bare), `check --replay <dur>` with a window it takes; anything
/// else is its usage.
#[test]
fn the_verb_takes_its_forms_and_refuses_others() {
    for rest in ["", "why", "  why  "] {
        assert!(
            cmd_occupancy(Some("s-none"), rest).starts_with("OK 1\noccupancy="),
            "{rest:?}"
        );
    }
    assert!(cmd_occupancy(Some("s-none"), "check --replay 24h").starts_with("OK "));
    for rest in [
        "check",
        "check --replay",
        "check --replay 3d",
        "check --replay soon",
        "why not",
        "bogus",
    ] {
        assert_eq!(cmd_occupancy(Some("s-none"), rest), USAGE, "{rest:?}");
    }
    assert!(cmd_occupancy(None, "check --replay 1h").starts_with("ERR occupancy"));
}

/// A POKE BETWEEN THE MINUTE'S LOOKS TAKES NO ROW (the facts ring is a row a minute, §6): the
/// board is taken afresh, but the rows, the sessions ready and the slot-minutes are the ticks'
/// alone, so a day of rows stays a day however often the disk watch is poked. NEGATIVE
/// CONTROL: each tick takes its row.
#[test]
fn a_poke_between_ticks_takes_no_row() {
    let sessions = [(7, "a".to_string())];
    let mut j = Judged::default();
    j.tell(
        "a",
        Some(said(
            Occupancy::Observe,
            Some(NOW - 3 * HOUR_MS),
            QUIET,
            None,
        )),
    );
    let mut ticks = 0;
    for m in 0..=30 {
        let t = NOW + m * MIN_MS;
        let snap = at(t, THREE_ONE_FREE, Pressure::Normal, Some(0), Some(vec![7]));
        step(&mut j, &snap, t, &sessions);
        assert!(j.looked(&sessions, t, Look::Tick).is_some(), "a tick's row");
        ticks += 1;
        for poke in 1..=3 {
            let t = t + poke * 10_000;
            let snap = at(t, THREE_ONE_FREE, Pressure::Normal, Some(0), Some(vec![7]));
            step(&mut j, &snap, t, &sessions);
            assert!(
                j.looked(&sessions, t, Look::Poke).is_none(),
                "a poke's none"
            );
        }
    }
    assert_eq!(j.rows.len(), ticks, "a row a tick");
    assert!(
        j.rows
            .iter()
            .zip(j.rows.iter().skip(1))
            .all(|(a, b)| b.at - a.at == MIN_MS),
        "a minute apart"
    );
    assert_eq!(j.ready_now(), 1);
}
