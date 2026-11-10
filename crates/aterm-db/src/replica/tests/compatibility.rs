// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::net::UnixStream;

fn seed_log(lab: &Lab) -> aterm_secrets::membership_log::Log {
    seed_log_with(lab, 0, Holder::Wrapped)
}

/// How many rotations of the vault's key the long-log workloads' history holds past
/// [`seed_log`]'s. Each is what a rotation adds where the fold is off
/// (`client::log_entries::rotate_epochs_after_remove`): an `[everyone]` epoch and a Commons
/// epoch, a wrap of each to the Macs that hold it, the Commons control entry resealed under the
/// new key, and one put under the `[everyone]` key (a `secrets set` under the current key adds
/// that put alone). Together 9,605 records, 7.2 MB, and 7,207 records, 5.6 MB, where only the
/// first Mac holds the older rotations ([`Holder::Handoff`]). On a Mac whose log had grown to
/// 4.5 MB each quiet round cost seconds of CPU
/// (`docs/measured/commons-long-log-rounds-2026-11-07.md`).
const LONG_LOG_ROTATIONS: u64 = 1_200;

/// How the second Mac holds the epochs of the long history's rotations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Holder {
    /// Each is wrapped to it, as to the first.
    Wrapped,
    /// Only the newest is, as for a Mac admitted after the rest, which a vault no longer wraps to
    /// it: it opens each older one through the first Mac's handoff ([`pair_from_lab_with`]).
    Handoff,
}

/// [`seed_log`], with `history` rotations of the vault's key past it ([`LONG_LOG_ROTATIONS`]), each
/// an `[everyone]` and a Commons epoch, held by the second Mac as `holder` says.
fn seed_log_with(lab: &Lab, history: u64, holder: Holder) -> aterm_secrets::membership_log::Log {
    use aterm_secrets::{
        client::{save_records, test_found},
        membership_log::{Entrant, EpochAudience, Log, encode_records},
    };
    let founders = (0..lab.paths.len())
        .map(|n| Entrant {
            request: lab.bootstrap(n).request().unwrap(),
            node_key: [n as u8 + 1; 32],
            name: format!("compat-{n}"),
            label: format!("m{}", n + 1),
        })
        .collect();
    let mut log = Log::new(test_found(&lab.paths[0], founders, 1_000).unwrap()).unwrap();
    let bootstrap = lab.bootstrap(0);
    let request = bootstrap.request().unwrap();
    let actor = request.binding().actor();
    let incarnation = *request.binding().incarnation();
    let (draft, key) = log
        .draft_epoch(&actor, 1_001, EpochAudience::Commons, 1)
        .unwrap();
    let epoch = bootstrap
        .device()
        .with_seed(|seed| draft.sign(seed, incarnation))
        .unwrap();
    log.insert(epoch.clone()).unwrap();
    for (n, _) in lab.paths.iter().enumerate() {
        let recipient = lab.bootstrap(n).request().unwrap().binding().recipient();
        let draft = log
            .draft_wrap(&actor, 1_002 + n as u64, *epoch.id(), &key, recipient)
            .unwrap();
        let wrap = bootstrap
            .device()
            .with_seed(|seed| draft.sign(seed, incarnation))
            .unwrap();
        log.insert(wrap).unwrap();
    }
    let admin = aterm_secrets::client::admin_open(&lab.paths[0]).unwrap();
    let control = admin
        .doc
        .entries()
        .into_iter()
        .find(|entry| entry.name == aterm_secrets::commons::ENTRY)
        .unwrap();
    let draft = log
        .draft_commons_put(&actor, 1_100, *epoch.id(), &key, control.value.as_slice())
        .unwrap();
    let put = bootstrap
        .device()
        .with_seed(|seed| draft.sign(seed, incarnation))
        .unwrap();
    log.insert(put).unwrap();
    let sign = |draft: aterm_secrets::membership_log::Draft| {
        bootstrap
            .device()
            .with_seed(|seed| draft.sign(seed, incarnation))
            .unwrap()
    };
    for generation in 1..=history {
        let at = 2_000 + generation * 10;
        let wrapped = |n: usize| n == 0 || holder == Holder::Wrapped || generation == history;
        // Mint an epoch of `audience` at `ms` and wrap its key to each Mac that holds it.
        let mut mint = |ms: u64, audience: EpochAudience, generation: u64| {
            let (draft, key) = log.draft_epoch(&actor, ms, audience, generation).unwrap();
            let epoch = sign(draft);
            log.insert(epoch.clone()).unwrap();
            for n in (0..lab.paths.len()).filter(|&n| wrapped(n)) {
                let recipient = lab.bootstrap(n).request().unwrap().binding().recipient();
                let draft = log
                    .draft_wrap(&actor, ms + 1, *epoch.id(), &key, recipient)
                    .unwrap();
                log.insert(sign(draft)).unwrap();
            }
            (*epoch.id(), key)
        };
        let (everyone, key) = mint(at, EpochAudience::Everyone, generation);
        let (commons, commons_key) = mint(at + 3, EpochAudience::Commons, generation + 1);
        let draft = log
            .draft_commons_put(
                &actor,
                at + 5,
                commons,
                &commons_key,
                control.value.as_slice(),
            )
            .unwrap();
        log.insert(sign(draft)).unwrap();
        let entry = aterm_secrets::text::Entry {
            name: format!("history-{}", generation % 40),
            value: aterm_secrets::SecretBytes::from([b'x'; 64].as_slice()),
            audience: aterm_secrets::text::Audience::Everyone,
            if_absent: false,
            anchor: None,
        };
        let draft = log
            .draft_put(&actor, at + 6, everyone, &key, &entry)
            .unwrap();
        log.insert(sign(draft)).unwrap();
    }
    for paths in &lab.paths {
        save_records(paths, &encode_records(log.records())).unwrap();
    }
    log
}

fn pair_from_lab(lab: Lab) -> Pair {
    pair_from_lab_with(lab, 0, Holder::Wrapped)
}

