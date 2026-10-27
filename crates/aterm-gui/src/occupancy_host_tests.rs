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
            cores: vec![
                CoreView {
                    measured: true,
                    ..Default::default()
                };
                18
            ],
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
            slots_as_of_unix_ms: None,
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
            slots_as_of_unix_ms: None,
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

/// A look stores the board it took, whatever the process holds, and every host reads it.
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

/// The slot term holds only over consecutive looks that each saw a usable slot.
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

/// [`Judged::reading`] marks a board stale once more than [`STALE_AFTER`] has elapsed since it was taken.
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

/// A slot's holder counts for the tab whose process tree holds it while its current start record matches.
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
        tab_id: None,
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

/// The machine paces six lines in any hour across every session.
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

/// One free slot reserves for one session's `hold-room` line until [`RESERVE_MS`] elapses or the session's tab holds a slot.
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

/// An older hold already waiting takes a freed slot ahead of a younger hold.
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

/// A slot line's re-ask by the session holding its reservation (its write skipped) is granted
/// against that reservation, kept as it was taken and with no second line drawn, though a build
/// took the slot since; under `CountOwn` it is refused for its own reservation, and under
/// `RenewOnAsk` the reservation is taken again from the re-ask.
#[test]
fn a_skipped_writes_reask_is_granted_against_its_reservation() {
    let reask = |fault: Option<fault::Fault>| {
        let mut j = Judged::default();
        board_with(&mut j, NOW, 1, None);
        assert_eq!(j.pace("a", RuleId::HoldRoom, NOW, NOW), PaceGrant::Granted);
        board_with(&mut j, NOW + MIN_MS, 0, None);
        let mut again = || j.pace("a", RuleId::HoldRoom, NOW, NOW + MIN_MS);
        let grant = match fault {
            Some(f) => fault::with(f, again),
            None => again(),
        };
        (grant, j.pace.lines.len(), j.pace.reserved.clone())
    };
    let taken = vec![Reservation {
        sid: "a".to_string(),
        since_ms: NOW,
        line: Some(Pool::Facts),
    }];
    assert_eq!(reask(None), (PaceGrant::Granted, 1, taken.clone()));
    assert_eq!(
        reask(Some(fault::Fault::CountOwn)).0,
        PaceGrant::Reserved,
        "counted against its own reservation, the holder waits"
    );
    let (grant, _, renewed) = reask(Some(fault::Fault::RenewOnAsk));
    assert_eq!(grant, PaceGrant::Granted);
    assert_ne!(
        renewed, taken,
        "the renewing re-ask takes the reservation again"
    );
}

/// A typed wake line draws its own pool: granted with the facts pool spent, while a fact line
/// at the same moment is held by the machine; the wake pool spent holds the next wake line.
#[test]
fn a_wake_line_under_a_spent_facts_pool_is_granted() {
    let mut pace = Pace::default();
    let ask = |sid: &'static str, pool| Ask {
        sid,
        slot: false,
        hold_since_ms: NOW,
        usable: 0,
        pool,
    };
    for n in 0..MACHINE_LINES_PER_HOUR {
        let at = NOW + i64::try_from(n).unwrap() * MIN_MS;
        assert_eq!(pace.ask(&ask("f", Pool::Facts), at), PaceGrant::Granted);
    }
    let later = NOW + 10 * MIN_MS;
    assert_eq!(pace.ask(&ask("f", Pool::Facts), later), PaceGrant::Machine);
    for n in 0..WAKE_LINES_PER_HOUR {
        let at = later + i64::try_from(n).unwrap() * 1_000;
        assert_eq!(
            pace.ask(&ask("w", Pool::Wake), at),
            PaceGrant::Granted,
            "{n}"
        );
    }
    assert_eq!(
        pace.ask(&ask("w", Pool::Wake), later + MIN_MS),
        PaceGrant::Machine
    );
    assert_eq!(pace.lines.len(), MACHINE_LINES_PER_HOUR);
    assert_eq!(pace.wake_lines.len(), WAKE_LINES_PER_HOUR);
}

