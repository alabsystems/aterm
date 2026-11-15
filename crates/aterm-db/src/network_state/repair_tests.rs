// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn cause_json_round_trips_without_diagnostic_text() {
    for cause in [
        RepairCause::LegacyPastCrossing,
        RepairCause::TableUndeclared,
        RepairCause::Quota,
        RepairCause::Io,
        RepairCause::LanHeld,
        RepairCause::Unreachable,
        RepairCause::Decode,
        RepairCause::NotReady,
        RepairCause::NeedsUpdate,
        RepairCause::Other,
    ] {
        for outcome in [
            RepairOutcome::Failed(cause),
            RepairOutcome::Deferred(Some(cause)),
        ] {
            let mut observation = failed(1000);
            observation.outcome = outcome;
            let encoded = value(&observation);
            assert_eq!(encoded["cause"].as_str(), Some(cause.word()));
            assert_eq!(
                read(&json!({"last_repair":encoded}), "last_repair").unwrap(),
                Some(observation)
            );
        }
    }
    let mut deferred = failed(1000);
    deferred.outcome = RepairOutcome::Deferred(None);
    assert!(value(&deferred)["cause"].is_null());
    assert_eq!(
        read(&json!({"last_repair":value(&deferred)}), "last_repair").unwrap(),
        Some(deferred)
    );
    // A word this build does not know (a newer aterm's) reads as `other`, keeping none of it.
    let private = with(value(&failed(1000)), "cause", json!("private payload"));
    let read_back = read(&json!({"last_repair":private}), "last_repair")
        .unwrap()
        .unwrap();
    assert_eq!(read_back.outcome, RepairOutcome::Failed(RepairCause::Other));
    assert!(!format!("{read_back:?}").contains("private"));
    assert!(
        read(
            &json!({"last_repair":with(value(&failed(1000)), "cause", json!(7))}),
            "last_repair"
        )
        .is_err()
    );
    // An outcome that never names a cause refuses one.
    let exchanged = with(value(&failed(1000)), "outcome", json!("exchanged"));
    assert!(read(&json!({"last_repair":exchanged}), "last_repair").is_err());
}

/// A failure an older aterm recorded without its cause (m22's 43 `failed (cause
/// unavailable)` rows) reads as `other` with its stage, table and counts, so no failure
/// without a cause reaches a reader and what it received still counts; a stopped attempt
/// keeps reading with an older writer's cause dropped.
#[test]
fn a_failure_recorded_without_its_cause_reads_as_other_with_its_counts() {
    let mut legacy = with(value(&failed(1000)), "cause", Value::Null);
    let expected = RepairObservation {
        outcome: RepairOutcome::Failed(RepairCause::Other),
        ..failed(1000)
    };
    assert_eq!(
        read(&json!({"last_repair":legacy.clone()}), "last_repair").unwrap(),
        Some(expected.clone())
    );
    assert_eq!(expected.received, Some(3));
    legacy.as_object_mut().unwrap().remove("cause");
    assert_eq!(
        read(&json!({"last_repair":legacy}), "last_repair").unwrap(),
        Some(expected)
    );
    let stopped = with(value(&failed(1000)), "outcome", json!("cancelled"));
    let read_back = read(&json!({"last_repair":stopped}), "last_repair")
        .unwrap()
        .unwrap();
    assert_eq!(read_back.outcome, RepairOutcome::Cancelled);
    assert_eq!(read_back.cause(), None);
}

fn identity(n: u8) -> MachineIdentity {
    MachineIdentity {
        machine_id: [n; 16],
        incarnation: [1; 16],
        label: None,
    }
}

/// `row` with `key` set to `field`.
fn with(mut row: Value, key: &str, field: Value) -> Value {
    row.as_object_mut().unwrap().insert(key.into(), field);
    row
}

fn failed(at: u64) -> RepairObservation {
    RepairObservation {
        completed_at_ms: at,
        outbound: true,
        stage: Some(RepairStage::Application),
        outcome: RepairOutcome::Failed(RepairCause::Io),
        table: Some([7; 32]),
        sent: Some(2),
        received: Some(3),
        deferred: Some(0),
    }
}

fn trust() -> aterm_secrets::commons::journal::Trust {
    aterm_secrets::commons::journal::Trust {
        network: [1; 16],
        authority: [2; 32],
        genesis: [3; 32],
    }
}

