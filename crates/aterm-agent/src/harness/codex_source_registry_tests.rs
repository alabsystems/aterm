// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// The tests gated `cfg(unix)` here observe a process (`native_act::Process::observe`), which
// only Unix reads.

use super::*;

#[test]
fn fixture_front_door_drop_retires_clones_and_releases_only_its_slot() {
    let registry = Registry::default();
    let survivor = registry.fixture_connection();
    // More sequential owners than the whole table can retain. No global
    // size assertion: other tests may own live connections concurrently.
    for _ in 0..257 {
        let front_door = registry.fixture_connection();
        let token = front_door.clone();
        assert!(token.is_live());
        assert_ne!(token.id, survivor.id);
        drop(front_door);
        assert!(!token.is_live());
        assert!(!connections().lock().unwrap().contains_key(&token.id));
        assert!(survivor.is_live());
    }
    let token = survivor.clone();
    registry.connection_closed(token.id);
    drop(survivor);
    assert!(!token.is_live());
    assert!(!connections().lock().unwrap().contains_key(&token.id));
}

#[cfg(unix)]
#[test]
fn fixture_connections_in_separate_registries_keep_independent_eof() {
    let first_registry = Registry::default();
    let second_registry = Registry::default();
    let first = first_registry.connection(fixture_connection_id()).unwrap();
    let second = second_registry.connection(fixture_connection_id()).unwrap();
    assert_ne!(first.id, second.id);
    assert!(!first.same(&second));
    let retained_first = first.clone();

    first_registry.connection_closed(first.id);
    assert!(!first.is_live());
    assert!(!retained_first.is_live());
    assert!(
        second.is_live(),
        "a peer fixture's EOF cannot close this token"
    );
    assert!(
        second_registry.connection(second.id).unwrap().same(&second),
        "the other socket tenure remains the same live token"
    );
    second_registry.connection_closed(second.id);
    assert!(!second.is_live());
}

#[cfg(unix)]
#[test]
fn replaced_gate_reconnect_keeps_old_reader_custody_and_refuses_manager_retarget() {
    let home = aterm_tempfile::tempdir().unwrap();
    let mut reader =
        SourceReadiness::preload(SourceWorkLease::for_home(home.path()).unwrap(), None).unwrap();
    reader.complete(ReadyRole::ReaderLoaded).unwrap();
    let original = home.path().join(".aterm/codex-source-custody/gate");
    std::fs::rename(&original, original.with_extension("old")).unwrap();
    let new = SourceWorkLease::for_home(home.path()).unwrap();
    assert!(SourceReadiness::preload(new, Some(&mut reader)).is_err());
    assert!(reader.gate.is_some());
    let other = aterm_tempfile::tempdir().unwrap();
    let mut manager =
        SourceReadiness::preload(SourceWorkLease::for_home(other.path()).unwrap(), None).unwrap();
    manager.complete(ReadyRole::PolicyUnavailable).unwrap();
    assert!(
        SourceReadiness::preload(
            SourceWorkLease::for_home(home.path()).unwrap(),
            Some(&mut manager)
        )
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn direct_reader_retains_real_gate_through_eof_and_reconnect_until_retirement() {
    // Exercises the exact member readiness/lease transitions with a real
    // kernel gate. It does not fabricate a qualified SDK/source/manager.
    let home = aterm_tempfile::tempdir().unwrap();
    let registry = Registry::default();
    let connection = registry.fixture_connection();
    let mut reader =
        SourceReadiness::preload(SourceWorkLease::for_home(home.path()).unwrap(), None).unwrap();
    reader.complete(ReadyRole::ReaderLoaded).unwrap();
    assert!(reader.reader());
    assert!(!reader.manager());
    registry.connection_closed(connection.id);
    assert!(!connection.is_live());
    assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
    let mut reconnected = SourceReadiness::preload(
        SourceWorkLease::for_home(home.path()).unwrap(),
        Some(&mut reader),
    )
    .unwrap();
    drop(reader);
    assert!(!reconnected.reader(), "preload is not a new ready receipt");
    assert!(reconnected.complete(ReadyRole::PolicyUnavailable).is_err());
    assert!(
        reconnected
            .complete(ReadyRole::Manager(AuthState {
                generation: 1,
                owner: 2,
                source: 3,
                login: 4,
            }))
            .is_err()
    );
    assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
    reconnected.complete(ReadyRole::ReaderLoaded).unwrap();
    reconnected.complete(ReadyRole::ReaderLoaded).unwrap();
    assert!(reconnected.reader());
    assert!(!reconnected.manager());
    drop(reconnected); // same owned-state drop as explicit member retirement
    assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_ok());
}

#[cfg(unix)]
#[test]
fn reader_proof_role_is_stale_after_gate_path_replacement() {
    let home = aterm_tempfile::tempdir().unwrap();
    let mut reader =
        SourceReadiness::preload(SourceWorkLease::for_home(home.path()).unwrap(), None).unwrap();
    reader.complete(ReadyRole::ReaderLoaded).unwrap();
    let gate = home.path().join(".aterm/codex-source-custody/gate");
    std::fs::rename(&gate, gate.with_extension("displaced")).unwrap();
    let replacement = SourceWorkLease::for_home(home.path()).unwrap();
    assert!(!reader.reader());
    assert_eq!(reader.complete(ReadyRole::ReaderLoaded), Err(Error::Stale));
    assert!(
        reader.gate.is_some(),
        "unknown does not release the old lease"
    );
    drop(replacement);
}

#[test]
fn reader_reconnect_revision_cannot_roll_back_or_replace_equal_generation() {
    assert!(revision_descends(13, "same", 13, "same"));
    assert!(revision_descends(17, "new", 13, "old"));
    assert!(!revision_descends(11, "old", 13, "current"));
    assert!(!revision_descends(13, "other", 13, "current"));
}

#[cfg(unix)]
#[test]
fn manager_and_policy_readiness_cannot_be_relabelled_as_direct_reader() {
    for role in [
        ReadyRole::PolicyUnavailable,
        ReadyRole::Manager(AuthState {
            generation: 7,
            owner: 11,
            source: 13,
            login: 17,
        }),
    ] {
        let home = aterm_tempfile::tempdir().unwrap();
        let mut readiness =
            SourceReadiness::preload(SourceWorkLease::for_home(home.path()).unwrap(), None)
                .unwrap();
        readiness.complete(role).unwrap();
        readiness.complete(role).unwrap();
        assert!(!readiness.reader());
        assert!(readiness.complete(ReadyRole::ReaderLoaded).is_err());
        assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_ok());
    }
}

