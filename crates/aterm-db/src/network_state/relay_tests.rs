// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_closed_observer_keeps_its_queued_acknowledgement_readable() {
    let (mut client, mut observer) = CtlStream::pair().unwrap();
    observer.write_all(ACK).unwrap();
    drop(observer);
    let deadline = Instant::now() + EXCHANGE;
    while !aterm_uds::hangup::peer_closed(&client) {
        assert!(Instant::now() < deadline, "the observer never closed");
        aterm_uds::deadline::poll_yield();
    }
    let mut ack = [0; ACK.len()];
    read_exact(&mut client, &mut ack, deadline).unwrap();
    assert_eq!(&ack, ACK);
}

fn publish(handle: &ObservationHandle, state: NetworkObservation) {
    let mut publication = Publication::new(state, Instant::now());
    publication.contact_ages = publication
        .observation
        .peers
        .iter()
        .map(|peer| {
            (
                peer.slot,
                Duration::from_millis(
                    publication
                        .observation
                        .observed_at_ms
                        .saturating_sub(peer.last_seen_ms),
                ),
            )
        })
        .collect();
    handle.replace_publication(publication);
}

#[test]
fn relayed_source_age_survives_repeated_reads_and_clock_rollback() {
    let state = current();
    let requested = Instant::now();
    let first = decode_publication(
        &live_encode(&state, 12_000).unwrap(),
        state.observed_at_ms,
        requested,
    )
    .unwrap();
    let again = decode_publication(
        &live_encode(&state, 14_000).unwrap(),
        state.observed_at_ms,
        requested + Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(first.at, again.at);
    assert_eq!(
        first
            .current_at(
                requested + Duration::from_secs(4),
                Some(state.observed_at_ms)
            )
            .visibility,
        Visibility::Unavailable
    );
    assert_eq!(
        again
            .current_at(
                requested + Duration::from_secs(4),
                Some(state.observed_at_ms)
            )
            .visibility,
        Visibility::Unavailable
    );
    assert_eq!(first.observation.observed_at_ms, state.observed_at_ms);
}

#[test]
fn relayed_current_claim_requires_a_bounded_source_age() {
    let state = current();
    let mut value: Value = aterm_json::from_slice(&live_encode(&state, 0).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("source_age_ms");
    assert!(live_decode(&aterm_json::to_vec(&value).unwrap(), state.observed_at_ms).is_err());
    for age in [json!(-1), json!(15_001), json!("0"), json!(null)] {
        value
            .as_object_mut()
            .unwrap()
            .insert("source_age_ms".into(), age);
        assert!(live_decode(&aterm_json::to_vec(&value).unwrap(), state.observed_at_ms).is_err());
    }
}

#[test]
fn relay_carries_a_measured_contact_lease_within_its_bounds() {
    // A peer contacted every 24 s, read 40 s after its last contact: inside its 48 s lease.
    let mut state = current();
    let observed = state.observed_at_ms;
    state.peers[0].first_seen_ms = observed - 40_000;
    state.peers[0].last_seen_ms = observed - 40_000;
    state.peers[0].last_checked_ms = Some(observed - 40_000);
    let slot = state.peers[0].slot;
    let requested = Instant::now();
    let mut source = Publication::new(state, requested);
    source.contact_ages.insert(slot, Duration::from_secs(40));
    // With no lease the contact is past `REACHABLE`: the owner itself reads LastSeen, and a
    // frame that still claims Reachable is refused.
    assert_eq!(
        source.current_at(requested, Some(observed)).peers[0].reachability,
        Reachability::LastSeen
    );
    let unleased = live_encode_publication(&source, 0).unwrap();
    assert!(decode_publication(&unleased, observed, requested).is_err());
    source.observation.peers[0].lease_ms = Some(48_000);
    let wire = live_encode_publication(&source, 0).unwrap();
    let received = decode_publication(&wire, observed, requested).unwrap();
    assert_eq!(received.observation.peers[0].lease_ms, Some(48_000));
    let at = |seconds: u64| {
        received
            .current_at(
                requested + Duration::from_secs(seconds),
                Some(observed + seconds * 1000),
            )
            .peers[0]
            .reachability
    };
    assert_eq!(at(0), Reachability::Reachable);
    assert_eq!(at(8), Reachability::Reachable);
    assert_eq!(at(9), Reachability::LastSeen, "the lease ends at 48 s");
    let with_lease = |lease: Value| {
        let mut value: Value = aterm_json::from_slice(&wire).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .get_mut("live")
            .unwrap()
            .as_array_mut()
            .unwrap()[0]
            .as_object_mut()
            .unwrap()
            .insert("lease_ms".into(), lease);
        decode_publication(&aterm_json::to_vec(&value).unwrap(), observed, requested)
    };
    assert!(with_lease(json!(48_000)).is_ok());
    assert!(with_lease(json!(LEASE_MAX.as_millis() as u64)).is_ok());
    for lease in [
        // Below the floor, past the ceiling, shorter than the contact's age, or no number.
        json!(14_999),
        json!(LEASE_MAX.as_millis() as u64 + 1),
        json!(39_999),
        json!(null),
        json!("48000"),
        json!(-1),
    ] {
        assert!(with_lease(lease.clone()).is_err(), "{lease:?}");
    }
}

/// A frame from an owner older than the measured lease names none (its rows have no
/// `lease_ms`), and this reader takes it: each contact is read against `REACHABLE`, as that
/// owner read it. The other direction is not kept: a reader from before the lease refuses this
/// owner's frame once a Reachable row's contact is older than `REACHABLE`.
#[test]
fn a_frame_that_names_no_lease_is_read_against_the_reachable_window() {
    let mut state = current();
    let observed = state.observed_at_ms;
    state.peers[0].first_seen_ms = observed - 10_000;
    state.peers[0].last_seen_ms = observed - 10_000;
    state.peers[0].last_checked_ms = Some(observed - 10_000);
    let slot = state.peers[0].slot;
    let requested = Instant::now();
    let mut source = Publication::new(state, requested);
    source.contact_ages.insert(slot, Duration::from_secs(10));
    let mut value: Value =
        aterm_json::from_slice(&live_encode_publication(&source, 0).unwrap()).unwrap();
    for row in value
        .as_object_mut()
        .unwrap()
        .get_mut("live")
        .unwrap()
        .as_array_mut()
        .unwrap()
    {
        assert!(row.as_object_mut().unwrap().remove("lease_ms").is_some());
    }
    let received =
        decode_publication(&aterm_json::to_vec(&value).unwrap(), observed, requested).unwrap();
    assert_eq!(received.observation.peers[0].lease_ms, None);
    let at = |seconds: u64| {
        received
            .current_at(
                requested + Duration::from_secs(seconds),
                Some(observed + seconds * 1000),
            )
            .peers[0]
            .reachability
    };
    assert_eq!(at(5), Reachability::Reachable);
    assert_eq!(at(6), Reachability::LastSeen, "REACHABLE ends at 15 s");
}

#[test]
fn relay_preserves_monotonic_contact_age_after_a_prior_wall_rollback() {
    let mut state = current();
    let observed = state.observed_at_ms;
    state.peers[0].first_seen_ms = observed - 2000;
    state.peers[0].last_seen_ms = observed - 2000;
    state.peers[0].last_checked_ms = Some(observed - 2000);
    let requested = Instant::now();
    let mut source = Publication::new(state, requested);
    source
        .contact_ages
        .insert(source.observation.peers[0].slot, Duration::from_secs(10));
    let wire = live_encode_publication(&source, 0).unwrap();
    let received = decode_publication(&wire, observed, requested).unwrap();
    assert_eq!(
        received
            .current_at(requested + Duration::from_secs(7), Some(observed + 7000))
            .peers[0]
            .reachability,
        Reachability::LastSeen
    );
    let mut value: Value = aterm_json::from_slice(&wire).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .get_mut("live")
        .unwrap()
        .as_array_mut()
        .unwrap()[0]
        .as_object_mut()
        .unwrap()
        .remove("contact_age_ms");
    assert!(live_decode(&aterm_json::to_vec(&value).unwrap(), observed).is_err());
    for age in [json!(-1), json!(u64::MAX), json!(null), json!("10")] {
        value
            .as_object_mut()
            .unwrap()
            .get_mut("live")
            .unwrap()
            .as_array_mut()
            .unwrap()[0]
            .as_object_mut()
            .unwrap()
            .insert("contact_age_ms".into(), age);
        assert!(live_decode(&aterm_json::to_vec(&value).unwrap(), observed).is_err());
    }
}

#[test]
fn observer_rollback_history_round_trips_without_replacing_live_inventory_with_disk() {
    let root = socket_dir();
    let config = root.path().join("aterm.toml");
    let handle = ObservationHandle::default();
    let mut owner = Observer::open(&config, handle.clone()).unwrap();
    let identity = |id| MachineIdentity {
        machine_id: [id; 16],
        incarnation: [id; 16],
        label: None,
    };
    owner
        .local(
            identity(1),
            aterm_secrets::commons::journal::Trust {
                network: [1; 16],
                authority: [2; 32],
                genesis: [3; 32],
            },
        )
        .unwrap();
    let at = Instant::now();
    let exchange = |wall, sampled_at| account_exchange::Candidate {
        observation: AccountsExchangeObservation {
            scope_digest: [3; 32],
            local_actor: [4; 48],
            control_observation: [5; 64],
            inventory_count: 2,
            inventory_fingerprint: [6; 32],
            peer: identity(2),
            sampled_at_ms: wall,
        },
        sampled_at,
    };
    let round_trip = |publication: &Publication, wall| {
        let source = publication.observation.clone();
        let requested = publication.sampled_at();
        let wire = live_encode_publication(publication, 0).unwrap();
        let decoded = decode_publication(&wire, wall, requested).unwrap();
        assert_eq!(decoded.observation, source);
        assert_eq!(decoded.sampled_at(), publication.sampled_at());
        assert_eq!(
            decoded.current_at(requested, Some(wall)),
            publication.current_at(requested, Some(wall))
        );
        // The previous whole-frame clock guard rejects these genuine producer states.
        assert!(source.peers.iter().any(|peer| {
            peer.first_seen_ms > peer.last_seen_ms
                || peer.last_seen_ms > wall
                || peer.last_checked_ms.is_some_and(|checked| checked > wall)
                || peer.samples.iter().any(|sample| sample.at_ms > wall)
        }));
        assert!(decode_publication(&wire, wall - 1, requested).is_err());
        assert!(
            decode_publication(
                &live_encode_publication(publication, 15_001).unwrap(),
                wall,
                requested
            )
            .is_err()
        );
        let expired =
            decoded.current_at(requested + REACHABLE + Duration::from_secs(1), Some(wall));
        assert_eq!(expired.visibility, Visibility::Unavailable);
        assert_eq!(expired.observed_at_ms, source.observed_at_ms);
        assert_eq!(expired.accounts_exchange, source.accounts_exchange);
        assert_eq!(publication.observation, source);
        decoded
    };
    owner.contact(identity(2), 100_000, at);
    owner.accounts_exchange(exchange(100_000, at));
    owner.observe(Visibility::Active, 0, "observing", 100_000, at);

    // A fresh heartbeat retains the pre-rollback contact as unconfirmed history.
    owner.observe(
        Visibility::Active,
        0,
        "observing",
        90_000,
        at + Duration::from_secs(1),
    );
    let publication = handle.try_publication().unwrap();
    let relayed = round_trip(&publication, 90_000);
    assert_eq!(
        relayed.observation.peers[0].reachability,
        Reachability::LastSeen
    );
    assert_eq!(relayed.observation.peers[0].last_seen_ms, 100_000);

    // A genuinely new contact can precede its retained first contact on the wall clock.
    for (wall, elapsed) in [(90_001, 2), (90_002, 3)] {
        let sampled_at = at + Duration::from_secs(elapsed);
        owner.contact(identity(2), wall, sampled_at);
        owner.accounts_exchange(exchange(wall, sampled_at));
        owner.observe(Visibility::Active, 0, "observing", wall, sampled_at);
        let publication = handle.try_publication().unwrap();
        let relayed = round_trip(&publication, wall);
        let peer = &relayed.observation.peers[0];
        assert_eq!(peer.first_seen_ms, 100_000);
        assert_eq!(peer.last_seen_ms, wall);
        assert_eq!(peer.reachability, Reachability::Reachable);
        assert_eq!(
            peer.samples
                .iter()
                .map(|sample| sample.at_ms)
                .collect::<Vec<_>>(),
            [100_000, 90_000, 90_001]
        );
        assert_eq!(
            relayed.observation.accounts_exchange.unwrap().sampled_at_ms,
            wall
        );
    }
    assert_eq!(
        read_history(&config)
            .unwrap()
            .accounts_exchange
            .unwrap()
            .sampled_at_ms,
        100_000,
        "the newer live receipts are served from memory; the projection is the last checkpoint"
    );
}

fn socket_dir() -> aterm_tempfile::TempDir {
    // macOS's default temporary root plus the per-config suffix exceeds
    // sockaddr_un on some hosts. Keep socket fixtures in a short root.
    aterm_tempfile::Builder::new()
        .prefix("observer-")
        .tempdir_in("/tmp")
        .unwrap()
}

fn current() -> NetworkObservation {
    let now = aterm_time::unix_millis().unwrap();
    NetworkObservation {
        observed_at_ms: now,
        visibility: Visibility::Active,
        history: HistoryState::Ready,
        observer: Some(MachineIdentity {
            machine_id: [1; 16],
            incarnation: [2; 16],
            label: None,
        }),
        peers: vec![PeerObservation {
            slot: 3,
            identity: MachineIdentity {
                machine_id: [3; 16],
                incarnation: [4; 16],
                label: None,
            },
            first_seen_ms: now - 100,
            last_seen_ms: now,
            last_checked_ms: Some(now),
            last_repair: None,
            last_authenticated_endpoint: Some(AuthenticatedEndpoint {
                address: "127.0.0.1:41384".parse().unwrap(),
                observed_at_ms: now,
            }),
            reachability: Reachability::Reachable,
            instances: None,
            samples: vec![PresenceSample {
                at_ms: now,
                state: Reachability::Reachable,
            }],
            lease_ms: None,
        }],
        ..NetworkObservation::default()
    }
}

#[test]
fn live_relay_preserves_an_old_failed_attempt_without_renewing_its_time() {
    use aterm_types::network_state::{RepairCause, RepairObservation, RepairOutcome, RepairStage};
    let mut state = current();
    state.peers[0].last_repair = Some(RepairObservation {
        completed_at_ms: state.observed_at_ms - 60_000,
        outbound: false,
        stage: Some(RepairStage::Application),
        outcome: RepairOutcome::Failed(RepairCause::Io),
        table: Some([9; 32]),
        sent: Some(1),
        received: Some(2),
        deferred: Some(0),
    });
    let decoded = live_decode(&live_encode(&state, 0).unwrap(), state.observed_at_ms).unwrap();
    assert_eq!(decoded.peers[0].last_repair, state.peers[0].last_repair);
    assert_eq!(decoded.peers[0].reachability, Reachability::Reachable);
    assert_eq!(
        value(&decoded)["peers"][0]["last_repair"]["completed_at_ms"],
        json!(state.observed_at_ms - 60_000)
    );
}

#[test]
fn long_paths_keep_profiles_and_directory_permissions_separate() {
    let root = socket_dir();
    let first = root.path().join("first-configuration-".repeat(10));
    let second = root.path().join("second-configuration-".repeat(10));
    private_file::create_private_dir(&first).unwrap();
    private_file::create_private_dir(&second).unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink(&first, &alias).unwrap();
    let alias = alias.join("aterm.toml");
    let first = first.join("aterm.toml");
    let second = second.join("aterm.toml");
    let first_handle = ObservationHandle::default();
    let second_handle = ObservationHandle::default();
    let first_owner = Observer::open(&first, first_handle.clone()).unwrap();
    let _second_owner = Observer::open(&second, second_handle.clone()).unwrap();
    let mut first_state = current();
    let mut second_state = current();
    first_state.observer.as_mut().unwrap().machine_id = [7; 16];
    second_state.observer.as_mut().unwrap().machine_id = [8; 16];
    publish(&first_handle, first_state.clone());
    publish(&second_handle, second_state.clone());
    assert_eq!(read(&first).unwrap().observer, first_state.observer);
    assert_eq!(read(&alias).unwrap().observer, first_state.observer);
    assert_eq!(read(&second).unwrap().observer, second_state.observer);

    std::fs::set_permissions(directory(&first), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        read(&first).unwrap_err().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(read(&second).unwrap().observer, second_state.observer);
    std::fs::set_permissions(directory(&first), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(read(&first).unwrap().visibility, Visibility::Active);
    drop(first_owner);
    assert!(read(&first).is_err());
    assert!(read(&alias).is_err());
    assert_eq!(read(&second).unwrap().observer, second_state.observer);

    // An older owner used the short alias itself as its socket pathname.
    let legacy_handle = ObservationHandle::default();
    let legacy = Server::start_at(
        &directory(&alias),
        legacy_handle.clone(),
        directory(&alias).join(NAME),
    )
    .unwrap();
    publish(&legacy_handle, current());
    assert!(legacy.is_running());
    assert_eq!(read(&alias).unwrap().visibility, Visibility::Active);
}

#[test]
#[ignore = "child process fixture, invoked by cross_process_owner"]
fn child_observer() {
    use std::io::Read as _;
    let config =
        PathBuf::from(std::env::var_os("ATERM_TEST_OBSERVER_CONFIG").expect("fixture config"));
    let handle = ObservationHandle::default();
    let owner = Observer::open(&config, handle.clone()).unwrap();
    let state = current();
    publish(&handle, state.clone());
    private_file::write_private(
        &directory(&config),
        "projection.json",
        &encode(&state, None).unwrap(),
    )
    .unwrap();
    // Keep the readiness proof on its own line even after libtest's single-thread prefix.
    println!("\nobserver-ready");
    std::io::stdout().flush().unwrap();
    let _ = std::io::stdin().read(&mut [0; 1]);
    drop(owner);
}

#[test]
fn cross_process_owner_supplies_kernel_identity_then_releases_ownership() {
    struct ChildGuard(aterm_uds::spawnfd::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            aterm_uds::deadline::kill_within(&mut self.0, Duration::from_secs(60));
        }
    }
    let root = socket_dir();
    // A valid configuration can live beyond sockaddr_un's path capacity.
    let deep = root.path().join("long-configuration-directory-".repeat(5));
    private_file::create_private_dir(&deep).unwrap();
    let config = deep.join("aterm.toml");
    let executable = std::env::current_exe().unwrap();
    let mut command = aterm_uds::spawnfd::command(executable);
    command
        .args([
            "--exact",
            "network_state::relay::tests::child_observer",
            "--ignored",
            "--nocapture",
        ])
        .env("ATERM_TEST_OBSERVER_CONFIG", &config)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::inherit());
    let mut child = ChildGuard(command.spawn().unwrap());
    aterm_uds::deadline::prints(&mut child.0, "observer-ready", Duration::from_secs(60));
    assert!(matches!(
        Observer::open(&config, ObservationHandle::default()),
        Err(OpenFailure::Owned)
    ));
    let live = read(&config).unwrap();
    assert_eq!(live.via_local_process, Some(child.0.id()));
    assert_ne!(child.0.id(), std::process::id());
    assert_eq!(live.peers[0].reachability, Reachability::Reachable);
    drop(child.0.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut exit = aterm_uds::deadline::ExitWait::new(&child.0);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "observer child did not exit");
        exit.wait(deadline.saturating_duration_since(Instant::now()));
    }
    assert!(read(&config).is_err());
    let history = read_history(&config).unwrap();
    assert!(history.via_local_process.is_none());
    assert_eq!(history.peers[0].reachability, Reachability::Unknown);
    let successor = Observer::open(&config, ObservationHandle::default()).unwrap();
    drop(successor);
}

#[test]
fn owner_serves_live_memory_without_promoting_persisted_history() {
    let root = socket_dir();
    let config = root.path().join("aterm.toml");
    let handle = ObservationHandle::default();
    let owner = Observer::open(&config, handle.clone()).unwrap();
    assert!(matches!(
        Observer::open(&config, ObservationHandle::default()),
        Err(OpenFailure::Owned)
    ));
    let mut state = current();
    state.accounts_exchange = Some(aterm_types::network_state::AccountsExchangeObservation {
        scope_digest: [3; 32],
        local_actor: [4; 48],
        control_observation: [5; 64],
        inventory_count: 2,
        inventory_fingerprint: [6; 32],
        peer: state.peers[0].identity.clone(),
        sampled_at_ms: state.observed_at_ms - 60_000,
    });
    publish(&handle, state.clone());
    let seen = read(&config).unwrap();
    assert_eq!(seen.via_local_process, Some(std::process::id()));
    assert_eq!(seen.observed_at_ms, state.observed_at_ms);
    assert_eq!(seen.observer, state.observer);
    assert_eq!(seen.peers, state.peers);
    let mut unwrapped = seen;
    unwrapped.via_local_process = None;
    assert_eq!(unwrapped, state);
    assert_eq!(
        read(&config).unwrap().accounts_exchange,
        state.accounts_exchange,
        "a later authenticated local relay read cannot renew a historical exchange"
    );
    let history = encode(&state, None).unwrap();
    private_file::write_private(&directory(&config), "projection.json", &history).unwrap();
    drop(owner);
    assert!(read(&config).is_err());
    let retained = read_history(&config).unwrap();
    assert_eq!(retained.peers[0].reachability, Reachability::Unknown);
    assert!(retained.observer.is_none());
    assert!(retained.via_local_process.is_none());
    assert_eq!(retained.accounts_exchange, state.accounts_exchange);
    let successor = Observer::open(&config, ObservationHandle::default()).unwrap();
    drop(successor);
}

#[test]
fn old_live_frames_without_endpoint_metadata_remain_readable() {
    let state = current();
    let mut wire: Value = aterm_json::from_slice(&live_encode(&state, 0).unwrap()).unwrap();
    wire.as_object_mut()
        .unwrap()
        .get_mut("history")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("accounts_exchange");
    wire.as_object_mut()
        .unwrap()
        .get_mut("history")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("peers")
        .unwrap()
        .as_array_mut()
        .unwrap()[0]
        .as_object_mut()
        .unwrap()
        .remove("last_authenticated_endpoint");
    let decoded = live_decode(&aterm_json::to_vec(&wire).unwrap(), state.observed_at_ms).unwrap();
    assert_eq!(decoded.peers[0].identity, state.peers[0].identity);
    assert_eq!(decoded.peers[0].reachability, Reachability::Reachable);
    assert!(decoded.peers[0].last_authenticated_endpoint.is_none());
    assert!(decoded.accounts_exchange.is_none());
}

#[test]
fn busy_publisher_refuses_relay_read_without_refreshing_evidence() {
    let root = socket_dir();
    let config = root.path().join("aterm.toml");
    let handle = ObservationHandle::default();
    let owner = Observer::open(&config, handle.clone()).unwrap();
    let state = current();
    publish(&handle, state.clone());
    let publisher = handle.0.lock().unwrap();
    let published_at = publisher.at;
    let request_config = config.clone();
    let (send, receive) = std::sync::mpsc::channel();
    let request = thread::spawn(move || send.send(read(&request_config)).unwrap());
    let refused = receive.recv_timeout(Duration::from_secs(60));
    // Release before checking the hang detector, so a failed implementation
    // can finish and its server can be joined during test cleanup.
    drop(publisher);
    request.join().unwrap();
    let error = refused
        .expect("busy publication must not hold the relay request")
        .expect_err("a busy publisher supplies no current evidence");
    assert!(!matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ));

    assert_eq!(handle.snapshot_if_fresh(Duration::MAX), Some(state));
    assert_eq!(handle.0.lock().unwrap().at, published_at);
    drop(owner);
}

