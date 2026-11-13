// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The history fold the Account card and `accounts history` share ([`read`]): the days it gives
//! are the ones `accounts history` prints (its own tests, `cli_history_tests.rs`, run over this
//! fold and pin the printed lines), an account with no label here is found by its key, and the
//! stamp moves only with this Mac's series files.

use super::*;
use crate::harness::account_series::Fig;

const H: i64 = 3600;
const D: i64 = 86_400;
/// Sat 20 Sep 2025 23:00 UTC.
const NOW: i64 = 1_758_409_200;
const UUID: &str = "5d0c1f2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f";
const OTHER_UUID: &str = "9e8d7c6b-5a49-4382-b716-0f1e2d3c4b5a";
const MACHINE: &str = "m1";

/// Seven hours west of UTC.
fn west(_: i64) -> i64 {
    -7 * H
}

fn tmp(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-account-history-{tag}-"))
        .tempdir()
        .expect("a scratch directory")
}

fn fig(pct_x10: u32, reset: i64, at: i64) -> Fig {
    Fig {
        pct_x10,
        reset,
        at,
        src: accounts::SOURCE_TAP.into(),
        epoch: None,
    }
}

fn reading(
    at: i64,
    five: Option<(u32, i64)>,
    week: Option<(u32, i64)>,
    key: Option<AccountKey>,
) -> SeriesRow {
    SeriesRow {
        t: at,
        by: "m".into(),
        said: Said::Fig {
            five_hour: five.map(|(p, r)| fig(p, r, at)),
            seven_day: week.map(|(p, r)| fig(p, r, at)),
            ac: None,
            key,
        },
    }
}

fn write_series(state: &Path, label: &str, machine: &str, rows: &[SeriesRow]) -> PathBuf {
    let path = account_series::path_of(state, label, machine).expect("a series path");
    std::fs::create_dir_all(path.parent().expect("its directory")).expect("mkdir");
    let text: String = rows
        .iter()
        .map(|r| r.line().expect("a row") + "\n")
        .collect();
    std::fs::write(&path, text).expect("write");
    path
}

fn labelled(label: &str) -> HistoryWho {
    HistoryWho {
        label: Some(label.to_owned()),
        ..HistoryWho::default()
    }
}

/// THE FOLD GIVES THE DAYS `accounts history` PRINTS. The seed of
/// `history_prints_a_line_per_local_day_with_its_ledger_lines`: seven hours west of UTC, a
/// reading at 18:00 on Friday and one at 08:00 on Saturday (both Saturday in UTC) are two local
/// days, oldest first, each with its week's last figure and its closed five-hour window, and no
/// ledger line joins a day. Negative control: under UTC (`|_| 0`) they are one day.
#[test]
fn the_fold_gives_the_days_accounts_history_prints() {
    let dir = tmp("days");
    let week = NOW + 3 * D;
    write_series(
        dir.path(),
        "work",
        MACHINE,
        &[
            reading(
                NOW - 22 * H,
                Some((300, NOW - 20 * H)),
                Some((400, week)),
                None,
            ),
            reading(
                NOW - 8 * H,
                Some((600, NOW - 4 * H)),
                Some((470, week)),
                None,
            ),
        ],
    );
    let read_under =
        |tz_at: fn(i64) -> i64| read(dir.path(), MACHINE, &labelled("work"), 30 * D, tz_at, NOW);
    let held = read_under(west);
    assert_eq!(held.oldest, Some(NOW - 22 * H));
    assert_eq!(held.rows.len(), 2, "both readings, no close among them");
    /// A day's midnight, its week's last figure and its five-hour window's two.
    type Day = (i64, Option<u32>, Option<(u32, u32)>);
    let days: Vec<Day> = held
        .days
        .iter()
        .map(|day| {
            let sum = day.sum.as_ref();
            (
                day.t,
                sum.and_then(|s| s.seven_day).map(|w| w.last),
                sum.and_then(|s| s.five_hour.as_ref())
                    .map(|f| (f.closed, f.peak)),
            )
        })
        .collect();
    let friday = account_series::midnight(account_series::local_day(NOW - 22 * H, west), west);
    assert_eq!(
        days,
        [
            (friday, Some(400), Some((1, 300))),
            (friday + D, Some(470), Some((1, 600))),
        ]
    );
    assert!(
        held.days.iter().all(|day| day.lines.is_empty()),
        "no ledger or journal line joins the card's days"
    );
    assert_eq!(read_under(|_| 0).days.len(), 1, "CONTROL: one day in UTC");
}