/// [`pair_from_lab`] over a log with `history` rotations of the vault's key past [`seed_log`]'s,
/// held by the second Mac as `holder` says. A [`Holder::Handoff`] Mac has imported the first Mac's
/// handoff of each epoch it cannot open, as the repair rounds before a quiet one would have brought
/// it.
fn pair_from_lab_with(lab: Lab, history: u64, holder: Holder) -> Pair {
    seed_log_with(&lab, history, holder);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    assert!(session.legacy_observation().unwrap().is_some());
    let paths = [
        lab._home.path().join("compat-table-a"),
        lab._home.path().join("compat-table-b"),
    ];
    let mut store = Store::create(&session, &paths[0]).unwrap();
    let scope = store.scope();
    store
        .commit_catalog(&session, b"legacy-compatible-metadata")
        .unwrap();
    drop((store, session));
    let peer = lab.bootstrap(1);
    let session = Session::open(&peer).unwrap();
    Store::join(&session, &paths[1], scope).unwrap();
    drop(session);
    if holder == Holder::Handoff {
        let needs = |bootstrap: &Bootstrap| {
            let session = Session::open(bootstrap).unwrap();
            session.history_needs(None, usize::MAX).unwrap().0
        };
        let older = needs(&peer);
        assert_eq!(
            older.len() as u64,
            2 * (history - 1),
            "CONTROL: the late Mac opens no older epoch of either audience"
        );
        let actor = peer.request().unwrap().binding().actor();
        let handoffs = Session::open(&bootstrap)
            .unwrap()
            .handoffs(actor, &older)
            .unwrap();
        let session = Session::open(&peer).unwrap();
        for (source, handoff) in older.iter().zip(handoffs) {
            let handoff = handoff.expect("the first Mac issues each older epoch's handoff");
            session.import_handoff(*source, &handoff).unwrap();
        }
        drop(session);
        assert!(
            needs(&peer).is_empty(),
            "each older epoch opens by its handoff"
        );
    }
    Pair { lab, scope, paths }
}

pub(super) fn log_pair() -> Pair {
    pair_from_lab(Lab::new(&[Role::Writer, Role::Writer]))
}

fn round(
    pair: &Pair,
    policies: [ProbePolicy; 2],
    repairs: &mut [Repair; 2],
) -> [Result<Report>; 2] {
    let a = pair.replica(0);
    let b = pair.replica(1);
    let (mut left, mut right) = UnixStream::pair().unwrap();
    for stream in [&left, &right] {
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(60)))
            .unwrap();
    }
    let (ra, rb) = repairs.split_at_mut(1);
    let (actor_a, actor_b) = (a.actor, b.actor);
    thread::scope(|scope| {
        let server = scope.spawn(move || {
            b.exchange_with_policy(
                &mut right,
                actor_a,
                Side::Responder,
                &mut rb[0],
                policies[1],
            )
        });
        let client =
            a.exchange_with_policy(&mut left, actor_b, Side::Initiator, &mut ra[0], policies[0]);
        // A rejected old probe closes its connection before the fresh retry.
        drop(left);
        [client, server.join().unwrap()]
    })
}

#[test]
fn old_wire_head_differs_for_the_same_authenticated_log_and_retries_before_data() {
    let pair = log_pair();
    let a = pair.replica(0);
    let current = a.round(pair.replica(1).actor).unwrap();
    let legacy = current.legacy_observation.unwrap();
    assert_eq!(legacy[32..], current.observation[32..]);
    assert_ne!(legacy[..32], current.observation[..32]);
    let mut repairs = [Repair::default(), Repair::default()];
    let first = round(
        &pair,
        [ProbePolicy::Compatible, ProbePolicy::Legacy],
        &mut repairs,
    );
    assert!(first.iter().all(Result::is_err));
    assert!(repairs[0].retry_probe);
    assert_eq!(
        pair.count(1),
        0,
        "a failed probe passed no application record"
    );
    repairs[0].retry_probe = false;
    let second = round(
        &pair,
        [ProbePolicy::Compatible, ProbePolicy::Legacy],
        &mut repairs,
    );
    assert!(second.iter().all(Result::is_ok), "{second:?}");
    assert_eq!(pair.count(1), 1);
    assert!(!repairs[0].retry_probe);
}

#[test]
fn old_initiators_and_current_older_peers_both_keep_their_exact_wire_head() {
    for policies in [
        [ProbePolicy::Legacy, ProbePolicy::Compatible],
        [ProbePolicy::Compatible, ProbePolicy::Projection],
        [ProbePolicy::Projection, ProbePolicy::Compatible],
    ] {
        let pair = log_pair();
        let result = round(&pair, policies, &mut [Repair::default(), Repair::default()]);
        assert!(result.iter().all(Result::is_ok), "{result:?}");
        assert_eq!(pair.count(1), 1);
    }
}

#[test]
fn strict_projection_never_uses_a_cached_legacy_hint_or_retries_a_mismatch() {
    let pair = log_pair();
    let mut repairs = [Repair::default(), Repair::default()];
    repairs[0].legacy_probe = true;
    let result = round(
        &pair,
        [ProbePolicy::Projection, ProbePolicy::Legacy],
        &mut repairs,
    );
    assert!(result.iter().all(Result::is_err));
    assert!(!repairs[0].retry_probe);
    assert_eq!(pair.count(1), 0);
}

#[test]
fn malformed_probes_and_failures_after_ready_are_not_compatibility_retries() {
    let pair = log_pair();
    let a = pair.replica(0);
    let b = pair.replica(1);
    let current = a.round(b.actor).unwrap();
    for payload in [
        b"bad".to_vec(),
        [b"WRONG", &current.observation[..]].concat(),
    ] {
        let mut frame = Vec::new();
        control::send_frame(&mut frame, &payload).unwrap();
        for alternate in [None, Some(current.observation), current.legacy_observation] {
            let error = current
                .receive_probe_value(
                    &mut Cursor::new(&frame),
                    READY,
                    &current.observation,
                    alternate,
                )
                .unwrap_err();
            assert!(!error.retryable());
        }
    }
    let (mut left, mut right) = UnixStream::pair().unwrap();
    left.set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    right
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    let mut repair = Repair::default();
    thread::scope(|scope| {
        let peer = scope.spawn(move || {
            let probe = control::receive_frame(&mut right, 69).unwrap();
            assert_eq!(&probe[..5], PROBE);
            control::send_frame(&mut right, &[READY, &probe[5..]].concat()).unwrap();
            // Deliberately stop after READY, before receiving any key inventory.
        });
        assert!(
            a.exchange_with_compatibility(&mut left, b.actor, Side::Initiator, &mut repair)
                .is_err()
        );
        peer.join().unwrap();
    });
    assert!(!repair.retry_probe);
    assert_eq!(pair.count(1), 0);
}

