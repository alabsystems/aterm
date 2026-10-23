// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the recovery soak judge across passing ledgers, broken criteria, and edge cases.

use super::*;
use crate::recovery_ledger::{EndClass, LaunchKind, Pressure, PrevRun, Tri, parse_rows};

const T0: u64 = 1_790_000_000;

fn launch(day: u64, pid: u32, prev_pid: u32, class: EndClass, keeper: KeeperAtLaunch) -> Row {
    Row {
        at: T0 + day * DAY,
        pid,
        started: T0 + day * DAY - 1,
        started_us: None,
        build: 1_790_305_290,
        version: "0.120.0".into(),
        launch: LaunchKind::Cold,
        prev: PrevRun {
            pid: Some(prev_pid),
            build: Some(1_790_305_290),
            version: Some("0.120.0".into()),
            class,
            uptime: None,
            stall: Tri::No,
            pressure: Pressure::Normal,
            trial: 0,
        },
        keeper: Some(keeper),
        restores: None,
    }
}

fn exact(n: u32) -> Restores {
    Restores {
        exact: n,
        ..Restores::default()
    }
}

fn met_ledger() -> Vec<Row> {
    let mut rows = Vec::new();
    let mut pid = 100;
    for day in 0..16 {
        let prev = pid;
        pid += 1;
        let killed = matches!(day, 3 | 8 | 13);
        let class = if day == 0 {
            EndClass::None
        } else if killed {
            EndClass::Killed
        } else {
            EndClass::Clean
        };
        let mut row = launch(day, pid, prev, class, KeeperAtLaunch::On);
        if killed {
            row.restores = Some(exact(2));
        }
        rows.push(row);
    }
    rows
}

fn now_after(rows: &[Row]) -> u64 {
    rows.last().map_or(T0, |r| r.at) + 3600
}

fn criterion<'a>(soak: &'a Soak, name: &str) -> &'a Criterion {
    soak.criteria
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no criterion {name}"))
}

#[track_caller]
fn only_broken(soak: &Soak, broken: &str) {
    for c in &soak.criteria {
        assert_eq!(c.met, c.name != broken, "{}: {}", c.name, c.detail);
    }
    assert!(!soak.met());
}

#[test]
fn a_ledger_that_meets_the_bar_is_met() {
    let rows = met_ledger();
    let soak = judge(&rows, now_after(&rows), &P4);
    assert!(soak.met(), "{:#?}", soak.criteria);
    assert_eq!(soak.kills_recovered, 3);
    assert_eq!(soak.restores.exact, 6);
    assert_eq!(soak.streak_on, 16);
    assert_eq!(soak.streak_since, Some(T0));
    let lines = soak.lines();
    assert_eq!(
        lines.last().map(String::as_str),
        Some("verdict     MET (5 criteria)")
    );
    assert!(
        lines.iter().any(|l| l.contains("(Q1)")),
        "the lab read is named as not judged: {lines:#?}"
    );
}

#[test]
fn the_judge_reads_the_rows_the_ledger_writes() {
    let rows = met_ledger();
    let text: String = rows.iter().map(|r| r.to_line() + "\n").collect();
    let parsed = parse_rows(&text);
    assert_eq!(parsed, rows);
    assert!(judge(&parsed, now_after(&parsed), &P4).met());
}

#[test]
fn an_empty_ledger_meets_nothing() {
    let soak = judge(&[], T0, &P4);
    assert!(soak.criteria.iter().filter(|c| c.met).count() <= 2);
    for name in ["keeper-on", "real-kills", "exact"] {
        assert!(!criterion(&soak, name).met, "{name}");
    }
    assert!(!soak.met());
    assert!(
        soak.lines()
            .last()
            .is_some_and(|l| l.starts_with("verdict     NOT MET")),
    );
}

#[test]
fn too_short_a_streak_is_not_met() {
    let rows = met_ledger();
    let soak = judge(&rows, T0 + 13 * DAY + 3600, &P4);
    only_broken(&soak, "keeper-on");
}

#[test]
fn a_launch_with_the_link_off_breaks_the_streak() {
    let mut rows = met_ledger();
    rows[10].keeper = Some(KeeperAtLaunch::Off);
    let soak = judge(&rows, now_after(&rows), &P4);
    only_broken(&soak, "keeper-on");
    assert_eq!(soak.streak_since, Some(T0 + 11 * DAY));
}