#[cfg(unix)]
#[test]
fn constructor_quota_borrows_only_its_actual_scope_and_gate_incarnation() {
    // Constructor/kernel custody only, not a source or provider capability.
    let home = aterm_tempfile::tempdir().unwrap();
    let process = Process::observe(std::process::id()).unwrap();
    let lifetime = super::super::account_work::Lifetime::default();
    let scope = lifetime.enter().unwrap();
    let foreign = lifetime.enter().unwrap();
    let registry = Registry::default();
    let token = aterm_uds::rand::hex_token::<16>().unwrap();
    registry.state.lock().unwrap().launches.insert(
        token.clone(),
        Launch {
            sid: "s-quota-constructor-fixture".into(),
            image: process.image.clone(),
            expected_env: Vec::new(),
            expected_unset: Vec::new(),
            process: None,
            typed: false,
            gate: Some(Arc::new(SourceWorkLease::for_home(home.path()).unwrap())),
            _scope: Some(scope.clone()),
            initial: None,
            native_restart: None,
        },
    );
    let reservation = LaunchReservation {
        token: token.clone(),
        executable: std::env::current_exe().unwrap(),
        state: Arc::downgrade(&registry.state),
    };
    assert!(reservation.native_usage_source(&foreign).is_err());
    let borrowed = reservation.native_usage_source(&scope).unwrap();
    {
        let state = registry.state.lock().unwrap();
        assert!(Arc::ptr_eq(
            &borrowed.inner,
            state.launches[&token].gate.as_ref().unwrap()
        ));
    }
    scope.cancellation().cancel();
    assert!(reservation.native_usage_source(&scope).is_err());
    assert_eq!(registry.revoke_cancelled_unclaimed().unwrap(), 1);
    assert!(reservation.native_usage_source(&scope).is_err());
    assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
    drop(borrowed);
    assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_ok());
}