#[test]
fn equal_legacy_history_never_allows_a_denied_third_member() {
    use aterm_secrets::{
        client::{admin_open, admin_publish},
        swarm_roster::{ROSTER_ENTRY, SwarmRoster},
        text::Audience,
    };
    let lab = Lab::new(&[Role::Writer; 3]);
    let admin = admin_open(&lab.paths[0]).unwrap();
    let mut doc = admin.doc.clone();
    let roster = SwarmRoster {
        members: Vec::new(),
        deny_labels: vec!["m3".to_owned()],
        deny_nodes: Vec::new(),
    };
    doc.set_entry(
        &Audience::Everyone,
        ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .unwrap();
    // Only the first member applies this authenticated vault version. The
    // subsequent signed log and Commons epoch are identical on all members.
    admin_publish(&lab.paths[0], &admin, &doc).unwrap();
    let pair = pair_from_lab(lab);
    let a = pair.replica(0);
    let b = pair.replica(1);
    let first = a.round(b.actor).unwrap();
    let second = b.round(a.actor).unwrap();
    assert_eq!(first.legacy_observation, second.legacy_observation);
    assert_ne!(first.observation, second.observation);
    let mut repairs = [Repair::default(), Repair::default()];
    let strict = round(&pair, [ProbePolicy::Projection; 2], &mut repairs);
    assert!(strict.iter().all(Result::is_err));
    assert!(repairs.iter().all(|repair| !repair.retry_probe));
    let probe = round(&pair, [ProbePolicy::Compatible; 2], &mut repairs);
    assert!(probe.iter().all(Result::is_err));
    assert!(repairs[0].retry_probe);
    repairs[0].retry_probe = false;
    let compatible = round(&pair, [ProbePolicy::Compatible; 2], &mut repairs);
    assert!(compatible.iter().all(Result::is_ok), "{compatible:?}");
    let excluded = pair.lab.bootstrap(2).request().unwrap().binding().actor();
    let bootstrap = pair.lab.bootstrap(0);
    let session = Session::observe(&bootstrap).unwrap();
    assert!(session.peer(excluded).is_err());
    assert!(session.handoffs(b.actor, &[]).is_ok());
    assert!(session.handoffs(excluded, &[]).is_err());
    drop(session);
    let mut bytes = Cursor::new(Vec::new());
    assert!(
        a.exchange_with_compatibility(
            &mut bytes,
            excluded,
            Side::Initiator,
            &mut Repair::default()
        )
        .is_err()
    );
    assert!(
        bytes.into_inner().is_empty(),
        "denied peer received no probe or application bytes"
    );
}

#[test]
fn legacy_probe_alias_does_not_replace_the_local_change_fence() {
    use aterm_secrets::{
        client::{load_records, save_records},
        membership_log::{Body, encode_records},
    };
    let pair = log_pair();
    let a = pair.replica(0);
    let b = pair.replica(1);
    let captured = a.round(b.actor).unwrap();
    let bootstrap = pair.lab.bootstrap(0);
    let request = bootstrap.request().unwrap();
    let stored = load_records(&pair.lab.paths[0]).unwrap();
    let mut log = stored.log().unwrap().clone();
    let removed = log.draft(
        &request.binding().actor(),
        2_000,
        Body::Remove {
            member: b.actor,
            cutoff: *log.network(),
        },
    );
    let record = bootstrap
        .device()
        .with_seed(|seed| removed.sign(seed, *request.binding().incarnation()))
        .unwrap();
    log.insert(record).unwrap();
    save_records(&pair.lab.paths[0], &encode_records(log.records())).unwrap();
    assert!(
        captured
            .local(b.actor, |_| -> Result<()> {
                panic!("a changed local projection must not admit any batch")
            })
            .is_err()
    );
}

#[test]
fn native_atdb2_worker_retries_once_on_a_fresh_authenticated_channel() {
    let pair = log_pair();
    let a = pair.endpoint(0, true);
    let mut b = pair.endpoint(1, true);
    b.legacy_projection = true;
    let listener =
        aterm_net::lan::Listener::bind("127.0.0.1:0".parse().unwrap(), LinkScope::Loopback)
            .unwrap();
    let address = listener.local_addr().unwrap();
    thread::scope(|scope| {
        let server = scope.spawn(|| {
            let mut repair = PeerRepair::default();
            for attempt in 0..2 {
                let connection = aterm_spec::wait::until_within(
                    aterm_spec::wait::MUST_HAPPEN,
                    "fresh authenticated application channel",
                    || match listener.accept() {
                        Ok(connection) => Some(connection),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => None,
                        Err(error) => panic!("application listener failed: {error}"),
                    },
                );
                b.exchange(connection, Side::Responder, &mut repair)
                    .unwrap();
                if attempt == 0 {
                    assert!(repair.application().is_none());
                    assert!(repair.application_error().is_some());
                    assert_eq!(pair.count(1), 0);
                }
            }
            assert!(repair.application().is_some());
            assert!(repair.application_error().is_none());
            assert!(!repair.retry_application_probe());
        });
        let (repair, success, report) =
            crate::runtime::test_worker(a, address, PeerRepair::default());
        assert!(success);
        assert!(report.control.is_ok());
        assert!(repair.application().is_some());
        assert!(repair.application_error().is_none());
        assert!(!repair.retry_application_probe());
        server.join().unwrap();
    });
    assert_eq!(pair.count(1), 1);
}

/// A worker whose redial to an older peer fails (here refused: the peer's listener is gone once
/// the first exchange runs) records that dial's cause at its transport, not the first exchange's
/// probe failure, which the redial was there to recover from.
/// NEGATIVE CONTROL: with the redial keeping what the first exchange left
/// (`Endpoint::redial_keeps_first_attempt`, the worker before CS1), the same round reads as an
/// application failure with the probe's cause, as the second half asserts.
#[test]
fn a_worker_redial_that_fails_is_recorded_with_its_dial_cause() {
    use aterm_types::network_state::{RepairCause, RepairOutcome, RepairStage};
    let recorded = |keeps_first_attempt: bool| {
        let pair = log_pair();
        let mut a = pair.endpoint(0, true);
        a.redial_keeps_first_attempt = keeps_first_attempt;
        let mut b = pair.endpoint(1, true);
        b.legacy_projection = true;
        // A plain socket, so closing it leaves the admitted connection's proof standing.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::scope(|scope| {
            let server = scope.spawn(move || {
                let stream = aterm_spec::wait::until_within(
                    aterm_spec::wait::MUST_HAPPEN,
                    "first application channel",
                    || match aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Ready,
                        aterm_uds::AcceptedMode::Blocking,
                    ) {
                        Ok((stream, _)) => Some(stream),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => None,
                        Err(error) => panic!("application listener failed: {error}"),
                    },
                );
                // Nothing listens once the first exchange runs, so the worker's redial is refused.
                drop(listener);
                let connection = Connection::admit(stream, LinkScope::Loopback).unwrap();
                let mut repair = PeerRepair::default();
                b.exchange(connection, Side::Responder, &mut repair)
                    .unwrap();
                assert!(repair.application_error().is_some());
            });
            let (_, success, report) =
                crate::runtime::test_worker(a, address, PeerRepair::default());
            server.join().unwrap();
            assert!(!success);
            report.observation().unwrap().1
        })
    };
    let fixed = recorded(false);
    assert_eq!(fixed.stage, Some(RepairStage::Transport));
    assert_eq!(
        fixed.outcome,
        RepairOutcome::Failed(RepairCause::Unreachable)
    );
    let mutant = recorded(true);
    assert_eq!(mutant.stage, Some(RepairStage::Application));
    assert!(
        matches!(mutant.outcome, RepairOutcome::Failed(cause) if cause != RepairCause::Unreachable),
        "{mutant:?}"
    );
}

