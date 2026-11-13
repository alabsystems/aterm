// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(target_os = "macos")]
#[test]
fn legacy_history_moves_only_after_its_writer_releases_it() {
    use std::os::unix::fs::MetadataExt as _;
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let legacy = config.with_file_name("network-state");
    let mut observer = Observer::open(&config, handle()).unwrap();
    observer.local(peer(1, 1), trust()).unwrap();
    let at = Instant::now();
    observer.accounts_exchange(account_exchange::Candidate {
        observation: exchange(),
        sampled_at: at,
    });
    observer.observe(
        Visibility::Active,
        0,
        "observing",
        2000,
        at + Duration::from_secs(1),
    );
    drop(observer);
    let quiet = directory(&config);
    // Reconstruct a pre-migration store, including its real journal and sidecars.
    std::fs::remove_file(&legacy).unwrap();
    std::fs::rename(&quiet, &legacy).unwrap();
    let inode = std::fs::metadata(legacy.join("observations.astream"))
        .unwrap()
        .ino();
    let owner =
        BrokerLog::open_with(legacy.join("observations.astream"), Durability::Strict).unwrap();
    assert!(matches!(
        Observer::open(&config, handle()),
        Err(OpenFailure::Owned)
    ));
    assert!(legacy.is_dir());
    drop(owner);
    let observer = Observer::open(&config, handle()).unwrap();
    assert_eq!(observer.state.accounts_exchange, Some(exchange()));
    assert!(aterm_tempfile::is_quiet(&directory(&config)));
    assert_eq!(
        std::fs::metadata(directory(&config).join("observations.astream"))
            .unwrap()
            .ino(),
        inode
    );
    assert!(legacy.is_file());
    assert!(std::fs::create_dir_all(&legacy).is_err());
}

fn reachable_publication(at: Instant) -> Publication {
    Publication {
        observation: NetworkObservation {
            revision: 23,
            observed_at_ms: 100_000,
            visibility: Visibility::Active,
            history: HistoryState::Ready,
            observer: Some(peer(1, 1)),
            peers: vec![PeerObservation {
                slot: 1,
                identity: peer(2, 1),
                first_seen_ms: 99_000,
                last_seen_ms: 100_000,
                last_checked_ms: Some(100_000),
                last_repair: None,
                last_authenticated_endpoint: None,
                reachability: Reachability::Reachable,
                instances: None,
                samples: vec![PresenceSample {
                    at_ms: 100_000,
                    state: Reachability::Reachable,
                }],
                lease_ms: None,
            }],
            ..NetworkObservation::default()
        },
        at,
        contact_ages: BTreeMap::from([(1, Duration::ZERO)]),
    }
}

#[test]
fn publication_cannot_become_current_again_after_wall_clock_rollback() {
    let at = Instant::now();
    let publication = reachable_publication(at);
    let current = publication.current_at(at, Some(100_000));
    assert_eq!(current.visibility, Visibility::Active);
    assert_eq!(current.peers[0].reachability, Reachability::Reachable);
    for (now, wall) in [
        (at + Duration::from_secs(20), Some(120_000)),
        (at + Duration::from_secs(21), Some(113_000)),
        (at - Duration::from_secs(1), Some(100_000)),
        (at, Some(99_999)),
        (at, None),
    ] {
        let expired = publication.current_at(now, wall);
        assert_eq!(expired.visibility, Visibility::Unavailable);
        assert_eq!(expired.peers[0].reachability, Reachability::Unknown);
        let mut expected = current.clone();
        expected.visibility = expired.visibility;
        expected.detail.clone_from(&expired.detail);
        expected.peers[0].reachability = Reachability::Unknown;
        assert_eq!(expired, expected, "expiry changed retained evidence");
        assert_eq!(
            value(&expired)["peers"][0]["reachability"],
            json!("unknown")
        );
    }
    assert_eq!(publication.observation, current);
}

#[test]
fn a_nonactive_publication_has_no_current_peer_claim() {
    let at = Instant::now();
    for (visibility, observed_at_ms) in [
        (Visibility::Unavailable, 100_000),
        (Visibility::Checking, 0),
        (Visibility::Checking, 100_000),
        (Visibility::Offline, 100_000),
        (Visibility::Disabled, 100_000),
        (Visibility::Waiting, 100_000),
    ] {
        let mut publication = reachable_publication(at);
        publication.observation.visibility = visibility;
        publication.observation.observed_at_ms = observed_at_ms;
        let current = publication.current_at(at, Some(100_000));
        let mut expected = publication.observation.clone();
        expected.peers[0].reachability = Reachability::Unknown;
        assert_eq!(
            current, expected,
            "{visibility:?} kept a current peer claim"
        );
        assert_eq!(
            publication.observation.peers[0].reachability,
            Reachability::Reachable
        );
    }
}

#[test]
fn peer_age_includes_elapsed_time_since_its_published_observation() {
    let at = Instant::now();
    let mut publication = Publication {
        observation: NetworkObservation {
            observed_at_ms: 100_000,
            visibility: Visibility::Active,
            peers: vec![PeerObservation {
                slot: 1,
                identity: peer(2, 1),
                first_seen_ms: 91_000,
                last_seen_ms: 91_000,
                last_checked_ms: Some(91_000),
                last_repair: None,
                last_authenticated_endpoint: None,
                reachability: Reachability::Reachable,
                instances: None,
                samples: Vec::new(),
                lease_ms: None,
            }],
            ..NetworkObservation::default()
        },
        at,
        contact_ages: BTreeMap::from([(1, Duration::from_secs(9))]),
    };
    assert_eq!(
        publication
            .current_at(at + Duration::from_secs(6), Some(101_000))
            .peers[0]
            .reachability,
        Reachability::Reachable
    );
    assert_eq!(
        publication
            .current_at(at + Duration::from_secs(7), Some(101_000))
            .peers[0]
            .reachability,
        Reachability::LastSeen
    );
    for checked in [None, Some(0), Some(100_001)] {
        publication.observation.peers[0].last_checked_ms = checked;
        assert_eq!(
            publication.current_at(at, Some(101_000)).peers[0].reachability,
            Reachability::Unknown
        );
    }
}

#[test]
fn checkpoint_publication_preserves_the_input_sample_instant() {
    let root = aterm_tempfile::tempdir().unwrap();
    let handle = ObservationHandle::default();
    let mut observer = Observer::open(&root.path().join("config"), handle.clone()).unwrap();
    let sampled = Instant::now() - Duration::from_secs(20);
    observer.observe(Visibility::Active, 0, "sampled", 100_000, sampled);
    let publication = handle.try_publication().unwrap();
    assert_eq!(publication.at, sampled);
    assert_eq!(
        publication
            .current_at(sampled + Duration::from_secs(21), Some(101_000))
            .visibility,
        Visibility::Unavailable
    );
}