#[test]
fn monotonic_publication_age_and_nonrecursive_source_are_required() {
    let root = socket_dir();
    private_file::create_private_dir(root.path()).unwrap();
    let handle = ObservationHandle::default();
    let server = Server::start(root.path(), handle.clone()).unwrap();
    publish(&handle, current());
    // A wall-clock-fresh snapshot cannot conceal a stopped observer loop.
    handle.0.lock().unwrap().at = Instant::now() - REACHABLE - Duration::from_secs(1);
    let unchanged = handle.snapshot();
    // Query uses network-state beside config, so use a direct client here.
    let mut client =
        aterm_uds::connect::until(&root.path().join(NAME), Instant::now() + EXCHANGE).unwrap();
    write_all(&mut client, REQUEST, Instant::now() + EXCHANGE).unwrap();
    assert!(read_exact(&mut client, &mut [0; 4], Instant::now() + EXCHANGE).is_err());
    assert_eq!(handle.snapshot(), unchanged);
    let mut relayed = current();
    relayed.via_local_process = Some(42);
    publish(&handle, relayed);
    let mut client =
        aterm_uds::connect::until(&root.path().join(NAME), Instant::now() + EXCHANGE).unwrap();
    write_all(&mut client, REQUEST, Instant::now() + EXCHANGE).unwrap();
    assert!(read_exact(&mut client, &mut [0; 4], Instant::now() + EXCHANGE).is_err());
    drop(server);
}