/// This process's user plus system CPU time so far, as the kernel counts it.
fn process_cpu() -> Duration {
    let pid = std::process::id();
    let sweep = aterm_sysprobe::processes().expect("this platform reads its processes");
    let own = sweep
        .procs
        .iter()
        .find(|reading| reading.pid == pid)
        .and_then(|reading| reading.cpu_ns)
        .expect("this process reads its own CPU time");
    Duration::from_nanos(own)
}

/// One repair round between `a` (initiator) and `b` (responder) over loopback TLS.
fn loopback_round(
    listener: &TcpListener,
    (a, b): (&Endpoint, &Endpoint),
    (ra, rb): (&mut PeerRepair, &mut PeerRepair),
) {
    let address = listener.local_addr().unwrap();
    thread::scope(|threads| {
        let server = threads.spawn(|| {
            let connection = Connection::admit(
                aterm_uds::accept(
                    listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .unwrap()
                .0,
                LinkScope::Loopback,
            )
            .unwrap();
            b.exchange(connection, Side::Responder, rb).unwrap();
        });
        let connection =
            Connection::connect(address, LinkScope::Loopback, Duration::from_secs(60)).unwrap();
        a.exchange(connection, Side::Initiator, ra).unwrap();
        server.join().unwrap();
    });
}

/// What a Commons repair round that moves nothing costs: a converged loopback pair of log-backed
/// writers sharing one table, over the current database protocol. It prints the process CPU
/// both sides of each round spent together (run it alone, so no other test's work is counted),
/// beside the cost of the CPU reading itself, and asserts only that the measured rounds moved
/// nothing: a time is never a pass condition. `verify perf commons-quiet` runs it.
#[test]
#[ignore = "isolated CPU measurement; run with verify perf commons-quiet"]
fn a_round_that_moves_nothing_reports_its_cpu() {
    quiet_rounds(8, Workload::Explicit);
}

/// The same quiet pair kept busy long enough to sample its current-source process. This
/// runs alone when requested by `verify perf commons-quiet --sustained`.
#[test]
#[ignore = "sustained isolated CPU measurement; run with verify perf commons-quiet --sustained"]
fn a_sustained_quiet_pair_reports_its_cpu() {
    quiet_rounds(256, Workload::Explicit);
}

/// Ordinary catalog discovery and subscription are part of this workload;
/// neither side receives an explicit application table list.
#[test]
#[ignore = "isolated CPU measurement; run with verify perf commons-quiet --catalog"]
fn a_quiet_catalog_pair_reports_its_cpu() {
    quiet_rounds(8, Workload::Catalog);
}

/// A history ask over a vault history asks the kept historical keys nothing: it asks whether each
/// epoch opens (`Bootstrap::holds_histories`), not for its key, so a round's asks leave the keys
/// its table reads take where those find them. Nine rotations give the log 18 epochs, more than
/// the sixteen keys kept.
/// NEGATIVE CONTROL: `Session::history_needs` asking `Bootstrap::historicals` (and keeping none of
/// its answers) fails the second assertion.
#[test]
fn a_history_ask_over_a_vault_history_asks_no_kept_key() {
    let pair = pair_from_lab_with(Lab::new(&[Role::Writer, Role::Writer]), 9, Holder::Wrapped);
    let bootstrap = pair.lab.bootstrap(1);
    let session = Session::open(&bootstrap).unwrap();
    let asks = aterm_secrets::commons::roster::test_kept_key_asks();
    let (needs, _) = session.history_needs(None, usize::MAX).unwrap();
    assert!(
        needs.is_empty(),
        "CONTROL: the second Mac holds every epoch"
    );
    assert_eq!(
        aterm_secrets::commons::roster::test_kept_key_asks(),
        asks,
        "a history ask asks no kept key"
    );
}

/// What the suite keeps of the measurements above: a converged catalog pair's rounds move nothing
/// and visit both its tables. The asserts are a superset of the explicit pair's.
/// An explicitly listed table's rounds are kept by
/// [`quiet_rounds_over_a_vault_history_move_nothing`].
/// GAP: the `WitnessLogs` loop at the end of `quiet_rounds` runs only when measured
/// (`control::tests` holds its round robin), and so do the explicit pair over [`seed_log`] alone
/// and the [`LONG_LOG_ROTATIONS`] log.
#[test]
fn quiet_catalog_rounds_move_nothing_and_visit_both_tables() {
    quiet_rounds(SUITE_ROUNDS, Workload::Catalog);
}

#[test]
#[ignore = "sustained isolated CPU measurement; run with verify perf commons-quiet --catalog --sustained"]
fn a_sustained_quiet_catalog_pair_reports_its_cpu() {
    quiet_rounds(256, Workload::Catalog);
}

/// [`a_round_that_moves_nothing_reports_its_cpu`] over a long vault history
/// ([`LONG_LOG_ROTATIONS`]): how much a round that moves nothing costs for the records the vault's
/// log holds.
#[test]
#[ignore = "isolated CPU measurement; run with verify perf commons-quiet --long-log"]
fn a_quiet_pair_over_a_long_log_reports_its_cpu() {
    quiet_rounds(8, Workload::LongLog(LONG_LOG_ROTATIONS));
}

#[test]
#[ignore = "sustained isolated CPU measurement; run with verify perf commons-quiet --long-log --sustained"]
fn a_sustained_quiet_pair_over_a_long_log_reports_its_cpu() {
    quiet_rounds(256, Workload::LongLog(LONG_LOG_ROTATIONS));
}

/// What the suite keeps of the long-log measurement: over 41 rotations of the vault's key (one
/// put's name written twice under two keys), a converged pair's rounds move nothing.
#[test]
fn quiet_rounds_over_a_vault_history_move_nothing() {
    quiet_rounds(SUITE_ROUNDS, Workload::LongLog(41));
}

/// [`a_quiet_pair_over_a_long_log_reports_its_cpu`] where the second Mac was admitted after the
/// history and opens each older epoch through a handoff, as most Macs of a fleet do.
#[test]
#[ignore = "isolated CPU measurement; run with verify perf commons-quiet --late-joiner"]
fn a_quiet_late_joiner_over_a_long_log_reports_its_cpu() {
    quiet_rounds(8, Workload::LateJoiner(LONG_LOG_ROTATIONS));
}

#[test]
#[ignore = "sustained isolated CPU measurement; run with verify perf commons-quiet --late-joiner --sustained"]
fn a_sustained_quiet_late_joiner_over_a_long_log_reports_its_cpu() {
    quiet_rounds(256, Workload::LateJoiner(LONG_LOG_ROTATIONS));
}

/// What the suite keeps of the late-joiner measurement: a Mac that opens the 80 older epochs of 40
/// rotations (an `[everyone]` and a Commons epoch each) through handoffs takes part in rounds that
/// move nothing.
#[test]
fn quiet_rounds_of_a_late_joiner_move_nothing() {
    quiet_rounds(SUITE_ROUNDS, Workload::LateJoiner(41));
}

#[test]
#[ignore = "process-wide retained-history diagnostic; run this exact test alone with --ignored --nocapture"]
fn a_quiet_catalog_pair_reports_retained_history_reads() {
    quiet_rounds_observed(8, Workload::Catalog, true);
}

#[test]
#[ignore = "process-wide single-root policy diagnostic; run this exact test alone with --ignored --nocapture"]
fn one_root_policy_reads_report_their_retained_history_work() {
    let pair = catalog_pair();
    let bootstrap = pair.lab.bootstrap(0);
    let _source = bootstrap.local_source_read().unwrap();
    let session = Session::observe(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(&pair.lab.paths[0], bootstrap.trust());
    let policy = crate::catalog::policy::Policy::default();
    let catalog = directory.catalog_scope();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let inventories = || {
        [catalog, pair.scope].map(|scope| {
            directory
                .observe_table(&session, scope)
                .unwrap()
                .inventory(&session)
                .unwrap()
                .copied()
                .collect::<BTreeSet<_>>()
        })
    };
    let before_inventory = inventories();
    let catalog_path = directory.path(catalog).unwrap();
    let journal_paths = [
        catalog_path.join("table.astream"),
        catalog_path.join("table.checkpoint"),
        pair.paths[0].join("table.astream"),
        pair.paths[0].join("table.checkpoint"),
    ];
    let journal_images = || {
        journal_paths
            .iter()
            .map(|path| std::fs::read(path).unwrap())
            .collect::<Vec<_>>()
    };
    let before_images = journal_images();
    assert!(
        directory
            .permits(&session, &policy, pair.scope.digest(), now)
            .unwrap()
    );
    // Amortize the process-clock sweep over enough calls to report the policy path,
    // rather than setup or a single timer reading. Run this test alone.
    let calls = 128_u32;
    let clock_start = process_cpu();
    let clock_read = process_cpu() - clock_start;
    let observe = |id| {
        let before = aterm_secrets::client::test_retained_history_read_counts();
        let start = process_cpu();
        let mut allowed = true;
        for _ in 0..calls {
            allowed &= directory.permits(&session, &policy, id, now).unwrap();
        }
        let cpu = process_cpu() - start;
        let counts = aterm_secrets::client::test_retained_history_read_counts()
            .checked_delta(before)
            .expect("single-root counters did not decrease");
        (allowed, counts, cpu)
    };
    let (catalog_allowed, catalog_reads, catalog_cpu) = observe(catalog.digest());
    let (application_allowed, application_reads, application_cpu) = observe(pair.scope.digest());
    assert!(catalog_allowed && application_allowed);
    assert_eq!(catalog_reads, Default::default());
    assert!(application_reads.same_directory.lookups > 0);
    assert_eq!(application_reads.empty, Default::default());
    assert_eq!(application_reads.other_directory, Default::default());
    assert_eq!(application_reads.unavailable, Default::default());
    assert_eq!(application_reads.same_directory.rebuilt, 0);
    assert_eq!(application_reads.same_directory.extended, 0);
    assert_eq!(application_reads.same_directory.errors, 0);
    assert_eq!(application_reads.replay_records_attempted, 0);
    assert_eq!(application_reads.publications, 0);
    assert_eq!(inventories(), before_inventory);
    assert!(
        journal_images() == before_images,
        "policy observation changed journal bytes"
    );
    eprintln!(
        "\nCOMMONS-POLICY-READS {}",
        aterm_json::json!({
            "schema": 2,
            "scope": "retained-history-read-cache",
            "workload": "single-root-policy-permits",
            "calls_per_case": calls,
            "clock_read_cpu_ns": u64::try_from(clock_read.as_nanos()).unwrap(),
            "catalog_process_cpu_ns": u64::try_from(catalog_cpu.as_nanos()).unwrap(),
            "application_process_cpu_ns": u64::try_from(application_cpu.as_nanos()).unwrap(),
            "source_guard_held": true,
            "catalog_allowed": catalog_allowed,
            "application_allowed": application_allowed,
            "catalog_counts": history_read_counts_value(catalog_reads),
            "application_counts": history_read_counts_value(application_reads),
            "source_inventories_unchanged": true,
            "source_journal_images_unchanged": true,
        })
    );
}

fn catalog_pair() -> Pair {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    seed_log(&lab);
    let bootstrap = lab.bootstrap(0);
    let session = Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(&lab.paths[0], bootstrap.trust());
    let scope = directory
        .open_application(&session, "quiet-catalog", "rows", 1)
        .unwrap();
    directory
        .open_store(&session, scope)
        .unwrap()
        .commit_catalog(&session, b"quiet catalog fixture")
        .unwrap();
    let peer = crate::catalog::Directory::new(&lab.paths[1], bootstrap.trust());
    let paths = [directory.path(scope).unwrap(), peer.path(scope).unwrap()];
    assert!(!paths[1].exists(), "the receiver must discover the table");
    drop(session);
    Pair { lab, scope, paths }
}

fn catalog_inventories(pair: &Pair) -> [BTreeMap<Digest, BTreeSet<Digest>>; 2] {
    std::array::from_fn(|n| {
        let bootstrap = pair.lab.bootstrap(n);
        let session = Session::observe(&bootstrap).unwrap();
        let directory = crate::catalog::Directory::new(&pair.lab.paths[n], bootstrap.trust());
        [directory.catalog_scope(), pair.scope]
            .into_iter()
            .map(|scope| {
                let store = directory.observe_table(&session, scope).unwrap();
                (
                    scope.digest(),
                    store.inventory(&session).unwrap().copied().collect(),
                )
            })
            .collect()
    })
}

/// What a quiet-round measurement repairs ([`quiet_rounds`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Workload {
    /// One explicitly listed table, over a short log ([`log_pair`]).
    Explicit,
    /// A catalog-discovered application table beside the catalog ([`catalog_pair`]).
    Catalog,
    /// [`Workload::Explicit`] over this many rotations of the vault's key past [`seed_log`]'s
    /// ([`LONG_LOG_ROTATIONS`] when measured).
    LongLog(u64),
    /// [`Workload::LongLog`] whose second Mac opens every epoch but the newest through a
    /// handoff ([`Holder::Handoff`]).
    LateJoiner(u64),
}

impl Workload {
    /// Its name in a report, which `verify perf commons-quiet` checks.
    const fn word(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Catalog => "catalog",
            Self::LongLog(_) => "long-log",
            Self::LateJoiner(_) => "late-joiner",
        }
    }
    /// How many tables each of its rounds visits.
    const fn tables(self) -> u64 {
        match self {
            Self::Catalog => 2,
            Self::Explicit | Self::LongLog(_) | Self::LateJoiner(_) => 1,
        }
    }
    /// How many rotations of the vault's key its log holds past [`seed_log`]'s.
    const fn history(self) -> u64 {
        match self {
            Self::LongLog(rotations) | Self::LateJoiner(rotations) => rotations,
            Self::Explicit | Self::Catalog => 0,
        }
    }
}

