// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::{
    catalog::{
        PER_CREATOR,
        policy::Policy,
        ring::{self, RingState},
    },
    table::{
        store::{claim_dir, claim_name},
        tests::{Lab, repair_commons, repair_commons_among_first, repair_commons_at},
    },
};
use aterm_json::json;
use aterm_secrets::commons::roster::Role;
use std::{ops::RangeInclusive, time::Duration};

const DAY: u64 = EPOCH_S;
/// Day 20,000 since the Unix epoch, a fixed clock for the tests that set one.
const D: u64 = 20_000;

#[test]
#[cfg(unix)]
fn native_claims_write_waits_for_unchanged_custody() {
    native_claims_custody(true);
}

#[test]
#[cfg(unix)]
fn native_claims_own_table_waits_for_unchanged_custody() {
    native_claims_custody(false);
}

#[cfg(unix)]
fn native_claims_custody(writing: bool) {
    let lab = Lab::new(&[Role::Writer]);
    let now = at(D, 600);
    write_at(&lab.paths[0], &row(1, now), now, Caps::default()).unwrap();
    let held = aterm_uds::private_file::PrivateLock::take_with(
        &lab.paths[0].dir,
        "commons.lock",
        Duration::ZERO,
        "private custody held",
        || {},
    )
    .unwrap();
    let mut held = Some(held);
    let result = aterm_uds::file_lock::with_lock_wait_observer(
        move |_| {
            if let Some(held) = held.take() {
                drop(held);
            }
        },
        || {
            let started = Instant::now() - Duration::from_secs(3);
            if writing {
                write_started(&lab.paths[0], &row(2, now), now, Caps::default(), started)
            } else {
                own_table_started(
                    &lab.paths[0],
                    now,
                    started + Duration::from_secs(60),
                    started,
                )
                .map(|_| ())
            }
        },
    );
    assert!(result.is_ok(), "writing={writing}: {result:?}");
    let held = aterm_uds::private_file::PrivateLock::take_with(
        &lab.paths[0].dir,
        "commons.lock",
        Duration::ZERO,
        "private custody held",
        || {},
    )
    .unwrap();
    let started = Instant::now();
    assert!(
        own_table_started(
            &lab.paths[0],
            now,
            started - Duration::from_secs(1),
            started
        )
        .is_err()
    );
    assert!(
        write_started(
            &lab.paths[0],
            &row(3, now),
            now,
            Caps::default(),
            started - Duration::from_secs(61)
        )
        .is_err()
    );
    drop(held);
}

#[test]
#[cfg(unix)]
fn an_actual_claims_read_waits_for_unchanged_native_custody() {
    use std::os::unix::fs::MetadataExt as _;
    let lab = Lab::new(&[Role::Writer]);
    let expected = *lab.bootstrap(0).request().unwrap().binding().machine();
    let held = aterm_uds::private_file::PrivateLock::take_with(
        &lab.paths[0].dir,
        "commons.lock",
        Duration::ZERO,
        "private custody held",
        || {},
    )
    .unwrap();
    let named = std::fs::metadata(lab.paths[0].dir.join("commons.lock")).unwrap();
    let identity = (named.dev(), named.ino());
    let mut held = Some(held);
    let result = aterm_uds::file_lock::with_lock_wait_observer(
        move |file| {
            let actual = file.metadata().unwrap();
            assert_eq!((actual.dev(), actual.ino()), identity);
            if let Some(held) = held.take() {
                drop(held);
            }
        },
        || {
            read_started(
                &lab.paths[0],
                at(D, 600),
                Caps::default(),
                Instant::now() - Duration::from_secs(3),
            )
        },
    );
    let read = result.unwrap();
    assert_eq!(read.me, expected);
    assert!(read.writable.is_ok());
    let held = aterm_uds::private_file::PrivateLock::take_with(
        &lab.paths[0].dir,
        "commons.lock",
        Duration::ZERO,
        "private custody held",
        || {},
    )
    .unwrap();
    assert!(
        read_started(
            &lab.paths[0],
            at(D, 600),
            Caps::default(),
            Instant::now() - Duration::from_secs(61)
        )
        .is_err()
    );
    let still_held = aterm_uds::private_file::PrivateLock::take_with(
        &lab.paths[0].dir,
        "commons.lock",
        Duration::ZERO,
        "private custody held",
        || {},
    );
    assert!(still_held.is_err());
    drop(held);
}

/// `secs` into day `day`.
const fn at(day: u64, secs: u64) -> u64 {
    day * DAY + secs
}

/// A row lapsing a day after `now`, as the harness writes one.
fn row(counter: u64, now: u64) -> Value {
    json!({"counter": counter, "until": now + DAY, "claims": []})
}

fn counter(read: &Read, machine: &[u8; 16]) -> Option<u64> {
    read.rows
        .iter()
        .find(|(m, _)| m == machine)
        .and_then(|(_, body)| body["counter"].as_u64())
}

/// `rounds` rounds of repair naming day `day`'s claims table, each Mac on its clock in `now`, as a
/// push names the table that moved. A table both Macs of an exchange hold takes one round; one a
/// Mac must take up from the other takes a second, and a shortfall fails in the fixture
/// (`repair_commons_first`). The tests here assert rows, counters and writability in known day
/// tables, not how repair reached them.
/// GAP: the tests that repair through this see no ring-table scan order and no idle laps with a
/// clock set back. `repair_on_a_clock_set_back_a_day_never_resurrects_a_retired_epoch`,
/// `an_interrupted_day_table_enrollment_costs_nothing_and_resumes`,
/// `an_unrewritable_ring_state_is_moved_aside_and_claims_go_on` and
/// `a_damaged_ring_state_is_rebuilt_and_repair_goes_on` stay on the scan.
pub(super) fn repair_day(lab: &Lab, rounds: usize, day: u64, now: &[u64]) {
    let table = CLAIMS.scope(lab.bootstrap(0).trust(), day).unwrap();
    let members: Vec<usize> = (0..lab.paths.len()).collect();
    repair_commons_among_first(lab, rounds, &members, now, table.digest());
}

