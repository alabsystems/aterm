// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    database::Command,
    documents::application,
    table::tests::{Lab, repair_commons},
};
use aterm_secrets::commons::{bootstrap::Bootstrap, roster::Role};

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

/// Take up the window's tables of `day` by a peer's offer of every one, as repair does.
fn adopt_window(session: &Session<'_>, directory: &Directory, day: u64) {
    let offered = ids(directory, day - 1..=day + 1);
    directory
        .adopt_ring_tables(session, &Policy::default(), at(day, 60), &offered)
        .unwrap();
}

/// Put `bytes` where the claims ring's state goes, as damage would.
fn damage_state(session: &Session<'_>, bytes: &[u8]) {
    let dir = claim_dir(session).unwrap();
    aterm_journal::write_private_durable(&dir, &CLAIMS.state_name(), bytes).unwrap();
}

/// A state damaged, and its floor's copy lost with it, while the clock is set back a day is
/// rebuilt with a floor no lower than the one it lost: day D+3, the last table the pass on day
/// D+2 took up, bounds it at day D+1, so day D, which that pass deleted, stays refused whoever
/// offers it.
/// NEGATIVE CONTROL: a rebuilt floor of 0 (`rebuild_ring_state` without `rebuilt_floor`) lets
/// the clock set back to day D+1 place the window from day D, whose offered table is taken up
/// again (`left: [20000, 20001, 20002, 20003]`).
#[test]
fn a_ring_state_rebuilt_on_a_clock_set_back_keeps_a_retired_day_refused() {
    let lab = Lab::new(&[Role::Writer]);
    with_writer(&lab, |session, directory| {
        adopt_window(session, directory, D);
        adopt_window(session, directory, D + 2);
        assert_eq!(dirs(directory, D - 2..=D + 4), vec![D + 1, D + 2, D + 3]);
        damage_state(session, b"ATRS1-not-a-state");
        std::fs::remove_file(claim_dir(session).unwrap().join(CLAIMS.floor_name())).unwrap();
        let back = at(D + 1, 60);
        let offered = ids(directory, D - 1..=D + 3);
        directory
            .adopt_ring_tables(session, &Policy::default(), back, &offered)
            .unwrap();
        assert_eq!(dirs(directory, D - 2..=D + 4), vec![D + 1, D + 2, D + 3]);
        let day = CLAIMS.scope(directory.trust, D).unwrap().digest();
        assert!(
            !directory
                .permits(session, &Policy::default(), day, back)
                .unwrap()
        );
        assert!(directory.enroll_ring(session, &CLAIMS, D, back).is_err());
    });
}

/// A ring state deleted, or damaged, and found a day or 500 days on, is rebuilt from every table
/// this Mac holds however old, enrolled or only reserved: a repair round asks for the writer,
/// whose pass deletes each table below its window, with its enrollment claim, and forgets it.
/// NEGATIVE CONTROL: a missing state read as an empty one (`retire` rebuilding only an unreadable
/// state) leaves day D-1's table on disk a day on (`left: [19999, 20000, 20001]`); a rebuild that
/// looks only 400 days either side of the clock leaves day D's whole window on disk 500 days on
/// (`left: [19999, 20000, 20001]`, `right: []`); a repair round that reads a missing state as an
/// empty one (`existing_ring_subscriptions` by `ring_state`) asks for no writer 500 days on
/// (`asks for the writer, day 500`).
#[test]
fn a_lost_ring_state_leaves_no_ring_storage_behind() {
    for (damage, later, kept) in [
        (None, D + 1, vec![D, D + 1]),
        (None, D + 500, vec![]),
        (Some(&b"ATRS1-not-a-state"[..]), D + 500, vec![]),
    ] {
        let lab = Lab::new(&[Role::Writer]);
        with_writer(&lab, |session, directory| {
            directory
                .enroll_ring(session, &CLAIMS, D - 1, at(D, 60))
                .unwrap();
            adopt_window(session, directory, D);
            assert_eq!(dirs(directory, D - 1..=D + 1), vec![D - 1, D, D + 1]);
            let claims = claim_dir(session).unwrap();
            let claim = claims.join(claim_name(CLAIMS.scope(directory.trust, D - 1).unwrap()));
            assert!(claim.exists());
            match damage {
                None => std::fs::remove_file(claims.join(CLAIMS.state_name())).unwrap(),
                Some(bytes) => damage_state(session, bytes),
            }
            let existing =
                directory.existing_ring_subscriptions(session, &Policy::default(), at(later, 60));
            assert!(
                !matches!(existing, Ok(Some(_))),
                "asks for the writer, day {}",
                later - D
            );
            let state = directory.retire(session, &CLAIMS, at(later, 60)).unwrap();
            assert_eq!(dirs(directory, D - 1..=D + 1), kept, "day {}", later - D);
            assert!(!claim.exists(), "day {}", later - D);
            assert!(state.held.iter().all(|day| *day >= state.floor));
            assert_eq!(directory.ring_state(session, &CLAIMS).unwrap(), state);
        });
    }
}