#[test]
fn a_row_from_before_the_keeper_word_breaks_the_streak() {
    let mut rows = met_ledger();
    rows[10].keeper = None;
    let soak = judge(&rows, now_after(&rows), &P4);
    only_broken(&soak, "keeper-on");
}

#[test]
fn the_newest_launch_with_the_link_off_has_no_streak() {
    let mut rows = met_ledger();
    if let Some(last) = rows.last_mut() {
        last.keeper = Some(KeeperAtLaunch::Off);
    }
    let soak = judge(&rows, now_after(&rows), &P4);
    only_broken(&soak, "keeper-on");
    assert_eq!(soak.streak_since, None);
    assert!(
        criterion(&soak, "keeper-on")
            .detail
            .starts_with("the newest launch had the keeper link off"),
    );
    if let Some(last) = rows.last_mut() {
        last.keeper = None;
    }
    let soak = judge(&rows, now_after(&rows), &P4);
    assert!(
        criterion(&soak, "keeper-on")
            .detail
            .contains("a build before `keeper=` wrote it"),
    );
}

#[test]
fn absent_and_armed_launches_keep_the_streak() {
    let mut rows = met_ledger();
    rows[5].keeper = Some(KeeperAtLaunch::Absent);
    rows[6].keeper = Some(KeeperAtLaunch::Armed);
    rows[6].launch = LaunchKind::Successor;
    rows[6].prev.class = EndClass::Handoff;
    let soak = judge(&rows, now_after(&rows), &P4);
    assert!(soak.met(), "{:#?}", soak.criteria);
    assert_eq!(
        (soak.streak_on, soak.streak_armed, soak.streak_absent),
        (14, 1, 1)
    );
}

#[test]
fn two_kills_are_not_enough() {
    let mut rows = met_ledger();
    rows[8].restores = None;
    rows[8].prev.class = EndClass::Clean;
    let soak = judge(&rows, now_after(&rows), &P4);
    only_broken(&soak, "real-kills");
}

#[test]
fn a_kill_is_an_unexpected_end_and_a_quit_is_not() {
    let mut rows = met_ledger();
    rows[3].prev.class = EndClass::Signal(11);
    rows[8].prev.class = EndClass::Panic;
    assert!(judge(&rows, now_after(&rows), &P4).met());
    rows[13].prev.class = EndClass::Clean;
    let soak = judge(&rows, now_after(&rows), &P4);
    only_broken(&soak, "real-kills");
    assert_eq!(soak.other_recoveries, 1);
    assert!(soak.lines().iter().any(|l| l.starts_with("other")));
}

#[test]
fn too_few_exact_sessions_are_not_met() {
    let mut rows = met_ledger();
    rows[13].restores = Some(Restores {
        exact: 2,
        screen: 1,
        whys: vec![("in-flight".into(), 1)],
        in_flight_lost: 512,
        ..Restores::default()
    });
    let soak = judge(&rows, now_after(&rows), &P4);
    only_broken(&soak, "exact");
    assert_eq!(soak.kills_in_flight, 1);
    assert_eq!(soak.restores.in_flight_lost, 512);
}

#[test]
fn the_exact_floor_is_inclusive() {
    let mut rows = met_ledger();
    rows[3].restores = Some(exact(4));
    rows[8].restores = Some(exact(4));
    rows[13].restores = Some(Restores {
        exact: 1,
        screen: 1,
        whys: vec![("resizes".into(), 1)],
        ..Restores::default()
    });
    let soak = judge(&rows, now_after(&rows), &P4);
    assert_eq!(permille(soak.restores.exact, 10), 900);
    assert!(soak.met(), "{:#?}", soak.criteria);
}

#[test]
fn a_blank_screen_is_not_met() {
    let mut rows = met_ledger();
    rows[3].restores = Some(exact(10));
    rows[8].restores = Some(Restores {
        exact: 10,
        meta: 1,
        whys: vec![("no-region".into(), 1)],
        ..Restores::default()
    });
    let soak = judge(&rows, now_after(&rows), &P4);
    only_broken(&soak, "none-blank");
}