#[test]
fn replacement_socket_is_never_removed_by_previous_server() {
    let root = socket_dir();
    private_file::create_private_dir(root.path()).unwrap();
    let server = Server::start(root.path(), ObservationHandle::default()).unwrap();
    let path = root.path().join(NAME);
    std::fs::remove_file(&path).unwrap();
    let replacement = CtlListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let identity = endpoint(root.path()).unwrap();
    drop(server);
    assert_eq!(endpoint(root.path()).unwrap(), identity);
    drop(replacement);
}

#[test]
fn unsafe_endpoints_are_refused_without_unlinking_them() {
    let root = socket_dir();
    private_file::create_private_dir(root.path()).unwrap();
    let path = root.path().join(NAME);
    std::fs::write(&path, b"keep").unwrap();
    assert!(Server::start(root.path(), ObservationHandle::default()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"keep");
    std::fs::remove_file(&path).unwrap();
    let target = root.path().join("target");
    std::fs::write(&target, b"keep").unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert!(Server::start(root.path(), ObservationHandle::default()).is_err());
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"keep");
}

#[test]
fn expiring_contacts_preserve_a_fresh_snapshot_and_its_other_peers() {
    for (seen_age_ms, checked_age_ms) in [(14_500, 14_500), (14_500, 0), (0, 14_500)] {
        let mut state = current();
        let observed = state.observed_at_ms;
        let mut fresh = state.peers[0].clone();
        fresh.slot = 4;
        fresh.identity.machine_id = [5; 16];
        fresh.identity.incarnation = [6; 16];
        state.peers.push(fresh);
        let old = &mut state.peers[0];
        old.first_seen_ms = observed - 20_000;
        old.last_seen_ms = observed - seen_age_ms;
        old.last_checked_ms = Some(observed - checked_age_ms);
        old.samples[0].at_ms = old.last_seen_ms;
        let requested = Instant::now();
        let initial =
            decode_publication(&live_encode(&state, 0).unwrap(), observed, requested).unwrap();
        assert_eq!(
            initial.current_at(requested, Some(observed)).peers[0].reachability,
            Reachability::Reachable
        );
        let received = decode_publication(
            &live_encode(&state, 1000).unwrap(),
            observed + 1000,
            requested,
        )
        .unwrap();
        let aged = received.current_at(requested, Some(observed + 1000));
        assert_eq!(aged.visibility, Visibility::Active);
        assert_eq!(aged.observed_at_ms, observed);
        assert_eq!(aged.peers[0].reachability, Reachability::LastSeen);
        assert_eq!(aged.peers[1].reachability, Reachability::Reachable);
    }
}

