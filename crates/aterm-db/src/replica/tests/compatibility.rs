// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::net::UnixStream;

fn seed_log(lab: &Lab) -> aterm_secrets::membership_log::Log {
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
    for paths in &lab.paths {
        save_records(paths, &encode_records(log.records())).unwrap();
    }
    log
}

fn pair_from_lab(lab: Lab) -> Pair {
    seed_log(&lab);
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
    Pair { lab, scope, paths }
}

fn log_pair() -> Pair {
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
            let connection =
                Connection::admit(listener.accept().unwrap().0, LinkScope::Loopback).unwrap();
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
    quiet_rounds(8, false);
}

/// The same quiet pair kept busy long enough to sample its current-source process. This
/// runs alone when requested by `verify perf commons-quiet --sustained`.
#[test]
#[ignore = "sustained isolated CPU measurement; run with verify perf commons-quiet --sustained"]
fn a_sustained_quiet_pair_reports_its_cpu() {
    quiet_rounds(256, false);
}

/// Ordinary catalog discovery and subscription are part of this workload;
/// neither side receives an explicit application table list.
#[test]
#[ignore = "isolated CPU measurement; run with verify perf commons-quiet --catalog"]
fn a_quiet_catalog_pair_reports_its_cpu() {
    quiet_rounds(8, true);
}

/// What the suite keeps of the measurements above: a converged catalog pair's rounds move nothing
/// and visit both its tables. The asserts are a superset of the explicit pair's.
/// GAP: the explicit pair runs only when measured, so a defect that moves records only on an
/// explicitly listed table is not seen here, and neither is the `WitnessLogs` loop at the end of
/// `quiet_rounds` (`control::tests` holds its round robin).
#[test]
fn quiet_catalog_rounds_move_nothing_and_visit_both_tables() {
    quiet_rounds(2, true);
}

#[test]
#[ignore = "sustained isolated CPU measurement; run with verify perf commons-quiet --catalog --sustained"]
fn a_sustained_quiet_catalog_pair_reports_its_cpu() {
    quiet_rounds(256, true);
}

#[test]
#[ignore = "process-wide retained-history diagnostic; run this exact test alone with --ignored --nocapture"]
fn a_quiet_catalog_pair_reports_retained_history_reads() {
    quiet_rounds_observed(8, true, true);
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
    assert!(application_reads.journal_read_attempts > 0);
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

fn quiet_rounds(measured: u32, catalog: bool) {
    quiet_rounds_observed(measured, catalog, false);
}

fn quiet_rounds_observed(measured: u32, catalog: bool, history_reads: bool) {
    let warm = if catalog { 8 } else { 2 };
    let pair = if catalog { catalog_pair() } else { log_pair() };
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
    if measured > 8 {
        eprintln!(
            "\nCOMMONS-QUIET-READY {}",
            aterm_json::json!({"pid": std::process::id(), "rounds": measured})
        );
    }
    let reads_before = history_reads.then(aterm_secrets::client::test_retained_history_read_counts);
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
            reads.journal_read_attempts > 0,
            "the actual cache must be observed"
        );
        eprintln!(
            "\nCOMMONS-HISTORY-READS {}",
            aterm_json::json!({
                "schema": 1,
                "scope": "retained-history-read-cache",
                "workload": if catalog { "catalog" } else { "explicit" },
                "tables": if catalog { 2 } else { 1 },
                "warmup_rounds": warm,
                "rounds": measured,
                "moved_records": 0,
                "counts": history_read_counts_value(reads),
            })
        );
        return;
    }
    eprintln!(
        "COMMONS-QUIET {}",
        aterm_json::json!({
            "schema": 1,
            "workload": if catalog { "catalog" } else { "explicit" },
            "tables": if catalog { 2 } else { 1 },
            "warmup_rounds": warm,
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
    if measured < 8 {
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
        rows.iter().map(|row| row.lookups).sum::<u64>()
    );
    for row in rows {
        assert_eq!(
            row.lookups,
            row.unchanged + row.extended + row.rebuilt + row.errors
        );
    }
    for row in [counts.empty, counts.other_directory, counts.unavailable] {
        assert_eq!(
            row.unchanged, 0,
            "unchanged reuse requires a supplied prefix"
        );
        assert_eq!(row.extended, 0, "suffix replay requires a supplied prefix");
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