#[test]
fn contact_age_survives_a_wall_rollback_before_publication() {
    let root = aterm_tempfile::tempdir().unwrap();
    let handle = ObservationHandle::default();
    let mut observer = Observer::open(&root.path().join("config"), handle.clone()).unwrap();
    let at = Instant::now();
    observer.contact(peer(2, 1), 100_000, at);
    observer.observe(
        Visibility::Active,
        0,
        "sampled",
        102_000,
        at + Duration::from_secs(10),
    );
    let publication = handle.try_publication().unwrap();
    assert_eq!(
        publication.observation.peers[0].reachability,
        Reachability::Reachable
    );
    assert_eq!(
        publication
            .current_at(at + Duration::from_secs(17), Some(109_000))
            .peers[0]
            .reachability,
        Reachability::LastSeen
    );
}

#[test]
fn missing_or_overflowing_contact_age_cannot_authorize_reachability() {
    let at = Instant::now();
    let mut publication = Publication::new(
        NetworkObservation {
            observed_at_ms: 100_000,
            visibility: Visibility::Active,
            peers: vec![PeerObservation {
                slot: 1,
                identity: peer(2, 1),
                first_seen_ms: 99_000,
                last_seen_ms: 99_000,
                last_checked_ms: Some(99_000),
                last_repair: None,
                last_authenticated_endpoint: None,
                reachability: Reachability::Reachable,
                instances: None,
                samples: Vec::new(),
                lease_ms: None,
            }],
            ..NetworkObservation::default()
        },
        at,
    );
    assert_eq!(
        publication.current_at(at, Some(100_000)).peers[0].reachability,
        Reachability::Unknown
    );
    publication.contact_ages.insert(1, Duration::MAX);
    assert_eq!(
        publication
            .current_at(at + Duration::from_secs(1), Some(101_000))
            .peers[0]
            .reachability,
        Reachability::Unknown
    );
}
#[test]
fn contended_snapshot_has_no_new_evidence_and_recovers_after_publish() {
    let handle = handle();
    let mut publisher = handle.0.lock().unwrap();
    publisher.observation.observed_at_ms = 1000;
    publisher.observation.revision = 7;
    let reader = handle.clone();
    let (send, receive) = std::sync::mpsc::channel();
    let task = std::thread::spawn(move || send.send(reader.try_snapshot()).unwrap());
    let result = receive.recv_timeout(Duration::from_secs(60));
    drop(publisher);
    task.join().unwrap();
    assert_eq!(
        result.unwrap(),
        None,
        "a busy publisher must not stall the reader"
    );
    let published = handle.try_snapshot().unwrap();
    assert_eq!(published.observed_at_ms, 1000);
    assert_eq!(published.revision, 7);
}

fn peer(machine: u8, incarnation: u8) -> MachineIdentity {
    MachineIdentity {
        machine_id: [machine; 16],
        incarnation: [incarnation; 16],
        label: None,
    }
}
fn handle() -> ObservationHandle {
    ObservationHandle::default()
}
fn trust() -> aterm_secrets::commons::journal::Trust {
    aterm_secrets::commons::journal::Trust {
        network: [1; 16],
        authority: [2; 32],
        genesis: [3; 32],
    }
}
fn exchange() -> AccountsExchangeObservation {
    AccountsExchangeObservation {
        scope_digest: [3; 32],
        local_actor: [4; 48],
        control_observation: [5; 64],
        inventory_count: 2,
        inventory_fingerprint: [6; 32],
        peer: peer(2, 1),
        sampled_at_ms: 1000,
    }
}
#[test]
fn historical_exchange_codec_is_bounded_and_old_history_has_no_witness() {
    let observation = NetworkObservation {
        accounts_exchange: Some(exchange()),
        ..NetworkObservation::default()
    };
    let wire = encode(&observation, None).unwrap();
    assert_eq!(
        decode(&wire).unwrap().accounts_exchange,
        observation.accounts_exchange
    );
    let mut old: Value = aterm_json::from_slice(&wire).unwrap();
    old.as_object_mut().unwrap().remove("accounts_exchange");
    assert_eq!(
        decode(&aterm_json::to_vec(&old).unwrap())
            .unwrap()
            .accounts_exchange,
        None
    );
    for (field, value) in [
        ("scope_digest", json!("short")),
        ("local_actor", json!("short")),
        ("control_observation", json!("short")),
        ("inventory_fingerprint", json!("short")),
        ("inventory_count", json!(crate::replica::MAX_RECORDS + 1)),
        ("sampled_at_ms", json!(0)),
    ] {
        let mut invalid: Value = aterm_json::from_slice(&wire).unwrap();
        invalid
            .as_object_mut()
            .unwrap()
            .get_mut("accounts_exchange")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(field.into(), value);
        assert!(
            decode(&aterm_json::to_vec(&invalid).unwrap()).is_err(),
            "{field}"
        );
    }
}
#[test]
fn journal_reopen_preserves_exchange_without_renewing_contact_or_sample() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let at = Instant::now();
    let expected = exchange();
    let mut observer = Observer::open(&config, handle()).unwrap();
    observer.local(peer(1, 1), trust()).unwrap();
    observer.accounts_exchange(account_exchange::Candidate {
        observation: expected.clone(),
        sampled_at: at,
    });
    let mut replay = expected.clone();
    replay.sampled_at_ms = 99_000;
    observer.accounts_exchange(account_exchange::Candidate {
        observation: replay,
        sampled_at: at,
    });
    observer.observe(
        Visibility::Active,
        0,
        "observing",
        2000,
        at + Duration::from_secs(1),
    );
    assert_eq!(observer.state.accounts_exchange, Some(expected.clone()));
    assert!(
        observer.state.peers.is_empty(),
        "historical agreement is not a contact"
    );
    assert_eq!(
        read_history(&config).unwrap().accounts_exchange,
        Some(expected.clone())
    );
    drop(observer);
    let mut reopened = Observer::open(&config, handle()).unwrap();
    assert_eq!(reopened.accounts_exchange_sampled, None);
    reopened.observe(
        Visibility::Disabled,
        0,
        "disabled",
        50_000,
        at + Duration::from_secs(60),
    );
    assert_eq!(reopened.state.accounts_exchange, Some(expected));
    assert!(reopened.state.peers.is_empty());
}
#[test]
fn unchanged_account_exchanges_publish_live_and_checkpoint_periodically() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let at = Instant::now();
    let published = handle();
    let mut observer = Observer::open(&config, published.clone()).unwrap();
    observer.local(peer(1, 1), trust()).unwrap();
    observer.contact(peer(2, 1), 1000, at);
    observer.contact(peer(3, 1), 1000, at);
    let projection = |observer: &Observer| {
        private_file::read_private(&observer.dir, "projection.json", MAX_BYTES).unwrap()
    };
    let first = exchange();
    observer.accounts_exchange(account_exchange::Candidate {
        observation: first.clone(),
        sampled_at: at,
    });
    observer.observe(Visibility::Active, 0, "observing", 2000, at);
    let first_head = observer.log.as_ref().unwrap().stats().head;
    let first_projection = projection(&observer);
    assert_eq!(
        read_history(&config).unwrap().accounts_exchange,
        Some(first.clone())
    );

    let repeated = AccountsExchangeObservation {
        sampled_at_ms: 3000,
        ..first
    };
    observer.accounts_exchange(account_exchange::Candidate {
        observation: repeated.clone(),
        sampled_at: at + Duration::from_secs(1),
    });
    observer.observe(
        Visibility::Active,
        0,
        "observing",
        3000,
        at + Duration::from_secs(1),
    );
    assert_eq!(
        published.snapshot().accounts_exchange,
        Some(repeated.clone())
    );
    assert_eq!(observer.log.as_ref().unwrap().stats().head, first_head);
    assert_eq!(projection(&observer), first_projection);

    let alternate = AccountsExchangeObservation {
        peer: peer(3, 1),
        sampled_at_ms: 4000,
        ..repeated
    };
    observer.accounts_exchange(account_exchange::Candidate {
        observation: alternate.clone(),
        sampled_at: at + Duration::from_secs(2),
    });
    observer.observe(
        Visibility::Active,
        0,
        "observing",
        4000,
        at + Duration::from_secs(2),
    );
    assert_eq!(
        published.snapshot().accounts_exchange,
        Some(alternate.clone())
    );
    assert_eq!(
        observer.log.as_ref().unwrap().stats().head,
        first_head,
        "an alternate peer does not turn unchanged local evidence into a durable transition"
    );
    assert_eq!(projection(&observer), first_projection);

    observer.contact(peer(2, 1), 62_000, at + CHECKPOINT);
    observer.contact(peer(3, 1), 62_000, at + CHECKPOINT);
    observer.observe(Visibility::Active, 0, "observing", 62_000, at + CHECKPOINT);
    let periodic_head = observer.log.as_ref().unwrap().stats().head;
    assert!(periodic_head > first_head);
    assert_eq!(
        read_history(&config).unwrap().accounts_exchange,
        Some(alternate.clone())
    );
    let log = observer.log.as_ref().unwrap();
    let records = log.read_range(observer.latest.unwrap(), 1);
    assert_eq!(
        decode(&records[0].body).unwrap().accounts_exchange,
        Some(alternate.clone()),
        "the journal stores the latest original sample, not checkpoint time"
    );

    let mut inventory_changed = alternate;
    inventory_changed.inventory_fingerprint[0] ^= 1;
    inventory_changed.sampled_at_ms = 5000;
    observer.accounts_exchange(account_exchange::Candidate {
        observation: inventory_changed.clone(),
        sampled_at: at + CHECKPOINT + Duration::from_secs(1),
    });
    observer.observe(
        Visibility::Active,
        0,
        "observing",
        63_000,
        at + CHECKPOINT + Duration::from_secs(1),
    );
    assert_eq!(
        published.snapshot().accounts_exchange,
        Some(inventory_changed.clone())
    );
    assert_eq!(
        observer.log.as_ref().unwrap().stats().head,
        periodic_head,
        "changed retained IDs are a transition, which waits for the settle interval"
    );
    observer.observe(
        Visibility::Active,
        0,
        "observing",
        63_000 + SETTLE.as_millis() as u64,
        at + CHECKPOINT + SETTLE,
    );
    let inventory_head = observer.log.as_ref().unwrap().stats().head;
    assert_eq!(inventory_head.0, periodic_head.0 + 1);
    assert_eq!(
        read_history(&config).unwrap().accounts_exchange,
        Some(inventory_changed.clone())
    );

    let mut control_changed = inventory_changed;
    control_changed.control_observation[0] ^= 1;
    control_changed.sampled_at_ms = 6000;
    observer.accounts_exchange(account_exchange::Candidate {
        observation: control_changed.clone(),
        sampled_at: at + CHECKPOINT + SETTLE + Duration::from_secs(1),
    });
    observer.observe(
        Visibility::Active,
        0,
        "observing",
        94_000,
        at + CHECKPOINT + SETTLE + Duration::from_secs(1),
    );
    assert_eq!(observer.log.as_ref().unwrap().stats().head, inventory_head);
    observer.observe(
        Visibility::Active,
        0,
        "observing",
        123_000,
        at + CHECKPOINT + 2 * SETTLE,
    );
    assert_eq!(
        observer.log.as_ref().unwrap().stats().head.0,
        inventory_head.0 + 1,
        "a changed control context is written with the next checkpoint"
    );
    assert_eq!(
        read_history(&config).unwrap().accounts_exchange,
        Some(control_changed)
    );
}