/// Run `f` in a session on `paths`, holding the control writer where `write`.
fn with_session<T>(
    paths: &Paths,
    write: bool,
    f: impl FnOnce(&Session<'_>, &Directory, &Bootstrap) -> T,
) -> T {
    let bootstrap = Bootstrap::required(paths).unwrap();
    let session = if write {
        Session::open(&bootstrap).unwrap()
    } else {
        Session::observe(&bootstrap).unwrap()
    };
    let directory = Directory::new(paths, bootstrap.trust());
    f(&session, &directory, &bootstrap)
}

/// The days among `days` whose claims table has storage here.
fn ring_dirs(paths: &Paths, days: RangeInclusive<u64>) -> Vec<u64> {
    with_session(paths, false, |_, directory, _| {
        days.filter(|day| {
            let scope = CLAIMS.scope(directory.trust(), *day).unwrap();
            directory.path(scope).unwrap().exists()
        })
        .collect()
    })
}

/// The days among `days` whose claims table has an enrollment claim here.
fn ring_claims(paths: &Paths, days: RangeInclusive<u64>) -> Vec<u64> {
    with_session(paths, false, |session, directory, _| {
        let dir = claim_dir(session).unwrap();
        days.filter(|day| {
            let scope = CLAIMS.scope(directory.trust(), *day).unwrap();
            dir.join(claim_name(scope)).exists()
        })
        .collect()
    })
}

fn ring_state(paths: &Paths) -> RingState {
    with_session(paths, false, |session, directory, _| {
        directory.ring_state(session, &CLAIMS).unwrap()
    })
}

fn catalog_records(paths: &Paths) -> usize {
    with_session(paths, false, |session, directory, _| {
        let scope = directory.catalog_scope();
        Store::observe_if_exists(session, &directory.path(scope).unwrap(), scope)
            .unwrap()
            .map_or(0, |store| store.inventory(session).unwrap().count())
    })
}

/// Records retained in each window table at `now`.
fn ring_counts(paths: &Paths, now: u64) -> Vec<(u64, usize)> {
    with_session(paths, false, |session, directory, _| {
        let state = directory.ring_state(session, &CLAIMS).unwrap();
        ring_logs(session, directory, &state, now)
            .unwrap()
            .into_iter()
            .map(|(source, log)| {
                let Source::Ring(epoch) = source else {
                    unreachable!("ring_logs reads ring tables")
                };
                (epoch, log.snapshot().retained)
            })
            .collect()
    })
}

fn retire_at(paths: &Paths, now: u64) {
    with_session(paths, true, |session, directory, _| {
        directory.retire(session, &CLAIMS, now).unwrap();
    });
}

/// Append `payload` to the claims table of `day` as `paths`'s writer, as any build could.
fn commit_ring(paths: &Paths, day: u64, now: u64, payload: &[u8]) {
    with_session(paths, true, |session, directory, _| {
        let mut store = directory.enroll_ring(session, &CLAIMS, day, now).unwrap();
        store.commit_catalog(session, payload).unwrap();
    });
}

/// When the first seal of the old table was signed, in whole seconds.
fn sealed_at(paths: &Paths) -> u64 {
    with_session(paths, false, |session, directory, bootstrap| {
        let first = legacy_descriptor(bootstrap, 1).unwrap();
        let twin = Descriptor {
            version: 2,
            ..first.clone()
        };
        directory.snapshot_if_exists(session).unwrap().entries[&first.scope.digest()]
            .declarations
            .iter()
            .filter(|d| d.descriptor == twin)
            .map(|d| d.authored_ms)
            .min()
            .unwrap()
            / 1000
    })
}

/// The read of an F19 build from before the ring (0d14d71c12): both its paths, the observation
/// and the declaring fallback, first require `accounts/claims/1` subscribed with only its own
/// declaration.
fn legacy_read(paths: &Paths) -> Result<Vec<([u8; 16], Value)>> {
    let bootstrap = Bootstrap::required(paths).map_err(crate::table::failure)?;
    let session = Session::observe(&bootstrap)?;
    let directory = Directory::new(paths, bootstrap.trust());
    let first = legacy_descriptor(&bootstrap, 1)?;
    let snapshot = directory.snapshot_existing(&session)?;
    let entry = snapshot
        .entries
        .get(&first.scope.digest())
        .ok_or_else(|| Error::new("application table is not initialized"))?;
    if entry.state != State::Subscribed || entry.declarations.iter().any(|d| d.descriptor != first)
    {
        return Err(Error::new(
            "application table has an incompatible or unavailable descriptor",
        ));
    }
    let store = Store::observe(&session, &directory.path(first.scope)?, first.scope)?;
    let log = Log::read_with(&session, &store, first.scope, decode)?;
    Ok(latest(&[(Source::Legacy(1), log)], &machines(&session)?)
        .into_values()
        .map(|latest| (latest.machine, latest.body))
        .collect())
}

/// That build's write: declare `accounts/claims/1`, then append the row.
fn legacy_write(paths: &Paths, body: &Value) -> Result<()> {
    let bootstrap = Bootstrap::required(paths).map_err(crate::table::failure)?;
    let session = Session::open(&bootstrap)?;
    let directory = Directory::new(paths, bootstrap.trust());
    let scope = directory.publish(&session, &legacy_descriptor(&bootstrap, 1)?)?;
    let machine = me(&session, &machines(&session)?)?;
    let mut store = Store::open(&session, &directory.path(scope)?, scope)?;
    store.commit_catalog(&session, &encode(&command(&machine, body)?)?)?;
    Ok(())
}

#[test]
fn each_mac_writes_its_own_row_and_every_member_reads_each_ones_latest() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    write(&lab.paths[0], &json!({"counter": 1, "claims": []})).unwrap();
    write(&lab.paths[0], &json!({"counter": 2, "claims": []})).unwrap();
    write(&lab.paths[1], &json!({"counter": 1, "claims": []})).unwrap();
    let (zero, one) = (read(&lab.paths[0]).unwrap(), read(&lab.paths[1]).unwrap());
    assert_ne!(zero.me, one.me);
    assert_eq!(zero.writable, Ok(()));
    assert_eq!(
        (counter(&zero, &zero.me), counter(&zero, &one.me)),
        (Some(2), None)
    );
    assert_eq!(
        (counter(&one, &one.me), counter(&one, &zero.me)),
        (Some(1), None)
    );

    // The rows cross: each Mac reads both, each Mac's latest.
    repair_commons(&lab, 6);
    for (n, paths) in lab.paths[..2].iter().enumerate() {
        let read = read(paths).unwrap();
        assert_eq!(counter(&read, &zero.me), Some(2), "Mac {n}");
        assert_eq!(counter(&read, &one.me), Some(1), "Mac {n}");
        assert_eq!(read.rows.len(), 2);
    }

    // The same row written twice is one record.
    write(&lab.paths[1], &json!({"counter": 1, "claims": []})).unwrap();
    assert_eq!(counter(&read(&lab.paths[1]).unwrap(), &one.me), Some(1));
}

#[test]
fn a_row_at_another_macs_key_is_inert() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let now = at(D, 60);
    write_at(&lab.paths[0], &row(1, now), now, Caps::default()).unwrap();
    write_at(&lab.paths[1], &row(1, now), now, Caps::default()).unwrap();
    repair_day(&lab, 1, D, &[now, now]);
    let zero = read_at(&lab.paths[0], now, Caps::default()).unwrap().me;
    let one = read_at(&lab.paths[1], now, Caps::default()).unwrap().me;
    // Mac 1 signs a row at Mac 0's key.
    let forged = command(&zero, &row(9, now)).unwrap();
    commit_ring(&lab.paths[1], D, now, &encode(&forged).unwrap());
    repair_day(&lab, 1, D, &[now, now]);
    // Inert only means something once the forged record is here, beside each Mac's own row.
    assert_eq!(ring_counts(&lab.paths[0], now), vec![(D, 3)]);
    for (n, paths) in lab.paths[..2].iter().enumerate() {
        let read = read_at(paths, now, Caps::default()).unwrap();
        assert_eq!(counter(&read, &zero), Some(1), "Mac {n}");
        // A forged row that counted would replace its signer's own: rows go by signer.
        assert_eq!(counter(&read, &one), Some(1), "Mac {n}");
    }
}

/// After a Commons writer succession this Mac's earlier seat stays a member, and the last row it
/// signed stays in the day table it went into until it lapses. To this Mac that row is its own
/// past, never another Mac's claim: its read leaves it out of the members' rows, so the harness
/// never records this Mac's own home accounts as held by the earlier seat's machine, and the
/// current seat's first row starts at 1 and stands. The read gives that row apart
/// (`Read::former`), so the claims this Mac still uses keep their `since` in the current seat's
/// first row (the harness's `a_writer_successor_keeps_the_seniority_of_the_claims_it_still_uses`).
/// A peer's row is read as before, and is never the former one.
///
/// Negative controls: with the read keeping every member's latest row, the earlier seat's row
/// came back beside the peer's (`left: Some(1)`, `right: None`: `the earlier seat's row read as
/// another Mac's`). With no former row given (2026-11-13), the earlier seat's claims were lost to
/// this Mac (`the earlier seat's row is given apart`, `left: None`).
#[test]
#[cfg(unix)]
fn a_writer_successor_reads_no_row_of_its_earlier_seat() {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let now = at(D, 600);
    let caps = Caps::default();
    let machine = |n: usize| *lab.bootstrap(n).request().unwrap().binding().machine();
    let (earlier, peer) = (machine(0), machine(2));
    let used = json!({"counter": 1, "until": now + DAY, "claims": [
        {"pool": "claude", "email": "a@example.test", "kind": "lease", "since": now - 600}
    ]});
    write_at(&lab.paths[0], &used, now, caps).unwrap();
    write_at(&lab.paths[2], &row(1, now), now, caps).unwrap();
    let day = CLAIMS.scope(lab.bootstrap(0).trust(), D).unwrap().digest();
    repair_commons_among_first(&lab, 1, &[0, 2], &[now; 3], day);
    let before = read_at(&lab.paths[0], now, caps).unwrap();
    assert_eq!(
        (counter(&before, &earlier), counter(&before, &peer)),
        (Some(1), Some(1)),
        "PRECONDITION: the earlier seat's row and a peer's"
    );
    assert_eq!(
        before.former, None,
        "PRECONDITION: a home that succeeds none"
    );
    let next = crate::generation::test_complete_successor(
        lab._home.path(),
        &lab.paths[0],
        &lab.paths[1],
        &lab.paths[0].home.join("config.toml"),
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    let current = *Bootstrap::required(&next)
        .unwrap()
        .request()
        .unwrap()
        .binding()
        .machine();
    let read = read_at(&lab.paths[0], now, caps).unwrap();
    assert_eq!(read.me, current);
    assert_eq!(
        counter(&read, &earlier),
        None,
        "the earlier seat's row read as another Mac's"
    );
    assert_eq!(counter(&read, &peer), Some(1), "a peer's row is read");
    assert_eq!(
        read.former.as_ref(),
        Some(&used),
        "the earlier seat's row is given apart"
    );
    write_at(&lab.paths[0], &row(1, now), now, caps).unwrap();
    let read = read_at(&lab.paths[0], now, caps).unwrap();
    assert_eq!(counter(&read, &current), Some(1));
    assert_eq!(counter(&read, &earlier), None);
    assert_eq!(read.rows.len(), 2);
    assert_eq!(read.former.as_ref(), Some(&used));
}

/// A claim record has one wire spelling: the same command with a space after its opening brace
/// parses, and is refused.
/// NEGATIVE CONTROL: a decoder without its canonical check (e1083090a5) takes the respelled
/// payload: `a respelled claim decodes`.
#[test]
fn a_respelled_claim_record_is_refused() {
    let canonical = encode(&command(&[7; 16], &row(1, at(D, 60))).unwrap()).unwrap();
    assert!(decode(&canonical).is_ok());
    assert_eq!(canonical.first(), Some(&b'{'));
    let mut respelled = vec![b'{', b' '];
    respelled.extend_from_slice(&canonical[1..]);
    assert_eq!(
        Command::decode(&respelled).unwrap(),
        Command::decode(&canonical).unwrap()
    );
    assert!(decode(&respelled).is_err(), "a respelled claim decodes");
}

#[test]
fn a_reader_reads_rows_and_writes_none() {
    let lab = Lab::new(&[Role::Writer, Role::Reader]);
    write(&lab.paths[0], &json!({"counter": 1})).unwrap();
    assert!(write(&lab.paths[1], &json!({"counter": 1})).is_err());
    assert!(write(&lab.paths[0], &json!(["not", "a", "row"])).is_err());
    let large = json!({"counter": 2, "claims": [], "pad": "x".repeat(MAX_ROW - 256)});
    write(&lab.paths[0], &large).unwrap();
    let big = "x".repeat(MAX_ROW);
    assert!(write(&lab.paths[0], &json!({"pad": big})).is_err());
    repair_commons(&lab, 6);
    let reader = read(&lab.paths[1]).unwrap();
    assert!(reader.writable.is_err());
    assert_eq!(reader.rows.len(), 1);
    assert_eq!(reader.rows[0].1, large);
}

#[test]
fn a_stale_whole_row_cannot_replace_another_local_clients_claims() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    // Two clients read the same empty table before either publishes its whole set.
    assert!(read(paths).unwrap().rows.is_empty());
    assert!(read(paths).unwrap().rows.is_empty());
    let first = json!({"counter": 1, "claims": ["first@example.invalid"]});
    let stale = json!({"counter": 1, "claims": ["second@example.invalid"]});
    write(paths, &first).unwrap();
    write(paths, &first).unwrap(); // A lost reply may retry the current row.
    assert!(write(paths, &stale).is_err());
    let current = read(paths).unwrap();
    assert_eq!(current.rows, vec![(current.me, first.clone())]);

    let skipped = json!({"counter": 3, "claims": ["second@example.invalid"]});
    assert!(write(paths, &skipped).is_err());
    let next = json!({"counter": 2, "claims": ["first@example.invalid", "second@example.invalid"]});
    write(paths, &next).unwrap();
    // An old request's durable receipt must not acknowledge a superseded claim set.
    assert!(write(paths, &first).is_err());
    let current = read(paths).unwrap();
    assert_eq!(current.rows, vec![(current.me, next)]);
}

