// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_vitals::services::{FSEVENTSD, HOLD_MS, Service};

const GIB: u64 = 1 << 30;

fn reading(footprint: u64) -> Reading {
    Reading {
        total: 128 * GIB,
        services: vec![Service {
            coalition: 7,
            pid: 537,
            start_s: 1_790_000_000,
            uid: 0,
            name: "fseventsd".into(),
            path: Some(FSEVENTSD.path.into()),
            live: 1,
            footprint,
            ..Service::default()
        }],
        ..Reading::default()
    }
}

/// `(size, told, restart known)` of each service the note names.
fn heard(note: Option<HostNote>) -> Option<Vec<(Size, bool, bool)>> {
    match note? {
        HostNote::ServiceMemory { told, .. } => Some(
            told.services
                .iter()
                .map(|s| (s.size, s.told(), s.restart.is_some()))
                .collect(),
        ),
        other => panic!("{other:?}"),
    }
}

/// The window hears the first answer, the service followed before it is told, the edge
/// once the hold is met, a figure moved by a sixteenth, and the end; a failed reading, an
/// unchanged one and a figure moved by less tell nothing.
#[test]
fn the_window_hears_the_first_answer_each_change_and_the_end() {
    let mut b = Board::new(Some(true));
    let small = reading(4 << 20);
    let big = reading(40 * GIB);
    assert_eq!(heard(step(&mut b, 0, Some(&small))), Some(vec![]));
    assert_eq!(heard(step(&mut b, 60_000, Some(&small))), None);
    assert_eq!(heard(step(&mut b, 120_000, None)), None, "a failed reading");
    assert_eq!(
        heard(step(&mut b, 180_000, Some(&big))),
        Some(vec![(Size::Huge, false, true)]),
        "followed, not yet told"
    );
    assert_eq!(heard(step(&mut b, 240_000, Some(&big))), None);
    assert_eq!(
        heard(step(&mut b, 180_000 + HOLD_MS, Some(&big))),
        Some(vec![(Size::Huge, true, true)])
    );
    assert_eq!(
        heard(step(
            &mut b,
            240_000 + HOLD_MS,
            Some(&reading(40 * GIB + (1 << 20)))
        )),
        None,
        "a megabyte more is no news"
    );
    assert_eq!(
        heard(step(&mut b, 300_000 + HOLD_MS, Some(&reading(44 * GIB)))),
        Some(vec![(Size::Huge, true, true)]),
        "a tenth more is"
    );
    assert_eq!(
        heard(step(&mut b, 360_000 + HOLD_MS, Some(&small))),
        Some(vec![]),
        "the end"
    );
}

/// A press opens one restart's tab: another within five minutes opens nothing, nor does a
/// press for an account `sudo` would refuse. NEGATIVE CONTROL: five minutes on, a press
/// opens again.
#[test]
fn a_second_press_within_minutes_opens_nothing() {
    let t0 = Instant::now();
    assert_eq!(press_refused(None, t0, Some(true)), None);
    assert_eq!(press_refused(None, t0, None), None, "unread: sudo decides");
    assert!(press_refused(Some(t0), t0 + Duration::from_secs(60), Some(true)).is_some());
    assert!(press_refused(None, t0, Some(false)).is_some());
    assert_eq!(press_refused(Some(t0), t0 + PRESS_QUIET, Some(true)), None);
}

/// With no watch's reading (before the first, or in a process that does not watch), a press
/// reads the service now and opens the restart's tab from the margin under the large bar,
/// where `aterm init --only` restarts it. NEGATIVE CONTROL: under it, a service aterm knows
/// no restart for, and a failed read open nothing.
#[test]
fn a_press_with_no_reading_reads_the_service_now() {
    let at = |fp: u64| Some(reading(fp));
    assert!(read_holds("fseventsd", at(141 * GIB).as_ref()));
    assert!(read_holds("fseventsd", at(7 * GIB + (512 << 20)).as_ref()));
    assert!(!read_holds("fseventsd", at(6 * GIB).as_ref()));
    assert!(!read_holds("mds", at(141 * GIB).as_ref()));
    assert!(!read_holds("fseventsd", None));
}

/// A refusal names why, once, in the press's words.
#[test]
fn a_refused_press_says_why_once() {
    assert!(!refuse("fseventsd no longer holds past its bar"));
    assert_eq!(
        take_refusal().as_deref(),
        Some("Opened nothing: fseventsd no longer holds past its bar")
    );
    assert_eq!(take_refusal(), None);
}

#[test]
fn the_status_fields_say_dash_until_read() {
    let s = status_fields();
    assert!(s.starts_with("service_mem="), "{s}");
    assert!(s.contains(" service_mem_top="), "{s}");
    let b = Board::new(None);
    assert_eq!(b.judge.word(), "-");
}