/// How many rounds the suite's own runs of [`quiet_rounds`] take. A run of more is a measurement
/// (`verify perf commons-quiet`'s, and the retained-history diagnostic's): it waits for the
/// fixture's files to settle before the rounds it reports on.
const SUITE_ROUNDS: u32 = 2;

/// How many rounds a measurement runs again after its wait for the fixture's files to settle,
/// before the rounds it measures, as a live Mac's rounds run back to back. The rounds just after
/// that idle run slowly while the CPU comes back to speed: a short pair's took 21 to 25 ms of wall
/// time, falling over about seven rounds to the 8.6 ms back-to-back rounds take, and a reading of
/// the process's CPU time, the same work each time, cost 5.7 to 7.9 ms just after the wait where
/// it costs about 2 ms (2026-11-09).
const AFTER_IDLE_WARM: u32 = 8;

fn quiet_rounds(measured: u32, workload: Workload) {
    quiet_rounds_observed(measured, workload, false);
}

fn quiet_rounds_observed(measured: u32, workload: Workload, history_reads: bool) {
    let catalog = workload == Workload::Catalog;
    let warm = if catalog { 8 } else { 2 };
    let pair = match workload {
        Workload::Explicit => log_pair(),
        Workload::Catalog => catalog_pair(),
        Workload::LongLog(epochs) => pair_from_lab_with(
            Lab::new(&[Role::Writer, Role::Writer]),
            epochs,
            Holder::Wrapped,
        ),
        Workload::LateJoiner(epochs) => pair_from_lab_with(
            Lab::new(&[Role::Writer, Role::Writer]),
            epochs,
            Holder::Handoff,
        ),
    };
    let endpoint = |n| {
        if catalog {
            let deadline = Instant::now() + Duration::from_secs(60);
            Endpoint::from_bootstrap_until(&pair.lab.bootstrap(n), deadline)
                .unwrap()
                .unwrap()
                .with_commons_until(
                    pair.lab.paths[n].clone(),
                    pair.lab.paths[n].home.join("config.toml"),
                    deadline,
                )
                .unwrap()
        } else {
            pair.endpoint(n, true)
        }
    };
    let a = endpoint(0);
    let b = endpoint(1);
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    for _ in 0..warm {
        loopback_round(&listener, (&a, &b), (&mut ra, &mut rb));
    }
    let inventories = catalog.then(|| catalog_inventories(&pair));
    if let Some([left, right]) = &inventories {
        assert_eq!(left, right, "catalog warm-up must converge operation IDs");
        assert_eq!(left[&pair.scope.digest()].len(), 1);
        assert!(left.values().all(|records| !records.is_empty()));
    }
    let mut visited = BTreeSet::new();
    let measuring = measured > SUITE_ROUNDS;
    let mut warmed = warm;
    if measuring {
        // An idle Mac's rounds asked long ago whether it holds each epoch of its history, sixteen
        // a round on each side (`replica::BATCH`), and it answers each one that opened from what
        // opened it (`Bootstrap::holds_histories`), where this pair's warm-up asked 32 of the
        // 2,400 epochs a long log's rotations add: each side asks every one once, as a whole cycle
        // of rounds would, so the rounds measured ask what an idle Mac's ask.
        for n in 0..2 {
            let bootstrap = pair.lab.bootstrap(n);
            let session = Session::open(&bootstrap).unwrap();
            let (needs, _) = session.history_needs(None, usize::MAX).unwrap();
            assert!(needs.is_empty(), "a converged pair holds every epoch");
        }
        // An idle Mac's vault and journals stood unchanged long before its rounds, where this
        // pair's setup and warm-up wrote them just now, and a journal view or a file digest of a
        // file changed within the settled time of a read reads it again at each ask
        // (`aterm_secrets::client::test_settled_after`).
        // A measurement waits that time out, and a tenth of a second past it, so its rounds read
        // what an idle Mac's read, and then warms the CPU again ([`AFTER_IDLE_WARM`]). Measured
        // at once, the first two seconds of a sustained run's rounds, and all eight of the
        // default lane's, read the control journal again, and over the short log the vault's
        // files too: a sustained short run opened 588 files a round where a settled one opened
        // 241 (2026-11-09).
        std::thread::sleep(
            aterm_secrets::client::test_settled_after() + Duration::from_millis(100),
        );
        for _ in 0..AFTER_IDLE_WARM {
            loopback_round(&listener, (&a, &b), (&mut ra, &mut rb));
        }
        warmed += AFTER_IDLE_WARM;
    }
    if measured > 8 {
        eprintln!(
            "\nCOMMONS-QUIET-READY {}",
            aterm_json::json!({"pid": std::process::id(), "rounds": measured})
        );
    }
    let reads_before = history_reads.then(aterm_secrets::client::test_retained_history_read_counts);
    let opens = [a.test_bootstrap_opens(), b.test_bootstrap_opens()];
    let reading = process_cpu();
    let start = process_cpu();
    let reading = start - reading;
    for _ in 0..measured {
        loopback_round(&listener, (&a, &b), (&mut ra, &mut rb));
        for repair in [&ra, &rb] {
            let application = repair.application().expect("a completed application round");
            if catalog {
                assert!(repair.application_error().is_none());
                assert_eq!(application.deferred, 0, "a measured round deferred repair");
                visited.insert(application.table.expect("a catalog workload table"));
            }
            assert_eq!(
                application.sent
                    + application.received
                    + application.keys_sent
                    + application.keys_received
                    + application.conflicts_sent
                    + application.conflicts_received,
                0,
                "a measured round moved records"
            );
            let membership = repair.membership().expect("a membership exchange");
            assert_eq!(membership.sent + membership.received, 0);
        }
    }
    let spent = process_cpu() - start;
    assert_eq!(
        [a.test_bootstrap_opens(), b.test_bootstrap_opens()],
        opens,
        "a quiet round takes the bootstrap its side kept and opens none"
    );
    // Every round has joined its responder. End the read-count interval before
    // inventory verification and the separate witness microchecks below.
    let reads = reads_before.map(|before| {
        aterm_secrets::client::test_retained_history_read_counts()
            .checked_delta(before)
            .expect("retained-history counters never reset or wrap in this diagnostic")
    });
    if let Some(before) = &inventories {
        let [left, _] = before;
        assert_eq!(visited, left.keys().copied().collect());
        assert_eq!(&catalog_inventories(&pair), before);
    }
    if let Some(reads) = reads {
        assert!(
            reads.same_directory.lookups > 0,
            "the actual cache must be observed"
        );
        eprintln!(
            "\nCOMMONS-HISTORY-READS {}",
            aterm_json::json!({
                "schema": 1,
                "scope": "retained-history-read-cache",
                "workload": workload.word(),
                "tables": workload.tables(),
                "warmup_rounds": warmed,
                "rounds": measured,
                "moved_records": 0,
                "counts": history_read_counts_value(reads),
            })
        );
        return;
    }
    // Both Macs hold the same records file: the history every check of a round reads.
    let records = aterm_secrets::client::load_records(&pair.lab.paths[0]).unwrap();
    eprintln!(
        "COMMONS-QUIET {}",
        aterm_json::json!({
            "schema": 1,
            "workload": workload.word(),
            "tables": workload.tables(),
            "history_rotations": workload.history(),
            "log_records": records.len(),
            "log_bytes": records.encode().len(),
            "warmup_rounds": warmed,
            "rounds": measured,
            "process_cpu_ns": u64::try_from(spent.as_nanos()).unwrap(),
            "per_round_cpu_ns": u64::try_from((spent / measured).as_nanos()).unwrap(),
            "clock_read_cpu_ns": u64::try_from(reading.as_nanos()).unwrap(),
            "moved_records": 0,
        })
    );
    eprintln!(
        "COMMONS-NOOP-ROUND process CPU, both sides, per round over {measured} rounds: {:?} \
         (one CPU reading costs {reading:?})",
        spent / measured,
    );
    // A measuring run only: the witness costs below are numbers nothing here asserts on.
    if !measuring {
        return;
    }
    // What a round paid for its peer's witness when it was not held: before the cache held one
    // log per roster peer, every round with more than 16 peers round-robin paid it.
    let peer = pair.lab.bootstrap(1);
    let request = peer.request().unwrap();
    let witness = peer.log_witness().unwrap().expect("a log-backed peer");
    let witness = aterm_secrets::membership_log::encode_records(witness.records());
    let check = |logs: &crate::control::WitnessLogs| {
        logs.check(peer.trust(), &request, &witness).unwrap();
    };
    const CHECKS: u32 = 200;
    let start = process_cpu();
    for _ in 0..CHECKS {
        check(&crate::control::WitnessLogs::new(1, usize::MAX));
    }
    let missed = process_cpu() - start;
    let held = crate::control::WitnessLogs::new(1, usize::MAX);
    check(&held);
    let start = process_cpu();
    for _ in 0..CHECKS {
        check(&held);
    }
    let hit = process_cpu() - start;
    eprintln!(
        "COMMONS-WITNESS process CPU per check of a {} byte witness: decoded {:?}, held {:?}",
        witness.len(),
        missed / CHECKS,
        hit / CHECKS,
    );
}