#[test]
fn malformed_or_exhausted_counters_cannot_replace_retained_claims() {
    let lab = Lab::new(&[Role::Writer]);
    for body in [
        json!({"claims": []}),
        json!({"counter": 0}),
        json!({"counter": -1}),
        json!({"counter": "1"}),
        json!({"counter": 2}),
        json!({"counter": u64::MAX}),
    ] {
        assert!(write(&lab.paths[0], &body).is_err());
        assert!(read(&lab.paths[0]).unwrap().rows.is_empty());
    }
    write(&lab.paths[0], &json!({"counter": 1})).unwrap();

    let now = at(D, 60);
    for held in [json!({"claims": []}), json!({"counter": u64::MAX})] {
        let lab = Lab::new(&[Role::Writer]);
        let paths = &lab.paths[0];
        // Retained signed data need not have used this build's write API.
        let machine = read_at(paths, now, Caps::default()).unwrap().me;
        commit_ring(
            paths,
            D,
            now,
            &encode(&command(&machine, &held).unwrap()).unwrap(),
        );
        for body in [json!({"counter": 1}), json!({"counter": u64::MAX})] {
            // An exact retry of an already retained, valid maximum counter is harmless.
            let result = write_at(paths, &body, now, Caps::default());
            assert_eq!(result.is_ok(), body == held);
        }
        let current = read_at(paths, now, Caps::default()).unwrap();
        assert_eq!(current.rows, vec![(current.me, held)]);
    }
}

pub(crate) fn publication_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    publication_transition_with(state, action, Caps::default())
}

fn publication_transition_with(
    state: &aterm_spec::interp::State,
    action: &str,
    caps: Caps,
) -> aterm_spec::interp::State {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let now = at(D, 60);
    let first = json!({"counter": 1, "tag": 1, "claims": ["first@example.invalid"]});
    let next = json!({"counter": 2, "tag": 2, "claims": ["first@example.invalid", "second@example.invalid"]});
    if state["current"] >= 1 {
        write_at(paths, &first, now, caps).unwrap();
    }
    if state["current"] == 2 {
        write_at(paths, &next, now, caps).unwrap();
    }
    let (phase, body) = match action {
        "Initialize" => (1, first),
        "Retry" => (2, first),
        "Stale" => (
            3,
            json!({"counter": 1, "tag": 2, "claims": ["second@example.invalid"]}),
        ),
        "Advance" => (4, next),
        "SupersededRetry" => (5, first),
        "Skip" => (
            6,
            json!({"counter": 3, "tag": 2, "claims": ["second@example.invalid"]}),
        ),
        _ => panic!("unknown claim publication action"),
    };
    let accepted = write_at(paths, &body, now, caps).is_ok();
    let current = read_at(paths, now, caps).unwrap().rows[0].1["tag"]
        .as_i64()
        .unwrap();
    let count: usize = ring_counts(paths, now).iter().map(|(_, count)| count).sum();
    let mut output = state.clone();
    output.insert("phase", phase);
    output.insert("accepted", i64::from(accepted));
    output.insert("current", current);
    output.insert("count", i64::try_from(count).unwrap());
    output
}

/// Every claim publication transition matches the model under the default day-table caps and
/// under caps of one row, where the second revision spills into the next day's table.
/// NEGATIVE CONTROL: a stale replacement that was accepted breaks `NoStaleReplacement` and is no
/// model successor.
#[test]
fn claim_publication_conforms_and_unsafe_replacements_fail() {
    let model = aterm_models_db::claim_publication_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                for caps in [
                    Caps::default(),
                    Caps {
                        records: 1,
                        writer_records: 1,
                    },
                ] {
                    let next = publication_transition_with(&state, action.name, caps);
                    assert!(model.successors(action.name, &state).contains(&next));
                }
            }
        }
    }
    let before = publication_transition(&model.init_state(), "Initialize");
    let mut unsafe_write = publication_transition(&before, "Stale");
    unsafe_write.insert("accepted", 1);
    unsafe_write.insert("current", 2);
    unsafe_write.insert("count", 2);
    assert!(!model.check_invariant("NoStaleReplacement", &unsafe_write));
    assert!(!model.successors("Stale", &before).contains(&unsafe_write));
}

/// The code's step for each `DbClaimRing` action: the model's clock runs two ticks a day, its
/// days are the ring's, its row lapses a day after it is written (the harness's horizon), and its
/// observers (`lost`, `gone`, `reentered`, `blind`) read what the code deleted, held, wrote into
/// and read before it wrote.
pub(crate) fn ring_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    ring_step(
        &CLAIMS,
        |ring, now, own, until| target(ring, now, own, until, |_| false).unwrap(),
        state,
        action,
    )
}

/// [`ring_transition`] for any ring of period tables (`periods`): the model's day is one of its
/// periods, its row lapses a period after it is written, and `place` puts the row
/// (`ring::place` under that ring's errors).
fn ring_step(
    periods: &ring::Ring,
    place: impl Fn(&RingState, u64, Option<u64>, u64) -> u64,
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    const HELD: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];
    let period = periods.period_s;
    let half = period / 2;
    let get = |name: &str| u64::try_from(state[name]).unwrap();
    let int = |value: u64| i64::try_from(value).unwrap();
    let mut next = state.clone();
    let mut ring = RingState {
        floor: get("floor"),
        held: (1..)
            .zip(HELD)
            .filter(|(_, name)| get(name) == 1)
            .map(|(day, _)| day)
            .collect(),
    };
    let mut clock = get("day") * period + get("tick") * half;
    let retire = |ring: &mut RingState, next: &mut aterm_spec::interp::State, clock: u64| {
        let window = ring.window(periods, clock);
        let doomed = ring.ratchet(periods, clock);
        ring.held.retain(|day| !doomed.contains(day));
        let row = get("row");
        // The row goes with its table: below the window, or above it and below the floor. A live
        // row's table deleted below the window is lost; a row above the window at or above the
        // floor stays, out of sight, until the clock comes back to its table.
        if row > 0 && row < ring.floor && !window.contains(&row) {
            if row < *window.start() && get("until") * half > clock {
                next.insert("lost", 1);
            }
            next.insert("row", 0);
            next.insert("until", 0);
        }
        if let Some(last) = doomed.iter().max() {
            next.insert("gone", int(get("gone").max(last + 1)));
        }
    };
    match action {
        "Tick" => {
            clock += half;
            next.insert("tick", int(clock % period / half));
            next.insert("day", int(clock / period));
        }
        "ClockBack" | "ClockCorrected" => {
            clock -= period;
            next.insert("day", int(clock / period));
        }
        "Retire" => retire(&mut ring, &mut next, clock),
        "Enroll" => {
            retire(&mut ring, &mut next, clock);
            for day in ring.window(periods, clock) {
                ring.hold(periods, day, clock).unwrap();
            }
        }
        "Write" => {
            retire(&mut ring, &mut next, clock);
            // The writer's own latest row, where the window it reads holds it.
            let window = ring.window(periods, clock);
            let own = u64::try_from(next["row"]).unwrap();
            if own > *window.end() {
                next.insert("blind", 1);
            }
            let own = (own > 0 && window.contains(&own)).then_some(own);
            let until = clock + period;
            let day = place(&ring, clock, own, until);
            ring.hold(periods, day, clock).unwrap();
            if day < get("gone") {
                next.insert("reentered", 1);
            }
            next.insert("row", int(day));
            next.insert("until", int(until / half));
        }
        _ => panic!("unknown claim ring action"),
    }
    next.insert("floor", int(ring.floor));
    next.insert("e", int(ring.current(periods, clock)));
    for (day, name) in (1..).zip(HELD) {
        next.insert(name, i64::from(ring.held.contains(&day)));
    }
    next
}