#[test]
fn retained_endpoint_requires_exact_identity_and_never_refreshes_reachability() {
    let root = aterm_tempfile::tempdir().unwrap();
    let now = Instant::now();
    let mut observer = Observer::open(&root.path().join("config"), handle()).unwrap();
    let identity = peer(2, 1);
    let endpoint = AuthenticatedEndpoint {
        address: "192.0.2.3:41384".parse().unwrap(),
        observed_at_ms: 2000,
    };
    observer.endpoint(&identity, endpoint.clone());
    assert!(observer.state.peers.is_empty());
    observer.contact(identity.clone(), 1000, now);
    observer.endpoint(&peer(3, 1), endpoint.clone());
    observer.endpoint(&peer(2, 2), endpoint.clone());
    assert!(
        observer.state.peers[0]
            .last_authenticated_endpoint
            .is_none()
    );
    observer.endpoint(&identity, endpoint.clone());
    observer.observe(
        Visibility::Active,
        1,
        "old contact",
        20_000,
        now + Duration::from_secs(19),
    );
    assert_eq!(observer.state.peers[0].reachability, Reachability::LastSeen);
    assert_eq!(observer.state.peers[0].last_seen_ms, 1000);
    assert_eq!(
        observer.state.peers[0].last_authenticated_endpoint,
        Some(endpoint.clone())
    );
    let restored = decode(&encode(&observer.state, None).unwrap()).unwrap();
    assert_eq!(
        restored.peers[0].last_authenticated_endpoint,
        Some(endpoint)
    );
    assert_eq!(restored.peers[0].reachability, Reachability::Unknown);
    observer.contact(peer(2, 2), 21_000, now + Duration::from_secs(20));
    assert!(
        observer.state.peers[0]
            .last_authenticated_endpoint
            .is_none()
    );
}