fn history_read_counts_value(
    counts: aterm_secrets::client::RetainedHistoryReadCounts,
) -> aterm_json::Value {
    use aterm_secrets::client::RetainedHistoryReadOutcomes;

    let rows = [
        counts.empty,
        counts.same_directory,
        counts.other_directory,
        counts.unavailable,
    ];
    assert_eq!(
        counts.journal_read_attempts,
        rows.iter()
            .map(|row| row.lookups - row.unmoved)
            .sum::<u64>()
    );
    for row in rows {
        assert_eq!(
            row.lookups,
            row.unmoved + row.unchanged + row.extended + row.rebuilt + row.errors
        );
    }
    for row in [counts.empty, counts.other_directory, counts.unavailable] {
        assert_eq!(
            row.unchanged, 0,
            "unchanged reuse requires a supplied prefix"
        );
        assert_eq!(row.extended, 0, "suffix replay requires a supplied prefix");
        assert_eq!(row.unmoved, 0, "unmoved reuse requires a cached history");
    }
    assert_eq!(
        counts.replay_records_attempted,
        counts.replay_records_accepted + counts.replay_failures
    );
    assert!(counts.replacements <= counts.publications);
    assert!(
        counts.publications
            <= rows
                .iter()
                .map(|row| row.extended + row.rebuilt)
                .sum::<u64>()
    );
    let row = |value: RetainedHistoryReadOutcomes| {
        aterm_json::json!({
            "lookups": value.lookups,
            "unmoved": value.unmoved,
            "unchanged": value.unchanged,
            "extended": value.extended,
            "rebuilt": value.rebuilt,
            "errors": value.errors,
        })
    };
    aterm_json::json!({
        "empty": row(counts.empty),
        "same_directory": row(counts.same_directory),
        "other_directory": row(counts.other_directory),
        "unavailable": row(counts.unavailable),
        "journal_read_attempts": counts.journal_read_attempts,
        "replay_records_attempted": counts.replay_records_attempted,
        "replay_records_accepted": counts.replay_records_accepted,
        "replay_failures": counts.replay_failures,
        "publications": counts.publications,
        "replacements": counts.replacements,
    })
}