/// A retirement cut short after it deleted a period's table storage, before its enrollment
/// claim, leaves the claim alone to hold the period: a ring state lost then is rebuilt holding
/// it from the claim, and the next writer pass deletes the claim.
/// NEGATIVE CONTROL: a rebuild that reads the claims folder for `table-<hex>` names (4f9aaa2144)
/// decodes no claim, holds no period, and leaves the claim behind: `claim left, day 500`.
#[test]
fn a_lost_ring_state_rebuilds_a_period_held_by_its_claim_alone() {
    let lab = Lab::new(&[Role::Writer]);
    with_writer(&lab, |session, directory| {
        directory
            .enroll_ring(session, &CLAIMS, D - 1, at(D, 60))
            .unwrap();
        adopt_window(session, directory, D);
        let scope = CLAIMS.scope(directory.trust, D - 1).unwrap();
        std::fs::remove_dir_all(directory.path(scope).unwrap()).unwrap();
        let claims = claim_dir(session).unwrap();
        let claim = claims.join(claim_name(scope));
        assert!(claim.exists());
        std::fs::remove_file(claims.join(CLAIMS.state_name())).unwrap();
        let rebuilt = directory
            .rebuild_ring_state(session, &CLAIMS, at(D, 60))
            .unwrap();
        assert!(rebuilt.held.contains(&(D - 1)), "held: {:?}", rebuilt.held);
        directory.retire(session, &CLAIMS, at(D + 500, 60)).unwrap();
        assert!(!claim.exists(), "claim left, day 500");
    });
}

/// A writer pass on a clock three days ahead raises the floor to day D+2 and deletes day D's
/// window. Back on day D, the window is the clock's again: none of its tables is held yet, a
/// peer's offer of days D-1 to D+1 is taken up for reading, the tables at and above the floor stay
/// for the clock to come back to, and no write goes below the floor.
/// NEGATIVE CONTROL: a window that never comes below the floor (`RingState::current` as
/// `clock.max(floor + 1)`) stays three days ahead, its tables all held:
/// `assertion failed: tables.is_empty()`.
#[test]
fn a_clock_back_from_running_ahead_takes_its_window_back() {
    let lab = Lab::new(&[Role::Writer]);
    let policy = Policy::default();
    with_writer(&lab, |session, directory| {
        adopt_window(session, directory, D);
        adopt_window(session, directory, D + 3);
        assert_eq!(dirs(directory, D - 3..=D + 6), vec![D + 2, D + 3, D + 4]);
        let back = at(D, 120);
        let tables = directory
            .existing_ring_subscriptions(session, &policy, back)
            .unwrap()
            .unwrap();
        assert!(tables.is_empty());
        adopt_window(session, directory, D);
        let six = vec![D - 1, D, D + 1, D + 2, D + 3, D + 4];
        assert_eq!(dirs(directory, D - 3..=D + 6), six);
        let state = directory.ring_state(session, &CLAIMS).unwrap();
        assert_eq!(state.floor, D + 2);
        assert_eq!(state.held, six.iter().copied().collect::<BTreeSet<_>>());
        let day = CLAIMS.scope(directory.trust, D).unwrap().digest();
        assert!(directory.permits(session, &policy, day, back).unwrap());
        assert!(directory.enroll_ring(session, &CLAIMS, D, back).is_err());
        let tables = directory
            .existing_ring_subscriptions(session, &policy, back)
            .unwrap()
            .unwrap();
        assert_eq!(tables.len(), 3);
    });
}