#[test]
fn old_version_one_history_has_no_endpoint_and_future_address_age_is_unknown() {
    let mut observation = reachable_publication(Instant::now()).observation;
    let wire = encode(&observation, None).unwrap();
    let mut old: Value = aterm_json::from_slice(&wire).unwrap();
    old.as_object_mut()
        .unwrap()
        .get_mut("peers")
        .unwrap()
        .as_array_mut()
        .unwrap()[0]
        .as_object_mut()
        .unwrap()
        .remove("last_authenticated_endpoint");
    let decoded = decode(&aterm_json::to_vec(&old).unwrap()).unwrap();
    assert!(decoded.peers[0].last_authenticated_endpoint.is_none());
    assert_eq!(decoded.peers[0].identity, observation.peers[0].identity);
    observation.peers[0].last_authenticated_endpoint = Some(AuthenticatedEndpoint {
        address: "[::1]:41384".parse().unwrap(),
        observed_at_ms: u64::MAX,
    });
    let encoded = encode(&observation, None).unwrap();
    assert_eq!(
        aterm_json::from_slice::<Value>(&encoded).unwrap()["version"],
        json!(1)
    );
    assert_eq!(
        decode(&encoded).unwrap().peers[0].last_authenticated_endpoint,
        observation.peers[0].last_authenticated_endpoint
    );
    assert_eq!(
        value(&observation)["peers"][0]["last_authenticated_endpoint"],
        json!({"address":"[::1]:41384","observed_at_ms":u64::MAX,"age_ms":null})
    );
}
#[test]
fn history_restores_stable_slots_but_never_live_reachability() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("aterm.toml");
    let now = Instant::now();
    let out = handle();
    let mut observer = Observer::open(&config, out.clone()).unwrap();
    observer.local(peer(1, 1), trust()).unwrap();
    observer.contact(peer(2, 1), 1000, now);
    observer.observe(Visibility::Active, 12, "observing", 1000, now);
    assert!(
        matches!(Observer::open(&config, handle()), Err(OpenFailure::Owned)),
        "a concurrent observer is ownership contention, not corrupt history"
    );
    assert_eq!(
        out.snapshot().peers[0].reachability,
        Reachability::Reachable
    );
    assert_eq!(observer.log.as_ref().unwrap().stats().retained_records, 1);
    let checkpoint_at = now + CHECKPOINT;
    observer.contact(peer(2, 1), 61_000, checkpoint_at);
    observer.observe(Visibility::Active, 12, "observing", 61_000, checkpoint_at);
    assert_eq!(
        observer.log.as_ref().unwrap().stats().retained_records,
        2,
        "a small journal is appended, not compacted"
    );
    let read = read_history(&config).unwrap();
    assert_eq!(read.visibility, Visibility::Unavailable);
    assert_eq!(
        read.observer, None,
        "saved identity is not this reader's identity"
    );
    assert_eq!(read.peers.len(), 1, "address hints never become machines");
    assert_eq!(read.peers[0].reachability, Reachability::Unknown);
    assert_eq!(read.peers[0].last_checked_ms, None);
    assert_eq!(read.peers[0].last_seen_ms, 61_000);
    drop(observer);
    let restarted = handle();
    let mut reopened = Observer::open(&config, restarted.clone()).unwrap();
    assert_eq!(restarted.snapshot().observer, None);
    assert_eq!(
        reopened.state.observer,
        Some(peer(1, 1)),
        "retain the identity guard internally"
    );
    reopened.local(peer(1, 2), trust()).unwrap();
    assert_eq!(reopened.state.peers[0].reachability, Reachability::Unknown);
    reopened.contact(peer(2, 2), 62_000, checkpoint_at + Duration::from_secs(1));
    reopened.contact(peer(3, 1), 62_000, checkpoint_at + Duration::from_secs(1));
    reopened.observe(
        Visibility::Active,
        0,
        "observing",
        62_000,
        checkpoint_at + Duration::from_secs(1),
    );
    assert_eq!(restarted.snapshot().observer, Some(peer(1, 2)));
    assert_eq!(
        reopened
            .state
            .peers
            .iter()
            .map(|p| p.slot)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(reopened.state.peers[0].first_seen_ms, 1000);
    assert_eq!(reopened.state.peers[0].identity.incarnation, [2; 16]);
    reopened.unconfirm_local();
    reopened.publish();
    assert_eq!(restarted.snapshot().observer, None);
    assert_eq!(reopened.state.observer, Some(peer(1, 2)));
    assert_eq!(restarted.snapshot().peers.len(), 2);
    reopened.local(peer(1, 2), trust()).unwrap();
    assert!(
        reopened.local(peer(9, 1), trust()).is_err(),
        "another observer cannot inherit prior contacts"
    );
    reopened.publish();
    assert_eq!(restarted.snapshot().observer, None);
    let mut foreign = trust();
    foreign.network = [7; 16];
    assert!(
        reopened.local(peer(1, 1), foreign).is_err(),
        "another network cannot inherit identity slots"
    );
}
#[test]
fn wall_time_jump_and_unavailable_observer_withdraw_current_claims() {
    let root = aterm_tempfile::tempdir().unwrap();
    let now = Instant::now();
    let mut observer = Observer::open(&root.path().join("config"), handle()).unwrap();
    observer.contact(peer(2, 1), 1000, now);
    observer.observe(Visibility::Active, 0, "ok", 1000, now);
    observer.observe(
        Visibility::Active,
        0,
        "ok",
        100_000,
        now + Duration::from_millis(100),
    );
    assert_eq!(
        observer.state.peers[0].reachability,
        Reachability::LastSeen,
        "sleep cannot preserve a stale green cell"
    );
    observer.contact(peer(2, 1), 100_001, now + Duration::from_secs(1));
    observer.observe(
        Visibility::Unavailable,
        0,
        "unavailable",
        100_001,
        now + Duration::from_secs(1),
    );
    assert_eq!(observer.state.peers[0].reachability, Reachability::Unknown);
    assert_eq!(
        observer.state.peers[0].samples.last().unwrap().state,
        Reachability::Unknown
    );
}
#[test]
fn repeated_contacts_coalesce_and_history_is_bounded() {
    let root = aterm_tempfile::tempdir().unwrap();
    let now = Instant::now();
    let mut observer = Observer::open(&root.path().join("config"), handle()).unwrap();
    observer.contact(peer(2, 1), 1000, now);
    observer.observe(Visibility::Active, 0, "ok", 1000, now);
    let head = observer.log.as_ref().unwrap().stats().head;
    for tick in 1..10 {
        observer.contact(
            peer(2, 1),
            1000 + tick * 1000,
            now + Duration::from_secs(tick),
        );
        observer.observe(
            Visibility::Active,
            tick as usize,
            "ok",
            1000 + tick * 1000,
            now + Duration::from_secs(tick),
        );
    }
    assert_eq!(
        observer.log.as_ref().unwrap().stats().head,
        head,
        "packets and repeated success do not append per heartbeat"
    );
    for tick in 10..50 {
        observer.observe(
            Visibility::Unavailable,
            0,
            "unavailable",
            tick * 1000,
            now + Duration::from_secs(tick),
        );
        observer.contact(peer(2, 1), tick * 1000, now + Duration::from_secs(tick));
        observer.observe(
            Visibility::Active,
            0,
            "ok",
            tick * 1000,
            now + Duration::from_secs(tick),
        );
    }
    assert_eq!(
        observer.log.as_ref().unwrap().stats().head.0,
        head.0 + 1,
        "forty seconds of a flapping peer make one checkpoint"
    );
    assert_eq!(observer.state.peers[0].samples.len(), MAX_SAMPLES);
}
#[test]
fn read_only_history_does_not_create_state() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    assert_eq!(
        read_history(&config).unwrap().history,
        HistoryState::Unavailable
    );
    let observed = read_history(&config).unwrap();
    assert_eq!(observed.visibility, Visibility::Unavailable);
    assert!(!observed.detail.is_empty());
    assert!(!directory(&config).exists());
}
#[test]
fn corrupt_projection_cannot_replace_authoritative_journal_history() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let now = Instant::now();
    let mut observer = Observer::open(&config, handle()).unwrap();
    observer.contact(peer(2, 1), 1000, now);
    observer.observe(Visibility::Active, 0, "ok", 1000, now);
    drop(observer);
    private_file::write_private(&directory(&config), "projection.json", b"bad cache").unwrap();
    assert!(read_history(&config).is_err());
    let restored = Observer::open(&config, handle()).unwrap();
    assert_eq!(restored.state.peers[0].identity.machine_id, [2; 16]);
    drop(restored);
    let path = directory(&config).join("observations.astream");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 1;
    std::fs::write(path, bytes).unwrap();
    assert!(
        Observer::open(&config, handle()).is_err(),
        "cache never heals a corrupt journal"
    );
}
#[test]
fn identities_are_bounded_without_reassigning_old_slots() {
    let root = aterm_tempfile::tempdir().unwrap();
    let now = Instant::now();
    let config = root.path().join("config");
    let mut observer = Observer::open(&config, handle()).unwrap();
    for n in 0..=MAX_PEERS {
        let mut identity = peer(0, 1);
        identity.machine_id[..2].copy_from_slice(&(n as u16).to_be_bytes());
        observer.contact(identity, 1000, now);
    }
    assert_eq!(observer.state.peers.len(), MAX_PEERS);
    assert!(observer.state.retention_limited);
    assert_eq!(observer.state.peers[0].slot, 1);
    assert_eq!(observer.state.peers[MAX_PEERS - 1].slot, MAX_PEERS as u64);
    observer.observe(Visibility::Active, 0, "ok", 1000, now);
    let head = observer.log.as_ref().unwrap().stats().head;
    let retained = observer.state.peers.clone();
    assert!(read_history(&config).unwrap().retention_limited);
    for elapsed in 1..=3 {
        let mut overflow = peer(0, 1);
        overflow.machine_id[..2].copy_from_slice(&(MAX_PEERS as u16).to_be_bytes());
        let at = now + Duration::from_secs(elapsed);
        observer.contact(overflow, 1000 + elapsed * 1000, at);
        observer.observe(Visibility::Active, 0, "ok", 1000 + elapsed * 1000, at);
        assert_eq!(observer.log.as_ref().unwrap().stats().head, head);
        assert_eq!(observer.state.peers, retained);
        assert!(observer.state.retention_limited);
    }
}
#[test]
fn unavailable_storage_keeps_network_health_independent_without_rebuilding_history() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let now = Instant::now();
    let mut durable = Observer::open(&config, handle()).unwrap();
    durable.contact(peer(2, 1), 1000, now);
    durable.observe(Visibility::Active, 0, "ok", 1000, now);
    drop(durable);
    let before = std::fs::read(directory(&config).join("observations.astream")).unwrap();
    let mut memory = Observer::unavailable(&config, handle());
    memory.contact(peer(3, 1), 2000, now);
    memory.observe(Visibility::Active, 3, "presence active", 2000, now);
    assert_eq!(memory.state.visibility, Visibility::Active);
    assert_eq!(memory.state.history, HistoryState::Unavailable);
    assert_eq!(memory.state.peers.len(), 1);
    assert_eq!(memory.state.peers[0].reachability, Reachability::Unknown);
    assert_eq!(memory.state.unverified_endpoints, 3);
    assert_eq!(
        std::fs::read(directory(&config).join("observations.astream")).unwrap(),
        before
    );
}