/// A replica's rounds take the bootstrap it keeps while that stands (`Bootstrap::stands`), and
/// open one afresh, and keep it, only once it falls: quiet rounds open none, a vault publication
/// makes the next round open one and the round after it none, and a device claim replaced since
/// the open makes the next round meet that open's refusal.
///
/// NEGATIVE CONTROL: `Replica::bootstrap` opening at every round fails the quiet rounds' count;
/// one taking what it keeps unchecked fails at the publication (the round's own admission meets
/// the changed vault); one returning the refusal of what it kept in place of opening fails at the
/// publication.
#[test]
fn a_replica_keeps_its_bootstrap_while_it_stands() {
    use aterm_secrets::client;
    let pair = log_pair();
    let replica = pair.replica(0);
    for _ in 0..4 {
        replica.round(None).unwrap();
    }
    assert_eq!(
        replica.test_bootstrap_opens(),
        0,
        "quiet rounds take the bootstrap the replica opened"
    );
    let paths = &pair.lab.paths[0];
    let admin = client::admin_open(paths).unwrap();
    client::admin_publish(paths, &admin, &admin.doc).unwrap();
    replica.round(None).unwrap();
    assert_eq!(
        replica.test_bootstrap_opens(),
        1,
        "a vault publication makes the next round open afresh"
    );
    replica.round(None).unwrap();
    assert_eq!(
        replica.test_bootstrap_opens(),
        1,
        "the round after it takes the bootstrap that open kept"
    );
    let claim = paths.dir.join("commons/device.claim");
    let mut bytes = std::fs::read(&claim).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(&claim, bytes).unwrap();
    let refused = replica
        .round(None)
        .err()
        .expect("a replaced device claim refuses the round");
    assert!(
        refused
            .to_string()
            .contains("Commons device differs from its immutable claim"),
        "the round meets the fresh open's refusal: {refused}"
    );
}