/// Storage another Mac's tables fill up to the catalog's capacity (the point where a table
/// declared here waits on `Capacity`) leaves the claims ring its slots: this Mac still takes up
/// a day table on each of four days, and the catalog still refuses a table of its own.
/// NEGATIVE CONTROL: no slots kept for the rings (`CATALOG_SLOTS` as `MAX_TABLES`) lets the
/// catalog's tables take every slot, and the first day table finds none:
/// `` called `Result::unwrap()` on an `Err` value: Error("retained table storage limit reached") ``.
#[test]
fn a_full_catalog_leaves_the_claims_ring_its_slots() {
    let lab = Lab::new(&[Role::Writer]);
    let policy = Policy::default();
    with_writer(&lab, |session, directory| {
        let waiting = Descriptor::fresh(directory.trust, "waiting", "data", 1).unwrap();
        let mut catalog = directory.catalog(session).unwrap();
        catalog
            .commit_catalog(session, &waiting.encode().unwrap())
            .unwrap();
        drop(catalog);
        let state = || directory.snapshot(session).unwrap().entries[&waiting.scope.digest()].state;
        let mut filled = 0;
        while state() == State::Subscribed {
            assert!(
                filled < MAX_TABLES,
                "the catalog never reached its capacity"
            );
            let scope = Scope::fresh(directory.trust).unwrap();
            Store::join(session, &directory.path(scope).unwrap(), scope).unwrap();
            filled += 1;
        }
        for day in D..=D + 3 {
            let now = at(day, 60);
            directory.ring_subscriptions(session, &policy, now).unwrap();
            directory.enroll_ring(session, &CLAIMS, day, now).unwrap();
        }
        assert_eq!(dirs(directory, D..=D + 3), vec![D + 2, D + 3]);
        let own = Descriptor::fresh(directory.trust, "own", "data", 1).unwrap();
        assert!(directory.publish(session, &own).is_err());
    });
}

/// The journal's week ring (`JOURNAL`, lab model design §11 step 4) has its own slots too: with
/// the catalog's last shared slot assigned (a declared table awaiting storage) and the registry in
/// its reserved slot, this Mac still appends to three weeks' journal tables (keeping the two its
/// window still holds) and takes up day tables of the claims ring, and the declared table keeps
/// its slot, since ring storage counts against none of the catalog's; a further declaration still
/// waits on `Capacity`.
/// NEGATIVE CONTROL: with `JOURNAL` left out of `RINGS` (its storage then counted against the
/// catalog's slots), the declared table loses its slot: `` left: Capacity, right: Subscribed ``.
#[test]
fn the_journal_ring_has_its_own_slots_beside_a_full_catalog_and_the_registry() {
    const WEEK: u64 = JOURNAL.period_s;
    let lab = Lab::new(&[Role::Writer]);
    let policy = Policy::default();
    with_writer(&lab, |session, directory| {
        let waiting = Descriptor::fresh(directory.trust, "waiting", "data", 1).unwrap();
        let mut catalog = directory.catalog(session).unwrap();
        catalog
            .commit_catalog(session, &waiting.encode().unwrap())
            .unwrap();
        drop(catalog);
        for _ in 1..CATALOG_SLOTS {
            let scope = Scope::fresh(directory.trust).unwrap();
            Store::join(session, &directory.path(scope).unwrap(), scope).unwrap();
        }
        let registry = super::super::reserved(directory.trust)[0].clone();
        directory.publish(session, &registry).unwrap();
        let state = |descriptor: &Descriptor| {
            directory.snapshot(session).unwrap().entries[&descriptor.scope.digest()].state
        };
        assert_eq!(state(&waiting), State::Subscribed);
        let first = D / 7;
        for week in first..first + 3 {
            let now = week * WEEK + 60;
            directory.ring_subscriptions(session, &policy, now).unwrap();
            let (placed, _) = directory
                .append_journal(session, now, now, &[7; 64])
                .unwrap()
                .unwrap();
            assert_eq!(placed, week);
            let day = CLAIMS.epoch_of(now);
            directory.enroll_ring(session, &CLAIMS, day, now).unwrap();
        }
        // The first week lies below the window by the third and is deleted.
        let held = directory.ring_state(session, &JOURNAL).unwrap().held;
        assert_eq!(held, BTreeSet::from([first + 1, first + 2]));
        assert_eq!(state(&waiting), State::Subscribed);
        assert_eq!(state(&registry), State::Subscribed);
        let later = Descriptor::fresh(directory.trust, "later", "data", 1).unwrap();
        let mut catalog = directory.catalog(session).unwrap();
        catalog
            .commit_catalog(session, &later.encode().unwrap())
            .unwrap();
        drop(catalog);
        assert_eq!(state(&later), State::Capacity);
        assert!(directory.publish(session, &later).is_err());
    });
}