/// An observer over a scratch journal with this Mac's identity admitted and its first
/// checkpoint written at `at`.
fn observing(config: &Path, at: Instant) -> Observer {
    let mut observer = Observer::open(config, handle()).unwrap();
    observer.local(peer(1, 1), trust()).unwrap();
    observer.observe(Visibility::Active, 0, "observing", 1000, at);
    observer
}
fn head(observer: &Observer) -> u64 {
    observer.log.as_ref().unwrap().stats().head.0
}
fn projection_bytes(config: &Path) -> Vec<u8> {
    private_file::read_private(&directory(config), "projection.json", MAX_BYTES).unwrap()
}
fn numbered(n: u16) -> MachineIdentity {
    let mut identity = peer(0, 1);
    identity.machine_id[..2].copy_from_slice(&n.to_be_bytes());
    identity
}
/// One observation a second from `from` up to `to` seconds after `at`, the wall clock in step.
fn tick(observer: &mut Observer, at: Instant, from: u64, to: u64) {
    for second in from..=to {
        observer.observe(
            Visibility::Active,
            0,
            "observing",
            1000 + second * 1000,
            at + Duration::from_secs(second),
        );
    }
}

#[test]
fn transitions_inside_one_settle_interval_make_one_checkpoint() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let at = Instant::now();
    let published = handle();
    let mut observer = Observer::open(&config, published.clone()).unwrap();
    observer.local(peer(1, 1), trust()).unwrap();
    observer.observe(Visibility::Active, 0, "observing", 1000, at);
    let first = head(&observer);
    let first_projection = projection_bytes(&config);
    // Forty peers appear, one per half second, each a durable transition; every observation
    // in between publishes them live.
    for n in 0..40_u64 {
        let now = at + Duration::from_millis(500 * (n + 1));
        observer.contact(numbered(n as u16 + 2), 1500 + 500 * n, now);
        observer.observe(Visibility::Active, 0, "observing", 1500 + 500 * n, now);
        assert_eq!(published.snapshot().peers.len(), n as usize + 1);
    }
    assert_eq!(
        head(&observer),
        first,
        "a transition inside the settle interval waits for its checkpoint"
    );
    assert_eq!(projection_bytes(&config), first_projection);
    observer.observe(
        Visibility::Active,
        0,
        "observing",
        1000 + SETTLE.as_millis() as u64,
        at + SETTLE,
    );
    assert_eq!(
        head(&observer),
        first + 1,
        "one checkpoint carries them all"
    );
    assert_eq!(read_history(&config).unwrap().peers.len(), 40);
    // More transitions before the next interval ends wait again.
    for n in 40..60_u64 {
        let now = at + SETTLE + Duration::from_millis(500 * (n - 39));
        observer.contact(numbered(n as u16 + 2), 40_000 + n, now);
        observer.observe(Visibility::Active, 0, "observing", 40_000 + n, now);
    }
    assert_eq!(head(&observer), first + 1);
    observer.observe(
        Visibility::Active,
        0,
        "observing",
        1000 + 2 * SETTLE.as_millis() as u64,
        at + 2 * SETTLE,
    );
    assert_eq!(head(&observer), first + 2);
    assert_eq!(read_history(&config).unwrap().peers.len(), 60);
}