#[test]
fn live_decoder_rejects_stale_future_and_unconfirmed_current_peers() {
    let state = current();
    let wire = live_encode(&state, 0).unwrap();
    assert!(live_decode(&wire, state.observed_at_ms).is_ok());
    assert!(live_decode(&wire, 0).is_err());
    assert!(live_decode(&wire, state.observed_at_ms - 1).is_err());
    assert!(live_decode(&wire, state.observed_at_ms + 15_001).is_err());
    for invalid in [
        Visibility::Unavailable,
        Visibility::Waiting,
        Visibility::Disabled,
    ] {
        let mut state = state.clone();
        state.visibility = invalid;
        assert!(live_decode(&live_encode(&state, 0).unwrap(), state.observed_at_ms).is_err());
    }
    let mut missing = state.clone();
    missing.peers[0].last_checked_ms = None;
    assert!(live_decode(&live_encode(&missing, 0).unwrap(), state.observed_at_ms).is_err());
    let mut future = state.clone();
    future.peers[0].last_seen_ms += 1;
    assert!(live_decode(&live_encode(&future, 0).unwrap(), state.observed_at_ms).is_err());
    let mut future_check = state.clone();
    future_check.peers[0].last_checked_ms = Some(state.observed_at_ms + 1);
    assert!(
        live_decode(
            &live_encode(&future_check, 0).unwrap(),
            state.observed_at_ms
        )
        .is_err()
    );
}