/// A Mac whose retained storage already fills every slot (an older build's catalog tables past
/// the shared slots) keeps its journal local: the append is kept as `Kept::NoSlot`, and neither
/// writes nor creates a table, where an enrollment would fail.
/// NEGATIVE CONTROL: without the storage check in `Directory::append_within`, the enrollment
/// fails the append: `` called `Result::unwrap()` on an `Err` value: Error("retained table
/// storage limit reached") ``.
#[test]
fn a_journal_append_with_no_storage_slot_is_kept_local() {
    let lab = Lab::new(&[Role::Writer]);
    with_writer(&lab, |session, directory| {
        directory.catalog(session).unwrap();
        while std::fs::read_dir(&directory.root).unwrap().count() <= MAX_TABLES {
            let scope = Scope::fresh(directory.trust).unwrap();
            Store::join(session, &directory.path(scope).unwrap(), scope).unwrap();
        }
        let now = (D / 7) * JOURNAL.period_s + 60;
        let kept = directory
            .append_journal(session, now, now, &[7; 64])
            .unwrap()
            .unwrap_err();
        assert_eq!(kept, Kept::NoSlot);
        let week = JOURNAL
            .scope(directory.trust, JOURNAL.epoch_of(now))
            .unwrap();
        assert!(!directory.path(week).unwrap().exists());
        assert_eq!(
            std::fs::read_dir(&directory.root).unwrap().count(),
            MAX_TABLES + 1
        );
    });
}

#[test]
fn adopt_ring_tables_skips_excess_periods_when_held_reaches_max_held() {
    let lab = Lab::new(&[Role::Writer]);
    let policy = Policy::default();
    with_writer(&lab, |session, directory| {
        // Pre-populate MAX_HELD - 1 periods at or above floor (D + 2..D + 2 + MAX_HELD - 1),
        // then step the clock back to D where the window (D - 1..=D + 1) offers 3 periods.
        let floor = D + 2;
        let held: BTreeSet<u64> = (floor..floor + (MAX_HELD as u64 - 1)).collect();
        let initial = RingState { floor, held };
        directory
            .save_ring_state(session, &CLAIMS, &initial)
            .unwrap();

        let back = at(D, 60);
        let offered = ids(directory, D - 1..=D + 1);
        let adopted = directory
            .adopt_ring_tables(session, &policy, back, &offered)
            .unwrap();
        assert_eq!(
            adopted.len(),
            1,
            "only the 1 remaining MAX_HELD slot is adopted without failing"
        );
        let after = directory.ring_state(session, &CLAIMS).unwrap();
        assert_eq!(after.held.len(), MAX_HELD);

        // A subsequent offer while at MAX_HELD skips cleanly without error.
        let second = directory
            .adopt_ring_tables(session, &policy, back, &offered)
            .unwrap();
        assert!(second.is_empty());
    });
}