#[test]
fn repair_history_preserves_failure_without_refreshing_contact_or_relabelling_a_peer() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("aterm.toml");
    let published = ObservationHandle::default();
    let mut observer = Observer::open(&config, published.clone()).unwrap();
    let peer = identity(2);
    let now = Instant::now();
    observer.local(identity(1), trust(), Vec::new).unwrap();
    observer.repair(&peer, failed(1500), now);
    assert!(
        observer.state.peers.is_empty(),
        "a report cannot create an observed peer"
    );
    observer.contact(peer.clone(), 1000, now, REACHABLE);
    observer.repair(&peer, failed(1500), now);
    observer.repair(&peer, failed(1200), now - Duration::from_secs(1));
    observer.observe(
        Visibility::Active,
        0,
        "observing",
        2000,
        now + Duration::from_secs(1),
    );
    let state = published.snapshot();
    assert_eq!(state.peers[0].last_seen_ms, 1000);
    assert_eq!(state.peers[0].last_repair, Some(failed(1500)));
    let history = read_history(&config).unwrap();
    assert_eq!(history.peers[0].last_repair, Some(failed(1500)));
    assert_eq!(history.peers[0].reachability, Reachability::Unknown);
    assert_eq!(
        value(&history.peers[0].last_repair.clone().unwrap())["outcome"],
        json!("failed")
    );
    let expired = published
        .try_publication()
        .unwrap()
        .current_at(now + Duration::from_secs(60), Some(61_000));
    assert_eq!(expired.peers[0].last_repair, Some(failed(1500)));
    assert_eq!(expired.peers[0].reachability, Reachability::Unknown);

    let mut replacement = peer.clone();
    replacement.incarnation = [2; 16];
    observer.repair(&replacement, failed(2500), now + Duration::from_secs(2));
    assert_eq!(observer.state.peers[0].last_repair, Some(failed(1500)));
    observer.contact(
        replacement.clone(),
        3000,
        now + Duration::from_secs(2),
        REACHABLE,
    );
    assert!(observer.state.peers[0].last_repair.is_none());
    observer.repair(&peer, failed(3500), now + Duration::from_secs(3));
    assert!(observer.state.peers[0].last_repair.is_none());
    observer.repair(&replacement, failed(3500), now + Duration::from_secs(3));
    assert_eq!(observer.state.peers[0].last_repair, Some(failed(3500)));
    observer.repair(&replacement, failed(500), now + Duration::from_secs(4));
    assert_eq!(
        observer.state.peers[0].last_repair,
        Some(failed(500)),
        "wall rollback does not suppress a later attempt"
    );
    observer.unconfirm_local();
    observer.repair(&replacement, failed(4000), now + Duration::from_secs(5));
    assert_eq!(observer.state.peers[0].last_repair, Some(failed(500)));
    let mut new_local = identity(1);
    new_local.incarnation = [2; 16];
    observer.local(new_local, trust(), Vec::new).unwrap();
    assert!(observer.state.peers[0].last_repair.is_none());
    assert!(observer.repair_sampled.is_empty());
}

#[test]
fn old_history_is_compatible_and_malformed_repair_metadata_is_refused() {
    assert!(read(&json!({}), "last_repair").unwrap().is_none());
    assert!(
        read(&json!({"last_repair":null}), "last_repair")
            .unwrap()
            .is_none()
    );
    let valid = json!({"last_repair":value(&failed(1000))});
    assert_eq!(read(&valid, "last_repair").unwrap(), Some(failed(1000)));
    assert_eq!(
        read(&valid, "last_catalog_repair").unwrap(),
        None,
        "each key keeps its own round"
    );
    assert_eq!(
        read(
            &json!({"last_catalog_repair":value(&failed(1000))}),
            "last_catalog_repair"
        )
        .unwrap(),
        Some(failed(1000))
    );
    let changes: [(&str, Value); 5] = [
        ("completed_at_ms", json!(0)),
        ("stage", json!("private remote text")),
        ("outcome", json!("all tables synchronized")),
        ("outbound", json!("true")),
        ("received", Value::Null),
    ];
    for (key, replacement) in changes {
        let mut invalid = valid.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .get_mut("last_repair")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.into(), replacement);
        assert!(read(&invalid, "last_repair").is_err(), "{key}");
    }
}