#[test]
fn producer_hint_capacity_round_trips_and_larger_counts_are_refused() {
    let mut state = current();
    let maximum = crate::runtime::MAX_UNVERIFIED_ENDPOINTS;
    for count in [0, 128, 129, maximum] {
        state.unverified_endpoints = count;
        let received = live_decode(&live_encode(&state, 0).unwrap(), state.observed_at_ms)
            .expect("a bounded producer count must decode");
        assert_eq!(received.unverified_endpoints, count);
    }
    for count in [maximum + 1, usize::MAX] {
        state.unverified_endpoints = count;
        assert!(
            live_decode(&live_encode(&state, 0).unwrap(), state.observed_at_ms).is_err(),
            "a count outside the producer's bound was accepted"
        );
    }
}

#[test]
fn malformed_frames_and_duplicate_identity_are_rejected() {
    let state = current();
    assert!(live_decode(b"{}", state.observed_at_ms).is_err());
    assert!(live_decode(&vec![b' '; MAX_BYTES + 1], state.observed_at_ms).is_err());
    let mut duplicate = state.clone();
    duplicate.peers.push(state.peers[0].clone());
    assert!(live_decode(&live_encode(&duplicate, 0).unwrap(), state.observed_at_ms).is_err());
    let mut oversized = state.clone();
    oversized.detail = "x".repeat(2049);
    assert!(live_decode(&live_encode(&oversized, 0).unwrap(), state.observed_at_ms).is_err());
}