/// Every `DbClaimRing` step the code takes is a model successor, over every reachable state.
/// NEGATIVE CONTROL: a hand-built retirement that deletes a live row's table breaks
/// `NoLiveRowRetired` and is no model successor. A projection that drops this Mac's row above the
/// window, whose table the code keeps (`ring_transition` without `row < ring.floor`), is no
/// successor either: `Retire from {.., "floor": 3, .., "h4": 1, .., "row": 4, ..} steps to
/// {.., "row": 0, ..}`.
#[test]
fn claim_ring_conforms() {
    let model = aterm_models_db::claim_ring_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut live = None;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = ring_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{} from {state:?} steps to {next:?}",
                    action.name
                );
            }
        }
        if live.is_none() && state["row"] > 0 && state["until"] > state["day"] * 2 + state["tick"] {
            live = Some(state);
        }
    }
    let live = live.expect("a reachable state holds a live row");
    let mut lost = ring_transition(&live, "Retire");
    lost.insert("row", 0);
    lost.insert("until", 0);
    lost.insert("lost", 1);
    assert!(!model.check_invariant("NoLiveRowRetired", &lost));
    assert!(!model.successors("Retire", &live).contains(&lost));
}

/// The `DbClaimRing` model holds the journal's week ring too (lab model design §11 step 4): every
/// step the code takes on `ring::JOURNAL`, whose period is 604,800 seconds, is a model successor
/// over every reachable state, the model's day being a week.
/// NEGATIVE CONTROL: with the Write step placed on the day ring (`ring::place(&CLAIMS, ..)` in
/// place of `JOURNAL`), a row that lives a week lapses past the day ring's window:
/// `` called `Result::unwrap()` on an `Err` value: Beyond ``.
#[test]
fn journal_ring_conforms() {
    let model = aterm_models_db::claim_ring_model();
    let journal = |state: &aterm_spec::interp::State, action: &str| {
        ring_step(
            &ring::JOURNAL,
            |state, now, own, until| {
                ring::place(&ring::JOURNAL, state, now, own, until, |_| false).unwrap()
            },
            state,
            action,
        )
    };
    let mut steps = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = journal(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{} from {state:?} steps to {next:?}",
                    action.name
                );
                steps += 1;
            }
        }
    }
    assert!(steps > 1);
}

/// An F19 build that rolled past its first table declared `claims.2` and up; a ring Mac's write
/// seals every generation it finds declared, so none stays subscribed and takes a catalog slot on
/// a new Mac, and declares nothing for a generation nobody declared.
/// NEGATIVE CONTROL: sealing generation 1 alone (`write_at`'s loop over `1..=1`) leaves
/// `claims.2` subscribed: `` left: Subscribed, right: Conflict ``.
#[test]
fn every_rolled_legacy_generation_is_sealed() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (old, new) = (&lab.paths[0], &lab.paths[1]);
    legacy_write(old, &json!({"counter": 1, "claims": []})).unwrap();
    with_session(old, true, |session, directory, bootstrap| {
        for generation in [2, 3] {
            directory
                .publish(session, &legacy_descriptor(bootstrap, generation).unwrap())
                .unwrap();
        }
    });
    repair_commons(&lab, 6);
    write(new, &json!({"counter": 1, "claims": []})).unwrap();
    with_session(new, true, |session, directory, bootstrap| {
        let snapshot = directory.snapshot(session).unwrap();
        for generation in 1..=3 {
            let retired = legacy_descriptor(bootstrap, generation).unwrap();
            assert_eq!(
                snapshot.entries[&retired.scope.digest()].state,
                State::Conflict,
                "claims.{generation}"
            );
        }
        let never = legacy_descriptor(bootstrap, 4).unwrap();
        assert!(!snapshot.entries.contains_key(&never.scope.digest()));
    });
}

/// The owner's swarm in miniature: 16 days of writes, three on even days and one on odd days, into
/// day tables of two rows each, so each even day spills into the next day's table. Every write
/// lands, the row reads back writable, at most four day tables and four enrollment claims exist,
/// and the catalog holds no record. The window saturates by about day 5, and after that every even
/// or odd day behaves alike; every time constant on this path, and the ring's `MAX_HELD` of 8, lies
/// within the 16.
/// GAP: this run never reaches the 63-table catalog limit itself. A slow leak that needs 17 or more
/// days to show and that the day-table, claim and catalog counts asserted each day cannot see is
/// not seen; the counts would catch any leak of those long before 63.
/// NEGATIVE CONTROL: with retirement off (`Directory::retire` returning before it deletes), the
/// day tables pile up past four on disk.
#[test]
fn a_year_of_daily_writes_never_reaches_capacity() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let caps = Caps {
        records: 2,
        writer_records: 2,
    };
    let mut next = 1;
    for day in D..D + 16 {
        let mut last = None;
        for hour in 1..=if day % 2 == 0 { 3 } else { 1 } {
            let now = at(day, hour * 3600);
            write_at(paths, &row(next, now), now, caps)
                .unwrap_or_else(|why| panic!("day {} write {hour}: {why}", day - D));
            last = Some(read_at(paths, now, caps).unwrap());
            next += 1;
        }
        let read = last.unwrap();
        assert_eq!(counter(&read, &read.me), Some(next - 1), "day {}", day - D);
        assert_eq!(read.writable, Ok(()), "day {}", day - D);
        assert!(
            ring_dirs(paths, D - 1..=D + 17).len() <= 4,
            "day {}",
            day - D
        );
        assert!(
            ring_claims(paths, D - 1..=D + 17).len() <= 4,
            "day {}",
            day - D
        );
        assert_eq!(catalog_records(paths), 0, "day {}", day - D);
    }
}

/// A row lands in a table no clock deletes while the row is live: one written in a day's last
/// second, lapsing a day later (the harness's horizon) or as late as the window allows, reads back
/// until it lapses while its writer's tables retire around it.
/// NEGATIVE CONTROL: a target that ignores the row's `until` (`target` without its
/// `epoch_of(until) - 1` term) puts the late row in that day's table, which the retirement two
/// days on deletes while the row is still live.
#[test]
fn no_live_row_is_in_a_retired_epoch() {
    for horizon in [DAY, 2 * DAY - 1] {
        let lab = Lab::new(&[Role::Writer]);
        let paths = &lab.paths[0];
        let written = at(D + 1, 0) - 1;
        let until = written + horizon;
        let body = json!({"counter": 1, "until": until, "claims": []});
        write_at(paths, &body, written, Caps::default()).unwrap();
        for now in [
            at(D + 1, DAY / 2),
            at(D + 2, 0) - 1,
            at(D + 2, 0),
            at(D + 2, DAY / 2),
            at(D + 3, 0) - 3,
            at(D + 3, 0),
        ] {
            retire_at(paths, now);
            let read = read_at(paths, now, Caps::default()).unwrap();
            if until > now {
                assert_eq!(
                    counter(&read, &read.me),
                    Some(1),
                    "horizon {horizon}, live at {now}"
                );
            }
        }
    }
}

/// NEGATIVE CONTROL: a target that never spills (`target` trying only its first table) refuses
/// the third write of the day.
#[test]
fn a_full_epoch_spills_into_the_next_and_recovers_at_midnight() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let caps = Caps {
        records: 100,
        writer_records: 2,
    };
    for counter in 1..=4 {
        let now = at(D, counter * 60);
        write_at(paths, &row(counter, now), now, caps).unwrap();
    }
    assert_eq!(ring_counts(paths, at(D, 300)), vec![(D, 2), (D + 1, 2)]);
    // Today's table and tomorrow's both hold two rows of this Mac's: it waits for midnight.
    let now = at(D, 300);
    assert!(write_at(paths, &row(5, now), now, caps).is_err());
    let read = read_at(paths, now, caps).unwrap();
    assert!(read.writable.is_err());
    assert_eq!(counter(&read, &read.me), Some(4));
    let midnight = at(D + 1, 0);
    assert_eq!(read_at(paths, midnight, caps).unwrap().writable, Ok(()));
    write_at(paths, &row(5, midnight), midnight, caps).unwrap();
    let read = read_at(paths, midnight, caps).unwrap();
    assert_eq!(counter(&read, &read.me), Some(5));
    assert_eq!(ring_dirs(paths, D - 1..=D + 3), vec![D, D + 1, D + 2]);
}

/// Three Macs split two and one; the lone Mac crosses midnight during the split and writes into
/// the next day's table. After the heal every Mac reads every Mac's latest row.
/// NEGATIVE CONTROL: reading from the current day's table on (`ring_logs` skipping the window's
/// first day) loses the rows of the day before: the lone Mac no longer finds its own row from
/// before midnight, and its next revision is refused.
#[test]
fn split_sides_write_the_same_day_tables_and_a_heal_merges_them() {
    let lab = Lab::new(&[Role::Writer, Role::Writer, Role::Writer]);
    let caps = Caps::default();
    let noon = at(D, DAY / 2);
    write_at(&lab.paths[0], &row(1, noon), noon, caps).unwrap();
    write_at(&lab.paths[2], &row(1, noon), noon, caps).unwrap();
    repair_day(&lab, 2, D, &[noon; 3]);
    // The split: Macs 0 and 1 on one side, Mac 2 alone on the other.
    let later = noon + 3600;
    write_at(&lab.paths[1], &row(1, later), later, caps).unwrap();
    write_at(&lab.paths[0], &row(2, later), later, caps).unwrap();
    let day = CLAIMS.scope(lab.bootstrap(0).trust(), D).unwrap().digest();
    repair_commons_among_first(&lab, 1, &[0, 1], &[later + 60; 3], day);
    let late = at(D + 1, 3600);
    write_at(&lab.paths[2], &row(2, late), late, caps).unwrap();
    // The heal, a little after midnight on every clock.
    let heal = at(D + 1, 7200);
    repair_day(&lab, 1, D, &[heal; 3]);
    // Day D+1 is Mac 2's alone. Mac 0 took it up in the heal above, where Mac 2 listed it, and
    // reads it in the first round; Mac 1 takes it up in the first round and reads it in the second.
    repair_day(&lab, 2, D + 1, &[heal; 3]);
    let me: Vec<_> = (0..3)
        .map(|n| read_at(&lab.paths[n], heal, caps).unwrap().me)
        .collect();
    for (n, paths) in lab.paths[..3].iter().enumerate() {
        let read = read_at(paths, heal, caps).unwrap();
        assert_eq!(
            me.iter()
                .map(|machine| counter(&read, machine))
                .collect::<Vec<_>>(),
            vec![Some(2), Some(1), Some(2)],
            "Mac {n}"
        );
    }
}