#[cfg(unix)]
#[test]
fn revoking_unclaimed_typed_permission_releases_gate_but_not_claimed_process() {
    // Constructor permission metadata only; this does not mint a qualified
    // SDK peer or a source/Ready proof.
    let home = aterm_tempfile::tempdir().unwrap();
    let process = Process::observe(std::process::id()).unwrap();
    let lifetime = super::super::account_work::Lifetime::default();
    let registry = Registry::default();
    let reserve = |token: &str, claimed: bool| {
        registry.state.lock().unwrap().launches.insert(
            token.into(),
            Launch {
                sid: "s-revocation-fixture".into(),
                image: process.image.clone(),
                expected_env: Vec::new(),
                expected_unset: Vec::new(),
                process: claimed.then(|| process.clone()),
                typed: true,
                gate: Some(Arc::new(SourceWorkLease::for_home(home.path()).unwrap())),
                _scope: Some(lifetime.enter().unwrap()),
                initial: None,
                native_restart: None,
            },
        );
        LaunchReservation {
            token: token.into(),
            executable: std::env::current_exe().unwrap(),
            state: Arc::downgrade(&registry.state),
        }
    };
    let unclaimed = reserve("unclaimed", false);
    assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
    unclaimed.revoke_unclaimed().unwrap();
    let publication =
        super::super::codex_source_gate::PublicationLock::for_home(home.path()).unwrap();
    assert!(
        !registry
            .state
            .lock()
            .unwrap()
            .launches
            .contains_key("unclaimed")
    );
    drop(publication);
    let claimed = reserve("claimed", true);
    assert_eq!(claimed.revoke_unclaimed(), Err(Error::Denied));
    assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
    registry.state.lock().unwrap().launches.clear();
    assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_ok());
}

#[cfg(unix)]
#[test]
fn unrelated_completion_retires_constructor_without_minting_a_source() {
    // Metadata lifecycle only. This deliberately cannot construct a real
    // qualified SDK peer or any positive native source proof.
    let home = aterm_tempfile::tempdir().unwrap();
    let process = Process::observe(std::process::id()).unwrap();
    let caller = slot::Caller {
        sid: "s-unrelated-fixture".into(),
        pid: process.pid,
        birth: process.birth,
    };
    let peer = || Peer {
        caller: caller.clone(),
        process: process.clone(),
        launch: Some("unrelated".into()),
    };
    let registry = Registry::default();
    let connection = registry.fixture_connection();
    let lifetime = super::super::account_work::Lifetime::default();
    registry.state.lock().unwrap().launches.insert(
        "unrelated".into(),
        Launch {
            sid: caller.sid.clone(),
            image: process.image.clone(),
            expected_env: Vec::new(),
            expected_unset: Vec::new(),
            process: Some(process.clone()),
            typed: true,
            gate: Some(Arc::new(SourceWorkLease::for_home(home.path()).unwrap())),
            _scope: Some(lifetime.enter().unwrap()),
            initial: None,
            native_restart: None,
        },
    );
    let operation = "1234567890abcdef1234567890abcdef";
    let allocation = registry
        .open_member(&peer(), &connection, operation)
        .unwrap();
    assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
    let registration = registry
        .complete_unrelated(
            peer(),
            &connection,
            &allocation.member,
            operation,
            NoNativeReaderKind::ApiKey,
        )
        .unwrap();
    assert!(registry.ready_source(&caller, &allocation.member).is_err());
    assert!(
        registry.state.lock().unwrap().launches["unrelated"]
            ._scope
            .is_none()
    );
    assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_ok());
    assert_eq!(
        registry
            .complete_unrelated(
                peer(),
                &connection,
                &allocation.member,
                operation,
                NoNativeReaderKind::ApiKey
            )
            .unwrap(),
        registration
    );
    assert_eq!(
        registry.complete_unrelated(
            peer(),
            &connection,
            &allocation.member,
            operation,
            NoNativeReaderKind::NoCredentials
        ),
        Err(Error::Stale)
    );
    let replacement = registry.fixture_connection();
    assert!(
        registry
            .open_member(&peer(), &replacement, operation)
            .is_err()
    );
    registry.connection_closed(connection.id);
    let reopened = registry
        .open_member(&peer(), &replacement, operation)
        .unwrap();
    assert_eq!(reopened.member, allocation.member);
    let refreshed = registry
        .complete_unrelated(
            peer(),
            &replacement,
            &allocation.member,
            operation,
            NoNativeReaderKind::ApiKey,
        )
        .unwrap();
    assert_ne!(refreshed, registration);
    assert!(
        registry
            .retire_member(
                peer(),
                &replacement,
                &allocation.member,
                Some(&registration)
            )
            .is_err()
    );
    registry
        .retire_member(peer(), &replacement, &allocation.member, Some(&refreshed))
        .unwrap();
    assert!(
        registry
            .complete_unrelated(
                peer(),
                &replacement,
                &allocation.member,
                operation,
                NoNativeReaderKind::ApiKey
            )
            .is_err()
    );
    registry.connection_closed(replacement.id);
}

