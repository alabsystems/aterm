// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// 2026-10-23 12:00:05 UTC, five seconds into a minute.
const T0: i64 = 1_792_756_805_000;
const MB: u64 = 1_000_000;

fn w(name: &str, scope: ProcScope, bytes: u64, n: u32) -> Writer {
    Writer {
        name: name.into(),
        bytes_per_s: 0.0,
        scope,
        n,
        bytes,
    }
}

fn names(top: &[ProgramHour]) -> Vec<(&str, ProcScope, u64)> {
    top.iter()
        .map(|p| (p.name.as_str(), p.scope, p.bytes))
        .collect()
}

#[test]
fn the_hour_scales_bytes_over_covered_time() {
    let mut h = WriteHour::new();
    for i in 0..10 {
        h.note(
            T0 + i * 30_000,
            30_000,
            &[w("rustc", ProcScope::Tab(3), 1_000 * MB, 9)],
        );
    }
    assert!((h.covered_s() - 300.0).abs() < 1e-9, "{}", h.covered_s());
    let top = h.top();
    assert_eq!(names(&top), [("rustc", ProcScope::Tab(3), 10_000 * MB)]);
    assert_eq!(top[0].n_max, 9);
    let per_hour = top[0].per_hour(h.covered_s()).expect("five minutes read");
    assert!((per_hour - 120e9).abs() < 1.0, "{per_hour}");
    assert_eq!(
        top[0].per_hour(MIN_COVERED_S - 0.5),
        None,
        "too little read"
    );
}

#[test]
fn minutes_older_than_an_hour_drop() {
    let mut h = WriteHour::new();
    h.note(T0, 1_000, &[w("cp", ProcScope::Other, 5 * MB, 1)]);
    h.note(
        T0 + 59 * SLOT_MS,
        1_000,
        &[w("rustc", ProcScope::Aterm, 7 * MB, 2)],
    );
    assert_eq!(
        names(&h.top()),
        [
            ("rustc", ProcScope::Aterm, 7 * MB),
            ("cp", ProcScope::Other, 5 * MB)
        ]
    );
    assert!((h.covered_s() - 2.0).abs() < 1e-9);
    let _ = h.at(T0 + 60 * SLOT_MS);
    assert_eq!(names(&h.top()), [("rustc", ProcScope::Aterm, 7 * MB)]);
    assert!((h.covered_s() - 1.0).abs() < 1e-9, "the first minute went");
    let _ = h.at(T0 + 119 * SLOT_MS);
    assert_eq!(names(&h.top()), [], "nothing written in the hour");
    assert_eq!(h.covered_s(), 0.0);
}

/// A reading that moves the hour without a window (the model's gap, or no sweep at all)
/// adds neither coverage nor bytes.
#[test]
fn a_reading_with_no_window_adds_neither_coverage_nor_bytes() {
    let mut h = WriteHour::new();
    h.note(T0, 2_000, &[w("cp", ProcScope::Other, 5 * MB, 1)]);
    for i in 1..30 {
        let _ = h.at(T0 + i * 60_000);
    }
    assert!((h.covered_s() - 2.0).abs() < 1e-9);
    assert_eq!(names(&h.top()), [("cp", ProcScope::Other, 5 * MB)]);
}

#[test]
fn a_clock_step_back_within_the_clamp_lands_in_the_open_minute() {
    let mut h = WriteHour::new();
    let open = h.at(T0);
    assert_eq!(open, T0.div_euclid(SLOT_MS));
    assert_eq!(
        h.at(T0 - 30_000),
        open,
        "30 s back, into the minute before: the open minute"
    );
    h.note(T0 - 30_000, 1_000, &[w("cp", ProcScope::Other, MB, 1)]);
    assert_eq!(h.covered.len(), 1, "{:?}", h.covered);
    assert_eq!(h.covered[0].0, open);
    // Past the clamp the whole hour moves back (the one back-step rule): the newest minute it
    // held is the one before the reading's, which opens its own.
    let back = h.at(T0 - 30_000 - 2 * SLOT_MS);
    assert_eq!(back, (T0 - 30_000 - 2 * SLOT_MS).div_euclid(SLOT_MS));
    assert_eq!(h.covered[0].0, back - 1, "moved with the clock");
    assert_eq!(names(&h.top()), [("cp", ProcScope::Other, MB)]);
    assert!((h.covered_s() - 1.0).abs() < 1e-9);
}

/// `name` is among the programs the hour keeps.
fn kept(h: &WriteHour, name: &str) -> bool {
    h.programs.iter().any(|p| p.name == name)
}