#[test]
fn repair_metadata_fits_a_full_bounded_history_checkpoint() {
    let stamp = 1_793_564_553_000;
    let mut observation = failed(stamp);
    observation.outcome = RepairOutcome::Exchanged;
    observation.sent = Some(u64::MAX);
    observation.received = Some(u64::MAX);
    observation.deferred = Some(u64::MAX);
    let state = NetworkObservation {
        observer: Some(identity(1)),
        observed_at_ms: stamp,
        peers: (0..MAX_PEERS)
            .map(|slot| PeerObservation {
                slot: u64::try_from(slot).unwrap(),
                identity: identity(u8::try_from(slot).unwrap()),
                first_seen_ms: stamp,
                last_seen_ms: stamp,
                last_checked_ms: Some(stamp),
                last_repair: Some(observation.clone()),
                last_authenticated_endpoint: Some(AuthenticatedEndpoint {
                    address: "[2001:db8:aaaa:bbbb:cccc:dddd:eeee:ffff]:65535"
                        .parse()
                        .unwrap(),
                    observed_at_ms: stamp,
                }),
                reachability: Reachability::Reachable,
                instances: None,
                samples: vec![
                    PresenceSample {
                        at_ms: stamp,
                        state: Reachability::Reachable
                    };
                    MAX_SAMPLES
                ],
                lease_ms: None,
            })
            .collect(),
        last_catalog_repair: Some(observation.clone()),
        ..Default::default()
    };
    let wire = encode(&state, Some(trust())).unwrap();
    let decoded = decode(&wire).unwrap();
    assert_eq!(decoded.peers.len(), MAX_PEERS);
    assert!(
        decoded
            .peers
            .iter()
            .all(|peer| peer.last_repair == Some(observation.clone()))
    );
    assert_eq!(decoded.last_catalog_repair, Some(observation));
}

/// A round over `table` that completed at `at` as `outcome`, having received `received`.
fn over(table: [u8; 32], at: u64, outcome: RepairOutcome, received: u64) -> RepairObservation {
    RepairObservation {
        completed_at_ms: at,
        outbound: true,
        stage: Some(RepairStage::Application),
        outcome,
        table: Some(table),
        sent: Some(0),
        received: Some(received),
        deferred: Some(0),
    }
}