#[cfg(unix)]
#[test]
fn app_pause_revokes_only_cancelled_unclaimed_permissions_and_bind_transfers_scope() {
    // Constructor lifetime metadata, not qualified peer/source authority.
    let home = aterm_tempfile::tempdir().unwrap();
    let process = Process::observe(std::process::id()).unwrap();
    let lifetime = super::super::account_work::Lifetime::default();
    let registry = Registry::default();
    let launch = |scope| Launch {
        sid: "s-drain-fixture".into(),
        image: process.image.clone(),
        expected_env: Vec::new(),
        expected_unset: Vec::new(),
        process: None,
        typed: true,
        gate: Some(Arc::new(SourceWorkLease::for_home(home.path()).unwrap())),
        _scope: Some(scope),
        initial: None,
        native_restart: None,
    };
    let pending = lifetime.enter().unwrap();
    let pending_cancel = pending.cancellation();
    registry
        .state
        .lock()
        .unwrap()
        .launches
        .insert("pending".into(), launch(pending));
    let bound = lifetime.enter().unwrap();
    let bound_cancel = bound.cancellation();
    let mut bound = launch(bound);
    bound.bind(process).unwrap();
    assert!(bound_cancel.retired());
    registry
        .state
        .lock()
        .unwrap()
        .launches
        .insert("bound".into(), bound);
    assert_eq!(registry.revoke_cancelled_unclaimed().unwrap(), 0);
    assert!(!pending_cancel.retired());
    let _pause = lifetime.pause().unwrap();
    assert_eq!(registry.revoke_cancelled_unclaimed().unwrap(), 1);
    assert!(pending_cancel.retired());
    assert!(
        !registry
            .state
            .lock()
            .unwrap()
            .launches
            .contains_key("pending")
    );
    assert!(
        registry.state.lock().unwrap().launches["bound"]
            .gate
            .is_some()
    );
    assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_err());
    assert_eq!(registry.revoke_cancelled_unclaimed().unwrap(), 0);
    registry.state.lock().unwrap().launches.clear();
    assert!(super::super::codex_source_gate::PublicationLock::for_home(home.path()).is_ok());
}

#[test]
fn eof_invalidates_connection_while_member_table_is_busy() {
    let registry = Registry::default();
    let connection = registry.fixture_connection();
    let held = registry.state.lock().unwrap();
    registry.connection_closed(connection.id);
    assert!(!connection.alive.load(Ordering::Acquire));
    drop(held);
}

#[cfg(unix)]
#[test]
fn orderly_allocation_retirement_is_bounded_and_never_reuses_member_ids() {
    // Allocation metadata only: no native source/credential or Ready proof
    // is constructed. Production Peer constructors require the qualified
    // image plus the actual PTY/constructor; this uses our real kernel PID.
    let process = Process::observe(std::process::id()).unwrap();
    let caller = slot::Caller {
        sid: "s-allocation-fixture".into(),
        pid: process.pid,
        birth: process.birth,
    };
    let peer = || Peer {
        caller: caller.clone(),
        process: process.clone(),
        launch: None,
    };
    let registry = Registry::default();
    let connection = registry.fixture_connection();
    let mut first = None;
    for number in 0..(MAX_MEMBERS + 32) {
        let operation = format!("{number:032x}");
        let allocation = registry
            .open_member(&peer(), &connection, &operation)
            .unwrap();
        assert!(hex(&allocation.member, 32));
        if first.is_none() {
            first = Some(allocation.member.clone());
        }
        registry
            .retire_member(peer(), &connection, &allocation.member, None)
            .unwrap();
        assert!(issued(&registry.state.lock().unwrap(), &peer(), &allocation.member).is_err());
    }
    let state = registry.state.lock().unwrap();
    let actor = state.actors.get(&actor_key(&caller)).unwrap();
    assert!(actor.issued.is_empty());
    assert_eq!(actor.highwater, (MAX_MEMBERS + 32) as u64);
    assert!(!actor.issued.contains_key(&first.unwrap()));
    drop(state);
    registry.connection_closed(connection.id);
}
