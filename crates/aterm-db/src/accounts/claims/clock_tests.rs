// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Claims under a wrong clock (docs/DESIGN-fluid-swarm-2026-10-28.md F22): a Mac whose peers'
//! majority reads its clock as off writes no row and hands none on; a Mac they split over, or one
//! that hears nobody, writes as before. Every clock here is injected: the ring clock of each
//! round, the `now` handed to `read_at`/`write_at`, and the readings the runtime's store keeps.

use super::*;
use crate::{
    accounts::claims::tests::repair_day,
    clock::{self, Clocks, Reading, Source, Standing},
    table::tests::{Lab, clock_round, repair_commons_at},
};
use aterm_json::json;
use aterm_secrets::commons::roster::Role;

const DAY: u64 = EPOCH_S;
/// Day 20,000 since the Unix epoch.
const D: u64 = 20_000;
const KEEP: Duration = Duration::from_secs(600);

/// `secs` into day `day`.
const fn at(day: u64, secs: u64) -> u64 {
    day * DAY + secs
}

fn row(counter: u64, now: u64) -> Value {
    json!({"counter": counter, "until": now + DAY, "claims": []})
}

fn counter(read: &Read, machine: &[u8; 16]) -> Option<u64> {
    read.rows
        .iter()
        .find(|(m, _)| m == machine)
        .and_then(|(_, body)| body["counter"].as_u64())
}

/// Mac `n`'s Commons machine.
fn machine(lab: &Lab, n: usize) -> [u8; 16] {
    *lab.bootstrap(n).request().unwrap().binding().machine()
}

/// Every Mac's machine and recipient, as the runtime's store judges them.
fn members(lab: &Lab) -> BTreeMap<[u8; 16], String> {
    let bindings: Vec<_> = (0..lab.paths.len())
        .map(|m| lab.bootstrap(m).request().unwrap().binding().clone())
        .collect();
    clock::members(bindings.iter())
}

/// Mac `n`'s claim directory, where its runtime keeps `clock.state`.
fn clock_dir(lab: &Lab, n: usize) -> std::path::PathBuf {
    clock::dir(&lab.bootstrap(n)).unwrap()
}

/// What Mac 0 reads of each of `peers` in one marked round, Mac 0's clock at `mine` and each
/// peer's at its `clocks` entry, kept in Mac 0's store as its runtime keeps them and judged at
/// `mine`.
fn hear(lab: &Lab, peers: &[usize], mine: u64, clocks: &[u64]) {
    let dir = clock_dir(lab, 0);
    let mut store = Clocks::open(&dir);
    for (&n, &theirs) in peers.iter().zip(clocks) {
        let [read, _] = clock_round(lab, (0, n), [mine, theirs], [true, true]);
        let reading = read.clock().expect("a reading of the mark");
        store.observe(machine(lab, n), reading, Instant::now());
    }
    store.tick(
        &dir,
        &members(lab),
        &machine(lab, 0),
        mine * 1000,
        Instant::now(),
        KEEP,
    );
}