/// An `await wake` answer's reservation takes the slot and no line, even with both pools spent,
/// and a slot line asked after it waits for it; the same reservation through a line draws one.
#[test]
fn a_reserve_takes_no_line() {
    let mut pace = Pace {
        lines: vec![NOW; MACHINE_LINES_PER_HOUR],
        wake_lines: vec![NOW; WAKE_LINES_PER_HOUR],
        ..Pace::default()
    };
    assert_eq!(pace.reserve("a", NOW, 1, NOW), Reserve::Granted);
    assert_eq!(pace.lines.len(), MACHINE_LINES_PER_HOUR);
    assert_eq!(pace.wake_lines.len(), WAKE_LINES_PER_HOUR);
    assert_eq!(pace.reserve("a", NOW, 1, NOW + 1_000), Reserve::Granted);
    assert_eq!(pace.reserved.len(), 1);
    assert_eq!(
        pace.reserve("b", NOW - MIN_MS, 1, NOW + 2_000),
        Reserve::Reserved
    );
    let mut pace = Pace::default();
    let line = Ask {
        sid: "a",
        slot: true,
        hold_since_ms: NOW,
        usable: 1,
        pool: Pool::Facts,
    };
    assert_eq!(pace.ask(&line, NOW), PaceGrant::Granted);
    assert_eq!(pace.lines.len(), 1, "a line's reservation draws its line");
}

/// A session holding an `await wake` answer's reservation draws its first slot line from that
/// line's pool, held by the machine while the pool is spent; its re-ask then draws none, and the
/// reservation stands as the answer took it.
#[test]
fn an_answers_reservation_draws_its_holders_first_line() {
    let line = |pool| Ask {
        sid: "a",
        slot: true,
        hold_since_ms: NOW,
        usable: 1,
        pool,
    };
    let mut pace = Pace {
        lines: vec![NOW; MACHINE_LINES_PER_HOUR],
        ..Pace::default()
    };
    assert_eq!(pace.reserve("a", NOW, 1, NOW), Reserve::Granted);
    assert_eq!(
        pace.ask(&line(Pool::Facts), NOW + 1_000),
        PaceGrant::Machine,
        "the facts pool is spent"
    );
    assert_eq!(pace.ask(&line(Pool::Wake), NOW + 2_000), PaceGrant::Granted);
    assert_eq!(pace.ask(&line(Pool::Wake), NOW + 3_000), PaceGrant::Granted);
    assert_eq!(pace.wake_lines.len(), 1, "one line drawn, re-asked");
    assert_eq!(pace.lines.len(), MACHINE_LINES_PER_HOUR);
    assert_eq!(
        pace.reserved,
        [Reservation {
            sid: "a".to_string(),
            since_ms: NOW,
            line: Some(Pool::Wake),
        }]
    );
}

/// Every build slot of three taken.
const NONE_OF_THREE: SlotsView = SlotsView::Read {
    n: 3,
    held: 3,
    free: 0,
    pending: 0,
    unknown: 0,
    discounted: 0,
};

/// Two build slots of three free.
const TWO_OF_THREE: SlotsView = SlotsView::Read {
    n: 3,
    held: 1,
    free: 2,
    pending: 0,
    unknown: 0,
    discounted: 0,
};

/// A board from a snapshot of `slots` that landed at `unix_ms`, its slots read then, taken then.
fn sampled_at(unix_ms: i64, slots: SlotsView) -> Option<(FactsBoard, Arc<Snapshot>)> {
    let snap = at(unix_ms, slots, Pressure::Normal, Some(0), None);
    Some((
        board_of(&snap, Some(unix_ms), Some(Level::Ok), unix_ms),
        Arc::new(snap),
    ))
}

/// Runs half an hour at one-second ticks under `mode` and returns `(sampler_reads, first_stop_tick)`.
fn sampled_half_hour(mode: Occupancy, asks_to_s: i64) -> (usize, Option<i64>) {
    let sessions = [(7, "a".to_string())];
    let kept = Mutex::new(Some(Judged::default()));
    let mut reads = 0;
    let mut stopped = None;
    let mut read = |now: i64| {
        reads += 1;
        sampled_at(now, THREE_ONE_FREE)
    };
    for s in 0..30 * 60 {
        let now = NOW + s * 1000;
        if s % 60 == 0 {
            let mut k = kept.lock().expect("the board");
            let j = k.as_mut().expect("judged");
            step(
                j,
                &at(now, THREE_ONE_FREE, Pressure::Normal, Some(0), None),
                now,
                &sessions,
            );
            if s <= asks_to_s {
                j.asked("a", now, mode);
            }
        }
        if !sample_pass(&kept, mode, now, &mut read) && stopped.is_none() {
            stopped = Some(s);
        }
    }
    (reads, stopped)
}