#[test]
fn deadline_expires_across_partial_progress() {
    let (mut reader, _writer) = CtlStream::pair().unwrap();
    assert!(read_exact(&mut reader, &mut [0; 1], Instant::now()).is_err());
    assert!(write_all(&mut reader, b"x", Instant::now()).is_err());
}

fn fake_reply(body: &[u8], size: u32) -> io::Result<NetworkObservation> {
    let root = socket_dir();
    let config = root.path().join("aterm.toml");
    let dir = directory(&config);
    private_file::create_private_dir(&dir).unwrap();
    let path = dir.join(NAME);
    let listener = CtlListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let body = body.to_vec();
    let (release, hold) = std::sync::mpsc::channel();
    let sender = thread::spawn(move || {
        let hang = Instant::now() + Duration::from_secs(60);
        let mut client = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Until(hang),
            aterm_uds::AcceptedMode::Blocking,
        )
        .expect("fixture client connected")
        .0;
        let deadline = Instant::now() + EXCHANGE;
        let mut request = [0; REQUEST.len()];
        read_exact(&mut client, &mut request, deadline).unwrap();
        write_all(&mut client, &size.to_be_bytes(), deadline).unwrap();
        let _ = write_all(&mut client, &body, deadline);
        // Keep native peer identity and timeout operations valid until the reader finishes.
        hold.recv_timeout(Duration::from_secs(60))
            .expect("fixture reader did not finish");
    });
    let answer = read(&config);
    release.send(()).unwrap();
    sender.join().unwrap();
    answer
}