#[test]
fn the_cap_evicts_the_smallest_hour() {
    let mut h = WriteHour::new();
    let full: Vec<Writer> = (1..=WRITE_PROGRAMS as u64)
        .map(|i| w(&format!("p{i}"), ProcScope::Other, i * MB, 1))
        .collect();
    h.note(T0, 1_000, &full);
    assert_eq!(h.programs.len(), WRITE_PROGRAMS);
    h.note(
        T0 + 1_000,
        1_000,
        &[w("big", ProcScope::Other, 100 * MB, 1)],
    );
    assert_eq!(h.programs.len(), WRITE_PROGRAMS);
    assert!(!kept(&h, "p1"), "the smallest went");
    assert!(kept(&h, "big"));
    // A newcomer smaller than every hour kept still comes in, in the smallest's place.
    h.note(T0 + 2_000, 1_000, &[w("tiny", ProcScope::Other, MB / 2, 1)]);
    assert!(kept(&h, "tiny"));
    assert!(!kept(&h, "p2"));
    // A tab's first program comes in however small, and the newcomer before it, now the
    // smallest outside its scope's first four, makes room.
    h.note(T0 + 3_000, 1_000, &[w("zsh", ProcScope::Tab(5), MB / 4, 1)]);
    assert!(kept(&h, "zsh"));
    assert!(!kept(&h, "tiny"));
    assert!(kept(&h, "p3"));
    assert_eq!(h.programs.len(), WRITE_PROGRAMS);
    // NEGATIVE CONTROL: no scope's first four makes room, though zsh's tab hour is the
    // smallest kept: the smallest of the crowded scope goes.
    h.note(T0 + 4_000, 1_000, &[w("cc", ProcScope::Other, MB / 8, 1)]);
    assert!(kept(&h, "cc"));
    assert!(!kept(&h, "p3"));
    let tab: Vec<_> = h
        .top()
        .into_iter()
        .filter(|p| p.scope == ProcScope::Tab(5))
        .collect();
    assert_eq!(names(&tab), [("zsh", ProcScope::Tab(5), MB / 4)]);
    let other: Vec<_> = h
        .top()
        .into_iter()
        .filter(|p| p.scope == ProcScope::Other)
        .map(|p| p.name)
        .collect();
    assert_eq!(other, ["big", "p64", "p63", "p62"]);
}

/// A STEADY NEWCOMER CLIMBS A FULL HOUR: a program that writes 1 MB a window into an hour
/// full of programs that each wrote more than 10 MB, with a one-off newcomer after it in
/// every window, comes in and climbs by what it adds until it leads its scope. NEGATIVE
/// CONTROL: the one-off newcomers take each other's places, so of the hours kept before it
/// only the two smallest went.
#[test]
fn a_steady_newcomer_climbs_a_full_hour() {
    let mut h = WriteHour::new();
    let full: Vec<Writer> = (1..=WRITE_PROGRAMS as u64)
        .map(|i| w(&format!("p{i}"), ProcScope::Other, (10 + i) * MB, 1))
        .collect();
    h.note(T0, 1_000, &full);
    for i in 1..=120 {
        h.note(
            T0 + i * 1_000,
            1_000,
            &[
                w("steady", ProcScope::Other, MB, 1),
                w(&format!("once{i}"), ProcScope::Other, 1_000, 1),
            ],
        );
    }
    let top = h.top();
    assert_eq!(
        (top[0].name.as_str(), top[0].bytes),
        ("steady", 120 * MB),
        "{:?}",
        names(&top)
    );
    assert!(!kept(&h, "p1") && !kept(&h, "p2"));
    assert!(
        (3..=WRITE_PROGRAMS).all(|i| kept(&h, &format!("p{i}"))),
        "{:?}",
        h.programs.iter().map(|p| &p.name).collect::<Vec<_>>()
    );
    assert!(kept(&h, "once120") && !kept(&h, "once119"));
}

/// WHERE EVERY HOUR KEPT LEADS ITS SCOPE (sixteen tabs of four), a newcomer comes in only
/// among its own scope's first four and past the smallest hour.
#[test]
fn with_no_spare_a_newcomer_must_lead_its_scope_and_outwrite_the_smallest() {
    let mut h = WriteHour::new();
    let full: Vec<Writer> = (0..WRITE_PROGRAMS as u64)
        .map(|i| w(&format!("p{i}"), ProcScope::Tab(i / 4), (10 + i) * MB, 1))
        .collect();
    h.note(T0, 1_000, &full);
    h.note(
        T0 + 1_000,
        1_000,
        &[w("fifth", ProcScope::Tab(15), 50 * MB, 1)],
    );
    assert!(
        !kept(&h, "fifth"),
        "the fifth of a full tab, though past the smallest hour"
    );
    h.note(
        T0 + 2_000,
        1_000,
        &[w("small", ProcScope::Tab(99), 5 * MB, 1)],
    );
    assert!(!kept(&h, "small"), "under the smallest hour");
    h.note(
        T0 + 3_000,
        1_000,
        &[w("new", ProcScope::Tab(99), 11 * MB, 1)],
    );
    assert!(kept(&h, "new"));
    assert!(!kept(&h, "p0"), "the smallest went");
    assert_eq!(h.programs.len(), WRITE_PROGRAMS);
}