#[test]
fn own_identity_and_trust_are_checkpointed_without_waiting() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let at = Instant::now();
    let mut observer = Observer::open(&config, handle()).unwrap();
    observer.observe(Visibility::Active, 0, "observing", 1000, at);
    let first = head(&observer);
    observer.local(peer(1, 1), trust()).unwrap();
    observer.observe(
        Visibility::Active,
        0,
        "observing",
        2000,
        at + Duration::from_secs(1),
    );
    assert_eq!(head(&observer), first + 1);
    // This Mac's next incarnation, with its trust context unchanged.
    observer.local(peer(1, 2), trust()).unwrap();
    observer.observe(
        Visibility::Active,
        0,
        "observing",
        3000,
        at + Duration::from_secs(2),
    );
    assert_eq!(head(&observer), first + 2);
    // A process that dies here never drops its observer: the guard is already in the journal.
    observer.log = None;
    drop(observer);
    let mut reopened = Observer::open(&config, handle()).unwrap();
    assert_eq!(reopened.state.observer, Some(peer(1, 2)));
    assert!(reopened.local(peer(9, 1), trust()).is_err());
}

#[test]
fn a_steady_contact_cadence_makes_no_reachability_transition() {
    // Slower than REACHABLE, as in a lab of dozens of peers: the measured period keeps the
    // lease, and the peer reads LastSeen only once it misses a whole scheduled contact.
    const PERIOD: u64 = 24;
    const { assert!(Duration::from_secs(PERIOD).as_millis() > REACHABLE.as_millis()) };
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let at = Instant::now();
    let published = handle();
    let mut observer = Observer::open(&config, published.clone()).unwrap();
    observer.local(peer(1, 1), trust()).unwrap();
    observer.contact(peer(2, 1), 1000, at);
    tick(&mut observer, at, 0, PERIOD - 1);
    // The first gap is past the lease of a peer not yet measured: a candidate. The second
    // confirms it, and the third contact has the period.
    observer.contact(
        peer(2, 1),
        1000 + PERIOD * 1000,
        at + Duration::from_secs(PERIOD),
    );
    tick(&mut observer, at, PERIOD, 2 * PERIOD - 1);
    assert_eq!(observer.state.peers[0].reachability, Reachability::LastSeen);
    observer.contact(
        peer(2, 1),
        1000 + 2 * PERIOD * 1000,
        at + Duration::from_secs(2 * PERIOD),
    );
    tick(&mut observer, at, 2 * PERIOD, 2 * PERIOD);
    let measured = observer.state.peers[0].samples.clone();
    assert_eq!(measured.last().unwrap().state, Reachability::Reachable);
    // The lease travels with the live observation, on the Reachable peer, and is no part of
    // the state the journal stores.
    assert_eq!(
        published.try_publication().unwrap().observation().peers[0].lease_ms,
        Some(2 * PERIOD * 1000)
    );
    assert_eq!(observer.state.peers[0].lease_ms, None);
    for cycle in 3..=40 {
        tick(
            &mut observer,
            at,
            (cycle - 1) * PERIOD + 1,
            cycle * PERIOD - 1,
        );
        // What a reader of the publication sees just before the next contact.
        let now = at + Duration::from_secs(cycle * PERIOD - 1);
        let publication = published.try_publication().unwrap();
        assert_eq!(
            publication
                .current_at(now, Some(1000 + (cycle * PERIOD - 1) * 1000))
                .peers[0]
                .reachability,
            Reachability::Reachable,
            "cycle {cycle}"
        );
        observer.contact(
            peer(2, 1),
            1000 + cycle * PERIOD * 1000,
            at + Duration::from_secs(cycle * PERIOD),
        );
        tick(&mut observer, at, cycle * PERIOD, cycle * PERIOD);
    }
    assert_eq!(
        observer.state.peers[0].samples, measured,
        "a peer contacted on schedule adds no presence sample"
    );
    assert!(!observer.state.retention_limited);
    // It misses a whole contact: LastSeen at twice its period, and no earlier.
    let last = 40 * PERIOD;
    tick(&mut observer, at, last + 1, last + 2 * PERIOD - 1);
    assert_eq!(
        observer.state.peers[0].reachability,
        Reachability::Reachable
    );
    tick(&mut observer, at, last + 2 * PERIOD, last + 2 * PERIOD);
    assert_eq!(observer.state.peers[0].reachability, Reachability::LastSeen);
    assert_eq!(
        published
            .try_publication()
            .unwrap()
            .current_at(
                at + Duration::from_secs(last + 2 * PERIOD),
                Some(1000 + (last + 2 * PERIOD) * 1000)
            )
            .peers[0]
            .reachability,
        Reachability::LastSeen
    );
}

#[test]
fn a_contact_lease_is_measured_and_bounded() {
    let at = Instant::now();
    let secs = Duration::from_secs;
    let first = Contact::first(at);
    assert_eq!(first.lease(), REACHABLE, "one contact measures no period");
    // Faster than half of REACHABLE: the floor.
    assert_eq!(first.then(at + secs(4)).lease(), REACHABLE);
    // A gap inside the lease: a longer one is the period at once, a shorter one lowers it a
    // quarter of the way.
    let uneven = first.then(at + secs(10)).then(at + secs(24));
    assert_eq!(uneven.period, Some(secs(14)));
    let uneven = uneven.then(at + secs(50));
    assert_eq!(uneven.period, Some(secs(26)));
    let uneven = uneven.then(at + secs(60));
    assert_eq!(uneven.period, Some(secs(22)));
    assert!(
        uneven.lease() > secs(26),
        "alternating gaps keep the slower one's lease"
    );
    // A gap past the lease is a candidate, and the period only once the next gap is at least
    // half of it: a slow peer is measured by its third contact.
    let slow = first.then(at + secs(24));
    assert_eq!((slow.period, slow.candidate), (None, Some(secs(24))));
    assert_eq!(slow.lease(), REACHABLE);
    let slow = slow.then(at + secs(48));
    assert_eq!((slow.period, slow.candidate), (Some(secs(24)), None));
    assert_eq!(slow.lease(), secs(48));
    // The confirming gap may be the shorter one, down to half; the longer is kept.
    let halved = first.then(at + secs(24)).then(at + secs(36));
    assert_eq!(halved.period, Some(secs(24)));
    // No contact older than LEASE_MAX reads Reachable, however slow the peer is.
    let slowest = first.then(at + secs(100)).then(at + secs(200));
    assert_eq!(slowest.lease(), LEASE_MAX);
    assert_eq!(slowest.period, Some(LEASE_MAX / 2));
    // A contact that is not later than the last one measures nothing.
    assert_eq!(slowest.then(at).at, slowest.at);
    assert_eq!(slowest.then(slowest.at).period, slowest.period);
}