/// NEGATIVE CONTROL: a window placed by the clock alone (`RingState::current` without its floor)
/// lets the clock set back a day place the window from day D again, so day D's table is
/// permitted (`assertion failed: !directory.permits(session, &Policy::default(), day,
/// back).unwrap()`, and `claim_ring_conforms` fails).
#[test]
fn a_clock_set_back_a_day_cannot_reenter_a_retired_epoch() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let caps = Caps::default();
    let first = at(D, 60);
    write_at(paths, &row(1, first), first, caps).unwrap();
    // Two days on, day D's table retires with its lapsed row: the next row starts afresh.
    let ahead = at(D + 2, 60);
    write_at(paths, &row(1, ahead), ahead, caps).unwrap();
    assert_eq!(ring_dirs(paths, D - 2..=D + 4), vec![D + 2]);
    // The clock set back a day: writes and repair stay at or above the floor, and the row of
    // day D+2 is still read.
    let back = at(D + 1, 120);
    write_at(paths, &row(2, back), back, caps).unwrap();
    with_session(paths, true, |session, directory, _| {
        directory
            .ring_subscriptions(session, &Policy::default(), back)
            .unwrap();
        let day = CLAIMS.scope(directory.trust(), D).unwrap().digest();
        assert!(
            !directory
                .permits(session, &Policy::default(), day, back)
                .unwrap()
        );
    });
    assert_eq!(ring_dirs(paths, D - 2..=D), Vec::<u64>::new());
    assert_eq!(ring_claims(paths, D - 2..=D), Vec::<u64>::new());
    assert!(ring_state(paths).floor > D);
}

/// A retirement that fails after it deleted, before it forgot what it deleted, leaves the floor
/// raised: the clock set back, behind it, writes nothing, never into day D's table, and the next
/// pass on the righted clock forgets the deleted day.
/// NEGATIVE CONTROL: the floor written only with the forgotten days (`retire`'s first save
/// removed) stays where it was when the pass fails, and the clock set back enrolls day D again.
#[test]
fn a_crash_inside_a_retire_is_finished_by_the_next_pass() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let caps = Caps::default();
    let first = at(D, 60);
    write_at(paths, &row(1, first), first, caps).unwrap();
    ring::FAIL_AFTER_DELETE.with(|fail| fail.set(true));
    let ahead = at(D + 2, 60);
    assert!(write_at(paths, &row(1, ahead), ahead, caps).is_err());
    assert_eq!(ring_dirs(paths, D..=D), Vec::<u64>::new());
    assert!(ring_state(paths).held.contains(&D));
    // Day D's row went with its table, so a fresh row starts at counter 1.
    let back = at(D, 120);
    let wrote = write_at(paths, &row(1, back), back, caps);
    assert_eq!(ring_dirs(paths, D - 1..=D), Vec::<u64>::new());
    assert_eq!(ring_claims(paths, D - 1..=D), Vec::<u64>::new());
    assert!(wrote.is_err(), "the clock set back is behind the floor");
    write_at(paths, &row(1, ahead), ahead, caps).unwrap();
    let state = ring_state(paths);
    assert!(state.held.iter().all(|day| *day >= state.floor));
}

/// Mac 0 retires day D two days ahead; repairs beside Mac 1, which still holds day D, never bring
/// day D back to Mac 0, even with Mac 0's clock set back a day.
/// NEGATIVE CONTROL: `ring_permits` placing the window by the clock alone admits day D's table
/// for a repair on Mac 0's clock set back a day.
#[test]
fn repair_on_a_clock_set_back_a_day_never_resurrects_a_retired_epoch() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let caps = Caps::default();
    let now = at(D, 60);
    write_at(&lab.paths[0], &row(1, now), now, caps).unwrap();
    write_at(&lab.paths[1], &row(1, now), now, caps).unwrap();
    repair_commons_at(&lab, 6, &[now, now]);
    let ahead = at(D + 2, 60);
    write_at(&lab.paths[0], &row(1, ahead), ahead, caps).unwrap();
    repair_commons_at(&lab, 6, &[ahead, now]);
    let back = at(D + 1, 60);
    repair_commons_at(&lab, 6, &[back, now]);
    assert_eq!(ring_dirs(&lab.paths[0], D - 1..=D), Vec::<u64>::new());
    assert_eq!(ring_claims(&lab.paths[0], D - 1..=D), Vec::<u64>::new());
    assert_eq!(ring_dirs(&lab.paths[1], D..=D), vec![D]);
    with_session(&lab.paths[0], false, |session, directory, _| {
        let id = CLAIMS.scope(directory.trust(), D).unwrap().digest();
        assert!(
            !directory
                .permits(session, &Policy::default(), id, back)
                .unwrap()
        );
    });
}

/// The claims of `machine`'s latest row in `read`.
fn claims_of(read: &Read, machine: &[u8; 16]) -> Option<Value> {
    read.rows
        .iter()
        .find(|(m, _)| m == machine)
        .map(|(_, body)| body["claims"].clone())
}

/// Mac 0's clock runs two days ahead for one write, which deletes its copy of day D's table
/// holding both Macs' rows, and comes back. Back on day D, Mac 0's window is the clock's: repair
/// carries day D back to Mac 0 for reading, so it reads Mac 1's row at once, and it writes nothing
/// while its clock is behind its floor (day D+1), never into day D's table, whose copy at Mac 1
/// holds its own earlier signed row. On day D+1 it writes on in day D+2's table, which it wrote
/// while ahead and kept, so its signed history there runs on, and both Macs read its new row.
/// NEGATIVE CONTROL: a window that never comes below the floor (`RingState::current` as
/// `clock.max(floor + 1)`) keeps Mac 0 two days ahead, off day D's table and Mac 1's row (`the
/// named table was not repaired in the last round`). `target` refusing only once the floor lies
/// beyond the window (its clock check as `state.floor > last`) lets Mac 0 write on day D, into
/// day D+1's table (`assertion failed: read.writable.is_err()`).
#[test]
fn a_clock_back_from_running_ahead_reads_its_peers_and_never_forks_its_history() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let caps = Caps::default();
    let now = at(D, 60);
    for paths in &lab.paths {
        write_at(paths, &row(1, now), now, caps).unwrap();
    }
    repair_day(&lab, 1, D, &[now, now]);
    let me: Vec<_> = (0..2)
        .map(|n| read_at(&lab.paths[n], now, caps).unwrap().me)
        .collect();
    let ahead = at(D + 2, 60);
    write_at(&lab.paths[0], &row(1, ahead), ahead, caps).unwrap();
    assert_eq!(ring_dirs(&lab.paths[0], D - 1..=D + 3), vec![D + 2]);
    let back = at(D, 3600);
    repair_day(&lab, 2, D, &[back, back]);
    let read = read_at(&lab.paths[0], back, caps).unwrap();
    assert!(read.writable.is_err());
    assert_eq!(counter(&read, &me[1]), Some(1));
    let marked = json!({"counter": 2, "until": back + DAY, "claims": ["back@example.invalid"]});
    assert!(write_at(&lab.paths[0], &marked, back, caps).is_err());
    let floor = at(D + 1, 60);
    let next = counter(&read_at(&lab.paths[0], floor, caps).unwrap(), &me[0]).map_or(1, |c| c + 1);
    assert_eq!(next, 2, "Mac 0 reads its own row of day D+2 on day D+1");
    let marked = json!({"counter": next, "until": floor + DAY, "claims": ["back@example.invalid"]});
    write_at(&lab.paths[0], &marked, floor, caps).unwrap();
    repair_day(&lab, 2, D + 2, &[floor, floor]);
    for (n, paths) in lab.paths[..2].iter().enumerate() {
        let read = read_at(paths, floor, caps).unwrap();
        assert_eq!(read.writable, Ok(()), "Mac {n}");
        assert_eq!(
            claims_of(&read, &me[0]),
            Some(json!(["back@example.invalid"])),
            "Mac {n}"
        );
    }
    let later = floor + 60;
    write_at(&lab.paths[0], &row(next + 1, later), later, caps).unwrap();
    write_at(&lab.paths[1], &row(2, later), later, caps).unwrap();
    assert_eq!(ring_dirs(&lab.paths[0], D - 1..=D + 3), vec![D + 2]);
}