#[test]
fn a_kill_the_keeper_did_not_bring_back_is_a_miss() {
    let mut rows = met_ledger();
    rows[10].prev.class = EndClass::Killed;
    let killed_pid = rows[9].pid;
    let soak = judge(&rows, now_after(&rows), &P4);
    only_broken(&soak, "no-miss");
    assert_eq!(
        soak.misses,
        vec![Miss {
            at: rows[10].at,
            killed_pid
        }]
    );

    let mut absent = rows.clone();
    absent[9].keeper = Some(KeeperAtLaunch::Absent);
    let soak = judge(&absent, now_after(&absent), &P4);
    only_broken(&soak, "no-miss");
    assert_eq!(soak.misses.len(), 1);

    let mut off = rows.clone();
    off[9].keeper = Some(KeeperAtLaunch::Off);
    assert!(judge(&off, now_after(&off), &P4).misses.is_empty());

    let mut none_answered = rows.clone();
    none_answered[10].keeper = Some(KeeperAtLaunch::Absent);
    assert!(
        judge(&none_answered, now_after(&none_answered), &P4)
            .misses
            .is_empty()
    );

    let trimmed: Vec<Row> = rows[10..].to_vec();
    assert!(judge(&trimmed, now_after(&trimmed), &P4).misses.is_empty());

    let mut armed = rows;
    armed[9].keeper = Some(KeeperAtLaunch::Armed);
    assert_eq!(judge(&armed, now_after(&armed), &P4).misses.len(), 1);
}

#[test]
fn a_session_the_recovery_refused_is_a_miss() {
    let mut rows = met_ledger();
    rows[13].restores = Some(Restores {
        exact: 2,
        refused: 1,
        ..Restores::default()
    });
    let soak = judge(&rows, now_after(&rows), &P4);
    for c in &soak.criteria {
        let broken = matches!(c.name, "exact" | "no-miss");
        assert_eq!(c.met, !broken, "{}: {}", c.name, c.detail);
    }
    assert_eq!(soak.restores.refused, 1);
    assert_eq!(soak.kills_recovered, 3);
    assert!(soak.misses.is_empty(), "no launch recovered nothing");

    rows[3].restores = Some(exact(10));
    let soak = judge(&rows, now_after(&rows), &P4);
    only_broken(&soak, "no-miss");

    let mut all_refused = met_ledger();
    all_refused[13].restores = Some(Restores {
        refused: 2,
        ..Restores::default()
    });
    let soak = judge(&all_refused, now_after(&all_refused), &P4);
    assert_eq!(soak.kills_recovered, 2);
    assert!(!criterion(&soak, "no-miss").met);
    assert!(!criterion(&soak, "real-kills").met);
}

#[test]
fn a_recovery_after_an_unknown_end_is_not_said_to_be_on_purpose() {
    let mut rows = met_ledger();
    rows[5].prev.class = EndClass::Unknown;
    rows[5].restores = Some(exact(1));
    rows[6].prev.class = EndClass::Clean;
    rows[6].restores = Some(exact(1));
    rows[7].prev.class = EndClass::Clean;
    rows[7].restores = Some(Restores {
        refused: 1,
        ..Restores::default()
    });
    let soak = judge(&rows, now_after(&rows), &P4);
    assert!(soak.met(), "{:#?}", soak.criteria);
    assert_eq!(
        (soak.other_recoveries, soak.unclassified_recoveries),
        (1, 1)
    );
    assert_eq!(soak.kills_recovered, 3);
}

#[test]
fn the_losses_are_summed() {
    let mut rows = met_ledger();
    rows[3].restores = Some(Restores {
        exact: 9,
        screen: 1,
        history_lost: 300,
        history_short: 1,
        tail_overflows: 1,
        whys: vec![("tail-overflow".into(), 1)],
        ..Restores::default()
    });
    rows[8].restores = Some(Restores {
        exact: 9,
        screen: 1,
        whys: vec![("tail-overflow".into(), 1)],
        ..Restores::default()
    });
    rows[13].restores = Some(exact(10));
    let soak = judge(&rows, now_after(&rows), &P4);
    assert!(soak.met(), "{:#?}", soak.criteria);
    assert_eq!(soak.restores.whys, vec![("tail-overflow".to_string(), 2)]);
    assert_eq!(soak.restores.tail_overflows, 1);
    assert_eq!(soak.kills_history, 1);
    assert_eq!(soak.restores.history_lost, 300);
    assert!(
        soak.lines()
            .iter()
            .any(|l| l.contains("why not exact: tail-overflow 2")),
        "{:#?}",
        soak.lines()
    );
}