/// A table round's reads stand on the check each store's open ran in their session
/// (`Round::store`), and its matching inventory on the operation inventory's fence
/// (`Round::fenced_observation`): over the explicit pair, whose first Mac holds a record the
/// second lacks until the first round sends it, each of the initiator's rounds checks each store
/// it opens once. The first round opens its store four times (its tables' witnesses, the conflict
/// proof inventory, the operation inventory and the batch that sends the record) and observes no
/// matching inventory, since the two inventories differ; each later round opens it twice (the
/// conflict proof inventory and the operation inventory) and observes one. The inventory reads
/// checked each store again, seven checks for the first round's four opens and six for a later
/// round's three, whose third open was the matching inventory's (2026-11-09).
/// NEGATIVE CONTROL: `Round::inventory`, the conflict round's proof inventory or
/// `table_witnesses` reading through a checked accessor fails it, and so does the matching
/// inventory reading the store again where its fence stands.
#[test]
fn each_round_checks_a_store_once_as_it_opens() {
    let pair = log_pair();
    let (a, b) = (pair.endpoint(0, true), pair.endpoint(1, true));
    let (mut ra, mut rb) = (PeerRepair::default(), PeerRepair::default());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut opened = Vec::new();
    for _ in 0..=SUITE_ROUNDS {
        let (opens, checks) = crate::table::store::test_opens_and_checks();
        loopback_round(&listener, (&a, &b), (&mut ra, &mut rb));
        let (now_opens, now_checks) = crate::table::store::test_opens_and_checks();
        let (opens, checks) = (now_opens - opens, now_checks - checks);
        assert_eq!(checks, opens, "a round checks each store it opens once");
        let matched = ra.application().unwrap().matching_inventory.is_some();
        opened.push((opens, matched));
    }
    assert_eq!(
        opened[0],
        (4, false),
        "the first round with a peer reads its tables' witnesses and sends the record its peer \
         lacks"
    );
    assert!(
        opened[1..].iter().all(|round| *round == (2, true)),
        "a converged round opens its store for its two inventories and observes its matching \
         inventory on the fence of the second: {opened:?}"
    );
}