/// Both Macs write on day D-1 and day D, Mac 1 claiming x on day D; then Mac 0's clock is set
/// back two days, below its floor (day D-1). Its window, the clock's, leaves out day D, which
/// holds its own latest row and Mac 1's claim of x, so it records no claim: it never claims x on
/// a view without Mac 1's claim, in a row its own day-D row outranks on Mac 1. Once its clock
/// reaches its floor, it reads Mac 1's claim again and writes on.
/// NEGATIVE CONTROL: `target` refusing only once the floor lies beyond the window (its clock
/// check as `state.floor > last`) lets Mac 0 write on day D-2, into day D-1's table (`assertion
/// failed: read.writable.is_err()`).
#[test]
fn a_clock_set_back_two_days_claims_nothing_on_a_view_without_its_peers_today() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let caps = Caps::default();
    let yesterday = at(D - 1, 60);
    for paths in &lab.paths {
        write_at(paths, &row(1, yesterday), yesterday, caps).unwrap();
    }
    repair_day(&lab, 1, D - 1, &[yesterday, yesterday]);
    let now = at(D, 60);
    for c in [2, 3] {
        let body = json!({"counter": c, "until": now + DAY, "claims": ["a@example.invalid"]});
        write_at(&lab.paths[0], &body, now + c, caps).unwrap();
    }
    let x = json!({"counter": 2, "until": now + DAY, "claims": ["x@example.invalid"]});
    write_at(&lab.paths[1], &x, now, caps).unwrap();
    repair_day(&lab, 1, D, &[now + 10, now + 10]);
    let me: Vec<_> = (0..2)
        .map(|n| read_at(&lab.paths[n], now + 10, caps).unwrap().me)
        .collect();
    let back = at(D - 2, 3600);
    repair_day(&lab, 1, D - 1, &[back, now + 20]);
    let read = read_at(&lab.paths[0], back, caps).unwrap();
    assert!(read.writable.is_err());
    let mine = json!({"counter": 2, "until": back + DAY, "claims": ["x@example.invalid"]});
    assert!(write_at(&lab.paths[0], &mine, back, caps).is_err());
    repair_day(&lab, 1, D - 1, &[back + 60, now + 60]);
    let peer = read_at(&lab.paths[1], now + 60, caps).unwrap();
    assert_eq!(counter(&peer, &me[0]), Some(3));
    assert_eq!(claims_of(&peer, &me[0]), Some(json!(["a@example.invalid"])));
    let floor = at(D - 1, 3600);
    let read = read_at(&lab.paths[0], floor, caps).unwrap();
    assert_eq!(read.writable, Ok(()));
    assert_eq!(claims_of(&read, &me[1]), Some(json!(["x@example.invalid"])));
    write_at(&lab.paths[0], &row(4, floor), floor, caps).unwrap();
}

/// Mac 0's clock is set back five days, then righted. Set back, it writes nothing (its clock is
/// behind its floor) and its retirement keeps day D's table, which holds its row and lies above
/// its window but not below its floor; righted, it writes on in that table, so its signed history
/// there runs on, and both Macs read its new row.
/// NEGATIVE CONTROL: deleting every held period outside the window (`RingState::outdated` without
/// `*epoch < floor`) deletes Mac 0's day D table while it is set back (`left: []`,
/// `right: [20000]`); without that assertion, its righted write starts a fresh day D table and
/// forks its own history at Mac 1's copy: `left: Err("local proposal writer has withheld,
/// unavailable or refused history; recovery required")`, `Mac 0`.
#[test]
fn a_clock_set_back_days_then_righted_writes_on_in_its_own_tables() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let caps = Caps::default();
    let now = at(D, 60);
    for paths in &lab.paths {
        write_at(paths, &row(1, now), now, caps).unwrap();
    }
    repair_day(&lab, 1, D, &[now, now]);
    let me = read_at(&lab.paths[0], now, caps).unwrap().me;
    let back = at(D - 5, 60);
    assert!(write_at(&lab.paths[0], &row(2, back), back, caps).is_err());
    // Day D lies outside this clock's window, so no table can be named: one regular exchange,
    // whose retirement is what the next line judges.
    repair_commons_at(&lab, 1, &[back, now]);
    assert_eq!(ring_dirs(&lab.paths[0], D..=D), vec![D]);
    let right = at(D, 120);
    let next = counter(&read_at(&lab.paths[0], right, caps).unwrap(), &me).map_or(1, |c| c + 1);
    write_at(&lab.paths[0], &row(next, right), right, caps).unwrap();
    repair_day(&lab, 1, D, &[right, right]);
    for (n, paths) in lab.paths[..2].iter().enumerate() {
        let read = read_at(paths, right, caps).unwrap();
        assert_eq!(read.writable, Ok(()), "Mac {n}");
        assert_eq!(counter(&read, &me), Some(next), "Mac {n}");
    }
}

/// NEGATIVE CONTROL: checking `writable` on the target table alone accepts the write a day later,
/// while the refused record still sits in yesterday's table inside the window.
#[test]
fn refused_history_blocks_writes_inside_the_window_and_heals_when_its_epoch_retires() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let caps = Caps::default();
    let first = at(D, 60);
    write_at(paths, &row(1, first), first, caps).unwrap();
    commit_ring(paths, D, first, b"invalid claim command");
    for now in [first + 60, at(D + 1, 60)] {
        assert!(read_at(paths, now, caps).unwrap().writable.is_err());
        assert!(write_at(paths, &row(2, now), now, caps).is_err());
    }
    let healed = at(D + 2, 60);
    assert_eq!(read_at(paths, healed, caps).unwrap().writable, Ok(()));
    write_at(paths, &row(1, healed), healed, caps).unwrap();
    assert_eq!(ring_dirs(paths, D..=D), Vec::<u64>::new());
}

/// NEGATIVE CONTROL: clamping the target to the window's last day (in place of refusing) accepts
/// the first row into tomorrow's table, which a clock two days on deletes while the row is live.
#[test]
fn until_beyond_the_window_is_refused() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let now = at(D, 60);
    let beyond = json!({"counter": 1, "until": at(D + 3, 0), "claims": []});
    assert!(write_at(paths, &beyond, now, Caps::default()).is_err());
    assert_eq!(ring_dirs(paths, D - 1..=D + 2), Vec::<u64>::new());
    let latest = json!({"counter": 1, "until": at(D + 3, 0) - 1, "claims": []});
    write_at(paths, &latest, now, Caps::default()).unwrap();
    assert_eq!(ring_dirs(paths, D - 1..=D + 2), vec![D + 1]);
}

/// Mac 0's row at counter 57 sits in day D's table, which Mac 1 still holds; back two days later,
/// Mac 0 has retired it and writes counter 1 into its new day's table. Mac 1 reads counter 1.
/// NEGATIVE CONTROL: ranking by counter first (`latest`'s rank without its table tier) picks the
/// stale counter 57.
#[test]
fn a_mac_back_after_a_long_absence_ranks_above_its_old_row() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let caps = Caps::default();
    let now = at(D, 60);
    let mac = read_at(&lab.paths[0], now, caps).unwrap().me;
    let old = command(&mac, &row(57, now)).unwrap();
    commit_ring(&lab.paths[0], D, now, &encode(&old).unwrap());
    repair_day(&lab, 2, D, &[now, now]);
    assert_eq!(
        counter(&read_at(&lab.paths[1], now, caps).unwrap(), &mac),
        Some(57)
    );
    let back = at(D + 2, 60);
    write_at(&lab.paths[0], &row(1, back), back, caps).unwrap();
    let behind = at(D + 1, 60);
    repair_day(&lab, 2, D + 2, &[back, behind]);
    let read = read_at(&lab.paths[1], behind, caps).unwrap();
    assert_eq!(counter(&read, &mac), Some(1));
}

/// An F19 build from before the ring writes `accounts/claims/1`; a ring Mac's write seals it, and
/// once the old build hears the seal its reads and writes fail, so it keeps to its home accounts.
/// NEGATIVE CONTROL: without the seal (`Directory::seal` writing nothing), the old build still
/// reads its table, where the ring Mac's claims never appear.
#[test]
fn a_legacy_f19_mac_fails_safe_once_sealed() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (old, new) = (&lab.paths[0], &lab.paths[1]);
    legacy_write(old, &json!({"counter": 1, "claims": []})).unwrap();
    assert_eq!(legacy_read(old).unwrap().len(), 1);
    repair_commons(&lab, 6);
    write(new, &json!({"counter": 1, "claims": []})).unwrap();
    repair_commons(&lab, 6);
    assert!(legacy_read(old).is_err());
    assert!(legacy_write(old, &json!({"counter": 2, "claims": []})).is_err());
}

/// Where no earlier build declared `accounts/claims/1`, ring Macs declare nothing.
/// NEGATIVE CONTROL: a seal that does not look for the old declaration (committing the twin
/// wherever nothing else is declared at its identity) declares `accounts/claims/2` alone: a
/// subscribed table every Mac then reserves storage for.
#[test]
fn no_seal_without_a_legacy_declaration() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    for paths in &lab.paths {
        write(paths, &json!({"counter": 1, "claims": []})).unwrap();
        write(paths, &json!({"counter": 2, "claims": []})).unwrap();
    }
    repair_commons(&lab, 6);
    for paths in &lab.paths {
        assert_eq!(catalog_records(paths), 0);
        with_session(paths, false, |session, directory, bootstrap| {
            let first = legacy_descriptor(bootstrap, 1).unwrap();
            let snapshot = directory.snapshot_if_exists(session).unwrap();
            assert!(!snapshot.entries.contains_key(&first.scope.digest()));
            assert!(!directory.path(first.scope).unwrap().exists());
        });
    }
}