/// Three writers write on day D and Mac 0 repairs with each. Mac 0's clock then runs two days
/// ahead, and it reads Macs 1 and 2 in a marked round each: they read it ahead, so it writes no row
/// (the refusal says how far and against how many), its read says why it may not write, and it
/// hands no row on. Once its clock is right again and a round reads it agreeing, it writes on and
/// its peers read the row.
///
/// Negative control (today's code: no judgement, `clock_of` judging Unknown whatever the file),
/// Mac 0 wrote its row into day D+2's table, which its peers never read (`called
/// Result::unwrap_err() on an Ok value: ()`). The test written by hand on origin/main, a
/// `clock.state` it cannot read, failed the same way.
#[test]
fn a_clock_two_days_ahead_writes_no_claim() {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let caps = Caps::default();
    let now = at(D, 60);
    for paths in &lab.paths {
        write_at(paths, &row(1, now), now, caps).unwrap();
    }
    repair_day(&lab, 1, D, &[now; 3]);
    let a = machine(&lab, 0);

    let ahead = at(D + 2, 60);
    hear(&lab, &[1, 2], ahead, &[now, now]);
    let read = read_at(&lab.paths[0], ahead, caps).unwrap();
    // The row it would write next, on the window of its clock.
    let next = counter(&read, &a).map_or(1, |c| c + 1);
    let refused = write_at(&lab.paths[0], &row(next, ahead), ahead, caps).unwrap_err();
    assert_eq!(read.clock.standing, Standing::Off { ahead: true });
    assert_eq!((read.clock.against, read.clock.of), (2, 2));
    // At least two days less the round's span: never more than the clocks' distance.
    let two_days = 2 * DAY * 1000;
    assert!(
        (two_days - 60_000..two_days).contains(&read.clock.by_ms),
        "{:?}",
        read.clock
    );
    assert_eq!(Some(refused.0.clone()), read.clock.refusal());
    assert_eq!(read.writable, Err(refused.0.clone()));
    assert!(
        read.clock.skewed.is_empty(),
        "an Off Mac names nobody skewed"
    );
    assert_eq!(
        own_table(
            &lab.paths[0],
            ahead,
            Instant::now() + Duration::from_secs(2)
        )
        .unwrap_err()
        .0,
        refused.0,
        "no row to hand on"
    );

    // The clock comes back and one round each reads it agreeing: the clock refuses nothing. Its
    // rounds two days ahead took up the tables of that window, so it writes once its clock
    // reaches their floor (F19's rolled tables, unchanged here), and its peers read the row.
    let back = at(D, 120);
    hear(&lab, &[1, 2], back, &[back, back]);
    let read = read_at(&lab.paths[0], back, caps).unwrap();
    assert_eq!(read.clock.standing, Standing::Agrees);
    assert_eq!(read.clock.refusal(), None);
    let floor = at(D + 1, 120);
    hear(&lab, &[1, 2], floor, &[floor, floor]);
    let read = read_at(&lab.paths[0], floor, caps).unwrap();
    assert_eq!(
        (read.writable.clone(), read.clock.standing),
        (Ok(()), Standing::Agrees)
    );
    let next = counter(&read, &a).map_or(1, |c| c + 1);
    write_at(&lab.paths[0], &row(next, floor), floor, caps).unwrap();
    repair_day(&lab, 2, D + 2, &[floor; 3]);
    for (n, paths) in lab.paths[..3].iter().enumerate().skip(1) {
        assert_eq!(
            counter(&read_at(paths, floor, caps).unwrap(), &a),
            Some(next),
            "Mac {n}"
        );
    }
}

/// Mac 0 runs two days ahead and hears Mac 1 (honest, so it reads Mac 0 ahead) and Mac 2, which
/// lies that its clock is Mac 0's: one each way is a tie, so Mac 0 is Split. The db writes as
/// before (only the harness refuses leases under Split), and Mac 0 names Mac 1 as skewed.
///
/// Negative control: with a tie counted as Agrees (`2 * g > n` as `2 * g >= n`), Mac 0 stood
/// Agrees (`left: Agrees`, `right: Split`), the standing the harness would lease on.
#[test]
fn a_tie_refuses_leases_and_keeps_home_claims() {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let caps = Caps::default();
    let now = at(D, 60);
    for paths in &lab.paths {
        write_at(paths, &row(1, now), now, caps).unwrap();
    }
    repair_day(&lab, 1, D, &[now; 3]);
    let ahead = at(D + 2, 60);
    hear(&lab, &[1, 2], ahead, &[now, ahead]);
    let read = read_at(&lab.paths[0], ahead, caps).unwrap();
    assert_eq!(read.clock.standing, Standing::Split);
    assert_eq!((read.clock.against, read.clock.of), (1, 2));
    assert_eq!(read.writable, Ok(()));
    assert_eq!(
        read.clock
            .skewed
            .iter()
            .map(|skewed| skewed.machine)
            .collect::<Vec<_>>(),
        vec![machine(&lab, 1)]
    );
    // Its next row, on the clock's window (which no longer holds day D's), lands.
    let next = counter(&read, &machine(&lab, 0)).map_or(1, |c| c + 1);
    let home = json!({"counter": next, "until": ahead + DAY, "claims": [{"label": "home"}]});
    write_at(&lab.paths[0], &home, ahead, caps).unwrap();
}