/// AN ACCOUNT WITH NO LABEL HERE IS FOUND BY ITS KEY. Its readings were booked under another
/// row's file; asked for by key alone the fold finds those rows and none of the row's own
/// account's (the unkeyed and the stranger's). Negative control: asked for with no key, it finds
/// nothing.
#[test]
fn an_account_with_no_label_is_found_by_key() {
    let dir = tmp("key");
    let key = AccountKey::claude(UUID).expect("key");
    let other = AccountKey::claude(OTHER_UUID).expect("key");
    let week = NOW + 3 * D;
    write_series(
        dir.path(),
        "home",
        MACHINE,
        &[
            reading(NOW - 3 * D, None, Some((900, week)), Some(other)),
            reading(NOW - 2 * D, None, Some((200, week)), Some(key)),
            reading(NOW - 2 * H, None, Some((300, week)), Some(key)),
        ],
    );
    // Another Mac's file of the same key is never read.
    write_series(
        dir.path(),
        "home",
        "m2",
        &[reading(NOW - 5 * D, None, Some((100, week)), Some(key))],
    );
    let who = HistoryWho {
        label: None,
        pk: None,
        key: Some(key),
    };
    let held = read(dir.path(), MACHINE, &who, 30 * D, |_| 0, NOW);
    assert!(!held.is_empty());
    assert_eq!(held.oldest, Some(NOW - 2 * D), "this Mac's rows of the key");
    assert_eq!(
        held.rows.iter().map(|row| row.t).collect::<Vec<_>>(),
        [NOW - 2 * D, NOW - 2 * H]
    );
    assert_eq!(
        held.days
            .iter()
            .map(|day| day.sum.as_ref().and_then(|s| s.seven_day).map(|w| w.last))
            .collect::<Vec<_>>(),
        [Some(200), Some(300)]
    );
    let nobody = read(
        dir.path(),
        MACHINE,
        &HistoryWho::default(),
        30 * D,
        |_| 0,
        NOW,
    );
    assert!(
        nobody.is_empty(),
        "CONTROL: no key and no label finds nothing"
    );
    assert!(nobody.days.is_empty() && nobody.rows.is_empty());
}

/// THE STAMP MOVES WITH THIS MAC'S SERIES FILES AND NO OTHER. A row appended to one of this
/// Mac's files, and a new file of its, each change it; another Mac's file does not.
#[test]
fn the_stamp_moves_only_with_this_macs_series_files() {
    let dir = tmp("stamp");
    let week = NOW + 3 * D;
    let empty = stamp(dir.path(), MACHINE);
    let one = [reading(NOW - 2 * H, None, Some((300, week)), None)];
    write_series(dir.path(), "work", MACHINE, &one);
    let first = stamp(dir.path(), MACHINE);
    assert_ne!(first, empty, "a new file");
    assert_eq!(
        stamp(dir.path(), MACHINE),
        first,
        "unchanged files, one stamp"
    );
    write_series(dir.path(), "work", "m2", &one);
    assert_eq!(stamp(dir.path(), MACHINE), first, "another Mac's file");
    let two = [
        one[0].clone(),
        reading(NOW - H, None, Some((310, week)), None),
    ];
    write_series(dir.path(), "work", MACHINE, &two);
    let longer = stamp(dir.path(), MACHINE);
    assert_ne!(longer, first, "a longer file");
    write_series(dir.path(), "play", MACHINE, &one);
    assert_ne!(stamp(dir.path(), MACHINE), longer, "one more file");
}