/// A Mac that has spent its creator budget cannot seal: its ring write still lands, and the old
/// table stays subscribed.
/// NEGATIVE CONTROL: a seal without its creator-budget check (`seal`'s `PER_CREATOR` test
/// removed) commits a ninth declaration for this Mac.
#[test]
fn a_seal_respects_the_catalog_budgets() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    legacy_write(&lab.paths[0], &json!({"counter": 1, "claims": []})).unwrap();
    repair_commons(&lab, 6);
    let paths = &lab.paths[1];
    with_session(paths, true, |session, directory, bootstrap| {
        for n in 0..PER_CREATOR {
            let own =
                Descriptor::application(bootstrap.trust(), "budget", &format!("t{n}"), 1).unwrap();
            directory.publish(session, &own).unwrap();
        }
    });
    let records = catalog_records(paths);
    write(paths, &json!({"counter": 1, "claims": []})).unwrap();
    assert_eq!(catalog_records(paths), records);
    with_session(paths, true, |session, directory, bootstrap| {
        let first = legacy_descriptor(bootstrap, 1).unwrap();
        assert!(directory.seal(session, &first).is_err());
        let snapshot = directory.snapshot(session).unwrap();
        assert_eq!(
            snapshot.entries[&first.scope.digest()].state,
            State::Subscribed
        );
    });
    assert_eq!(catalog_records(paths), records);
}

/// A ring Mac reads the old build's live row while the old table is subscribed and for two days
/// after its seal, and not after.
/// NEGATIVE CONTROL: without the old tables' read (`legacy_logs` returning none), the old
/// build's live claim is missing on the ring Mac before the seal.
#[test]
fn fixed_macs_honour_live_legacy_rows_until_the_grace_ends() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (old, new) = (&lab.paths[0], &lab.paths[1]);
    legacy_write(
        old,
        &json!({"counter": 1, "claims": ["held@example.invalid"]}),
    )
    .unwrap();
    let mac = legacy_read(old).unwrap()[0].0;
    repair_commons(&lab, 6);
    assert_eq!(counter(&read(new).unwrap(), &mac), Some(1));
    write(new, &json!({"counter": 1, "claims": []})).unwrap();
    let sealed = sealed_at(new);
    let caps = Caps::default();
    assert_eq!(
        counter(&read_at(new, sealed + DAY, caps).unwrap(), &mac),
        Some(1)
    );
    assert_eq!(
        counter(&read_at(new, sealed + 2 * DAY + 1, caps).unwrap(), &mac),
        None
    );
}

/// The Mac that ran the old build continues its counter from its old row; past the grace on its
/// clock it starts afresh, and a peer still inside the grace ranks that fresh row above the old.
/// NEGATIVE CONTROL: ranking every table alike (`Source::tier` 0 for a ring row too) picks the
/// old row's counter 5 over the counter 1 row the Mac wrote after its own tables retired.
#[test]
fn own_ring_row_outranks_own_legacy_row_and_continues_its_counter() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (mac, peer) = (&lab.paths[0], &lab.paths[1]);
    legacy_write(mac, &json!({"counter": 5, "claims": []})).unwrap();
    repair_commons(&lab, 6);
    assert!(write(mac, &json!({"counter": 1, "claims": []})).is_err());
    write(mac, &json!({"counter": 6, "claims": []})).unwrap();
    let read = read(mac).unwrap();
    assert_eq!(counter(&read, &read.me), Some(6));
    let me = read.me;
    let sealed = sealed_at(mac);
    let caps = Caps::default();
    let late = sealed + 2 * DAY + 1;
    write_at(mac, &row(1, late), late, caps).unwrap();
    let behind = late - DAY;
    repair_commons_at(&lab, 8, &[late, behind]);
    assert_eq!(counter(&read_at(peer, behind, caps).unwrap(), &me), Some(1));
}

/// Mac 0's first write of day D+1 is cut short after its day table's enrollment claim and before
/// the table's journal (an unavailable destination here; a crash, a kill or a full disk leaves the
/// same pending claim). A pending table holds no row, so it costs nothing: Mac 0 reads every row
/// at once, a repair round resumes the enrollment and brings Mac 1's row of day D+1, and Mac 0's
/// next write lands, all on day D+1.
/// NEGATIVE CONTROL: `Store::observe_if_exists` reading a pending claim as established custody
/// (its pending arm removed) fails Mac 0's first read on day D+1 with "table enrollment is not
/// yet complete"; `Round::table` taking up only storage with no claim at all (`absent` also
/// requiring the claim file missing) never resumes the enrollment, so Mac 1's row of day D+1
/// never reaches Mac 0.
#[test]
fn an_interrupted_day_table_enrollment_costs_nothing_and_resumes() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let caps = Caps::default();
    let now = at(D, 60);
    for paths in &lab.paths {
        write_at(paths, &row(1, now), now, caps).unwrap();
    }
    repair_commons_at(&lab, 6, &[now, now]);
    let me: Vec<_> = (0..2)
        .map(|n| read_at(&lab.paths[n], now, caps).unwrap().me)
        .collect();
    let counters = |n: usize, now: u64| {
        let read = read_at(&lab.paths[n], now, caps).unwrap();
        assert_eq!(read.writable, Ok(()), "Mac {n}");
        me.iter()
            .map(|machine| counter(&read, machine))
            .collect::<Vec<_>>()
    };
    let next = at(D + 1, 60);
    // The enrollment a first write of the day makes (`write_at`'s `enroll_ring`), cut short
    // after its claim by a file where the table's journal goes.
    with_session(&lab.paths[0], true, |session, directory, _| {
        let blocked = directory
            .path(CLAIMS.scope(directory.trust(), D + 1).unwrap())
            .unwrap();
        std::fs::write(&blocked, b"not a table").unwrap();
        assert!(
            directory
                .enroll_ring(session, &CLAIMS, D + 1, next)
                .is_err()
        );
        std::fs::remove_file(&blocked).unwrap();
    });
    assert_eq!(ring_claims(&lab.paths[0], D + 1..=D + 1), vec![D + 1]);
    assert_eq!(counters(0, next), vec![Some(1), Some(1)]);
    write_at(&lab.paths[1], &row(2, next), next, caps).unwrap();
    repair_commons_at(&lab, 12, &[next, next]);
    assert_eq!(counters(0, next), vec![Some(1), Some(2)]);
    write_at(&lab.paths[0], &row(2, next), next, caps).unwrap();
    repair_commons_at(&lab, 6, &[next, next]);
    for n in 0..2 {
        assert_eq!(counters(n, next), vec![Some(2), Some(2)], "Mac {n}");
    }
}

/// Mac 0 writes on day D, and on day D+2, which deletes day D's table; its state is then damaged
/// while its clock is set back a day. The rebuilt floor is the one it lost, from the floor's
/// second copy, so day D stays refused: neither a peer's offer nor a write takes it up again,
/// and the write lands in day D+2's table.
/// NEGATIVE CONTROL: a rebuild that reads no copy (`rebuilt_floor` bounded by the tables held
/// alone, day D+2 - 2) places the set-back window from day D again:
/// `assertion failed: ring_state(paths).floor > D`.
#[test]
fn a_damaged_ring_state_keeps_its_floor_on_a_clock_set_back() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let caps = Caps::default();
    let first = at(D, 60);
    write_at(paths, &row(1, first), first, caps).unwrap();
    let ahead = at(D + 2, 60);
    write_at(paths, &row(1, ahead), ahead, caps).unwrap();
    assert_eq!(ring_dirs(paths, D - 2..=D + 4), vec![D + 2]);
    damage_ring_state(paths, b"ATRS1-not-a-state");
    let back = at(D + 1, 60);
    write_at(paths, &row(2, back), back, caps).unwrap();
    assert!(ring_state(paths).floor > D);
    with_session(paths, true, |session, directory, _| {
        let day = CLAIMS.scope(directory.trust(), D).unwrap().digest();
        directory
            .adopt_ring_tables(session, &Policy::default(), back, &[day].into())
            .unwrap();
        assert!(
            !directory
                .permits(session, &Policy::default(), day, back)
                .unwrap()
        );
    });
    assert_eq!(ring_dirs(paths, D - 2..=D + 4), vec![D + 2]);
    let read = read_at(paths, back, caps).unwrap();
    assert_eq!(counter(&read, &read.me), Some(2));
}

/// Put `bytes` where `paths`'s claims ring state goes, as damage, a partial restore or a later
/// build's format after a downgrade would.
fn damage_ring_state(paths: &Paths, bytes: &[u8]) {
    with_session(paths, true, |session, _, _| {
        let dir = claim_dir(session).unwrap();
        aterm_journal::write_private_durable(&dir, &CLAIMS.state_name(), bytes).unwrap();
    });
}

/// Declare an application table on `paths` and append one record to it; the table's scope.
fn declare_with_record(paths: &Paths) -> crate::table::Scope {
    with_session(paths, true, |session, directory, bootstrap| {
        let descriptor = Descriptor::application(bootstrap.trust(), "probe", "rows", 1).unwrap();
        let scope = directory.publish(session, &descriptor).unwrap();
        let mut store = directory.enroll(session, scope).unwrap();
        store.commit_catalog(session, b"probe row").unwrap();
        scope
    })
}