#[test]
fn connected_peer_cannot_forward_malformed_oversized_or_stale_frames() {
    let rejected = |error: io::Error, stage| {
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error_stage(&error), Some(stage));
    };
    rejected(fake_reply(b"{}", 2).unwrap_err(), "snapshot-version");
    rejected(fake_reply(b"\xff", 1).unwrap_err(), "snapshot-json");
    rejected(
        fake_reply(b"", (MAX_BYTES + 1) as u32).unwrap_err(),
        "frame-size",
    );
    let mut stale = current();
    stale.observed_at_ms -= 15_001;
    let wire = live_encode(&stale, 0).unwrap();
    rejected(
        fake_reply(&wire, wire.len() as u32).unwrap_err(),
        "sample-stale",
    );
    let mut future = current();
    future.observed_at_ms += 60_000;
    let wire = live_encode(&future, 0).unwrap();
    rejected(
        fake_reply(&wire, wire.len() as u32).unwrap_err(),
        "sample-clock",
    );
    let wire = live_encode(&current(), 15_001).unwrap();
    rejected(
        fake_reply(&wire, wire.len() as u32).unwrap_err(),
        "source-stale",
    );
}

#[test]
fn live_listener_and_unsafe_socket_mode_are_not_replaced() {
    let root = socket_dir();
    private_file::create_private_dir(root.path()).unwrap();
    let path = root.path().join(NAME);
    let listener = CtlListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let original = endpoint(root.path()).unwrap();
    assert!(Server::start(root.path(), ObservationHandle::default()).is_err());
    assert_eq!(endpoint(root.path()).unwrap(), original);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(Server::start(root.path(), ObservationHandle::default()).is_err());
    assert!(path.exists());
    drop(listener);
}