#[test]
fn a_closed_tabs_writes_stay_in_all_tabs() {
    let mut h = WriteHour::new();
    h.note(
        T0,
        1_000,
        &[
            w("rustc", ProcScope::Tab(3), 5_000 * MB, 9),
            w("rustc", ProcScope::Aterm, 1_000 * MB, 2),
            w("git", ProcScope::Tab(4), 3 * MB, 1),
        ],
    );
    h.note(
        T0 + SLOT_MS,
        1_000,
        &[w("rustc", ProcScope::Tab(3), 2_000 * MB, 4)],
    );
    let all_tabs = |h: &WriteHour| -> u64 {
        h.top()
            .iter()
            .filter(|p| p.scope != ProcScope::Other)
            .map(|p| p.bytes)
            .sum()
    };
    let before = all_tabs(&h);
    h.fold_closed(&HashSet::from([4]));
    assert_eq!(
        all_tabs(&h),
        before,
        "all tabs keeps the closed tab's bytes"
    );
    let top = h.top();
    assert_eq!(
        names(&top),
        [
            ("rustc", ProcScope::Aterm, 8_000 * MB),
            ("git", ProcScope::Tab(4), 3 * MB)
        ]
    );
    assert_eq!(top[0].n_max, 9, "the most at once of either");
    assert!(top.iter().all(|p| p.scope != ProcScope::Tab(3)));
}

