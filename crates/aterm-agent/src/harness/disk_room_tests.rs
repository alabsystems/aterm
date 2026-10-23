// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The room meter on injected instants (design `docs/DESIGN-disk-awareness-2026-10-17.md` §6.3).

use super::*;

/// Midnight UTC of a fixed day (20,743 whole days); every instant is the
/// test's own.
const DAY0: i64 = 1_792_195_200;
const H: i64 = 3_600;
const D: i64 = 86_400;
const _: () = assert!(DAY0 % D == 0, "DAY0 is a midnight");
/// The owner's Mac's Data volume, 1.81 TiB.
const TOTAL: u64 = 1_948_404_040 * 1024;

fn gib(n: u64) -> u64 {
    n * GIB
}

/// A day on which free space fell `fall` GiB inside one two-hour window
/// (from 500 GiB, at noon), with readings every nine minutes.
fn fall_on(m: &mut RoomMeter, day: i64, fall: u64) {
    let noon = DAY0 + day * D + 12 * H;
    for step in 0..=12u64 {
        let lost = fall * step / 12;
        m.observe(
            noon + i64::try_from(step).expect("small") * 540,
            gib(500 - lost),
        );
    }
}

/// A fall inside the two-hour window counts whole; one spread over more
/// than two hours counts only what fell inside one window. NEGATIVE CONTROL:
/// a reading two hours and a minute after the peak does not measure against
/// it.
#[test]
fn a_fall_is_measured_against_the_two_hours_before_it() {
    let mut m = RoomMeter::new();
    m.observe(DAY0 + 9 * H, gib(300));
    m.observe(DAY0 + 9 * H + 30 * 60, gib(260));
    assert_eq!(m.days(), [(DAY0 / D, gib(40))]);
    let mut slow = RoomMeter::new();
    slow.observe(DAY0 + 9 * H, gib(300));
    slow.observe(DAY0 + 11 * H + 60, gib(200));
    assert_eq!(slow.days(), [(DAY0 / D, 0)], "past the window");
}

/// A later rise (a pass, an `rm`, a gc) cancels no earlier fall: the day keeps
/// its largest.
#[test]
fn a_later_rise_cancels_no_earlier_fall() {
    let mut m = RoomMeter::new();
    fall_on(&mut m, 0, 90);
    m.observe(DAY0 + 15 * H, gib(700));
    m.observe(DAY0 + 15 * H + 600, gib(690));
    assert_eq!(m.days(), [(DAY0 / D, gib(90))]);
}

/// ONE OUTLIER DAY DOES NOT SET THE ROOM: a 300 GiB copy on one day and
/// builds of 60-80 GiB on the others give a room of the second-largest day,
/// 80 GiB. NEGATIVE CONTROL: the outlier IS the week's largest fall, so a
/// plain maximum would have read 300.
#[test]
fn one_outlier_day_does_not_set_the_room() {
    let mut m = RoomMeter::new();
    for (day, fall) in [(0, 60), (1, 300), (2, 80), (3, 70)] {
        fall_on(&mut m, day, fall);
    }
    let now = DAY0 + 3 * D + 20 * H;
    assert_eq!(m.room(now, TOTAL), gib(80));
    assert_eq!(m.days().iter().map(|(_, f)| *f).max(), Some(gib(300)));
}

/// With readings on fewer than two days the room is the bounds' floor, and a
/// day older than seven ages out of the room (and of the meter at its next
/// reading).
#[test]
fn fewer_than_two_days_read_the_floor_and_a_week_old_day_ages_out() {
    let (lo, hi) = room_bounds(TOTAL);
    assert_eq!((lo, hi), (gib(16), TOTAL / 10));
    let mut m = RoomMeter::new();
    assert_eq!(m.room(DAY0, TOTAL), lo, "no history");
    fall_on(&mut m, 0, 120);
    assert_eq!(m.room(DAY0 + 20 * H, TOTAL), lo, "one day");
    fall_on(&mut m, 1, 100);
    assert_eq!(m.room(DAY0 + D + 20 * H, TOTAL), gib(100));
    assert_eq!(m.room(DAY0 + 7 * D + H, TOTAL), lo, "day 0 aged out");
    m.observe(DAY0 + 8 * D, gib(500));
    assert!(m.days().iter().all(|(d, _)| *d > DAY0 / D + 1));
}