#[test]
fn an_absence_does_not_lengthen_the_lease() {
    let at = Instant::now();
    let secs = Duration::from_secs;
    // Measured at 24 s, away for five minutes, then back at its pace: the gap is dismissed.
    let steady = Contact::first(at)
        .then(at + secs(24))
        .then(at + secs(48))
        .then(at + secs(72));
    assert_eq!(steady.period, Some(secs(24)));
    let back = steady.then(at + secs(372));
    assert_eq!((back.period, back.candidate), (Some(secs(24)), None));
    assert_eq!(
        back.lease(),
        secs(48),
        "an absence leaves the lease as it was"
    );
    let resumed = back.then(at + secs(396));
    assert_eq!((resumed.period, resumed.candidate), (Some(secs(24)), None));
    // A shorter absence, one some lease could cover, is a candidate the next gap dismisses.
    let short = steady.then(at + secs(172));
    assert_eq!(
        (short.period, short.candidate),
        (Some(secs(24)), Some(secs(100)))
    );
    let resumed = short.then(at + secs(196));
    assert_eq!((resumed.period, resumed.candidate), (Some(secs(24)), None));
    // Absences in a row, one contact between them (a Mac that answers once each time it
    // wakes), never read as a pace: the second does not confirm the first. Negative control:
    // a gap past LEASE_MAX that may be a candidate gives this peer the longest lease.
    let fast = Contact::first(at).then(at + secs(5)).then(at + secs(10));
    assert_eq!(fast.lease(), REACHABLE);
    let woke = fast
        .then(at + secs(3610))
        .then(at + secs(7210))
        .then(at + secs(10_810));
    assert_eq!((woke.period, woke.candidate), (Some(secs(5)), None));
    assert_eq!(
        woke.lease(),
        REACHABLE,
        "two absences running are not a slow pace"
    );
    // Nor does an absence confirm a candidate a lease could have covered.
    let away = steady.then(at + secs(172)).then(at + secs(3772));
    assert_eq!((away.period, away.candidate), (Some(secs(24)), None));
    // A peer whose pace really slowed is measured by the second slow gap.
    let slowed = steady.then(at + secs(132)).then(at + secs(192));
    assert_eq!(slowed.period, Some(secs(60)));

    // The observer's view: steady contacts every 5 s, one 300 s absence, three more contacts,
    // then silence. The peer reads LastSeen at twice its steady period (the 15 s floor), not at
    // a lease the absence stretched.
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let published = handle();
    let mut observer = Observer::open(&config, published.clone()).unwrap();
    observer.local(peer(1, 1), trust()).unwrap();
    let contact = |observer: &mut Observer, second: u64| {
        observer.contact(peer(2, 1), 1000 + second * 1000, at + secs(second));
        tick(observer, at, second, second);
    };
    for second in (0..=40).step_by(5) {
        contact(&mut observer, second);
    }
    tick(&mut observer, at, 41, 339);
    assert_eq!(observer.state.peers[0].reachability, Reachability::LastSeen);
    for second in [340, 345, 350, 355] {
        contact(&mut observer, second);
    }
    let reading = |second: u64| {
        published
            .try_publication()
            .unwrap()
            .current_at(at + secs(second), Some(1000 + second * 1000))
            .peers[0]
            .reachability
    };
    tick(&mut observer, at, 356, 369);
    assert_eq!(
        observer.state.peers[0].reachability,
        Reachability::Reachable
    );
    assert_eq!(reading(369), Reachability::Reachable);
    assert_eq!(
        published.try_publication().unwrap().observation().peers[0].lease_ms,
        Some(15_000)
    );
    tick(&mut observer, at, 370, 370);
    assert_eq!(observer.state.peers[0].reachability, Reachability::LastSeen);
    assert_eq!(reading(370), Reachability::LastSeen);
}

#[test]
fn idle_contact_progress_makes_one_checkpoint_per_period() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let at = Instant::now();
    let mut observer = observing(&config, at);
    let first = head(&observer);
    // Ten minutes of a healthy peer contacted every four seconds, observed every second: no
    // transition after the first contact's checkpoint.
    let mut heads = Vec::new();
    for second in 1..=600_u64 {
        if second % 4 == 0 {
            observer.contact(
                peer(2, 1),
                1000 + second * 1000,
                at + Duration::from_secs(second),
            );
        }
        tick(&mut observer, at, second, second);
        heads.push(head(&observer));
    }
    // The new peer is a transition: one checkpoint at SETTLE. Then one per CHECKPOINT.
    let settle = SETTLE.as_secs() as usize;
    let period = CHECKPOINT.as_secs() as usize;
    assert_eq!(heads[settle - 2], first);
    assert_eq!(heads[settle - 1], first + 1);
    let expected = 1 + (600 - settle) / period;
    assert_eq!(head(&observer), first + expected as u64);
    for (second, pair) in heads.windows(2).enumerate().skip(settle) {
        let due = (second + 2 - settle).is_multiple_of(period);
        assert_eq!(pair[1] - pair[0], u64::from(due), "second {}", second + 2);
    }
    assert_eq!(
        read_history(&config).unwrap().peers[0].last_seen_ms,
        1000 + 568 * 1000,
        "the periodic checkpoint carries contact progress"
    );
    assert_eq!(observer.state.peers[0].samples.len(), 1);
}

#[test]
fn the_journal_is_compacted_by_size_and_keeps_its_latest_checkpoint() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let at = Instant::now();
    let mut observer = observing(&config, at);
    // A state large enough that a few checkpoints pass COMPACT.
    for n in 0..200_u16 {
        observer.contact(numbered(n + 2), 1000, at);
    }
    let floor = |observer: &Observer| observer.log.as_ref().unwrap().read_floor();
    let retained = |observer: &Observer| observer.log.as_ref().unwrap().stats().retained_bytes;
    let mut compactions = 0;
    let mut appended_without_compaction = 0;
    for round in 1..=24_u64 {
        let before = floor(&observer);
        let records = observer.log.as_ref().unwrap().stats().retained_records;
        let now = at + CHECKPOINT * round as u32;
        observer.observe(Visibility::Active, 0, "observing", 1000 + round, now);
        assert_eq!(observer.state.history, HistoryState::Ready);
        let record = encode(&observer.state, observer.trust).unwrap().len();
        assert!(
            retained(&observer) <= COMPACT + record + 4096,
            "round {round}: {} bytes retained",
            retained(&observer)
        );
        if floor(&observer) > before {
            compactions += 1;
            assert!(
                matches!(Observer::open(&config, handle()), Err(OpenFailure::Owned)),
                "compacting the journal must retain exclusive observer ownership"
            );
            // The checkpoint before this one was kept until this one was durable.
            assert_eq!(observer.log.as_ref().unwrap().stats().retained_records, 2);
            assert!(records > 1);
        } else {
            appended_without_compaction += 1;
        }
        let log = observer.log.as_ref().unwrap();
        let latest = log.read_range(observer.latest.unwrap(), 1);
        assert_eq!(
            decode(&latest[0].body).unwrap().revision,
            observer.state.revision
        );
    }
    assert!(compactions >= 2, "{compactions}");
    assert!(
        appended_without_compaction > 2 * compactions,
        "{appended_without_compaction} appends, {compactions} compactions"
    );
    // A process that died between any two of those reopens the latest complete checkpoint.
    let revision = observer.state.revision;
    observer.log = None;
    drop(observer);
    let reopened = Observer::open(&config, handle()).unwrap();
    assert_eq!(reopened.state.revision, revision);
    assert_eq!(reopened.state.peers.len(), 200);
}