#[test]
fn each_scope_filter_gets_its_exact_top() {
    let mut h = WriteHour::new();
    let mut ws = Vec::new();
    for (i, name) in ["rustc", "cc", "ld", "cargo", "git", "zsh"]
        .iter()
        .enumerate()
    {
        ws.push(w(name, ProcScope::Tab(3), (60 - i as u64) * MB, 1));
        ws.push(w(name, ProcScope::Other, (100 - i as u64) * MB, 1));
    }
    ws.push(w("aterm", ProcScope::Aterm, 2 * MB, 1));
    ws.push(w("helper", ProcScope::Aterm, MB, 1));
    h.note(T0, 1_000, &ws);
    let mut every: Vec<_> = ws
        .iter()
        .map(|w| (w.name.as_str(), w.scope, w.bytes))
        .collect();
    every.sort_by_key(|p| std::cmp::Reverse(p.2));
    let top = h.top();
    let kept = names(&top);
    /// A deck filter: its label and the scopes it shows.
    type Filter = (&'static str, fn(ProcScope) -> bool);
    let filters: [Filter; 3] = [
        ("Mac", |_| true),
        ("all tabs", |s| s != ProcScope::Other),
        ("this tab", |s| s == ProcScope::Tab(3)),
    ];
    for (label, keep) in filters {
        let want: Vec<_> = every
            .iter()
            .copied()
            .filter(|p| keep(p.1))
            .take(TOP_WRITERS)
            .collect();
        let got: Vec<_> = kept
            .iter()
            .copied()
            .filter(|p| keep(p.1))
            .take(TOP_WRITERS)
            .collect();
        assert_eq!(got, want, "{label}");
    }
    // NEGATIVE CONTROL: the Mac's first four alone would leave this tab's empty.
    let mac_only: Vec<_> = every.iter().take(TOP_WRITERS).collect();
    assert!(mac_only.iter().all(|p| p.1 == ProcScope::Other));
    assert_eq!(
        kept.iter().filter(|p| p.1 == ProcScope::Tab(3)).count(),
        TOP_WRITERS
    );
}

/// NEGATIVE CONTROL: non-finite `covered_s` yields `None`, and repeated sub-clamp backward
/// steps eventually cross `CLAMP_BACK_S` relative to the open slot's newest timestamp.
#[test]
fn per_hour_rejects_infinite_coverage_and_repeated_backsteps_shift_the_hour() {
    let row = ProgramHour {
        name: "rustc".to_owned(),
        scope: ProcScope::Aterm,
        bytes: 3_600,
        n_max: 1,
    };
    assert_eq!(row.per_hour(f64::INFINITY), None);
    assert_eq!(row.per_hour(f64::NAN), None);
    assert_eq!(row.per_hour(3_600.0), Some(3_600.0));

    let mut h = WriteHour::new();
    assert_eq!(
        h.at(T0 + 2 * SLOT_MS),
        (T0 + 2 * SLOT_MS).div_euclid(SLOT_MS)
    );
    // First 40 s step back is within CLAMP_BACK_S (60 s), so it clamps to open_k.
    assert_eq!(
        h.at(T0 + 2 * SLOT_MS - 40_000),
        (T0 + 2 * SLOT_MS).div_euclid(SLOT_MS)
    );
    // A second 40 s step back sits 80 s behind the open slot's newest reading (> CLAMP_BACK_S),
    // so it shifts the hour backward instead of staying pinned.
    assert_eq!(
        h.at(T0 + 2 * SLOT_MS - 80_000),
        (T0 + 2 * SLOT_MS - 80_000).div_euclid(SLOT_MS)
    );
}

/// A1 §1.4 item 9: one back-step rule ([`crate::ring::lands`]) serves the ten-second ring,
/// its ten-minute buckets, the free history and this hour alike, and compares a reading with
/// the newest reading. Up to `CLAMP_BACK_S` (60 s) behind it, a reading in an earlier bucket
/// lands in the newest; further back, every held bucket is re-keyed so the newest held is the
/// one before the reading's, and the reading opens its own. Negative control: a `lands` with no
/// back step (`Back` taken as `Newest`, main's clamp) fails the re-keyed buckets (the mutant
/// the F4′ review ran); the scenario's five minutes back is one ten-minute bucket back, which
/// main's one-bucket clamp (`Long`, `FreeHistory`) kept in the newest.
#[test]
fn one_back_step_rule_rekeys_bucket_ring_long_free_history_and_write_hour_alike() {
    use crate::history::FreeHistory;
    use crate::ring::{BUCKET_MS, BucketRing, LONG_BUCKET_MS, Taken};
    let b = T0 - T0.rem_euclid(LONG_BUCKET_MS) + 5 * LONG_BUCKET_MS;
    let newest = b + 5_000;
    let near = newest - 30_000;
    let far = newest - 300_000;
    let tk = |at: i64| Taken::at(at, 1_000);

    let mut ring = BucketRing::default();
    let _ = ring.push(tk(newest), Some(1.0), false);
    let _ = ring.push(tk(near), Some(1.0), false);
    assert_eq!(
        ring.open_k(),
        Some(newest / BUCKET_MS),
        "near: the newest bucket"
    );
    let _ = ring.push(tk(far), Some(2.0), false);
    let k = far.div_euclid(BUCKET_MS);
    assert_eq!(ring.open_k(), Some(k));
    assert_eq!(ring.bucket(k - 1).map(|a| (a.n, a.max)), Some((2, 1.0)));
    assert_eq!(ring.bucket(k).map(|a| (a.n, a.max)), Some((1, 2.0)));

    let mut long = BucketRing::default();
    let _ = long.push(tk(b + 1_000), Some(1.0), false);
    let _ = long.push(tk(b + 11_000), Some(3.0), false);
    assert_eq!(long.long_k(), Some(b / LONG_BUCKET_MS));
    let _ = long.push(tk(b + 11_000 - 30_000), Some(3.0), false);
    assert_eq!(
        long.long_k(),
        Some(b / LONG_BUCKET_MS),
        "a clamped reading folds nothing"
    );
    let _ = long.push(tk(b + 11_000 - 300_000), Some(5.0), false);
    let folded = (b - 300_000).div_euclid(LONG_BUCKET_MS);
    assert_eq!(
        long.long_k(),
        Some(folded),
        "the re-keyed bucket folds into its own"
    );
    let cells = long.long_cells();
    let means: Vec<Option<f64>> = cells[cells.len() - 2..].iter().map(|c| c.mean).collect();
    assert_eq!(
        means,
        [Some(1.0), Some(3.0)],
        "the held one moved before it"
    );

    let mut free = FreeHistory::new();
    free.note(tk(newest), 10);
    free.note(tk(near), 9);
    assert_eq!(free.last_k(), Some(newest / LONG_BUCKET_MS));
    free.note(tk(far), 8);
    let fk = far.div_euclid(LONG_BUCKET_MS);
    assert_eq!(free.last_k(), Some(fk));
    let rows: Vec<(i64, u16)> = free
        .saved_rows()
        .iter()
        .map(|(at, a)| (at / LONG_BUCKET_MS, a.n))
        .collect();
    assert_eq!(rows, [(fk - 1, 2), (fk, 1)]);

    let mut hour = WriteHour::new();
    assert_eq!(hour.at(newest), newest / SLOT_MS);
    assert_eq!(hour.at(near), newest / SLOT_MS);
    hour.note(near, 1_000, &[w("cp", ProcScope::Other, MB, 1)]);
    let wk = far.div_euclid(SLOT_MS);
    assert_eq!(hour.at(far), wk);
    assert_eq!(hour.covered[0].0, wk - 1);

    assert_eq!(
        crate::ring::lands(Some(b / LONG_BUCKET_MS), Some(newest), far, LONG_BUCKET_MS),
        Lands::Back(2)
    );
    assert_eq!(
        b / LONG_BUCKET_MS - fk,
        1,
        "the scenario: one bucket back, which a one-bucket clamp keeps in the newest"
    );
}