/// `observe` and `off` do not run the sampler, while `prompt` samples every [`SAMPLE_EVERY_MS`] until [`ASK_KEEP_MS`] past the last ask.
#[test]
fn observe_samples_no_faster_than_its_looks() {
    let half_hour_s = 30 * 60;
    let every_s = SAMPLE_EVERY_MS / 1000;
    let all = usize::try_from(half_hour_s / every_s).unwrap();
    assert_eq!(
        sampled_half_hour(Occupancy::Prompt, half_hour_s),
        (all, None),
        "prompt, asked throughout"
    );
    for mode in [Occupancy::Observe, Occupancy::Off] {
        assert_eq!(
            sampled_half_hour(mode, half_hour_s),
            (0, Some(0)),
            "{mode:?}"
        );
        let mut j = Judged::default();
        j.asked("a", NOW, mode);
        assert!(!j.sampling(NOW, mode), "{mode:?}");
        assert!(!j.sample_due(NOW, mode), "{mode:?}");
    }
    let last_ask_s = 9 * 60;
    let last_s = last_ask_s + ASK_KEEP_MS / 1000;
    let kept = usize::try_from(last_s / every_s + 1).unwrap();
    assert_eq!(
        sampled_half_hour(Occupancy::Prompt, last_ask_s),
        (kept, Some(last_s + 1)),
        "the sampler keeps to the asks"
    );
    assert_eq!(sampled_half_hour(Occupancy::Prompt, -1), (0, Some(0)));
}

/// Under `prompt`, a slot taken between two minute looks restarts the `slots>=1 for=20m` term.
#[test]
fn under_prompt_the_slot_term_is_judged_on_the_samples() {
    let blip = NOW + 10 * MIN_MS + 5_000..NOW + 10 * MIN_MS + 10_000;
    let run = |mode: Occupancy| {
        let sessions = [(7, "a".to_string())];
        let kept = Mutex::new(Some(Judged::default()));
        let mut read = |now: i64| {
            let slots = if blip.contains(&now) {
                NONE_OF_THREE
            } else {
                THREE_ONE_FREE
            };
            sampled_at(now, slots)
        };
        for s in 0..=20 * 60 {
            let now = NOW + s * 1000;
            if s % 60 == 0 {
                let mut k = kept.lock().expect("the board");
                let j = k.as_mut().expect("judged");
                step(
                    j,
                    &at(now, THREE_ONE_FREE, Pressure::Normal, Some(0), None),
                    now,
                    &sessions,
                );
                j.asked("a", now, mode);
            }
            sample_pass(&kept, mode, now, &mut read);
        }
        let k = kept.lock().expect("the board");
        read_at_look(k.as_ref().expect("judged"), "a").slots_free_ms
    };
    assert_eq!(run(Occupancy::Observe), Some(20 * 60_000));
    assert_eq!(run(Occupancy::Prompt), None);
}

/// A hold begun three hours before [`NOW`].
const ROOM_SINCE: i64 = NOW - 3 * HOUR_MS;

/// A board over twenty-five minutes of looks to [`NOW`], one slot of three free throughout,
/// sessions `a` (tab 7) and `b` (tab 9) each seen holding one at the first: `hold-room`'s terms
/// hold at `NOW` for a hold begun [`ROOM_SINCE`].
fn room_board() -> Judged {
    let sessions = [(7, "a".to_string()), (9, "b".to_string())];
    let mut j = Judged::default();
    for m in (0..=25).rev() {
        let t = NOW - m * MIN_MS;
        let tabs = (m == 25).then(|| vec![7, 9]);
        step(
            &mut j,
            &at(t, THREE_ONE_FREE, Pressure::Normal, Some(0), tabs),
            t,
            &sessions,
        );
    }
    j
}

/// The board re-read for a write at `now_ms`: a snapshot of `slots` that landed at `landed_ms`,
/// its slots read `age_ms` before, `avail_gib` available.
fn reread(
    j: &mut Judged,
    now_ms: i64,
    landed_ms: i64,
    slots: SlotsView,
    avail_gib: u64,
    age_ms: Option<u64>,
) {
    let snap = Snapshot {
        avail: Some(avail_gib << 30),
        ..at(landed_ms, slots, Pressure::Normal, age_ms, None)
    };
    j.sample(
        board_of(&snap, Some(landed_ms), Some(Level::Ok), now_ms),
        Some(&snap),
    );
}