#[test]
fn a_crash_loses_at_most_the_settle_interval_and_a_drop_loses_nothing() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let at = Instant::now();
    let mut observer = observing(&config, at);
    observer.contact(peer(2, 1), 2000, at + Duration::from_secs(1));
    tick(&mut observer, at, 1, SETTLE.as_secs());
    let checkpointed = head(&observer);
    let saved = observer.state.peers[0].clone();
    // A transition inside the next interval, then the process dies: the observer is never
    // dropped, so nothing more is written.
    observer.contact(peer(3, 1), 40_000, at + SETTLE + Duration::from_secs(5));
    tick(
        &mut observer,
        at,
        SETTLE.as_secs() + 5,
        SETTLE.as_secs() + 6,
    );
    assert_eq!(head(&observer), checkpointed);
    let pending = observer.state.peers.len();
    assert_eq!(pending, 2);
    observer.log = None;
    drop(observer);
    // The reload is the complete last checkpoint: every peer of it, whole, and this Mac's
    // identity guard; only the pending transition is gone.
    let mut reopened = Observer::open(&config, handle()).unwrap();
    assert_eq!(reopened.state.history, HistoryState::Ready);
    assert_eq!(reopened.state.observer, Some(peer(1, 1)));
    assert_eq!(reopened.state.peers.len(), 1);
    assert_eq!(reopened.state.peers[0].identity, peer(2, 1));
    assert_eq!(reopened.state.peers[0].first_seen_ms, 2000);
    assert_eq!(reopened.state.peers[0].last_seen_ms, saved.last_seen_ms);
    assert_eq!(reopened.state.peers[0].samples, saved.samples);
    assert_eq!(
        read_history(&config).unwrap().peers,
        reopened.state.peers,
        "the projection is that same checkpoint"
    );
    // The same transition again, and this time the observer is dropped: it is written.
    reopened.local(peer(1, 1), trust()).unwrap();
    let later = at + SETTLE + Duration::from_secs(100);
    reopened.observe(Visibility::Active, 0, "observing", 200_000, later);
    let before = head(&reopened);
    reopened.contact(peer(3, 1), 201_000, later + Duration::from_secs(1));
    reopened.observe(
        Visibility::Active,
        0,
        "observing",
        201_000,
        later + Duration::from_secs(1),
    );
    assert_eq!(head(&reopened), before);
    drop(reopened);
    assert_eq!(read_history(&config).unwrap().peers.len(), 2);
    let last = Observer::open(&config, handle()).unwrap();
    assert_eq!(last.state.peers.len(), 2);
    assert_eq!(head(&last), before + 1);
}

fn round(
    outcome: aterm_types::network_state::RepairOutcome,
    completed_at_ms: u64,
) -> aterm_types::network_state::RepairObservation {
    aterm_types::network_state::RepairObservation {
        completed_at_ms,
        outbound: true,
        stage: None,
        outcome,
        table: Some([3; 32]),
        sent: Some(0),
        received: Some(0),
        deferred: Some(0),
    }
}

#[test]
fn a_history_is_closed_only_by_its_observers_drop() {
    use aterm_types::network_state::RepairOutcome;
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let at = Instant::now();
    // Nothing was ever observed: there is nothing an observer could have left out.
    assert!(read_closed_history(&config).unwrap().is_some());
    let mut observer = observing(&config, at);
    observer.contact(peer(2, 1), 2000, at + Duration::from_secs(1));
    let exchanged = round(RepairOutcome::Exchanged, 2000);
    observer.repair(&peer(2, 1), exchanged.clone(), at + Duration::from_secs(1));
    tick(&mut observer, at, 1, SETTLE.as_secs());
    // While its observer runs, the history has the round and says it is open.
    assert_eq!(
        read_history(&config).unwrap().peers[0].last_repair,
        Some(exchanged.clone())
    );
    assert!(read_closed_history(&config).unwrap().is_none());
    let checkpointed = head(&observer);
    drop(observer);
    // The drop wrote the closing record, with nothing else pending.
    let closed = read_closed_history(&config).unwrap().expect("closed");
    assert_eq!(closed.peers[0].last_repair, Some(exchanged.clone()));
    // The next observer keeps a closed history's rounds. One that only opens leaves it closed.
    let reopened = Observer::open(&config, handle()).unwrap();
    assert_eq!(head(&reopened), checkpointed + 1);
    assert_eq!(reopened.state.peers[0].last_repair, Some(exchanged.clone()));
    drop(reopened);
    assert!(read_closed_history(&config).unwrap().is_some());

    // The next run's round ends Deferred inside the settle interval, and the process dies
    // without dropping its observer.
    let later = at + Duration::from_secs(1000);
    let mut observer = Observer::open(&config, handle()).unwrap();
    observer.local(peer(1, 1), trust()).unwrap();
    observer.observe(Visibility::Active, 0, "observing", 1_000_000, later);
    let written = head(&observer);
    observer.repair(
        &peer(2, 1),
        round(RepairOutcome::Deferred(None), 1_001_000),
        later + Duration::from_secs(1),
    );
    observer.observe(
        Visibility::Active,
        0,
        "observing",
        1_001_000,
        later + Duration::from_secs(1),
    );
    assert_eq!(
        head(&observer),
        written,
        "the round waits for its checkpoint"
    );
    observer.log = None;
    drop(observer);
    // The retained round is the older one, and the history says it may not be the latest.
    assert_eq!(
        read_history(&config).unwrap().peers[0].last_repair,
        Some(exchanged)
    );
    assert!(read_closed_history(&config).unwrap().is_none());
    // The next observer keeps the peer and drops the round that may have been superseded.
    let reopened = Observer::open(&config, handle()).unwrap();
    assert_eq!(reopened.state.peers.len(), 1);
    assert_eq!(reopened.state.peers[0].last_repair, None);
    assert_eq!(reopened.published.snapshot().peers[0].last_repair, None);
    // Having written nothing, it leaves the history open.
    drop(reopened);
    assert!(read_closed_history(&config).unwrap().is_none());
}

#[test]
fn a_record_that_does_not_say_closed_is_open() {
    let root = aterm_tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    let at = Instant::now();
    let mut observer = observing(&config, at);
    observer.contact(peer(2, 1), 2000, at);
    drop(observer);
    assert!(read_closed_history(&config).unwrap().is_some());
    // The same record as an aterm from before records said wrote it.
    let dir = directory(&config);
    let mut value: Value = aterm_json::from_slice(&projection_bytes(&config)).unwrap();
    assert_eq!(
        value.as_object_mut().unwrap().remove("closed"),
        Some(Value::Bool(true))
    );
    private_file::write_private(
        &dir,
        "projection.json",
        &aterm_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    assert!(read_closed_history(&config).unwrap().is_none());
    assert_eq!(read_history(&config).unwrap().peers.len(), 1);
    // A projection that cannot be read is an error, not an open or a closed history.
    private_file::write_private(&dir, "projection.json", b"{").unwrap();
    assert!(read_closed_history(&config).is_err());
}