/// The room is clamped to the volume: a tiny daily fall reads the floor, a
/// huge one the cap; on a 128 GB disk the clamp is [5 %, 10 %] and not
/// empty (`[16 GiB, 10 %]` would be).
#[test]
fn the_room_is_clamped_to_the_volume_and_the_clamp_is_defined_on_a_small_one() {
    let mut m = RoomMeter::new();
    fall_on(&mut m, 0, 1);
    fall_on(&mut m, 1, 1);
    assert_eq!(m.room(DAY0 + D + 20 * H, TOTAL), gib(16));
    let mut big = RoomMeter::new();
    fall_on(&mut big, 0, 450);
    fall_on(&mut big, 1, 400);
    assert_eq!(big.room(DAY0 + D + 20 * H, TOTAL), TOTAL / 10);
    let small: u64 = 128_000_000_000;
    let (lo, hi) = room_bounds(small);
    assert!(lo <= hi);
    assert_eq!((lo, hi), (small * 5 / 100, small * 10 / 100));
    let huge: u64 = 8_000_000_000_000;
    assert_eq!(room_bounds(huge), (gib(16), gib(256)));
}

/// Encode and decode keep the day pairs (the in-day ring is not kept); a
/// corrupt file reads as no history; [`Rooms`] keeps at most eight volumes
/// and writes at most hourly.
#[test]
fn the_rooms_round_trip_and_a_corrupt_file_is_no_history() {
    let mut rooms = Rooms::default();
    for day in 0..3 {
        fall_on(
            rooms.meter("container:disk3").expect("room"),
            day,
            50 + day as u64 * 10,
        );
    }
    let text = rooms.encode();
    let back = Rooms::decode(&text);
    assert_eq!(
        back.room("container:disk3", DAY0 + 2 * D + 20 * H, TOTAL),
        gib(60)
    );
    assert_eq!(
        back.meters["container:disk3"].days(),
        rooms.meters["container:disk3"].days()
    );
    for bad in [
        "",
        "{",
        "{\"schema\":2}",
        "{\"schema\":1,\"volumes\":{\"k\":[[1]]}}",
    ] {
        let r = Rooms::decode(bad);
        assert_eq!(r.room("k", DAY0, TOTAL), room_bounds(TOTAL).0, "{bad}");
    }
    assert!(
        RoomMeter::decode(&aterm_json::from_str::<Value>("[[2,1],[1,1]]").expect("json")).is_none(),
        "days out of order"
    );
    let mut many = Rooms::default();
    for i in 0..MAX_ROOMS {
        assert!(many.meter(&format!("fsid:{i}")).is_some());
    }
    assert!(many.meter("fsid:99").is_none());
    assert!(many.meter("fsid:0").is_some(), "a known one stays");
    assert!(many.save_due(DAY0));
    assert!(!many.save_due(DAY0 + H - 1));
    assert!(many.save_due(DAY0 + H));
}

/// Saved and loaded through the file, by temp file and rename; a directory
/// with no file loads as no history.
#[test]
fn the_rooms_persist_in_the_harness_state_directory() {
    let tmp = super::super::tests::Tmp::new("room-file");
    assert_eq!(Rooms::load(tmp.path()), Rooms::default());
    let mut rooms = Rooms::default();
    fall_on(rooms.meter("fsid:7").expect("room"), 0, 30);
    rooms.save(tmp.path()).expect("saved");
    let back = Rooms::load(tmp.path());
    assert_eq!(back.meters["fsid:7"].days(), rooms.meters["fsid:7"].days());
}