/// The lab's latest round over the catalog that said something of it stays kept through the
/// rounds over other tables that follow it, with its peer and with others: in the live
/// observation, in the history and across a restart. A later round over the catalog that
/// deferred or brought records replaces it, with whichever peer; one that failed or was
/// stopped with nothing does not, and neither does one sampled before it. The catalog is the
/// observer's own lab's: the same table id under another trust is another table. The kept
/// round names no peer, so a peer's new installation leaves it; this Mac's own starts with
/// none, as does an observer that opens a history left unclosed, and a history written before
/// the round was kept reads with none.
/// MUTANT (`CATALOG_UNKEPT`): a round over the catalog is kept only as its peer's latest.
///
/// NEGATIVE CONTROLS (2026-11-12, each by an inverse edit then undone, each failing the
/// reading below that states it): a round that said nothing replacing the kept one; a round
/// sampled before the kept one replacing it; the record not carrying the kept round; this
/// Mac's new installation keeping the old one's round, and the old one's round still ordering
/// the new one's; an unclosed history's kept round reaching the next observer.
#[test]
fn the_latest_round_over_the_catalog_is_kept_through_the_rounds_over_other_tables() {
    let catalog = crate::catalog::scope(trust()).digest();
    let other = [7; 32];
    assert_ne!(catalog, other);
    let exchanged = |table, at| over(table, at, RepairOutcome::Exchanged, 0);
    let (peer, second) = (identity(2), identity(3));
    for mutant in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("aterm.toml");
        let published = ObservationHandle::default();
        let mut observer = Observer::open(&config, published.clone()).unwrap();
        let now = Instant::now();
        let after = |seconds: u64| now + Duration::from_secs(seconds);
        observer.local(identity(1), trust(), Vec::new).unwrap();
        observer.contact(peer.clone(), 1000, now, REACHABLE);
        observer.contact(second.clone(), 1000, now, REACHABLE);
        CATALOG_UNKEPT.with(|flag| flag.set(mutant));
        observer.repair(&peer, exchanged(catalog, 1100), after(10));
        // A round with another peer that was sampled before it and joined after it is not
        // the latest, whatever it brought.
        let earlier = over(catalog, 1050, RepairOutcome::Exchanged, 3);
        observer.repair(&second, earlier.clone(), after(9));
        assert_eq!(observer.state.peers[1].last_repair, Some(earlier));
        observer.repair(&peer, exchanged(other, 1200), after(11));
        observer.repair(&second, exchanged(other, 1250), after(12));
        CATALOG_UNKEPT.with(|flag| flag.set(false));
        assert_eq!(
            observer.state.peers[0].last_repair,
            Some(exchanged(other, 1200))
        );
        if mutant {
            assert_eq!(
                observer.state.last_catalog_repair, None,
                "MUTANT: only each peer's latest"
            );
            continue;
        }
        let kept = Some(exchanged(catalog, 1100));
        assert_eq!(observer.state.last_catalog_repair, kept);
        // Another lab's catalog is another table here.
        let foreign = aterm_secrets::commons::journal::Trust {
            network: [9; 16],
            ..trust()
        };
        observer.repair(
            &peer,
            exchanged(crate::catalog::scope(foreign).digest(), 1300),
            after(13),
        );
        assert_eq!(observer.state.last_catalog_repair, kept);
        // A round over the catalog that failed or was stopped with nothing says nothing.
        let failed = over(catalog, 1400, RepairOutcome::Failed(RepairCause::Io), 0);
        observer.repair(&second, failed.clone(), after(14));
        assert_eq!(observer.state.peers[1].last_repair, Some(failed));
        let stopped = over(catalog, 1450, RepairOutcome::Cancelled, 0);
        observer.repair(&second, stopped, after(15));
        assert_eq!(observer.state.last_catalog_repair, kept);
        // The live observation carries it, and so does the history its observer closes.
        observer.observe(Visibility::Active, 0, "observing", 2000, after(16));
        let live = published.snapshot();
        assert_eq!(live.last_catalog_repair, kept);
        drop(observer);
        let closed = read_closed_history(&config).unwrap().expect("closed");
        assert_eq!(closed.last_catalog_repair, kept);
        assert_ne!(closed.peers[0].last_repair, closed.last_catalog_repair);
        // A history from before the round was kept names none, and reads.
        let mut old: Value =
            aterm_json::from_slice(&encode(&closed, Some(trust())).unwrap()).unwrap();
        old.as_object_mut()
            .and_then(|record| record.remove("last_catalog_repair"))
            .expect("the record names it");
        let read_back = decode(&aterm_json::to_vec(&old).unwrap()).unwrap();
        assert_eq!(read_back.last_catalog_repair, None);
        assert_eq!(read_back.peers[0].last_repair, closed.peers[0].last_repair);
        // The next observer keeps a closed history's round. A later round over the catalog
        // that brought records replaces it, with whichever peer, and so does one that
        // deferred, and one that failed after it took a record.
        let mut observer = Observer::open(&config, published.clone()).unwrap();
        observer.local(identity(1), trust(), Vec::new).unwrap();
        assert_eq!(observer.state.last_catalog_repair, kept);
        for (at, outcome, received) in [
            (3000, RepairOutcome::Exchanged, 2),
            (3100, RepairOutcome::Deferred(None), 0),
            (3200, RepairOutcome::Failed(RepairCause::Io), 1),
            (3300, RepairOutcome::Exchanged, 0),
        ] {
            let later = over(catalog, at, outcome, received);
            observer.repair(&second, later.clone(), after(at / 100));
            observer.repair(&second, exchanged(other, at + 50), after(at / 100));
            assert_eq!(observer.state.last_catalog_repair, Some(later));
        }
        // A peer's new installation starts with no latest round, and the kept round, which
        // names no peer, stays.
        let mut replacement = second.clone();
        replacement.incarnation = [2; 16];
        observer.contact(replacement.clone(), 4000, after(40), REACHABLE);
        assert_eq!(observer.state.peers[1].last_repair, None);
        assert_eq!(
            observer.state.last_catalog_repair,
            Some(exchanged(catalog, 3300))
        );
        // This Mac's own new installation starts with none, and takes the next it is told.
        let mut new_local = identity(1);
        new_local.incarnation = [2; 16];
        observer.local(new_local, trust(), Vec::new).unwrap();
        assert_eq!(observer.state.last_catalog_repair, None);
        observer.repair(&peer, exchanged(catalog, 5000), after(5));
        assert_eq!(
            observer.state.last_catalog_repair,
            Some(exchanged(catalog, 5000)),
            "the round before the installation orders nothing after it"
        );
        // A history its observer did not close keeps no round for the next one.
        observer.observe(Visibility::Active, 0, "observing", 5000, after(50));
        assert!(read_history(&config).unwrap().last_catalog_repair.is_some());
        observer.die();
        let reopened = Observer::open(&config, ObservationHandle::default()).unwrap();
        assert_eq!(reopened.state.peers[0].last_repair, None);
        assert_eq!(reopened.state.last_catalog_repair, None);
    }
}