/// Records `paths` retains in the table `scope`.
fn table_records(paths: &Paths, scope: crate::table::Scope) -> usize {
    with_session(paths, false, |session, directory, _| {
        Store::observe_if_exists(session, &directory.path(scope).unwrap(), scope)
            .unwrap()
            .map_or(0, |store| store.inventory(session).unwrap().count())
    })
}

/// Mac 0's ring state is damaged after its first row. Mac 0 still reads that row at once; two
/// days on, repair beside Mac 1 carries the catalog, Mac 1's application table and Mac 1's claim
/// row to it, the state its writer pass rebuilt has retired the day table Mac 0 wrote, and Mac
/// 0's next write lands.
/// NEGATIVE CONTROL: `retire` without the rebuild (failing on the damaged state) leaves Mac 0
/// without Mac 1's row of day D+2, because taking a table up retires first; a rebuild that holds
/// nothing (`rebuild_ring_state` finding no storage) leaves day D's table on disk for good.
#[test]
fn a_damaged_ring_state_is_rebuilt_and_repair_goes_on() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let caps = Caps::default();
    let now = at(D, 60);
    write_at(&lab.paths[0], &row(1, now), now, caps).unwrap();
    damage_ring_state(&lab.paths[0], b"ATRS1-not-a-state");
    let read = read_at(&lab.paths[0], now, caps).unwrap();
    assert_eq!(counter(&read, &read.me), Some(1));
    assert_eq!(read.writable, Ok(()));
    let later = at(D + 2, 60);
    let app = declare_with_record(&lab.paths[1]);
    write_at(&lab.paths[1], &row(1, later), later, caps).unwrap();
    repair_commons_at(&lab, 10, &[later, later]);
    assert_eq!(catalog_records(&lab.paths[0]), 1);
    assert_eq!(table_records(&lab.paths[0], app), 1);
    let peer = read_at(&lab.paths[1], later, caps).unwrap().me;
    let read = read_at(&lab.paths[0], later, caps).unwrap();
    assert_eq!(counter(&read, &peer), Some(1));
    assert_eq!(counter(&read, &read.me), None);
    write_at(&lab.paths[0], &row(1, later), later, caps).unwrap();
    assert_eq!(ring_dirs(&lab.paths[0], D - 1..=D + 3), vec![D + 2]);
    assert_eq!(ring_claims(&lab.paths[0], D - 1..=D + 3), vec![D + 2]);
    let state = ring_state(&lab.paths[0]);
    assert_eq!((state.floor, state.held), (D + 1, [D + 2].into()));
}

/// A ring state that can be neither read nor renamed over (a directory where its file goes) is
/// moved aside, never deleted, by the writer pass a repair round asks for: repair carries the
/// catalog, Mac 1's application table and Mac 1's claim row of the day table both Macs hold to
/// Mac 0, Mac 0 still reads its own row, and its next write lands and reaches Mac 1.
/// NEGATIVE CONTROL: a reader's view of the ring that fails on the damage (`ring_view` returning
/// the `ring_state` error) fails every exchange at the window of the tables a peer offers, so
/// neither the catalog record nor the application row reaches Mac 0; without the move aside
/// (`save_ring_state` writing over the directory), the rebuilt state cannot be written, so the
/// ring tables drop out of Mac 0's repair and Mac 1's row never reaches it (`left: None`,
/// `right: Some(1)`), and every claim write there fails.
#[test]
fn an_unrewritable_ring_state_is_moved_aside_and_claims_go_on() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let caps = Caps::default();
    let now = at(D, 60);
    write_at(&lab.paths[0], &row(1, now), now, caps).unwrap();
    let state = with_session(&lab.paths[0], true, |session, _, _| {
        let state = claim_dir(session).unwrap().join(CLAIMS.state_name());
        std::fs::remove_file(&state).unwrap();
        std::fs::create_dir(&state).unwrap();
        std::fs::write(state.join("kept"), b"a person's file").unwrap();
        state
    });
    let app = declare_with_record(&lab.paths[1]);
    write_at(&lab.paths[1], &row(1, now), now, caps).unwrap();
    repair_commons_at(&lab, 10, &[now, now]);
    assert_eq!(catalog_records(&lab.paths[0]), 1);
    assert_eq!(table_records(&lab.paths[0], app), 1);
    let peer = read_at(&lab.paths[1], now, caps).unwrap().me;
    let read = read_at(&lab.paths[0], now, caps).unwrap();
    assert_eq!(counter(&read, &read.me), Some(1));
    assert_eq!(counter(&read, &peer), Some(1));
    write_at(&lab.paths[0], &row(2, now), now, caps).unwrap();
    repair_commons_at(&lab, 6, &[now, now]);
    assert_eq!(
        counter(&read_at(&lab.paths[1], now, caps).unwrap(), &read.me),
        Some(2)
    );
    // The damage is kept for a person to look at, beside the state written in its place.
    let aside = state.with_file_name(format!("{}.damaged-1", CLAIMS.state_name()));
    assert_eq!(
        std::fs::read(aside.join("kept")).unwrap(),
        b"a person's file"
    );
    assert!(std::fs::symlink_metadata(&state).unwrap().is_file());
}

#[test]
fn roster_denied_binding_cannot_write_or_project_claim_rows() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let caps = Caps::default();
    let now = at(D, 60);
    write_at(&lab.paths[0], &row(1, now), now, caps).unwrap();
    // Mac 1 writes a claim row with a spoofed label ("m1") to try to evade label-only filtering.
    let spoofed = json!({"counter": 1, "until": now + DAY, "label": "m1", "claims": ["stolen@example.invalid"]});
    write_at(&lab.paths[1], &spoofed, now, caps).unwrap();
    repair_commons_at(&lab, 6, &[now, now]);

    let m1_machine = read_at(&lab.paths[0], now, caps).unwrap().me;
    let m2_machine = read_at(&lab.paths[1], now, caps).unwrap().me;
    assert_eq!(
        counter(&read_at(&lab.paths[0], now, caps).unwrap(), &m2_machine),
        Some(1),
        "CONTROL: Mac 1's row is visible before roster denial"
    );

    let m2_binding = lab.bootstrap(1).request().unwrap().binding().clone();
    let publish_roster = |roster_toml: &str| {
        let admin = aterm_secrets::client::admin_open(&lab.paths[0]).unwrap();
        let mut doc = admin.doc.clone();
        doc.set_entry(
            &aterm_secrets::text::Audience::Everyone,
            aterm_secrets::SWARM_ROSTER_ENTRY,
            roster_toml.as_bytes(),
        )
        .unwrap();
        aterm_secrets::client::admin_publish(&lab.paths[0], &admin, &doc).unwrap();
        aterm_secrets::client::sync(&lab.paths[1], None).unwrap();
    };

    // 1. Deny Mac 1 by recipient/label in aterm-swarm-roster.
    let m1_rec = aterm_secrets::client::machine_identity(&lab.paths[0])
        .unwrap()
        .recipient()
        .to_bech32();
    let m2_rec = aterm_secrets::client::machine_identity(&lab.paths[1])
        .unwrap()
        .recipient()
        .to_bech32();
    publish_roster(&format!(
        "deny_labels = [\"m2\"]\n\n[[member]]\nlabel = \"m1\"\nvault_name = \"m1\"\nrecipient = \"{m1_rec}\"\n\n[[member]]\nlabel = \"m2\"\nvault_name = \"m2\"\nrecipient = \"{m2_rec}\"\n"
    ));
    let read0 = read_at(&lab.paths[0], now, caps).unwrap();
    assert_eq!(counter(&read0, &m1_machine), Some(1));
    assert_eq!(
        counter(&read0, &m2_machine),
        None,
        "denied recipient's row must be excluded even with spoofed body label"
    );
    assert!(
        read_at(&lab.paths[1], now, caps)
            .ok()
            .is_none_or(|read1| read1.writable.is_err()),
        "denied Mac must refuse read or report writable = Err"
    );
    assert!(
        write_at(&lab.paths[1], &row(2, now + 10), now + 10, caps).is_err(),
        "denied Mac must not write new claim rows"
    );

    // 2. Deny Mac 1 by writer key, incarnation, or machine id in deny_nodes (even when its
    // recipient is omitted from [[member]]).
    for denied_hex in [
        aterm_codec::hex::encode_lower(m2_binding.writer()),
        aterm_codec::hex::encode_lower(m2_binding.incarnation()),
        aterm_codec::hex::encode_lower(m2_binding.machine()),
    ] {
        publish_roster(&format!(
            "deny_nodes = [\"{denied_hex}\"]\n\n[[member]]\nlabel = \"m1\"\nvault_name = \"m1\"\nrecipient = \"{m1_rec}\"\n"
        ));
        let read0 = read_at(&lab.paths[0], now, caps).unwrap();
        assert_eq!(
            counter(&read0, &m2_machine),
            None,
            "row signed by binding with denied {denied_hex} must be excluded"
        );
        assert!(
            read_at(&lab.paths[1], now, caps)
                .ok()
                .is_none_or(|read1| read1.writable.is_err()),
            "local Mac with denied {denied_hex} must refuse read or report writable = Err"
        );
        assert!(
            write_at(&lab.paths[1], &row(2, now + 10), now + 10, caps).is_err(),
            "local Mac with denied {denied_hex} must not write claim rows"
        );
    }
}