fn own_table(bootstrap: &Bootstrap) -> Result<Descriptor> {
    Descriptor::application(bootstrap.trust(), "ring-test", "own", 1)
}

fn merge() -> Result<Vec<u8>> {
    Command::Merge {
        request: crate::crypto::random()?,
    }
    .encode()
}

/// A writer's own append names only its own previous record, never a peer's head: no merge is
/// spent and a peer's head never holds it up, where a catalog commit names every head.
/// NEGATIVE CONTROL: with `Store::commit_own` passing the projection's heads as parents (the
/// `commit_catalog_with_merges` shape), the append names Mac 2's record and this fails.
#[test]
fn an_own_append_names_only_its_own_predecessor() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let write = |n: usize| {
        application::with_store(&lab.paths[n], true, own_table, |session, store, _| {
            application::append(session, store, &merge()?).map(|receipt| receipt.id)
        })
        .unwrap()
    };
    let first = write(0);
    let peer = write(1);
    repair_commons(&lab, 4);
    application::with_store(&lab.paths[0], true, own_table, |session, store, _| {
        let receipt = store.commit_own(session, &merge()?)?;
        let records = store.catalog(session)?;
        assert!(
            records.iter().any(|record| record.header().id == peer),
            "Mac 2's record is a head here"
        );
        let header = records
            .iter()
            .map(|record| record.header())
            .find(|header| header.id == receipt.id)
            .unwrap();
        assert_eq!(header.previous, Some(first));
        assert_eq!(header.parents, [first]);
        Ok(())
    })
    .unwrap();
}

/// A writer that fills its record share spills into the next period's table, is refused once
/// every window table is full for it, and has room again once the clock reaches the next period.
/// NEGATIVE CONTROLS: with `Shares::full` ignoring the writer's record share, every append lands
/// in day D and this fails; with `Directory::has_room` reading a storage root not made yet (this
/// Mac's first table), the first append fails:
/// `` called `Result::unwrap()` on an `Err` value: Error("No such file or directory (os error 2)") ``.
#[test]
fn a_full_record_share_spills_into_the_next_period() {
    let lab = Lab::new(&[Role::Writer]);
    let shares = Shares {
        records: 100,
        bytes: usize::MAX,
        writer_records: 2,
        writer_bytes: usize::MAX,
    };
    with_writer(&lab, |session, directory| {
        let now = at(D, 100);
        let append =
            |now: u64, n: u8| directory.append_own(session, &CLAIMS, shares, now, now, &[n; 64]);
        let placed: Vec<u64> = (0..4).map(|n| append(now, n).unwrap().0).collect();
        assert_eq!(placed, [D, D, D + 1, D + 1]);
        assert_eq!(
            append(now, 4).unwrap_err().to_string(),
            Unplaced::Full.reason(),
            "both of this writer's tables are full"
        );
        assert_eq!(append(at(D + 1, 0), 5).unwrap().0, D + 2);
    });
}

/// A byte share spills the same way: a writer whose next record could pass its bytes goes to
/// the next period with records to spare.
/// NEGATIVE CONTROL: with `Shares::full` ignoring the writer's byte share, the second append
/// stays in day D and this fails.
#[test]
fn a_full_byte_share_spills_into_the_next_period() {
    let lab = Lab::new(&[Role::Writer]);
    // A 1,000-byte row's bound (its bytes and 4 KiB of envelope) fits 6,000 bytes once; its
    // sealed record takes more than 1,000 of them, so a second bound does not fit beside it.
    let shares = Shares {
        records: 100,
        bytes: usize::MAX,
        writer_records: 100,
        writer_bytes: 6_000,
    };
    with_writer(&lab, |session, directory| {
        let now = at(D, 100);
        let placed: Vec<u64> = (0..2)
            .map(|n| {
                directory
                    .append_own(session, &CLAIMS, shares, now, now, &[n; 1_000])
                    .unwrap()
                    .0
            })
            .collect();
        assert_eq!(placed, [D, D + 1]);
    });
}
