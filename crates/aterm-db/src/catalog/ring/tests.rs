// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::tests::Lab;
use aterm_secrets::commons::roster::Role;

const DAY: u64 = CLAIMS.period_s;
/// Day 20,000 since the Unix epoch, a fixed clock.
const D: u64 = 20_000;

const fn at(day: u64, secs: u64) -> u64 {
    day * DAY + secs
}

/// Run `f` as the first Mac's control writer.
fn with_writer<T>(lab: &Lab, f: impl FnOnce(&Session<'_>, &Directory) -> T) -> T {
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    f(&session, &directory)
}

/// The days among `days` whose claims table has storage.
fn dirs(directory: &Directory, days: RangeInclusive<u64>) -> Vec<u64> {
    days.filter(|day| {
        let scope = CLAIMS.scope(directory.trust, *day).unwrap();
        directory.path(scope).unwrap().exists()
    })
    .collect()
}

fn ids(directory: &Directory, days: impl IntoIterator<Item = u64>) -> BTreeSet<Digest> {
    days.into_iter()
        .map(|day| CLAIMS.scope(directory.trust, day).unwrap().digest())
        .collect()
}

/// NEGATIVE CONTROL: without the length prefixes (`Ring::scope` hashing the bare words),
/// `accounts`/`claims` and `accountsc`/`laims` name one table.
#[test]
fn ring_scopes_never_name_a_catalog_table() {
    let lab = Lab::new(&[Role::Writer]);
    let trust = lab.bootstrap(0).trust();
    let directory = Directory::new(&lab.paths[0], trust);
    let split = Ring {
        application: "accountsc",
        role: "laims",
        period_s: CLAIMS.period_s,
    };
    let mut declared = vec![directory.catalog_scope()];
    for (application, role) in [
        ("accounts", "claims"),
        ("accounts", "claims.2"),
        ("accounts", "claims.6"),
        ("accountsc", "laims"),
    ] {
        declared.push(
            Descriptor::application(trust, application, role, 1)
                .unwrap()
                .scope,
        );
    }
    let mut seen = BTreeSet::new();
    for epoch in [0, 1, D, D + 1, u64::MAX] {
        for ring in [&CLAIMS, &split] {
            let scope = ring.scope(trust, epoch).unwrap();
            assert!(!declared.contains(&scope), "{} {epoch}", ring.application);
            assert!(seen.insert(scope.digest()), "{} {epoch}", ring.application);
        }
    }
}

/// A Mac that holds no ring table lists none; each day it takes up the window's tables a peer
/// offers, never one outside the window, and deletes the one before the window. While that one is
/// still held, the read-only subscription asks for the writer.
/// NEGATIVE CONTROL: with retirement off (`Directory::retire` returning before it deletes), five
/// day tables are on disk on day 3.
#[test]
fn ring_window_is_subscribed_and_older_epochs_retire() {
    let lab = Lab::new(&[Role::Writer]);
    let policy = Policy::default();
    with_writer(&lab, |session, directory| {
        let now = at(D, 60);
        assert!(
            directory
                .ring_subscriptions(session, &policy, now)
                .unwrap()
                .is_empty()
        );
        assert!(
            directory
                .existing_ring_subscriptions(session, &policy, now)
                .unwrap()
                .is_some_and(|tables| tables.is_empty())
        );
        for day in D..D + 5 {
            let now = at(day, 60);
            let window: Vec<u64> = (day - 1..=day + 1).collect();
            // A peer offers every table from two days back to two days on.
            let offered = ids(directory, day - 2..=day + 2);
            directory
                .adopt_ring_tables(session, &policy, now, &offered)
                .unwrap();
            let tables = directory.ring_subscriptions(session, &policy, now).unwrap();
            assert_eq!(
                tables.keys().copied().collect::<BTreeSet<_>>(),
                ids(directory, window.clone()),
                "day {}",
                day - D
            );
            assert_eq!(dirs(directory, D - 3..=D + 7), window, "day {}", day - D);
            let existing = directory
                .existing_ring_subscriptions(session, &policy, now)
                .unwrap()
                .expect("the window is held");
            assert_eq!(
                existing.keys().copied().collect::<BTreeSet<_>>(),
                ids(directory, window),
                "day {}",
                day - D
            );
            if day + 1 < D + 5 {
                assert!(
                    directory
                        .existing_ring_subscriptions(session, &policy, at(day + 1, 60))
                        .unwrap()
                        .is_none(),
                    "day {}",
                    day - D
                );
            }
        }
    });
}

/// NEGATIVE CONTROL: `adopt_ring_tables` ignoring policy (its `enabled` and `excluded` checks
/// removed) takes up the whole window with Commons off and the excluded day with it on.
#[test]
fn policy_exclusion_and_disable_exclude_ring_tables() {
    let lab = Lab::new(&[Role::Writer]);
    with_writer(&lab, |session, directory| {
        let now = at(D, 60);
        let offered = ids(directory, D - 1..=D + 1);
        let off = Policy {
            enabled: false,
            excluded: BTreeSet::new(),
        };
        assert!(
            directory
                .adopt_ring_tables(session, &off, now, &offered)
                .unwrap()
                .is_empty()
        );
        assert_eq!(dirs(directory, D - 2..=D + 2), Vec::<u64>::new());

        let today = CLAIMS.scope(directory.trust, D).unwrap().digest();
        let excluded = Policy {
            enabled: true,
            excluded: BTreeSet::from([today]),
        };
        let adopted = directory
            .adopt_ring_tables(session, &excluded, now, &offered)
            .unwrap();
        assert_eq!(
            adopted.keys().copied().collect::<BTreeSet<_>>(),
            ids(directory, [D - 1, D + 1])
        );
        assert_eq!(dirs(directory, D - 2..=D + 2), vec![D - 1, D + 1]);
        assert!(!directory.permits(session, &excluded, today, now).unwrap());
        assert!(!directory.permits(session, &off, today, now).unwrap());
        assert!(
            directory
                .permits(session, &Policy::default(), today, now)
                .unwrap()
        );
        // Commons off or the catalog excluded lists none of the tables this Mac holds.
        let no_catalog = Policy {
            enabled: true,
            excluded: BTreeSet::from([directory.catalog_scope().digest()]),
        };
        for policy in [&off, &no_catalog] {
            assert!(
                directory
                    .ring_subscriptions(session, policy, now)
                    .unwrap()
                    .is_empty()
            );
            assert!(
                directory
                    .existing_ring_subscriptions(session, policy, now)
                    .unwrap()
                    .is_some_and(|tables| tables.is_empty())
            );
        }
    });
}

/// A ring table's storage never exists beside no catalog, which an observer reads as damaged
/// custody: the catalog's storage comes first, whether a peer's offer or a write takes the table
/// up, even where a catalog enrollment cut short left the table root behind.
/// NEGATIVE CONTROL: taking a table up before the catalog (`adopt_ring_tables` and `enroll_ring`
/// without `ensure_catalog`) leaves ring storage that `observe_with_config` refuses.
#[test]
fn a_ring_dir_never_outlives_the_catalog() {
    fn adopt(session: &Session<'_>, directory: &Directory) {
        let offered = ids(directory, [D]);
        directory
            .adopt_ring_tables(session, &Policy::default(), at(D, 60), &offered)
            .unwrap();
    }
    fn enroll(session: &Session<'_>, directory: &Directory) {
        directory
            .enroll_ring(session, &CLAIMS, D, at(D, 60))
            .unwrap();
    }
    for first in [adopt as fn(&Session<'_>, &Directory), enroll] {
        let lab = Lab::new(&[Role::Writer]);
        let config = lab.paths[0].home.join("config.toml");
        with_writer(&lab, |session, directory| {
            // `Directory::catalog` makes the root before it enrolls the catalog.
            aterm_journal::create_private_dir_durable(&directory.root).unwrap();
            assert!(
                directory
                    .observe_with_config(session, &config)
                    .unwrap()
                    .entries
                    .is_empty()
            );
            first(session, directory);
            assert_eq!(dirs(directory, D..=D), vec![D]);
            assert!(directory.observe_with_config(session, &config).is_ok());
        });
    }
}