#[test]
fn full_or_unresponsive_listener_is_not_evidence_of_a_stale_endpoint() {
    for kind in [
        io::ErrorKind::TimedOut,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::Interrupted,
    ] {
        assert!(!stale(&io::Error::from(kind)));
    }
    assert!(stale(&io::Error::from(io::ErrorKind::ConnectionRefused)));
    assert!(stale(&io::Error::from(io::ErrorKind::NotFound)));
}

#[test]
fn long_socket_path_preserves_live_observation_and_history() {
    let root = socket_dir();
    let long = root.path().join("x".repeat(160));
    private_file::create_private_dir(&long).unwrap();
    let config = long.join("aterm.toml");
    let handle = ObservationHandle::default();
    let mut owner = Observer::open(&config, handle.clone()).unwrap();
    owner.observe(
        Visibility::Active,
        0,
        "Discovery only",
        aterm_time::unix_millis().unwrap(),
        Instant::now(),
    );
    assert_eq!(handle.snapshot().visibility, Visibility::Active);
    assert!(owner.log.as_ref().unwrap().stats().retained_records > 0);
    assert_eq!(read_history(&config).unwrap().history, HistoryState::Ready);
    let live = read(&config).unwrap();
    assert_eq!(live.visibility, Visibility::Active);
    assert_eq!(live.via_local_process, Some(std::process::id()));
}

#[test]
fn failed_start_retries_after_cadence_without_reopening_history() {
    let root = socket_dir();
    let config = root.path().join("aterm.toml");
    let dir = directory(&config);
    private_file::create_private_dir(&dir).unwrap();
    let path = dir.join(NAME);
    std::fs::write(&path, b"unrelated").unwrap();
    let mut owner = Observer::open(&config, ObservationHandle::default()).unwrap();
    assert!(owner.relay.is_none());
    std::fs::remove_file(directory(&config).join(NAME)).unwrap();
    let retry = Instant::now();
    owner.relay_checked = retry - Duration::from_secs(10);
    owner.observe(
        Visibility::Active,
        0,
        "Discovery only",
        aterm_time::unix_millis().unwrap(),
        retry - Duration::from_secs(1),
    );
    assert!(owner.relay.is_none());
    owner.observe(
        Visibility::Active,
        0,
        "Discovery only",
        aterm_time::unix_millis().unwrap(),
        retry,
    );
    assert!(owner.relay.as_ref().unwrap().is_running());
    assert_eq!(read(&config).unwrap().visibility, Visibility::Active);
    assert!(matches!(
        Observer::open(&config, ObservationHandle::default()),
        Err(OpenFailure::Owned)
    ));
}

#[test]
fn finished_worker_is_replaced_at_retry_cadence() {
    let root = socket_dir();
    let config = root.path().join("aterm.toml");
    let mut owner = Observer::open(&config, ObservationHandle::default()).unwrap();
    let server = owner.relay.as_mut().unwrap();
    drop(server.wake.take());
    let hang = Instant::now() + Duration::from_secs(60);
    while !server.task.as_ref().unwrap().is_finished() {
        assert!(Instant::now() < hang, "observer reader did not stop");
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!server.is_running());
    let retry = Instant::now();
    owner.relay_checked = retry - Duration::from_secs(10);
    owner.observe(
        Visibility::Active,
        0,
        "Discovery only",
        aterm_time::unix_millis().unwrap(),
        retry,
    );
    assert!(owner.relay.as_ref().unwrap().is_running());
    assert_eq!(read(&config).unwrap().visibility, Visibility::Active);
}