/// A Mac that hears nobody, or whose readings are stale or dated in the future, judges Unknown
/// and writes: two days' skew read 31 minutes ago, or a minute from now, says nothing.
///
/// Negative control: with `fresh` counting a reading from the future (`now - at` saturating at
/// zero), Mac 0 stood Off on the readings dated a minute ahead (`dated in the future`, `left: Off
/// { ahead: true }`, `right: Unknown`).
#[test]
fn a_lone_mac_keeps_working() {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let caps = Caps::default();
    let now = at(D, 600);
    let skew = i64::try_from(2 * DAY * 1000).unwrap();
    let dir = clock_dir(&lab, 0);
    let mut counter = 0;
    for (case, at_ms) in [
        ("no readings", None),
        ("stale", Some(now * 1000 - 31 * 60 * 1000)),
        ("dated in the future", Some(now * 1000 + 60 * 1000)),
    ] {
        let readings = at_ms
            .map(|at_ms| {
                [1, 2]
                    .map(|n| {
                        let reading = Reading {
                            lo_ms: skew,
                            hi_ms: skew + 1000,
                            at_ms,
                            source: Source::Mark,
                        };
                        (machine(&lab, n), reading)
                    })
                    .into()
            })
            .unwrap_or_default();
        clock::State {
            last: Standing::Unknown,
            readings,
        }
        .write(&dir)
        .unwrap();
        let read = read_at(&lab.paths[0], now, caps).unwrap();
        assert_eq!(read.clock.standing, Standing::Unknown, "{case}");
        assert_eq!(read.writable, Ok(()), "{case}");
        counter += 1;
        write_at(&lab.paths[0], &row(counter, now), now, caps).unwrap();
    }
}

/// A Mac its peers last read two days ahead that wakes from a sleep past every reading's
/// freshness (or whose clock was set back since, dating them in its future) writes no row and
/// hands none on until a round reads it again: silence keeps Off.
///
/// Negative controls: with no fresh reading judged Unknown whatever the last standing, the waking
/// Mac read itself Unknown (`asleep`, `left: Unknown`, `right: Off { ahead: true }`); with
/// `write_at` writing whatever the clock (its refusal filtered out), it wrote its row (`asleep:
/// ()`).
#[test]
fn an_off_mac_writes_nothing_through_a_sleep() {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let caps = Caps::default();
    let now = at(D, 600);
    let skew = i64::try_from(2 * DAY * 1000).unwrap();
    let dir = clock_dir(&lab, 0);
    for (case, at_ms) in [
        ("asleep", now * 1000 - 31 * 60 * 1000),
        ("set back", now * 1000 + 60 * 1000),
    ] {
        let readings = [1, 2]
            .map(|n| {
                let reading = Reading {
                    lo_ms: skew,
                    hi_ms: skew + 1000,
                    at_ms,
                    source: Source::Mark,
                };
                (machine(&lab, n), reading)
            })
            .into();
        clock::State {
            last: Standing::Off { ahead: true },
            readings,
        }
        .write(&dir)
        .unwrap();
        let read = read_at(&lab.paths[0], now, caps).unwrap();
        assert_eq!(read.clock.standing, Standing::Off { ahead: true }, "{case}");
        let refused = write_at(&lab.paths[0], &row(1, now), now, caps)
            .map_err(|why| why.0)
            .expect_err(case);
        assert_eq!(read.writable, Err(refused.clone()), "{case}");
        assert!(refused.contains("last heard"), "{case}: {refused}");
    }
}

/// A Mac a day ahead of both its peers, or a day behind them, refuses its row too, judged Off
/// that way.
///
/// Negative control: with Off judged ahead only (the `2 * b > n` arm left out), the Mac a day
/// behind stood Split and wrote its row (`called Result::unwrap_err() on an Ok value: ()`).
#[test]
fn a_clock_one_day_ahead_and_one_behind_refuse_too() {
    let caps = Caps::default();
    let now = at(D, 3600);
    for (mine, way) in [(at(D + 1, 3600), "ahead"), (at(D - 1, 3600), "behind")] {
        let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
        for paths in &lab.paths {
            write_at(paths, &row(1, now), now, caps).unwrap();
        }
        repair_commons_at(&lab, 6, &[now; 3]);
        hear(&lab, &[1, 2], mine, &[now, now]);
        let read = read_at(&lab.paths[0], mine, caps).unwrap();
        let next = counter(&read, &machine(&lab, 0)).map_or(1, |c| c + 1);
        let refused = write_at(&lab.paths[0], &row(next, mine), mine, caps).unwrap_err();
        assert_eq!(Some(refused.0.clone()), read.clock.refusal(), "{way}");
        assert_eq!(
            read.clock.standing,
            Standing::Off {
                ahead: way == "ahead"
            },
            "{way}"
        );
        assert_eq!(read.writable, Err(refused.0), "{way}");
    }
}