/// The answer to session `sid`'s `hold-room` line at `now_ms`.
fn room_answer(j: &mut Judged, sid: &str, now_ms: i64) -> PaceGrant {
    j.answer(sid, RuleId::HoldRoom, ROOM_SINCE, now_ms)
}

/// A slot taken between the minute look and the write's re-read returns [`PaceGrant::Unmet`] without reserving a slot.
#[test]
fn a_slot_taken_since_the_look_holds_the_line() {
    let now = NOW + 30_000;
    let mut j = room_board();
    reread(&mut j, now, now, NONE_OF_THREE, 96, Some(0));
    assert_eq!(room_answer(&mut j, "a", now), PaceGrant::Unmet);
    assert!(j.pace.reserved.is_empty(), "no slot reserved for it");
    let mut j = room_board();
    reread(&mut j, now, now, THREE_ONE_FREE, 96, Some(0));
    assert!(
        matches!(room_answer(&mut j, "a", now), PaceGrant::Room(_)),
        "the control"
    );
    let mut j = room_board();
    assert_eq!(room_answer(&mut j, "a", now), PaceGrant::Stale);
    let mut j = room_board();
    assert!(matches!(
        room_answer(&mut j, "a", NOW + FACT_FRESH_MS),
        PaceGrant::Room(_)
    ));
}

/// A re-read older than [`FACT_FRESH_MS`] or without slot age returns [`PaceGrant::Stale`].
#[test]
fn a_stale_reread_refuses() {
    let now = NOW + 30_000;
    let edge = u64::try_from(FACT_FRESH_MS).unwrap();
    for (landed, age, stale) in [
        (now - FACT_FRESH_MS - 1, Some(0), true),
        (now - FACT_FRESH_MS, Some(0), false),
        (now, Some(edge + 1), true),
        (now, Some(edge), false),
        (now - 2_000, Some(edge - 2_000 + 1), true),
        (now - 2_000, Some(edge - 2_000), false),
        (now, None, true),
    ] {
        let mut j = room_board();
        reread(&mut j, now, landed, THREE_ONE_FREE, 96, age);
        let grant = room_answer(&mut j, "a", now);
        assert_eq!(
            grant == PaceGrant::Stale,
            stale,
            "landed {} ms before, slots {age:?}: {grant:?}",
            now - landed
        );
        assert!(stale || matches!(grant, PaceGrant::Room(_)), "{grant:?}");
    }
}

/// [`PaceGrant::Room`] carries the re-read board's figures rather than the earlier look's.
#[test]
fn the_room_answer_carries_the_reread_figures() {
    let now = NOW + 30_000;
    let mut j = room_board();
    let looked = read_at_look(&j, "a");
    assert_eq!(looked.slots, Some(SlotsSeen { usable: 1, n: 3 }));
    assert_eq!(looked.avail_mib, Some(96 << 10));
    reread(&mut j, now, now - 2_000, TWO_OF_THREE, 64, Some(1_000));
    let PaceGrant::Room(r) = room_answer(&mut j, "a", now) else {
        panic!("the room");
    };
    assert_eq!(r.slots, Some(SlotsSeen { usable: 2, n: 3 }));
    assert_eq!(r.avail_mib, Some(64 << 10));
    assert_eq!(r.taken_ms, now - 3_000);
    assert_eq!(r.slots_free_ms, Some(25 * 60_000 + 28_000));
    assert_ne!((r.slots, r.avail_mib), (looked.slots, looked.avail_mib));
    assert!(r.typeable_at(Some(now)));
}

/// A slot rule returns [`PaceGrant::Room`] or a hold reason, never [`PaceGrant::Granted`].
#[test]
fn a_slot_rule_is_never_answered_a_bare_grant() {
    let now = NOW + 30_000;
    let mut j = room_board();
    reread(&mut j, now, now, THREE_ONE_FREE, 96, Some(0));
    assert!(matches!(room_answer(&mut j, "a", now), PaceGrant::Room(_)));
    assert_eq!(room_answer(&mut j, "b", now + 1_000), PaceGrant::Reserved);
    assert_eq!(
        j.answer("b", RuleId::WaitNudge, ROOM_SINCE, now + 1_000),
        PaceGrant::Granted
    );
    for slots in [THREE_ONE_FREE, TWO_OF_THREE, NONE_OF_THREE] {
        for landed in [now, now - FACT_FRESH_MS - 1] {
            let mut j = room_board();
            reread(&mut j, now, landed, slots, 96, Some(0));
            let grant = room_answer(&mut j, "a", now);
            assert_ne!(grant, PaceGrant::Granted, "{slots:?} {landed}");
        }
    }
}

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
        rules: vec![
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
        ready_idle: None,
        wake: None,
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
            j.look_row(sessions, &[], t)
        })
        .collect()
}

/// A told hold past two hours in a tab that held a slot reads ready once the slot term spans twenty minutes.
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

    j.tell("a", Some(said(Occupancy::Observe, None, QUIET, None)));
    let t = NOW + 31 * MIN_MS;
    step(
        &mut j,
        &at(t, THREE_ONE_FREE, Pressure::Normal, Some(0), Some(vec![7])),
        t,
        &sessions,
    );
    let row = j.look_row(&sessions, &[], t);
    assert!(!ready(&row, "a"));
    assert_eq!(j.ready_now(), 0);
    assert_eq!(j.idle_ready_min(t), looks, "no minute added");
}

/// A session a met wake names (a ready wake whose need holds) reads ready in the look's row
/// though no hold of its does, and its slot-minute counts idle-ready (the plan's F14). CONTROLS:
/// a session no met wake names is not ready, nor is the same one at the next look once none does.
#[test]
fn a_met_wake_reads_its_session_ready_in_the_ring() {
    let sessions = [(7, "a".to_string()), (9, "b".to_string())];
    let ready = |row: &RingRow, sid: &str| row.session(sid).is_some_and(|s| s.ready);
    let mut j = Judged::default();
    let snap = |t| at(t, THREE_ONE_FREE, Pressure::Normal, Some(0), Some(vec![9]));
    step(&mut j, &snap(NOW), NOW, &sessions);
    let row = j.look_row(&sessions, &["a".to_string()], NOW);
    assert!(ready(&row, "a"));
    assert!(!ready(&row, "b"), "CONTROL: no wake of b's is met");
    assert_eq!((j.ready_now(), row.idle_ready()), (1, 1));
    let t = NOW + MIN_MS;
    step(&mut j, &snap(t), t, &sessions);
    let row = j.look_row(&sessions, &[], t);
    assert!(!ready(&row, "a"), "CONTROL: no wake of a's met now");
    assert_eq!(j.ready_now(), 0);
    assert_eq!(j.idle_ready_min(t), 1, "the one look it read ready");
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

/// `occupancy check --replay` replays the decider over the board's rows and the session's ledger.
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

/// A poke between minute ticks refreshes the board without adding a facts-ring row.
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
        assert!(
            j.looked(&sessions, &[], t, Look::Tick).is_some(),
            "a tick's row"
        );
        ticks += 1;
        for poke in 1..=3 {
            let t = t + poke * 10_000;
            let snap = at(t, THREE_ONE_FREE, Pressure::Normal, Some(0), Some(vec![7]));
            step(&mut j, &snap, t, &sessions);
            assert!(
                j.looked(&sessions, &[], t, Look::Poke).is_none(),
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

/// When session `a`'s hold, begun at [`ROOM_SINCE`], can first type `hold-room`'s line.
const ROOM_DUE: i64 = ROOM_SINCE + rules::HOLD_ROOM_AFTER_MS;

/// The lead the sampler takes before a line can go: the slot term's `for=` and one look.
fn lead_ms() -> i64 {
    let wait = machine_wait(HOLD_ROOM_SLOTS).expect("the slot term parses");
    i64::try_from(wait.for_ms).unwrap() + LOOK_EVERY_MS
}

/// Runs to [`ROOM_DUE`] under `mode` with `told` hold state and returns `(sampler_reads, slots_free_ms)`.
fn lead_run(mode: Occupancy, told: Option<Occupancy>) -> (usize, Option<u64>) {
    let blip = ROOM_DUE - 10 * MIN_MS + 5_000..ROOM_DUE - 10 * MIN_MS + 10_000;
    let sessions = [(7, "a".to_string())];
    let mut j = Judged::default();
    if let Some(told) = told {
        j.tell("a", Some(said(told, Some(ROOM_SINCE), QUIET, None)));
    }
    let kept = Mutex::new(Some(j));
    let mut reads = 0;
    let mut read = |now: i64| {
        reads += usize::from(now < ROOM_DUE);
        let slots = if blip.contains(&now) {
            NONE_OF_THREE
        } else {
            THREE_ONE_FREE
        };
        sampled_at(now, slots)
    };
    for s in 0..=25 * 60 {
        let now = ROOM_DUE - 25 * MIN_MS + s * 1000;
        if s % 60 == 0 {
            let mut k = kept.lock().expect("the board");
            let j = k.as_mut().expect("judged");
            step(
                j,
                &at(now, THREE_ONE_FREE, Pressure::Normal, Some(0), None),
                now,
                &sessions,
            );
            if now >= ROOM_DUE {
                j.asked("a", now, mode);
            }
        }
        sample_pass(&kept, mode, now, &mut read);
    }
    let k = kept.lock().expect("the board");
    (
        reads,
        read_at_look(k.as_ref().expect("judged"), "a").slots_free_ms,
    )
}

/// Under `prompt`, a told hold is sampled from its slot term's `for=` window before its line is due.
#[test]
fn a_told_hold_is_sampled_from_its_slot_terms_for_before_its_line_can_go() {
    let lead = usize::try_from(lead_ms() / SAMPLE_EVERY_MS).unwrap();
    assert_eq!(
        lead_run(Occupancy::Prompt, Some(Occupancy::Prompt)),
        (lead, None)
    );
    let looks = Some(25 * 60_000);
    assert_eq!(
        lead_run(Occupancy::Prompt, None),
        (0, looks),
        "nothing told"
    );
    for (mode, told) in [
        (Occupancy::Observe, Occupancy::Prompt),
        (Occupancy::Off, Occupancy::Prompt),
        (Occupancy::Prompt, Occupancy::Observe),
    ] {
        assert_eq!(
            lead_run(mode, Some(told)),
            (0, looks),
            "{mode:?} told {told:?}"
        );
    }
}

/// The sampler lead window runs from `for=` plus one look before each due line until [`ASK_KEEP_MS`] after.
#[test]
fn the_lead_runs_from_the_slot_terms_for_before_each_line_to_its_loops_asks() {
    let lead = lead_ms();
    let room = |verdict: SaidVerdict, at_ms: i64| {
        let mut word = said(
            Occupancy::Prompt,
            Some(ROOM_SINCE),
            [SaidVerdict::Quiet, verdict],
            None,
        );
        word.rules[1].at_ms = at_ms;
        word
    };
    let edges = |j: &Judged, due: i64| {
        [
            due - lead - 1,
            due - lead,
            due,
            due + ASK_KEEP_MS,
            due + ASK_KEEP_MS + 1,
        ]
        .map(|t| j.sampling(t, Occupancy::Prompt))
    };
    let window = [false, true, true, true, false];
    let mut j = Judged::default();
    j.tell("a", Some(room(SaidVerdict::Quiet, 0)));
    assert_eq!(edges(&j, ROOM_DUE), window, "the first line");
    let mut spent = Judged::default();
    spent.tell("a", Some(room(SaidVerdict::Spent, ROOM_DUE)));
    assert_eq!(
        edges(&spent, ROOM_DUE),
        [false; 5],
        "said spent, none told typed"
    );
    for mode in [Occupancy::Observe, Occupancy::Off] {
        for t in [ROOM_DUE - lead, ROOM_DUE, ROOM_DUE + ASK_KEEP_MS] {
            assert!(!j.sampling(t, mode), "{mode:?} at {t}");
            assert!(!j.sampler_due(t, mode), "{mode:?} at {t}");
        }
    }
    assert!(!j.sampler_due(ROOM_DUE - lead - 1, Occupancy::Prompt));
    assert!(
        j.sampler_due(ROOM_DUE - lead, Occupancy::Prompt),
        "one starts"
    );
    assert!(!j.sampler_due(ROOM_DUE, Occupancy::Prompt), "one runs");

    let mut typed_at = ROOM_DUE + 5 * MIN_MS;
    for n in 1..rules::HOLD_ROOM_PER_HOLD {
        j.tell("a", Some(room(SaidVerdict::Typed, typed_at)));
        j.tell("a", Some(room(SaidVerdict::Typed, typed_at)));
        let next = typed_at + rules::HOLD_ROOM_APART_MS;
        assert_eq!(edges(&j, next), window, "after line {n}");
        assert!(!j.sampling(typed_at + HOUR_MS, Occupancy::Prompt), "spaced");
        j.tell("a", Some(room(SaidVerdict::Quiet, typed_at + MIN_MS)));
        assert_eq!(edges(&j, next), window, "the line stays booked");
        typed_at = next + 5 * MIN_MS;
    }
    let mut closed = said(Occupancy::Prompt, None, QUIET, None);
    closed.rules[1].at_ms = typed_at;
    j.tell("a", Some(closed));
    j.tell("a", Some(room(SaidVerdict::Quiet, 0)));
    let after_last = typed_at - 5 * MIN_MS;
    assert_eq!(
        edges(&j, after_last),
        window,
        "a hold told closed keeps its lines"
    );
    j.tell("a", Some(room(SaidVerdict::Typed, typed_at)));
    let next = typed_at + rules::HOLD_ROOM_APART_MS;
    assert_eq!(edges(&j, next), [false; 5], "the cap's lines typed");
    j.tell("a", Some(room(SaidVerdict::Spent, next)));
    assert_eq!(edges(&j, next), [false; 5], "said spent");

    let since = typed_at + MIN_MS;
    let mut fresh = said(Occupancy::Prompt, Some(since), QUIET, None);
    fresh.rules[1].at_ms = 0;
    j.tell("a", Some(fresh));
    let due = since + rules::HOLD_ROOM_AFTER_MS;
    assert_eq!(edges(&j, due), window, "another hold counts afresh");
    j.tell("a", None);
    assert_eq!(edges(&j, due), [false; 5], "forgotten");
}

/// The sampler runs only while the owner's mode is `prompt`.
#[test]
fn the_sampler_keeps_to_the_owners_mode_each_check_alone() {
    for mode in [Occupancy::Observe, Occupancy::Off] {
        let mut j = Judged::default();
        j.asked("a", NOW, Occupancy::Prompt);
        assert!(j.sampling(NOW, Occupancy::Prompt), "the control");
        assert!(!j.sampling(NOW, mode), "{mode:?}");
        assert!(!j.sample_due(NOW, mode), "{mode:?}");
        let kept = Mutex::new(Some(j));
        let mut reads = 0;
        let mut read = |now: i64| {
            reads += 1;
            sampled_at(now, THREE_ONE_FREE)
        };
        assert!(!sample_pass(&kept, mode, NOW, &mut read), "{mode:?}");
        assert_eq!(reads, 0, "{mode:?}");

        let mut j = Judged::default();
        j.asked("a", NOW, mode);
        assert!(j.wanting.is_empty(), "{mode:?}");
        assert!(!j.sampling(NOW, Occupancy::Prompt), "{mode:?}");
        j.asked("a", NOW, Occupancy::Prompt);
        j.asked("b", NOW + 1, mode);
        assert!(j.wanting.is_empty(), "{mode:?}");
        assert!(!j.sampling(NOW + 2, Occupancy::Prompt), "{mode:?}");
    }
}

/// A slot rule's write re-reads the board only when the owner's mode is `prompt`.
#[test]
fn a_slot_rules_write_rereads_the_board_under_prompt_alone() {
    let now = NOW + 30_000;
    let run = |mode: Occupancy, rule: RuleId| {
        let kept = Mutex::new(Some(room_board()));
        let mut reads = 0;
        let mut reread = || {
            reads += 1;
            Some(Reread {
                snap: Arc::new(at(now, THREE_ONE_FREE, Pressure::Normal, Some(0), None)),
                landed_ms: now,
                disk: Some(Level::Ok),
            })
        };
        let grant = pace_on(
            &kept,
            mode,
            ("a", rule, ROOM_SINCE),
            &mut reread,
            &mut || Some(now),
        );
        (grant, reads)
    };
    for mode in [Occupancy::Observe, Occupancy::Off] {
        assert_eq!(
            run(mode, RuleId::HoldRoom),
            (PaceGrant::Stale, 0),
            "{mode:?}"
        );
    }
    for mode in [Occupancy::Observe, Occupancy::Off, Occupancy::Prompt] {
        assert_eq!(run(mode, RuleId::WaitNudge).1, 0, "{mode:?}");
    }
    let (grant, reads) = run(Occupancy::Prompt, RuleId::HoldRoom);
    assert!(matches!(grant, PaceGrant::Room(_)), "{grant:?}");
    assert_eq!(reads, 1);
}
