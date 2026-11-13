// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::super::{
    account_work, codex_assignment::prepare, codex_session_slot::metadata_fixture,
    codex_source_registry::FixtureConnection,
};
use super::*;

struct Fixture {
    registry: Registry,
    peer: Peer,
    connection: FixtureConnection,
    member: String,
    operation: String,
    registration: String,
    lifetime: account_work::Lifetime,
    scope: Option<account_work::Scope>,
    _home: aterm_tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        // Synthetic source/peer injection, like the registry's existing
        // metadata lifecycle tests. No supported SDK/owner/source admission is
        // claimed. Process observation, gates, connections and scopes are real.
        let home = aterm_tempfile::tempdir().unwrap();
        let process = Process::observe(std::process::id()).unwrap();
        let lifetime = account_work::Lifetime::default();
        let registry = Registry::for_app(&lifetime);
        let connection = registry.fixture_connection();
        let id = connection.id;
        let peer = Peer {
            caller: slot::Caller {
                sid: format!("s-use-fixture-{id}"),
                pid: process.pid,
                birth: process.birth,
            },
            process,
            launch: None,
        };
        let operation = "a".repeat(32);
        let allocation = registry
            .open_member(&peer, &connection, &operation)
            .unwrap();
        let registration = "b".repeat(32);
        let source = Arc::new(metadata_fixture::source(
            metadata_fixture::binding("source"),
            home.path().join("auth.json"),
            1,
        ));
        let mut readiness =
            SourceReadiness::preload(SourceWorkLease::for_home(home.path()).unwrap(), None)
                .unwrap();
        readiness.complete(ReadyRole::Manager(auth())).unwrap();
        let key = member_key(&peer.caller, &allocation.member).unwrap();
        let mut state = registry.state.lock().unwrap();
        let version = version(&mut state).unwrap();
        state.members.insert(
            key,
            Member {
                caller: peer.caller.clone(),
                process: peer.process.clone(),
                operation: operation.clone(),
                registration: registration.clone(),
                connection: connection.clone(),
                version,
                source,
                readiness,
                unknown: false,
                work_rebound: false,
            },
        );
        drop(state);
        let scope = Some(lifetime.enter().unwrap());
        Self {
            registry,
            peer,
            connection,
            member: allocation.member,
            operation,
            registration,
            lifetime,
            scope,
            _home: home,
        }
    }

    fn request(&self, counter: u64) -> UseRequest<'_> {
        UseRequest {
            member: &self.member,
            registration: &self.registration,
            nonce: UseNonce::parse(&format!("1234567890abcdef{counter:016x}")).unwrap(),
        }
    }

    fn begin(&self, counter: u64) -> String {
        // One real native operation per Begin, as the production front door.
        let scope = self
            .lifetime
            .enter()
            .or_else(|| self.scope.clone())
            .unwrap();
        match self
            .registry
            .begin_use(
                peer(&self.peer),
                &self.connection,
                self.request(counter),
                &scope,
            )
            .unwrap()
        {
            UseProgress::Active(receipt) => receipt.capability,
            UseProgress::Closed => panic!("unexpected closed use"),
        }
    }

    fn reservation(&self) -> Reservation {
        let mut state = self.registry.state.lock().unwrap();
        let actor = state.actors.get(&actor_key(&self.peer.caller)).unwrap();
        let cell = Arc::new(Mutex::new(Cell {
            origin: origin::Origin::capture(
                state
                    .members
                    .get(&member_key(&self.peer.caller, &self.member).unwrap())
                    .unwrap(),
            ),
            target: prepare::reservation_metadata_fixture(
                self.peer.caller.clone(),
                metadata_fixture::binding("source"),
                metadata_fixture::binding("target"),
            ),
            process: self.peer.process.clone(),
            capability: "f".repeat(64),
            issued: actor
                .issued
                .iter()
                .map(|(member, issued)| (member.clone(), issued.operation.clone()))
                .collect(),
            quiescent: BTreeMap::new(),
            phase: seal::Phase::Draining,
            target_resource: None,
            target_driver: Arc::new(false.into()),
            installed: None,
            target_issued: BTreeMap::new(),
            target_constructors: BTreeMap::new(),
            target_retiring: Vec::new(),
            resource_phase: target::resources::Phase::Initializing,
            _scope: Some(self.scope.as_ref().unwrap().clone()),
            retirement: None,
            cancelled: None,
        }));
        state
            .reservations
            .insert(actor_key(&self.peer.caller), cell.clone());
        Reservation {
            state: Arc::downgrade(&self.registry.state),
            cell,
        }
    }
}

fn auth() -> AuthState {
    AuthState {
        generation: 1,
        owner: 2,
        source: 3,
        login: 4,
    }
}

#[test]
fn terminal_reservation_keeps_metadata_without_pinning_its_own_custody() {
    // Synthetic source/peer; actual App captures, kernel locks and retirement.
    // No cancellation or Applied producer authority is claimed by this fixture.
    let mut fixture = Fixture::new();
    let reservation = fixture.reservation();
    let seal = seal::kernel_fixture(&reservation);
    let capture = reservation.cell.lock().unwrap().scope().unwrap().clone();
    drop(fixture.scope.take());
    let pause = fixture.lifetime.pause().unwrap();
    let custody = {
        let _state = fixture.registry.state.lock().unwrap();
        let mut cell = reservation.cell.lock().unwrap();
        let prior = std::mem::replace(&mut cell.phase, seal::Phase::Terminal);
        cell.phase = seal::Phase::Cancelling(Box::new(prior));
        let custody = cell.take_custody();
        assert!(cell.scope().is_err());
        assert!(!cell.custody_retired());
        custody
    };
    // Only outside State/Cell may the real resource and original Scope retire.
    drop(custody);
    assert!(!seal.current());
    assert!(
        crate::harness::codex_source_gate::PublicationLock::try_for_home(fixture._home.path())
            .unwrap()
            .is_some()
    );
    assert!(!reservation.cell.lock().unwrap().custody_retired());
    let pause = pause
        .try_wait()
        .expect_err("captured native Scope still blocks drain");
    drop(capture);
    assert!(reservation.cell.lock().unwrap().custody_retired());
    pause.try_wait().unwrap().resume().unwrap();
    // Both the external handle and registry still retain this metadata Cell.
    assert!(
        fixture
            .registry
            .state
            .lock()
            .unwrap()
            .reservations
            .contains_key(&actor_key(&fixture.peer.caller))
    );
}

#[test]
fn captured_native_source_kernel_keeps_original_operation_count_until_actual_drop() {
    // Synthetic source admission, actual kernel/Scope ownership. The source
    // weak handle never substitutes for an actual captured native resource.
    let mut fixture = Fixture::new();
    let reservation = fixture.reservation();
    let sealed = seal::kernel_fixture(&reservation);
    let captured = sealed.retained_kernel().unwrap();
    drop(fixture.scope.take());
    let pause = fixture.lifetime.pause().unwrap();
    let custody = reservation.cell.lock().unwrap().take_custody();
    drop(custody);
    assert!(!reservation.cell.lock().unwrap().custody_retired());
    let pause = pause
        .try_wait()
        .expect_err("actual kernel capture still owns original Scope");
    assert!(!sealed.current());
    drop(captured);
    assert!(reservation.cell.lock().unwrap().custody_retired());
    pause.try_wait().unwrap().resume().unwrap();
}

#[test]
fn paged_census_covers_full_cohort_and_rejects_changed_observations() {
    let fixture = Fixture::new();
    let reservation = fixture.reservation();
    let capability = fixture.begin(1);
    let context = fixture
        .registry
        .use_context(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    for ordinal in 1..MAX_MEMBERS {
        let child = fixture
            .registry
            .open_member_derived(
                &fixture.peer,
                &fixture.connection,
                &format!("{ordinal:032x}"),
                &context,
            )
            .unwrap();
        fixture
            .registry
            .retire_member(
                peer(&fixture.peer),
                &fixture.connection,
                &child.member,
                None,
            )
            .unwrap();
    }
    let expected = reservation
        .census(&fixture.peer)
        .unwrap()
        .into_iter()
        .map(|row| row.member)
        .collect::<Vec<_>>();
    let first = reservation.census_page(&fixture.peer, None).unwrap();
    assert_eq!(first.rows.len(), 16);
    let revision = first.revision;
    let first_next = first.next.clone().unwrap();
    let mut next = first.next;
    let mut actual = first
        .rows
        .into_iter()
        .map(|row| row.member)
        .collect::<Vec<_>>();
    while let Some(after) = next {
        let page = reservation
            .census_page(
                &fixture.peer,
                Some(CensusCursor {
                    revision: &revision,
                    after: &after,
                }),
            )
            .unwrap();
        assert_eq!(page.revision, revision);
        assert!(page.rows.len() <= 16);
        actual.extend(page.rows.into_iter().map(|row| row.member));
        next = page.next;
    }
    assert_eq!(actual, expected);
    assert_eq!(actual.len(), MAX_MEMBERS);
    assert!(matches!(
        reservation.census_page(
            &fixture.peer,
            Some(CensusCursor {
                revision: &revision,
                after: &"0".repeat(32),
            })
        ),
        Err(Error::Stale)
    ));
    // A metadata-injected credential refresh changes no member allocation.
    // Even so, continuation must reject a mixture of old and new observations.
    let key = member_key(&fixture.peer.caller, &fixture.member).unwrap();
    fixture
        .registry
        .state
        .lock()
        .unwrap()
        .members
        .get_mut(&key)
        .unwrap()
        .source = Arc::new(metadata_fixture::source(
        metadata_fixture::binding("source"),
        fixture._home.path().join("auth.json"),
        2,
    ));
    assert!(matches!(
        reservation.census_page(
            &fixture.peer,
            Some(CensusCursor {
                revision: &revision,
                after: &first_next,
            })
        ),
        Err(Error::Stale)
    ));
    assert_ne!(
        reservation
            .census_page(&fixture.peer, None)
            .unwrap()
            .revision,
        revision
    );
}

#[test]
fn reservation_origin_survives_explicit_retirement_but_not_gate_replacement() {
    // Synthetic member metadata; actual retirement, process and physical gate.
    let fixture = Fixture::new();
    let reservation = fixture.reservation();
    fixture
        .registry
        .retire_member(
            peer(&fixture.peer),
            &fixture.connection,
            &fixture.member,
            Some(&fixture.registration),
        )
        .unwrap();
    assert!(reservation.cell.lock().unwrap().origin.current());
    let directory = fixture._home.path().join(".aterm/codex-source-custody");
    std::fs::rename(directory.join("gate"), directory.join("retired-gate")).unwrap();
    let _replacement = SourceWorkLease::for_home(fixture._home.path()).unwrap();
    assert!(!reservation.cell.lock().unwrap().origin.current());
}

#[test]
fn reservation_census_keeps_same_grant_refresh_but_refuses_replaced_startup_authority() {
    let fixture = Fixture::new();
    let reservation = fixture.reservation();
    let key = member_key(&fixture.peer.caller, &fixture.member).unwrap();
    let refreshed = metadata_fixture::source(
        metadata_fixture::binding("source"),
        fixture._home.path().join("auth.json"),
        2,
    );
    {
        let mut state = fixture.registry.state.lock().unwrap();
        let member = state.members.get_mut(&key).unwrap();
        member.source = Arc::new(refreshed);
    }
    let census = reservation.census(&fixture.peer).unwrap();
    assert!(census[0].condition == CensusCondition::Manager);
    assert_eq!(census[0].credential_generation, Some(2));
    let replaced = metadata_fixture::with_authority(
        metadata_fixture::source(
            metadata_fixture::binding("source"),
            fixture._home.path().join("auth.json"),
            2,
        ),
        super::super::super::codex_assignment::SourceAuthority::Startup {
            nonce: "5".repeat(32),
        },
    );
    fixture
        .registry
        .state
        .lock()
        .unwrap()
        .members
        .get_mut(&key)
        .unwrap()
        .source = Arc::new(replaced);
    assert!(reservation.census(&fixture.peer).unwrap()[0].condition == CensusCondition::Unknown);
    assert!(
        reservation
            .quiesce(
                &fixture.peer,
                &fixture.connection,
                Quiescence {
                    member: &fixture.member,
                    registration: &fixture.registration,
                    credential_generation: 2,
                    digest: &format!("{:064x}", 2),
                    auth: Some(auth()),
                }
            )
            .is_err()
    );
}

#[test]
fn full_reservation_census_refuses_derived_allocation_without_actor_mutation() {
    let fixture = Fixture::new();
    let reservation = fixture.reservation();
    let capability = fixture.begin(1);
    let context = fixture
        .registry
        .use_context(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    // Real accepted/retired child transitions fill retained census history;
    // retirement does not claim that an old open nonce can be forgotten.
    for ordinal in 1..MAX_MEMBERS {
        let child = fixture
            .registry
            .open_member_derived(
                &fixture.peer,
                &fixture.connection,
                &format!("{ordinal:032x}"),
                &context,
            )
            .unwrap();
        fixture
            .registry
            .retire_member(
                peer(&fixture.peer),
                &fixture.connection,
                &child.member,
                None,
            )
            .unwrap();
    }
    let snapshot = || {
        let state = fixture.registry.state.lock().unwrap();
        let actor = state.actors.get(&actor_key(&fixture.peer.caller)).unwrap();
        (
            actor.highwater,
            actor.issued.keys().cloned().collect::<Vec<_>>(),
        )
    };
    let before = snapshot();
    let revision = reservation.revision(&fixture.peer).unwrap();
    assert!(matches!(
        fixture.registry.open_member_derived(
            &fixture.peer,
            &fixture.connection,
            &"f".repeat(32),
            &context,
        ),
        Err(Error::Busy)
    ));
    assert_eq!(
        snapshot(),
        before,
        "a refused child must not become untracked custody"
    );
    assert_eq!(reservation.revision(&fixture.peer).unwrap(), revision);
}
fn peer(value: &Peer) -> Peer {
    Peer {
        caller: value.caller.clone(),
        process: value.process.clone(),
        launch: None,
    }
}

#[test]
fn cancelled_unaccepted_nonce_fences_delayed_begin_and_prefix_reset() {
    let fixture = Fixture::new();
    assert!(matches!(
        fixture
            .registry
            .close_unaccepted_use(&fixture.peer, &fixture.connection, fixture.request(3))
            .unwrap(),
        UseProgress::Closed
    ));
    assert!(matches!(
        fixture
            .registry
            .begin_use(
                peer(&fixture.peer),
                &fixture.connection,
                fixture.request(3),
                fixture.scope.as_ref().unwrap()
            )
            .unwrap(),
        UseProgress::Closed
    ));
    let mut changed = fixture.request(3);
    changed.registration = "cccccccccccccccccccccccccccccccc";
    assert!(
        fixture
            .registry
            .close_unaccepted_use(&fixture.peer, &fixture.connection, changed)
            .is_err()
    );
    assert!(
        fixture
            .registry
            .begin_use(
                peer(&fixture.peer),
                &fixture.connection,
                fixture.request(2),
                fixture.scope.as_ref().unwrap()
            )
            .is_err()
    );
    let mut reset = fixture.request(4);
    reset.nonce = UseNonce::parse("ffffffffffffffff0000000000000004").unwrap();
    assert!(
        fixture
            .registry
            .begin_use(
                peer(&fixture.peer),
                &fixture.connection,
                reset,
                fixture.scope.as_ref().unwrap()
            )
            .is_err()
    );
    for invalid in [
        "1234567890abcdef0000000000000000",
        "1234567890ABCDEF0000000000000001",
        "1234",
    ] {
        assert!(UseNonce::parse(invalid).is_none());
    }
}

#[test]
fn accepted_capability_survives_refresh_for_cleanup_and_eof_retains_scope() {
    let mut fixture = Fixture::new();
    let capability = fixture.begin(1);
    let context = fixture
        .registry
        .use_context(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    assert!(context.current());
    {
        let mut state = fixture.registry.state.lock().unwrap();
        let member = state
            .members
            .get_mut(&member_key(&fixture.peer.caller, &fixture.member).unwrap())
            .unwrap();
        member.source = Arc::new(metadata_fixture::source(
            metadata_fixture::binding("source"),
            fixture._home.path().join("auth.json"),
            2,
        ));
        member.registration = "c".repeat(32);
    }
    assert!(context.current());
    assert_eq!(fixture.begin(1), capability);
    let mut changed = fixture.request(1);
    changed.registration = "cccccccccccccccccccccccccccccccc";
    assert!(
        fixture
            .registry
            .begin_use(
                peer(&fixture.peer),
                &fixture.connection,
                changed,
                fixture.scope.as_ref().unwrap()
            )
            .is_err()
    );
    fixture.registry.connection_closed(fixture.connection.id);
    assert!(!context.current());
    drop(context);
    let pause = fixture.lifetime.pause().unwrap();
    drop(fixture.scope.take());
    let pause = pause
        .try_wait()
        .expect_err("EOF and handle drop cannot retire the active use");
    let connection = fixture.registry.fixture_connection();
    fixture
        .registry
        .open_member(&fixture.peer, &connection, &fixture.operation)
        .unwrap();
    assert!(matches!(
        fixture
            .registry
            .begin_use(
                peer(&fixture.peer),
                &connection,
                fixture.request(1),
                &account_work::Lifetime::default().enter().unwrap()
            )
            .unwrap(),
        UseProgress::Active(_)
    ));
    assert!(
        fixture
            .registry
            .use_context(
                &fixture.peer,
                &connection,
                &fixture.member,
                fixture.request(1).nonce,
                &capability
            )
            .is_err(),
        "cap recovery is not fresh admission"
    );
    fixture
        .registry
        .end_use(
            &fixture.peer,
            &connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    fixture
        .registry
        .end_use(
            &fixture.peer,
            &connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    assert!(pause.try_wait().is_ok());
}

#[test]
fn policy_unavailable_and_no_reader_members_cannot_mint_managed_use() {
    let fixture = Fixture::new();
    let key = member_key(&fixture.peer.caller, &fixture.member).unwrap();
    fixture
        .registry
        .state
        .lock()
        .unwrap()
        .members
        .get_mut(&key)
        .unwrap()
        .readiness
        .role = Some(ReadyRole::PolicyUnavailable);
    assert!(
        fixture
            .registry
            .begin_use(
                peer(&fixture.peer),
                &fixture.connection,
                fixture.request(1),
                fixture.scope.as_ref().unwrap()
            )
            .is_err()
    );
    fixture.registry.state.lock().unwrap().members.remove(&key);
    assert!(
        fixture
            .registry
            .begin_use(
                peer(&fixture.peer),
                &fixture.connection,
                fixture.request(1),
                fixture.scope.as_ref().unwrap()
            )
            .is_err()
    );
    assert!(
        !fixture
            .registry
            .state
            .lock()
            .unwrap()
            .uses
            .contains_key(&key)
    );
}

#[test]
fn dedicated_cleanup_does_not_reopen_ready_or_fabricate_unknown_retired_use_receipts() {
    let mut fixture = Fixture::new();
    let capability = fixture.begin(1);
    let cleanup = fixture.registry.fixture_connection();
    // Original work connection is still live; cleanup must not steal/rebind it.
    fixture
        .registry
        .end_use(
            &fixture.peer,
            &cleanup,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    {
        let mut state = fixture.registry.state.lock().unwrap();
        let member = state
            .members
            .get_mut(&member_key(&fixture.peer.caller, &fixture.member).unwrap())
            .unwrap();
        member.unknown = true;
        member.readiness.role = Some(ReadyRole::PolicyUnavailable);
    }
    assert!(matches!(
        fixture
            .registry
            .close_unaccepted_use(&fixture.peer, &cleanup, fixture.request(2))
            .unwrap(),
        UseProgress::Closed
    ));
    fixture
        .registry
        .retire_member(
            peer(&fixture.peer),
            &fixture.connection,
            &fixture.member,
            Some(&fixture.registration),
        )
        .unwrap();
    assert!(
        fixture
            .registry
            .end_use(
                &fixture.peer,
                &cleanup,
                &fixture.member,
                fixture.request(1).nonce,
                &capability
            )
            .is_ok()
    );
    assert!(
        fixture
            .registry
            .begin_use(
                peer(&fixture.peer),
                &cleanup,
                fixture.request(3),
                fixture.scope.as_ref().unwrap()
            )
            .is_err()
    );
    fixture.registry.reap_gone();
    assert!(
        fixture
            .registry
            .end_use(
                &fixture.peer,
                &cleanup,
                &fixture.member,
                fixture.request(1).nonce,
                &capability
            )
            .is_ok()
    );
    assert!(matches!(
        fixture
            .registry
            .begin_use(
                peer(&fixture.peer),
                &cleanup,
                fixture.request(1),
                fixture.scope.as_ref().unwrap()
            )
            .unwrap(),
        UseProgress::Closed
    ));
    assert!(
        fixture
            .registry
            .close_unaccepted_use(&fixture.peer, &cleanup, fixture.request(3))
            .is_err()
    );
    assert!(
        fixture
            .registry
            .end_use(
                &fixture.peer,
                &cleanup,
                &fixture.member,
                fixture.request(1).nonce,
                &"0".repeat(64)
            )
            .is_err()
    );
    assert_eq!(
        fixture.registry.state.lock().unwrap().uses
            [&member_key(&fixture.peer.caller, &fixture.member).unwrap()]
            .highwater,
        2
    );
    drop(fixture.scope.take());
    let drained = fixture.lifetime.pause().unwrap().try_wait().unwrap();
    assert!(
        matches!(
            fixture
                .registry
                .prepare_use_cleanup(
                    &fixture.peer.caller,
                    &cleanup,
                    UseCleanupRequest::End {
                        member: &fixture.member,
                        nonce: fixture.request(1).nonce,
                        capability: &capability,
                    }
                )
                .unwrap(),
            CleanupAdmission::Closed
        ),
        "known exact replay is correlation-only after drain sealing"
    );
    assert!(
        fixture
            .registry
            .prepare_use_cleanup(
                &fixture.peer.caller,
                &cleanup,
                UseCleanupRequest::Close(fixture.request(3))
            )
            .is_err(),
        "a retired highwater cannot mint a late closed receipt"
    );
    drained.resume().unwrap();
}

#[test]
fn metadata_inspection_releases_registry_and_rechecks_retired_use() {
    let fixture = Fixture::new();
    let capability = fixture.begin(1);
    let context = fixture
        .registry
        .use_context(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    let result = context.expected_source_checked(&fixture.peer.caller, |gate| {
        assert!(fixture.registry.state.try_lock().is_ok());
        let current = gate.matches_home(fixture._home.path());
        assert!(current);
        assert!(matches!(
            fixture.registry.end_use(
                &fixture.peer,
                &fixture.connection,
                &fixture.member,
                fixture.request(1).nonce,
                &capability
            ),
            Err(Error::Busy)
        ));
        current
    });
    assert!(result.is_err(), "metadata cannot revive a retired use");
}

#[test]
fn source_continuation_joins_actual_namespace_and_denies_cancelled_work() {
    let fixture = Fixture::new();
    let capability = fixture.begin(1);
    let context = fixture
        .registry
        .use_context(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    assert!(
        context
            .expected_source(&fixture.peer.caller, fixture._home.path())
            .is_ok()
    );
    let other = aterm_tempfile::tempdir().unwrap();
    let _other_gate = SourceWorkLease::for_home(other.path()).unwrap();
    assert!(
        context
            .expected_source(&fixture.peer.caller, other.path())
            .is_err()
    );
    let pause = fixture.lifetime.pause().unwrap();
    assert!(!context.current());
    assert!(
        context
            .expected_source(&fixture.peer.caller, fixture._home.path())
            .is_err()
    );
    assert_eq!(
        fixture.begin(1),
        capability,
        "cancelled work still recovers its accepted capability for cleanup"
    );
    fixture
        .registry
        .end_use(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    drop(pause);
}

#[test]
fn startup_epoch_change_invalidates_work_scope_but_not_accepted_cleanup_replay() {
    let fixture = Fixture::new();
    let capability = fixture.begin(1);
    let context = fixture
        .registry
        .use_context(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    let mut state = fixture.registry.state.lock().unwrap();
    let member = state
        .members
        .get_mut(&member_key(&fixture.peer.caller, &fixture.member).unwrap())
        .unwrap();
    let changed = metadata_fixture::with_authority(
        metadata_fixture::source(
            metadata_fixture::binding("source"),
            fixture._home.path().join("auth.json"),
            1,
        ),
        crate::harness::codex_assignment::SourceAuthority::Startup {
            nonce: "5".repeat(32),
        },
    );
    assert!(member.source.native_intent().is_none() && changed.native_intent().is_none());
    assert!(!same_source_scope(&member.source, &changed));
    member.source = Arc::new(changed);
    drop(state);
    assert!(!context.current());
    assert_eq!(fixture.begin(1), capability);
    fixture
        .registry
        .end_use(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
}

#[test]
fn active_uses_never_evict_when_receipts_and_highwater_advance() {
    let fixture = Fixture::new();
    let first = fixture.begin(1);
    for counter in 2..=MAX_MEMBER_USES as u64 {
        fixture.begin(counter);
    }
    assert!(
        fixture
            .registry
            .begin_use(
                peer(&fixture.peer),
                &fixture.connection,
                fixture.request(33),
                fixture.scope.as_ref().unwrap()
            )
            .is_err()
    );
    fixture
        .registry
        .close_unaccepted_use(&fixture.peer, &fixture.connection, fixture.request(100))
        .unwrap();
    assert_eq!(fixture.begin(1), first);
    fixture.registry.connection_closed(fixture.connection.id);
    let state = fixture.registry.state.lock().unwrap();
    assert_eq!(
        state.uses[&member_key(&fixture.peer.caller, &fixture.member).unwrap()]
            .active
            .len(),
        MAX_MEMBER_USES
    );
}

#[test]
fn reserved_census_allows_parent_drain_and_derived_join_then_requires_end_before_quiescence() {
    let fixture = Fixture::new();
    let reservation = fixture.reservation();
    let original_revision = reservation.revision(&fixture.peer).unwrap();
    assert!(
        fixture
            .registry
            .open_member(&fixture.peer, &fixture.connection, &"d".repeat(32))
            .is_err()
    );
    assert_eq!(
        fixture
            .registry
            .open_member(&fixture.peer, &fixture.connection, &fixture.operation)
            .unwrap()
            .member,
        fixture.member
    );
    let capability = fixture.begin(1);
    let context = fixture
        .registry
        .use_context(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    let child = fixture
        .registry
        .open_member_derived(
            &fixture.peer,
            &fixture.connection,
            &"e".repeat(32),
            &context,
        )
        .unwrap();
    let joined_revision = reservation.revision(&fixture.peer).unwrap();
    assert_ne!(original_revision, joined_revision);
    fixture
        .registry
        .retire_member(
            peer(&fixture.peer),
            &fixture.connection,
            &child.member,
            None,
        )
        .unwrap();
    assert_eq!(
        joined_revision,
        reservation.revision(&fixture.peer).unwrap(),
        "known retirement satisfies the census without recensus"
    );
    assert!(
        reservation
            .census(&fixture.peer)
            .unwrap()
            .iter()
            .any(|row| row.member == child.member && row.condition == CensusCondition::Retired)
    );
    let report = || Quiescence {
        member: &fixture.member,
        registration: &fixture.registration,
        credential_generation: 1,
        digest: "0000000000000000000000000000000000000000000000000000000000000001",
        auth: Some(auth()),
    };
    assert!(
        reservation
            .quiesce(&fixture.peer, &fixture.connection, report())
            .is_err()
    );
    assert!(
        fixture
            .registry
            .retire_member(
                peer(&fixture.peer),
                &fixture.connection,
                &fixture.member,
                Some(&fixture.registration)
            )
            .is_err()
    );
    fixture
        .registry
        .end_use(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    reservation
        .quiesce(&fixture.peer, &fixture.connection, report())
        .unwrap();
    assert!(
        fixture
            .registry
            .begin_use(
                peer(&fixture.peer),
                &fixture.connection,
                fixture.request(2),
                fixture.scope.as_ref().unwrap()
            )
            .is_err()
    );
    let capability = reservation.capability(&fixture.peer).unwrap();
    let operation = reservation.operation(&fixture.peer).unwrap();
    drop(reservation);
    assert!(fixture.registry.has_reservation(&fixture.peer.caller));
    let recovered = fixture
        .registry
        .reservation_for_peer(&fixture.peer)
        .unwrap()
        .unwrap();
    assert_eq!(recovered.capability(&fixture.peer).unwrap(), capability);
    assert!(
        fixture
            .registry
            .resume_reservation(&fixture.peer, &operation, &capability)
            .is_ok()
    );
    assert!(
        fixture
            .registry
            .resume_reservation(&fixture.peer, &"0".repeat(32), &capability)
            .is_err()
    );
    assert!(
        fixture
            .registry
            .resume_reservation(&fixture.peer, &operation, &"0".repeat(64))
            .is_err()
    );
    assert!(
        fixture
            .registry
            .open_member(&fixture.peer, &fixture.connection, &"f".repeat(32))
            .is_err()
    );
}

#[test]
fn target_open_strings_cannot_allocate_without_actual_installed_custody() {
    let fixture = Fixture::new();
    let _reservation = fixture.reservation();
    let before = {
        let state = fixture.registry.state.lock().unwrap();
        let actor = state.actors.get(&actor_key(&fixture.peer.caller)).unwrap();
        (
            actor.highwater,
            actor.issued.len(),
            state.target_opens.len(),
        )
    };
    assert!(
        fixture
            .registry
            .prepare_target_open(
                &fixture.peer,
                &fixture.connection,
                &"a".repeat(32),
                &"c".repeat(64),
                &"e".repeat(32)
            )
            .is_err()
    );
    assert!(
        fixture
            .registry
            .prepare_target_preload_context(
                &fixture.peer,
                &fixture.connection,
                &"a".repeat(32),
                &"c".repeat(64),
                &fixture.member,
                &"e".repeat(32),
                PreloadSequence::parse("0123456789abcdef0000000000000001").unwrap()
            )
            .is_err()
    );
    let state = fixture.registry.state.lock().unwrap();
    let actor = state.actors.get(&actor_key(&fixture.peer.caller)).unwrap();
    assert_eq!(
        (
            actor.highwater,
            actor.issued.len(),
            state.target_opens.len()
        ),
        before
    );
}

#[test]
fn derived_child_owns_new_connection_without_transferring_parent_use_authority() {
    // Synthetic source admission only; actual peer process, source gate,
    // counted uses and independent persistent connection lifetimes.
    let fixture = Fixture::new();
    let _reservation = fixture.reservation();
    let capability = fixture.begin(1);
    let nonce = fixture.request(1).nonce;
    let context = fixture
        .registry
        .use_context(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            nonce,
            &capability,
        )
        .unwrap();
    let child_connection = fixture.registry.fixture_connection();
    let operation = "d".repeat(32);
    let child = fixture
        .registry
        .open_registered_derived(
            &fixture.peer.caller,
            &child_connection,
            &operation,
            UseIdentity {
                member: &fixture.member,
                nonce,
                capability: &capability,
            },
        )
        .unwrap();
    {
        let state = fixture.registry.state.lock().unwrap();
        let actor = state.actors.get(&actor_key(&fixture.peer.caller)).unwrap();
        assert!(
            actor
                .issued
                .get(&child.member)
                .unwrap()
                .connection
                .same(&child_connection)
        );
        assert!(
            actor
                .issued
                .get(&fixture.member)
                .unwrap()
                .connection
                .same(&fixture.connection)
        );
    }
    assert!(
        fixture
            .registry
            .open_registered_derived(
                &fixture.peer.caller,
                &child_connection,
                &"f".repeat(32),
                UseIdentity {
                    member: &fixture.member,
                    nonce,
                    capability: &"0".repeat(64)
                }
            )
            .is_err()
    );
    let mut foreign = fixture.peer.caller.clone();
    foreign.sid.push_str("-foreign");
    let mut reused = fixture.peer.caller.clone();
    reused.birth += 1;
    for caller in [&foreign, &reused] {
        assert!(
            fixture
                .registry
                .open_registered_derived(
                    caller,
                    &child_connection,
                    &"e".repeat(32),
                    UseIdentity {
                        member: &fixture.member,
                        nonce,
                        capability: &capability
                    },
                )
                .is_err()
        );
    }
    assert!(
        fixture
            .registry
            .registered_source_use(
                &fixture.peer.caller,
                &fixture.connection,
                &fixture.member,
                nonce,
                &capability,
            )
            .is_ok()
    );
    assert!(
        fixture
            .registry
            .registered_source_use(
                &fixture.peer.caller,
                &child_connection,
                &fixture.member,
                nonce,
                &capability,
            )
            .is_err(),
        "derived socket cannot borrow its parent's refresh/use authority"
    );
    fixture
        .registry
        .end_use(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            nonce,
            &capability,
        )
        .unwrap();
    assert_eq!(
        fixture
            .registry
            .open_member_derived(&fixture.peer, &child_connection, &operation, &context)
            .unwrap()
            .member,
        child.member
    );
    assert!(
        fixture
            .registry
            .open_member_derived(&fixture.peer, &child_connection, &"e".repeat(32), &context)
            .is_err()
    );
    assert_eq!(
        fixture
            .registry
            .open_registered_derived(
                &fixture.peer.caller,
                &child_connection,
                &operation,
                UseIdentity {
                    member: &fixture.member,
                    nonce,
                    capability: &capability
                },
            )
            .unwrap()
            .member,
        child.member,
        "exact allocation replay is not a fresh use"
    );
    assert!(
        fixture
            .registry
            .open_registered_derived(
                &fixture.peer.caller,
                &child_connection,
                &"e".repeat(32),
                UseIdentity {
                    member: &fixture.member,
                    nonce,
                    capability: &capability
                },
            )
            .is_err(),
        "closed parent cannot grant another constructor"
    );
    assert!(
        fixture
            .registry
            .registered_source_use(
                &fixture.peer.caller,
                &fixture.connection,
                &fixture.member,
                nonce,
                &capability,
            )
            .is_err(),
        "closed parent cannot refresh"
    );
    assert!(child_connection.is_live());
}

#[test]
fn owned_preload_retirement_retains_native_count_and_gate_until_actual_worker_drop() {
    // Explicit synthetic source injection; actual process/connection, kernel
    // gate, Pending worker and App release/retirement machinery are exercised.
    let mut fixture = Fixture::new();
    let operation = "d".repeat(32);
    let attempt = preload_sequence::Attempt {
        sequence: PreloadSequence::parse("0123456789abcdef0000000000000001").unwrap(),
        operation: operation.clone(),
        origin: preload_sequence::Origin::Ordinary,
    };
    let worker = preload_custody::enter_sequenced(
        &fixture.registry,
        &fixture.peer,
        &fixture.member,
        &operation,
        &fixture.connection,
        &attempt,
    )
    .unwrap();
    let key = member_key(&fixture.peer.caller, &fixture.member).unwrap();
    let pending_readiness = SourceReadiness::preload(
        SourceWorkLease::for_home(fixture._home.path()).unwrap(),
        None,
    )
    .unwrap();
    {
        let mut state = fixture.registry.state.lock().unwrap();
        let member = state.members.get_mut(&key).unwrap();
        member.operation = operation.clone();
        member.readiness = pending_readiness;
        let generation = member.source.credential_generation();
        let digest = member.source.digest().to_owned();
        state
            .preload_sequences
            .get_mut(&key)
            .unwrap()
            .loaded(&attempt, &fixture.registration, generation, &digest)
            .unwrap();
    }
    let cleanup = fixture.registry.fixture_connection();
    let retirement = || target::MemberRetirementRequest {
        operation: "cccccccccccccccccccccccccccccccc",
        member: &fixture.member,
        preload_operation: &operation,
        preload_sequence: attempt.sequence,
        registration: Some(&fixture.registration),
    };
    let target::MemberRetirementPreparation::Pending(command) = fixture
        .registry
        .prepare_member_retirement(&fixture.peer.caller, &cleanup, retirement())
        .unwrap()
    else {
        panic!("a live worker cannot already be Closed");
    };
    assert!(!fixture.registry.retire_member_owned(command).unwrap());
    assert!(worker.scope().cancelled());
    assert!(
        super::super::super::codex_source_gate::PublicationLock::for_home(fixture._home.path())
            .is_err()
    );
    drop(fixture.scope.take());
    let pause = fixture.lifetime.pause().unwrap();
    // Native retains the cleanup turn even after the observer command ended.
    let pause = match pause.try_wait() {
        Err(pause) => pause,
        Ok(_) => panic!("native cleanup remains counted"),
    };
    assert!(
        preload_custody::enter_sequenced(
            &fixture.registry,
            &fixture.peer,
            &fixture.member,
            &operation,
            &fixture.connection,
            &attempt
        )
        .is_err()
    );
    drop(worker);
    let target::MemberRetirementPreparation::Pending(command) = fixture
        .registry
        .prepare_member_retirement(&fixture.peer.caller, &cleanup, retirement())
        .unwrap()
    else {
        panic!("the retained native fence still requires final settlement");
    };
    assert!(fixture.registry.retire_member_owned(command).unwrap());
    assert!(
        super::super::super::codex_source_gate::PublicationLock::for_home(fixture._home.path())
            .is_ok()
    );
    let _drained = match pause.try_wait() {
        Ok(drained) => drained,
        Err(_) => panic!("actual original captures and cleanup retired"),
    };
    assert!(matches!(
        fixture
            .registry
            .prepare_member_retirement(&fixture.peer.caller, &cleanup, retirement()),
        Ok(target::MemberRetirementPreparation::Closed)
    ));
    let changed = target::MemberRetirementRequest {
        operation: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        ..retirement()
    };
    assert!(
        fixture
            .registry
            .prepare_member_retirement(&fixture.peer.caller, &cleanup, changed)
            .is_err()
    );
}

#[test]
fn unrelated_member_cannot_enter_managed_preload_or_leave_a_counted_attempt() {
    // Synthetic peer/source metadata only; call actual NoNativeReader
    // completion and PRELOAD admission. No API credential or owner is minted.
    let fixture = Fixture::new();
    let open = "e".repeat(32);
    let member = fixture
        .registry
        .open_member(&fixture.peer, &fixture.connection, &open)
        .unwrap()
        .member;
    fixture
        .registry
        .complete_unrelated(
            Peer {
                caller: fixture.peer.caller.clone(),
                process: fixture.peer.process.clone(),
                launch: None,
            },
            &fixture.connection,
            &member,
            &open,
            NoNativeReaderKind::ApiKey,
        )
        .unwrap();
    let env = Env {
        state: fixture._home.path().join("state"),
        aterm_state: None,
        cwd: fixture._home.path().into(),
        home: Some(fixture._home.path().into()),
        claude_dir: None,
        claude_config_unusable: None,
        claude_profile: None,
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        codex_home: None,
        xdg_config_home: None,
        codex_login: None,
        agy_conversation: None,
        now: 0,
        sid: fixture.peer.caller.sid.clone(),
        sock: None,
        config: None,
        claude_tmp: None,
        claude_running: None,
        tz_s: 0,
    };
    assert!(matches!(
        fixture.registry.preload_managed(
            &env,
            Peer {
                caller: fixture.peer.caller.clone(),
                process: fixture.peer.process.clone(),
                launch: None,
            },
            &member,
            &"f".repeat(32),
            &fixture.connection,
            PreloadSequence::parse("0123456789abcdef0000000000000001").unwrap()
        ),
        Err(Error::Denied)
    ));
    let key = member_key(&fixture.peer.caller, &member).unwrap();
    let state = fixture.registry.state.lock().unwrap();
    assert!(!state.preloads.contains_key(&key));
    assert!(!state.preload_sequences.contains_key(&key));
}

#[test]
fn owned_member_retirement_closes_original_target_open_without_fabricating_open_cleanup() {
    // Metadata target allocation injection only; real independent constructor
    // Scope/weak retirement, native release turn and caller identity are used.
    let fixture = Fixture::new();
    let reservation = fixture.reservation();
    let constructor = fixture.lifetime.enter().unwrap();
    target::open_cleanup_fixture(
        &fixture.registry,
        &reservation.cell,
        &fixture.peer,
        &fixture.connection,
        (&fixture.member, &fixture.operation),
        &constructor,
        true,
    );
    let assignment = reservation
        .cell
        .lock()
        .unwrap()
        .target
        .operation()
        .to_owned();
    let attempt = preload_sequence::Attempt {
        sequence: PreloadSequence::parse("0123456789abcdef0000000000000001").unwrap(),
        operation: "d".repeat(32),
        origin: preload_sequence::Origin::Target {
            assignment,
            context: "c".repeat(64),
        },
    };
    let key = member_key(&fixture.peer.caller, &fixture.member).unwrap();
    {
        let mut state = fixture.registry.state.lock().unwrap();
        preload_sequence::accept(
            &mut state,
            &key,
            &fixture.peer.process,
            &attempt,
            &constructor,
        )
        .unwrap();
        let member = state.members.get(&key).unwrap();
        let generation = member.source.credential_generation();
        let digest = member.source.digest().to_owned();
        state
            .preload_sequences
            .get_mut(&key)
            .unwrap()
            .loaded(&attempt, &fixture.registration, generation, &digest)
            .unwrap();
    }
    let request = || target::MemberRetirementRequest {
        operation: "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        member: &fixture.member,
        preload_operation: &attempt.operation,
        preload_sequence: attempt.sequence,
        registration: Some(&fixture.registration),
    };
    let target::MemberRetirementPreparation::Pending(command) = fixture
        .registry
        .prepare_member_retirement(&fixture.peer.caller, &fixture.connection, request())
        .unwrap()
    else {
        panic!("constructor capture is still held");
    };
    assert!(!fixture.registry.retire_member_owned(command).unwrap());
    assert!(constructor.cancelled());
    drop(constructor);
    let target::MemberRetirementPreparation::Pending(command) = fixture
        .registry
        .prepare_member_retirement(&fixture.peer.caller, &fixture.connection, request())
        .unwrap()
    else {
        panic!("actual native retirement still needs settlement");
    };
    assert!(fixture.registry.retire_member_owned(command).unwrap());
    assert_eq!(
        target::member_retirement_fixture_state(
            &fixture.registry,
            &fixture.peer.caller,
            &fixture.operation
        ),
        (true, true, true)
    );
    let state = fixture.registry.state.lock().unwrap();
    assert!(
        !state.actors[&actor_key(&fixture.peer.caller)]
            .issued
            .contains_key(&fixture.member)
    );
}

#[test]
fn derived_open_replay_keeps_exact_parent_and_cannot_allocate_after_parent_ends() {
    let fixture = Fixture::new();
    let _reservation = fixture.reservation();
    let capability = fixture.begin(1);
    let nonce = fixture.request(1).nonce;
    let context = fixture
        .registry
        .use_context(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            nonce,
            &capability,
        )
        .unwrap();
    let operation = "d".repeat(32);
    let child = fixture
        .registry
        .open_member_derived(&fixture.peer, &fixture.connection, &operation, &context)
        .unwrap();
    fixture
        .registry
        .end_use(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            nonce,
            &capability,
        )
        .unwrap();
    assert!(!context.current());
    assert_eq!(
        fixture
            .registry
            .open_member_derived(&fixture.peer, &fixture.connection, &operation, &context)
            .unwrap()
            .member,
        child.member
    );
    assert!(
        fixture
            .registry
            .open_member(&fixture.peer, &fixture.connection, &operation)
            .is_err(),
        "a derived nonce cannot become an ordinary factory"
    );
    let parent = UseIdentity {
        member: &fixture.member,
        nonce,
        capability: &capability,
    };
    assert_eq!(
        fixture
            .registry
            .resume_derived_open(&fixture.peer, &fixture.connection, &operation, parent)
            .unwrap()
            .unwrap()
            .member,
        child.member
    );
    assert!(
        fixture
            .registry
            .resume_derived_open(
                &fixture.peer,
                &fixture.connection,
                &operation,
                UseIdentity {
                    member: &fixture.member,
                    nonce,
                    capability: &"0".repeat(64)
                }
            )
            .is_err()
    );
    assert!(
        fixture
            .registry
            .open_member_derived(
                &fixture.peer,
                &fixture.connection,
                &"e".repeat(32),
                &context
            )
            .is_err()
    );
}

#[test]
fn registered_work_binds_actual_scope_connection_registration_and_instance() {
    let fixture = Fixture::new();
    let foreign = account_work::Lifetime::default();
    let foreign_scope = foreign.enter().unwrap();
    assert!(
        fixture
            .registry
            .registered_member_work(
                &fixture.peer.caller,
                &fixture.connection,
                fixture.request(1),
                &foreign_scope
            )
            .is_err()
    );
    let other_connection = fixture.registry.fixture_connection();
    assert!(
        fixture
            .registry
            .registered_member_work(
                &fixture.peer.caller,
                &other_connection,
                fixture.request(1),
                fixture.scope.as_ref().unwrap()
            )
            .is_err()
    );
    let mut wrong_registration = fixture.request(1);
    wrong_registration.registration = "cccccccccccccccccccccccccccccccc";
    let work = fixture
        .registry
        .registered_member_work(
            &fixture.peer.caller,
            &fixture.connection,
            wrong_registration,
            fixture.scope.as_ref().unwrap(),
        )
        .unwrap();
    assert!(fixture.registry.begin_registered_use(work).is_err());
    let work = fixture
        .registry
        .registered_member_work(
            &fixture.peer.caller,
            &fixture.connection,
            fixture.request(2),
            fixture.scope.as_ref().unwrap(),
        )
        .unwrap();
    assert!(Registry::default().begin_registered_use(work).is_err());
    let work = fixture
        .registry
        .registered_member_work(
            &fixture.peer.caller,
            &fixture.connection,
            fixture.request(3),
            fixture.scope.as_ref().unwrap(),
        )
        .unwrap();
    assert!(matches!(
        fixture.registry.begin_registered_use(work).unwrap(),
        UseProgress::Active(_)
    ));
}

#[test]
fn metadata_probe_leaves_state_available_and_late_begin_observes_close_fence() {
    let fixture = Fixture::new();
    let result = fixture
        .registry
        .begin_checked_with_gate(
            &fixture.peer.caller,
            &fixture.peer.process,
            &fixture.connection,
            fixture.request(1),
            fixture.scope.as_ref().unwrap(),
            |gate| {
                assert!(
                    fixture.registry.state.try_lock().is_ok(),
                    "no registry mutex during gate metadata I/O"
                );
                assert!(gate.current());
                fixture
                    .registry
                    .close_unaccepted_use(&fixture.peer, &fixture.connection, fixture.request(1))
                    .unwrap();
                true
            },
        )
        .unwrap();
    assert!(matches!(result, UseProgress::Closed));
    assert!(!has_active_uses(
        &fixture.registry.state.lock().unwrap(),
        &member_key(&fixture.peer.caller, &fixture.member).unwrap()
    ));
}

#[test]
fn dropped_ready_lease_cannot_keep_use_or_new_begin_alive_after_gate_replacement() {
    let fixture = Fixture::new();
    let capability = fixture.begin(1);
    let context = fixture
        .registry
        .use_context(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    let path = fixture
        ._home
        .path()
        .join(".aterm/codex-source-custody/gate");
    std::fs::rename(&path, path.with_extension("old")).unwrap();
    // Create a valid replacement through the same platform boundary as real
    // source work. The old context must fail because its identity changed,
    // even while the replacement itself is current.
    let replacement = SourceWorkLease::for_home(fixture._home.path()).unwrap();
    replacement.current().unwrap();
    assert!(!context.current());
    assert!(
        fixture
            .registry
            .begin_use(
                peer(&fixture.peer),
                &fixture.connection,
                fixture.request(2),
                fixture.scope.as_ref().unwrap()
            )
            .is_err()
    );
    assert_eq!(
        fixture.begin(1),
        capability,
        "old body still retrieves its cap for release-only cleanup"
    );
    fixture
        .registry
        .end_use(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
}

#[test]
fn release_only_cleanup_survives_work_eof_ready_loss_and_scope_cancellation() {
    let mut fixture = Fixture::new();
    let capability = fixture.begin(1);
    fixture.registry.connection_closed(fixture.connection.id);
    let connection = fixture.registry.fixture_connection();
    let cleanup = fixture
        .registry
        .registered_member_cleanup(
            &fixture.peer.caller,
            &connection,
            &fixture.member,
            fixture.scope.as_ref().unwrap(),
        )
        .unwrap();
    let pause = fixture.lifetime.pause().unwrap();
    drop(fixture.scope.take());
    assert!(
        fixture
            .registry
            .end_registered_use(&cleanup, fixture.request(1).nonce, &"f".repeat(64))
            .is_err()
    );
    assert!(matches!(
        fixture
            .registry
            .close_registered_use(&cleanup, fixture.request(1))
            .unwrap(),
        UseProgress::Active(_)
    ));
    fixture
        .registry
        .end_registered_use(&cleanup, fixture.request(1).nonce, &capability)
        .unwrap();
    fixture
        .registry
        .end_registered_use(&cleanup, fixture.request(1).nonce, &capability)
        .unwrap();
    assert!(matches!(
        fixture
            .registry
            .close_registered_use(&cleanup, fixture.request(1))
            .unwrap(),
        UseProgress::Closed
    ));
    let pause = pause
        .try_wait()
        .expect_err("release actor retains its actual Scope through the receipt");
    drop(cleanup);
    pause
        .try_wait()
        .expect("only acknowledged End and actor retirement release native work")
        .resume()
        .expect("the same drained epoch resumes");
}

#[test]
fn release_only_identity_rejects_wrong_app_registry_caller_and_cleanup_eof() {
    let fixture = Fixture::new();
    let foreign = account_work::Lifetime::default();
    let foreign_scope = foreign.enter().unwrap();
    assert!(
        fixture
            .registry
            .registered_member_cleanup(
                &fixture.peer.caller,
                &fixture.connection,
                &fixture.member,
                &foreign_scope
            )
            .is_err()
    );
    let mut caller = fixture.peer.caller.clone();
    caller.birth += 1;
    assert!(
        fixture
            .registry
            .registered_member_cleanup(
                &caller,
                &fixture.connection,
                &fixture.member,
                fixture.scope.as_ref().unwrap()
            )
            .is_err()
    );
    let connection = fixture.registry.fixture_connection();
    let cleanup = fixture
        .registry
        .registered_member_cleanup(
            &fixture.peer.caller,
            &connection,
            &fixture.member,
            fixture.scope.as_ref().unwrap(),
        )
        .unwrap();
    assert!(
        Registry::default()
            .close_registered_use(&cleanup, fixture.request(1))
            .is_err()
    );
    fixture.registry.connection_closed(connection.id);
    assert!(
        fixture
            .registry
            .close_registered_use(&cleanup, fixture.request(1))
            .is_err()
    );
}

#[test]
fn pending_begin_counts_before_waiting_and_close_fences_resume() {
    let mut fixture = Fixture::new();
    let work = fixture
        .registry
        .registered_member_work(
            &fixture.peer.caller,
            &fixture.connection,
            fixture.request(1),
            fixture.scope.as_ref().unwrap(),
        )
        .unwrap();
    assert!(matches!(
        fixture.registry.reserve_registered_use(&work).unwrap(),
        BeginAdmission::Pending
    ));
    assert!(matches!(
        fixture.registry.reserve_registered_use(&work).unwrap(),
        BeginAdmission::Pending
    ));
    let cleanup_connection = fixture.registry.fixture_connection();
    let cleanup = fixture
        .registry
        .registered_member_cleanup(
            &fixture.peer.caller,
            &cleanup_connection,
            &fixture.member,
            fixture.scope.as_ref().unwrap(),
        )
        .unwrap();
    let pause = fixture.lifetime.pause().unwrap();
    drop(fixture.scope.take());
    let pause = pause
        .try_wait()
        .expect_err("accepted pending intent is counted before worker dispatch");
    assert!(matches!(
        fixture
            .registry
            .close_registered_use(&cleanup, fixture.request(1)),
        Err(Error::Busy)
    ));
    assert!(
        matches!(
            fixture.registry.begin_registered_use(work),
            Err(Error::Busy)
        ),
        "delayed worker cannot publish after the Close fence"
    );
    drop(cleanup);
    pause
        .try_wait()
        .expect("all pending and cleanup custody retired")
        .resume()
        .unwrap();
    let scope = fixture.lifetime.enter().unwrap();
    let late = fixture
        .registry
        .registered_member_work(
            &fixture.peer.caller,
            &fixture.connection,
            fixture.request(1),
            &scope,
        )
        .unwrap();
    assert!(
        matches!(
            fixture.registry.begin_registered_use(late).unwrap(),
            UseProgress::Closed
        ),
        "fresh epoch cannot resurrect an old semantic request"
    );
}

#[test]
fn pending_begin_capacity_is_bounded_before_accept_and_never_evicts() {
    let fixture = Fixture::new();
    for counter in 1..=MAX_MEMBER_USES as u64 {
        let work = fixture
            .registry
            .registered_member_work(
                &fixture.peer.caller,
                &fixture.connection,
                fixture.request(counter),
                &fixture.lifetime.enter().unwrap(),
            )
            .unwrap();
        assert!(matches!(
            fixture.registry.reserve_registered_use(&work).unwrap(),
            BeginAdmission::Pending
        ));
    }
    let next = MAX_MEMBER_USES as u64 + 1;
    let work = fixture
        .registry
        .registered_member_work(
            &fixture.peer.caller,
            &fixture.connection,
            fixture.request(next),
            &fixture.lifetime.enter().unwrap(),
        )
        .unwrap();
    assert!(matches!(
        fixture.registry.reserve_registered_use(&work),
        Err(Error::Busy)
    ));
    {
        let state = fixture.registry.state.lock().unwrap();
        let uses = state
            .uses
            .get(&member_key(&fixture.peer.caller, &fixture.member).unwrap())
            .unwrap();
        assert_eq!(uses.highwater, MAX_MEMBER_USES as u64);
        assert_eq!(uses.pending.len(), MAX_MEMBER_USES);
        assert!(uses.active.is_empty());
    }
    fixture
        .registry
        .close_unaccepted_use(&fixture.peer, &fixture.connection, fixture.request(1))
        .unwrap();
    assert!(matches!(
        fixture.registry.reserve_registered_use(&work).unwrap(),
        BeginAdmission::Pending
    ));
    let mut changed = fixture.request(next);
    changed.registration = "cccccccccccccccccccccccccccccccc";
    let wrong = fixture
        .registry
        .registered_member_work(
            &fixture.peer.caller,
            &fixture.connection,
            changed,
            &fixture.lifetime.enter().unwrap(),
        )
        .unwrap();
    assert!(fixture.registry.reserve_registered_use(&wrong).is_err());
}

#[test]
fn pending_worker_definite_refusal_records_closed_without_capability() {
    let fixture = Fixture::new();
    let work = fixture
        .registry
        .registered_member_work(
            &fixture.peer.caller,
            &fixture.connection,
            fixture.request(1),
            &fixture.lifetime.enter().unwrap(),
        )
        .unwrap();
    fixture.registry.reserve_registered_use(&work).unwrap();
    {
        let mut state = fixture.registry.state.lock().unwrap();
        state
            .members
            .get_mut(&member_key(&fixture.peer.caller, &fixture.member).unwrap())
            .unwrap()
            .registration = "c".repeat(32);
    }
    assert!(matches!(
        fixture.registry.begin_registered_use(work),
        Err(Error::Busy)
    ));
    let state = fixture.registry.state.lock().unwrap();
    let uses = state
        .uses
        .get(&member_key(&fixture.peer.caller, &fixture.member).unwrap())
        .unwrap();
    assert!(uses.pending.is_empty() && uses.active.is_empty());
    assert!(uses.ending[&1].capability.is_none());
    assert!(uses.ending[&1].settled());
}

#[test]
fn paused_cleanup_resolves_pending_custody_without_ordinary_enter() {
    let mut fixture = Fixture::new();
    let work = fixture
        .registry
        .registered_member_work(
            &fixture.peer.caller,
            &fixture.connection,
            fixture.request(1),
            fixture.scope.as_ref().unwrap(),
        )
        .unwrap();
    fixture.registry.reserve_registered_use(&work).unwrap();
    drop(work);
    let pause = fixture.lifetime.pause().unwrap();
    drop(fixture.scope.take());
    assert!(fixture.lifetime.enter().is_none());
    let pause = pause
        .try_wait()
        .expect_err("pending original operation precedes sealing");
    let connection = fixture.registry.fixture_connection();
    let CleanupAdmission::Mutable(cleanup) = fixture
        .registry
        .prepare_use_cleanup(
            &fixture.peer.caller,
            &connection,
            UseCleanupRequest::Close(fixture.request(1)),
        )
        .unwrap()
    else {
        panic!("pending close needs its actual retained custody")
    };
    assert!(matches!(
        fixture.registry.settle_use_cleanup(cleanup).unwrap(),
        UseProgress::Closed
    ));
    let drained = pause
        .try_wait()
        .expect("pending and release custody both settled");
    assert!(
        matches!(
            fixture
                .registry
                .prepare_use_cleanup(
                    &fixture.peer.caller,
                    &connection,
                    UseCleanupRequest::Close(fixture.request(1))
                )
                .unwrap(),
            CleanupAdmission::Closed
        ),
        "lost Closed reply correlates after seal without a new counted turn"
    );
    drained.resume().unwrap();
    let scope = fixture.lifetime.enter().unwrap();
    let late = fixture
        .registry
        .registered_member_work(
            &fixture.peer.caller,
            &fixture.connection,
            fixture.request(1),
            &scope,
        )
        .unwrap();
    assert!(matches!(
        fixture.registry.begin_registered_use(late).unwrap(),
        UseProgress::Closed
    ));
}

#[test]
fn release_turn_fences_before_seal_but_unrecorded_late_close_stays_uncertain() {
    let mut fixture = Fixture::new();
    let pause = fixture.lifetime.pause().unwrap();
    drop(fixture.scope.take());
    let connection = fixture.registry.fixture_connection();
    let CleanupAdmission::Mutable(cleanup) = fixture
        .registry
        .prepare_use_cleanup(
            &fixture.peer.caller,
            &connection,
            UseCleanupRequest::Close(fixture.request(1)),
        )
        .unwrap()
    else {
        panic!("unrecorded Close requires a pre-seal release turn")
    };
    let pause = pause
        .try_wait()
        .expect_err("release fence must settle before proof issuance");
    assert!(matches!(
        fixture.registry.settle_use_cleanup(cleanup).unwrap(),
        UseProgress::Closed
    ));
    let drained = pause.try_wait().unwrap();
    assert!(matches!(
        fixture
            .registry
            .prepare_use_cleanup(
                &fixture.peer.caller,
                &connection,
                UseCleanupRequest::Close(fixture.request(1))
            )
            .unwrap(),
        CleanupAdmission::Closed
    ));
    assert!(
        matches!(
            fixture.registry.prepare_use_cleanup(
                &fixture.peer.caller,
                &connection,
                UseCleanupRequest::Close(fixture.request(2))
            ),
            Err(Error::Unavailable)
        ),
        "sealed retirement cannot claim an unfenced delayed Begin is Closed"
    );
    {
        let state = fixture.registry.state.lock().unwrap();
        assert_eq!(
            state
                .uses
                .get(&member_key(&fixture.peer.caller, &fixture.member).unwrap())
                .unwrap()
                .highwater,
            1
        );
    }
    drained.resume().unwrap();
    let scope = fixture.lifetime.enter().unwrap();
    let late = fixture
        .registry
        .registered_member_work(
            &fixture.peer.caller,
            &fixture.connection,
            fixture.request(1),
            &scope,
        )
        .unwrap();
    assert!(matches!(
        fixture.registry.begin_registered_use(late).unwrap(),
        UseProgress::Closed
    ));
}

#[test]
fn release_only_end_resolves_active_scope_after_pause_and_replays_without_entry() {
    let mut fixture = Fixture::new();
    let capability = fixture.begin(1);
    fixture.registry.connection_closed(fixture.connection.id);
    let pause = fixture.lifetime.pause().unwrap();
    drop(fixture.scope.take());
    let connection = fixture.registry.fixture_connection();
    let request = || UseCleanupRequest::End {
        member: &fixture.member,
        nonce: fixture.request(1).nonce,
        capability: &capability,
    };
    let CleanupAdmission::Mutable(cleanup) = fixture
        .registry
        .prepare_use_cleanup(&fixture.peer.caller, &connection, request())
        .unwrap()
    else {
        panic!("actual active operation still owns cleanup custody")
    };
    assert!(matches!(
        fixture.registry.settle_use_cleanup(cleanup).unwrap(),
        UseProgress::Closed
    ));
    let drained = pause.try_wait().unwrap();
    assert!(matches!(
        fixture
            .registry
            .prepare_use_cleanup(&fixture.peer.caller, &connection, request())
            .unwrap(),
        CleanupAdmission::Closed
    ));
    assert!(
        fixture
            .registry
            .prepare_use_cleanup(
                &fixture.peer.caller,
                &connection,
                UseCleanupRequest::End {
                    member: &fixture.member,
                    nonce: fixture.request(1).nonce,
                    capability: &"f".repeat(64)
                }
            )
            .is_err()
    );
    drained.resume().unwrap();
}

#[test]
fn actual_use_command_adapter_accounts_queue_before_waiting_and_cleanup_after_pause() {
    let mut fixture = Fixture::new();
    let operation = "1234567890abcdef0000000000000001";
    let begin = UseAction::Begin {
        registration: fixture.registration.clone(),
    };
    let command = fixture
        .registry
        .prepare_use_command(
            &fixture.peer.caller,
            &fixture.connection,
            operation,
            &fixture.member,
            &begin,
        )
        .unwrap();
    assert!(matches!(command, UseCommand::Begin(_)));
    drop(command); // Transport queue or original connection may disappear.
    let pause = fixture.lifetime.pause().unwrap();
    drop(fixture.scope.take());
    let pause = pause.try_wait().expect_err(
        "actual typed adapter has already retained Pending before a transport Waiting can exist",
    );
    let connection = fixture.registry.fixture_connection();
    let close = UseAction::Close {
        registration: fixture.registration.clone(),
    };
    let command = fixture
        .registry
        .prepare_use_command(
            &fixture.peer.caller,
            &connection,
            operation,
            &fixture.member,
            &close,
        )
        .unwrap();
    assert!(matches!(
        fixture.registry.execute_use_command(command).unwrap(),
        UseProgress::Closed
    ));
    let drained = pause.try_wait().unwrap();
    let replay = fixture
        .registry
        .prepare_use_command(
            &fixture.peer.caller,
            &fixture.connection,
            operation,
            &fixture.member,
            &begin,
        )
        .unwrap();
    assert!(
        matches!(replay, UseCommand::Settled(UseProgress::Closed)),
        "sealed epoch replay performs no ordinary enter"
    );
    drained.resume().unwrap();
}

#[test]
fn actual_use_command_adapter_never_invents_use_receipts_from_retired_member_highwater() {
    let mut fixture = Fixture::new();
    fixture
        .registry
        .retire_member(
            peer(&fixture.peer),
            &fixture.connection,
            &fixture.member,
            Some(&fixture.registration),
        )
        .unwrap();
    let pause = fixture.lifetime.pause().unwrap();
    drop(fixture.scope.take());
    let drained = pause.try_wait().unwrap();
    for action in [
        UseAction::Begin {
            registration: "d".repeat(32),
        },
        UseAction::Close {
            registration: "d".repeat(32),
        },
        UseAction::End {
            capability: "e".repeat(64),
        },
    ] {
        assert!(matches!(
            fixture.registry.prepare_use_command(
                &fixture.peer.caller,
                &fixture.connection,
                "1234567890abcdef0000000000000099",
                &fixture.member,
                &action
            ),
            Err(Error::Stale)
        ));
    }
    assert!(fixture.registry.state.lock().unwrap().uses.is_empty());
    drained.resume().unwrap();
}

#[test]
fn registry_phase_retains_actual_kernel_exclusion_after_handle_drop_and_retirement() {
    let fixture = Fixture::new();
    let reservation = fixture.reservation();
    let sealed = seal::kernel_fixture(&reservation);
    assert!(sealed.current());
    drop(reservation);
    // A legitimate old-member retirement does not invalidate native custody.
    fixture
        .registry
        .retire_member(
            peer(&fixture.peer),
            &fixture.connection,
            &fixture.member,
            Some(&fixture.registration),
        )
        .unwrap();
    assert!(sealed.current());
    drop(sealed);
    assert!(
        crate::harness::codex_source_gate::PublicationLock::for_home(fixture._home.path()).is_err()
    );
}

#[test]
fn kernel_phase_cannot_reopen_admission_after_gate_replacement() {
    let fixture = Fixture::new();
    let reservation = fixture.reservation();
    let sealed = seal::kernel_fixture(&reservation);
    let gate = fixture
        ._home
        .path()
        .join(".aterm/codex-source-custody/gate");
    std::fs::rename(&gate, gate.with_extension("old")).unwrap();
    aterm_uds::private_file::write_private_new(
        gate.parent().unwrap(),
        "gate",
        b"replacement",
        |_| Ok(()),
    )
    .unwrap();
    assert!(!sealed.current());
    let state = fixture.registry.state.lock().unwrap();
    assert!(matches!(
        reservation_allows_member(&state, &fixture.peer.caller, &fixture.member),
        Err(Error::Busy)
    ));
}

#[test]
fn preload_is_counted_before_io_and_unknown_result_cannot_hide_behind_old_ready() {
    let fixture = Fixture::new();
    let operation = "c".repeat(32);
    let worker = super::super::preload_custody::enter(
        &fixture.registry,
        &fixture.peer,
        &fixture.member,
        &operation,
        &fixture.connection,
    )
    .unwrap();
    let key = member_key(&fixture.peer.caller, &fixture.member).unwrap();
    {
        let state = fixture.registry.state.lock().unwrap();
        assert!(has_active_uses(&state, &key));
        assert!(
            super::super::preload_custody::check_completion(
                &state,
                &key,
                &operation,
                &fixture.connection
            )
            .is_err()
        );
    }
    assert!(matches!(
        fixture.registry.retire_member(
            peer(&fixture.peer),
            &fixture.connection,
            &fixture.member,
            Some(&fixture.registration)
        ),
        Err(Error::Busy)
    ));
    drop(worker); // unknown response: custody remains after the worker returns
    assert!(has_active_uses(
        &fixture.registry.state.lock().unwrap(),
        &key
    ));
    fixture
        .registry
        .retire_member(
            peer(&fixture.peer),
            &fixture.connection,
            &fixture.member,
            Some(&fixture.registration),
        )
        .unwrap();
    assert!(!has_active_uses(
        &fixture.registry.state.lock().unwrap(),
        &key
    ));
}

#[test]
fn same_grant_in_distinct_physical_book_and_slot_claims_still_requires_both_members_to_drain() {
    // Explicit synthetic grant/peer injection; actual private stores, claim
    // identities, kernel gates, members and quiescence are exercised. No MAC
    // source, supported image or complete seal qualification is claimed.
    let fixture = Fixture::new();
    let home = fixture._home.path();
    aterm_secrets::client::machine_identity_as(
        &aterm_secrets::client::Paths::locate_for_home(home),
        aterm_secrets::Making {
            test_binary: true,
            ..Default::default()
        },
    )
    .unwrap();
    let physical = |name: &str| {
        let (store, _) =
            crate::harness::pending_native_store::Store::open_owned(&home.join(name), home, 65536)
                .unwrap();
        store.physical_identity().unwrap()
    };
    let book_a = physical("book-a");
    let slot_a = physical("slot-a");
    let book_b = physical("book-b");
    let slot_b = physical("slot-b");
    assert!(book_a != book_b && slot_a != slot_b);
    let a = Arc::new(metadata_fixture::with_namespaces(
        metadata_fixture::source(
            metadata_fixture::binding("source"),
            home.join("a/auth.json"),
            1,
        ),
        book_a,
        slot_a,
    ));
    let b = Arc::new(metadata_fixture::with_namespaces(
        metadata_fixture::source(
            metadata_fixture::binding("source"),
            home.join("b/auth.json"),
            1,
        ),
        book_b,
        slot_b,
    ));
    assert_eq!(a.binding(), b.binding());
    let other = fixture
        .registry
        .open_member(&fixture.peer, &fixture.connection, &"d".repeat(32))
        .unwrap();
    let other_registration = "e".repeat(32);
    {
        let mut state = fixture.registry.state.lock().unwrap();
        state
            .members
            .get_mut(&member_key(&fixture.peer.caller, &fixture.member).unwrap())
            .unwrap()
            .source = a.clone();
        let mut readiness =
            SourceReadiness::preload(SourceWorkLease::for_home(home).unwrap(), None).unwrap();
        readiness.complete(ReadyRole::Manager(auth())).unwrap();
        let version = version(&mut state).unwrap();
        state.members.insert(
            member_key(&fixture.peer.caller, &other.member).unwrap(),
            Member {
                caller: fixture.peer.caller.clone(),
                process: fixture.peer.process.clone(),
                operation: other.operation,
                registration: other_registration.clone(),
                connection: fixture.connection.clone(),
                version,
                source: b.clone(),
                readiness,
                unknown: false,
                work_rebound: false,
            },
        );
    }
    let reservation = fixture.reservation();
    let quiet = |member: &str, registration: &str, source: &slot::SourceHandle| {
        reservation
            .quiesce(
                &fixture.peer,
                &fixture.connection,
                Quiescence {
                    member,
                    registration,
                    credential_generation: source.credential_generation(),
                    digest: source.digest(),
                    auth: Some(auth()),
                },
            )
            .unwrap();
    };
    quiet(&fixture.member, &fixture.registration, &a);
    assert!(matches!(
        seal::metadata_census_fixture(&reservation),
        Err(Error::Busy)
    ));
    quiet(&other.member, &other_registration, &b);
    seal::metadata_census_fixture(&reservation).unwrap();
}

#[test]
fn reservation_admission_requires_its_actual_live_app_scope_at_each_boundary() {
    let fixture = Fixture::new();
    let scope = fixture.scope.as_ref().unwrap();
    let foreign = account_work::Lifetime::default();
    let wrong = foreign.enter().unwrap();
    let state = fixture.registry.state.lock().unwrap();
    reservation_scope(&state, scope).unwrap();
    assert!(matches!(
        reservation_scope(&state, &wrong),
        Err(Error::Denied)
    ));
    assert!(matches!(
        reservation_scope(&State::default(), scope),
        Err(Error::Denied)
    ));
    // A cancellation between preflight and publication must fail the exact
    // same predicate used at the commit and returned-authority boundaries.
    scope.cancellation().cancel();
    assert!(matches!(
        reservation_scope(&state, scope),
        Err(Error::Stale)
    ));
    assert!(state.reservations.is_empty());
}

#[test]
fn sealed_independent_parent_cannot_mutate_actor_before_new_derived_admission_refusal() {
    let fixture = Fixture::new();
    let capability = fixture.begin(1);
    let context = fixture
        .registry
        .use_context(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    let reservation = fixture.reservation();
    let source = {
        let state = fixture.registry.state.lock().unwrap();
        state.members[&member_key(&fixture.peer.caller, &fixture.member).unwrap()]
            .source
            .clone()
    };
    seal::independent_phase_fixture(&reservation, &fixture.member, source);
    // Explicit synthetic independent-phase injection; actual accepted native
    // use and Actor ledger must still refuse before any allocation mutation.
    assert!(context.current());
    let before = {
        let state = fixture.registry.state.lock().unwrap();
        let actor = &state.actors[&actor_key(&fixture.peer.caller)];
        (
            actor.highwater,
            actor.issued.keys().cloned().collect::<Vec<_>>(),
            state.version,
        )
    };
    assert!(matches!(
        fixture.registry.open_member_derived(
            &fixture.peer,
            &fixture.connection,
            &"f".repeat(32),
            &context
        ),
        Err(Error::Busy)
    ));
    let state = fixture.registry.state.lock().unwrap();
    let actor = &state.actors[&actor_key(&fixture.peer.caller)];
    assert_eq!(
        (
            actor.highwater,
            actor.issued.keys().cloned().collect::<Vec<_>>(),
            state.version
        ),
        before
    );
}

#[test]
fn ending_waits_native_capture_and_exact_closed_replay_survives_app_seal() {
    // Synthetic member/peer; actual App scopes, pause/seal and connection.
    let mut fixture = Fixture::new();
    let capability = fixture.begin(1);
    let context = fixture
        .registry
        .use_context(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    let capture = UseSnapshot::capture(&fixture.registry.state.lock().unwrap(), &context)
        .unwrap()
        .scope;
    drop(fixture.scope.take());
    let pause = fixture.lifetime.pause().unwrap();
    assert!(matches!(
        fixture.registry.end_use(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability
        ),
        Err(Error::Busy)
    ));
    assert!(capture.cancelled());
    assert!(
        fixture
            .registry
            .use_context(
                &fixture.peer,
                &fixture.connection,
                &fixture.member,
                fixture.request(1).nonce,
                &capability
            )
            .is_err()
    );
    assert!(matches!(
        fixture.registry.close_unaccepted_use(
            &fixture.peer,
            &fixture.connection,
            fixture.request(1)
        ),
        Err(Error::Busy)
    ));
    let pause = match pause.try_wait() {
        Err(pause) => pause,
        Ok(_) => panic!("native capture still owns the operation"),
    };
    drop(capture);
    let _drained = pause
        .try_wait()
        .unwrap_or_else(|_| panic!("actual capture retirement drains"));
    assert!(matches!(
        fixture
            .registry
            .prepare_use_cleanup(
                &fixture.peer.caller,
                &fixture.connection,
                UseCleanupRequest::End {
                    member: &fixture.member,
                    nonce: fixture.request(1).nonce,
                    capability: &capability
                }
            )
            .unwrap(),
        CleanupAdmission::Closed
    ));
    assert!(
        fixture
            .registry
            .prepare_use_cleanup(
                &fixture.peer.caller,
                &fixture.connection,
                UseCleanupRequest::End {
                    member: &fixture.member,
                    nonce: fixture.request(1).nonce,
                    capability: &"0".repeat(64)
                }
            )
            .is_err()
    );
}

#[test]
fn pending_close_fences_late_worker_until_its_actual_scope_retires() {
    let fixture = Fixture::new();
    let scope = fixture.lifetime.enter().unwrap();
    let work = fixture
        .registry
        .registered_member_work(
            &fixture.peer.caller,
            &fixture.connection,
            fixture.request(1),
            &scope,
        )
        .unwrap();
    assert!(matches!(
        fixture.registry.reserve_registered_use(&work).unwrap(),
        BeginAdmission::Pending
    ));
    drop(scope);
    assert!(matches!(
        fixture.registry.close_unaccepted_use(
            &fixture.peer,
            &fixture.connection,
            fixture.request(1)
        ),
        Err(Error::Busy)
    ));
    assert!(matches!(
        fixture.registry.begin_registered_use(work),
        Err(Error::Busy)
    ));
    assert!(matches!(
        fixture
            .registry
            .close_unaccepted_use(&fixture.peer, &fixture.connection, fixture.request(1))
            .unwrap(),
        UseProgress::Closed
    ));
    assert!(
        fixture
            .registry
            .end_use(
                &fixture.peer,
                &fixture.connection,
                &fixture.member,
                fixture.request(1).nonce,
                &"0".repeat(64)
            )
            .is_err()
    );
}

#[test]
fn departed_member_row_cannot_hide_ending_native_work_from_source_census() {
    // Metadata fixture only; actual scopes and physical private namespaces.
    // This does not manufacture an InstalledTarget or supported owner/image.
    let fixture = Fixture::new();
    let home = fixture._home.path();
    aterm_secrets::client::machine_identity_as(
        &aterm_secrets::client::Paths::locate_for_home(home),
        aterm_secrets::Making {
            test_binary: true,
            ..Default::default()
        },
    )
    .unwrap();
    let physical = |name: &str| {
        let (store, _) =
            crate::harness::pending_native_store::Store::open_owned(&home.join(name), home, 65536)
                .unwrap();
        store.physical_identity().unwrap()
    };
    {
        let mut state = fixture.registry.state.lock().unwrap();
        state
            .members
            .get_mut(&member_key(&fixture.peer.caller, &fixture.member).unwrap())
            .unwrap()
            .source = Arc::new(metadata_fixture::with_namespaces(
            metadata_fixture::source(
                metadata_fixture::binding("source"),
                home.join("auth.json"),
                1,
            ),
            physical("ending-book"),
            physical("ending-slot"),
        ));
    }
    let capability = fixture.begin(1);
    let reservation = fixture.reservation();
    let key = member_key(&fixture.peer.caller, &fixture.member).unwrap();
    let capture = fixture.registry.state.lock().unwrap().uses[&key].active[&1]
        ._scope
        .clone();
    assert!(matches!(
        fixture.registry.end_use(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability
        ),
        Err(Error::Busy)
    ));
    let departed = {
        let mut state = fixture.registry.state.lock().unwrap();
        state
            .actors
            .get_mut(&actor_key(&fixture.peer.caller))
            .unwrap()
            .issued
            .remove(&fixture.member);
        state.members.remove(&key)
    };
    drop(departed);
    assert!(matches!(
        seal::metadata_census_fixture(&reservation),
        Err(Error::Busy)
    ));
    drop(capture);
    seal::metadata_census_fixture(&reservation).unwrap();
}

#[test]
fn kernel_departure_reaps_member_but_retains_native_ending_capture() {
    // Actual child death and native scope retirement; source/peer are metadata
    // fixtures, not a qualified SDK image or real provider/owner admission.
    struct Child(aterm_uds::spawnfd::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Child(
        aterm_uds::spawnfd::command("/bin/sleep")
            .arg("120")
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let process = loop {
        if let Some(process) = Process::observe(child.0.id())
            && process.image.path.ends_with(b"/sleep")
        {
            break process;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "our child reached its real executable"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let mut fixture = Fixture::new();
    let old_actor = actor_key(&fixture.peer.caller);
    let old_member = member_key(&fixture.peer.caller, &fixture.member).unwrap();
    fixture.peer.caller.pid = process.pid;
    fixture.peer.caller.birth = process.birth;
    fixture.peer.process = process.clone();
    {
        let mut state = fixture.registry.state.lock().unwrap();
        let mut actor = state.actors.remove(&old_actor).unwrap();
        actor.process = process.clone();
        state.actors.insert(actor_key(&fixture.peer.caller), actor);
        let mut member = state.members.remove(&old_member).unwrap();
        member.caller = fixture.peer.caller.clone();
        member.process = process;
        state.members.insert(
            member_key(&fixture.peer.caller, &fixture.member).unwrap(),
            member,
        );
    }
    let capability = fixture.begin(1);
    let context = fixture
        .registry
        .use_context(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    let capture = UseSnapshot::capture(&fixture.registry.state.lock().unwrap(), &context)
        .unwrap()
        .scope;
    let key = member_key(&fixture.peer.caller, &fixture.member).unwrap();
    let constructor = preload_custody::enter(
        &fixture.registry,
        &fixture.peer,
        &fixture.member,
        &fixture.operation,
        &fixture.connection,
    )
    .unwrap();
    let resource_source = fixture.registry.state.lock().unwrap().members[&key]
        .source
        .clone();
    let resource_capture = target::resources::pending_capture_fixture(
        &fixture.registry,
        &fixture.peer.caller,
        &fixture.peer.process,
        &fixture.connection,
        resource_source,
    );
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    fixture.registry.reap_gone();
    {
        let state = fixture.registry.state.lock().unwrap();
        assert!(!state.members.contains_key(&key));
        assert!(has_credential_uses(&state, &key));
        assert!(state.actors.contains_key(&actor_key(&fixture.peer.caller)));
        assert!(state.uses[&key].active.is_empty());
    }
    assert!(capture.cancelled());
    drop(capture);
    fixture.registry.reap_gone();
    {
        let state = fixture.registry.state.lock().unwrap();
        assert!(!state.uses.contains_key(&key));
        assert!(state.actors.contains_key(&actor_key(&fixture.peer.caller)));
        assert!(state.preloads.contains_key(&key));
        assert!(!state.preloads[&key].retired());
        assert!(
            preload_custody::check_completion(
                &state,
                &key,
                &fixture.operation,
                &fixture.connection
            )
            .is_err()
        );
    }
    drop(constructor);
    fixture.registry.reap_gone();
    {
        let state = fixture.registry.state.lock().unwrap();
        assert!(!state.preloads.contains_key(&key));
        assert!(
            state.actors.contains_key(&actor_key(&fixture.peer.caller)),
            "real native resource capture still owns its Scope"
        );
        assert!(target::resources::custody_pending(
            &state,
            &fixture.peer.caller
        ));
    }
    drop(resource_capture);
    fixture.registry.reap_gone();
    let state = fixture.registry.state.lock().unwrap();
    assert!(!state.uses.contains_key(&key));
    assert!(!state.preloads.contains_key(&key));
    assert!(!state.actors.contains_key(&actor_key(&fixture.peer.caller)));
}

#[test]
fn admitted_namespace_rebases_to_reserved_origin_without_granting_a_new_source() {
    let f = Fixture::new();
    // The existing use and the proposed switch have distinct real operations.
    // This is metadata/scope lifecycle injection, not signed Book admission.
    let use_scope = f.lifetime.enter().unwrap();
    let capability = match f
        .registry
        .begin_use(peer(&f.peer), &f.connection, f.request(1), &use_scope)
        .unwrap()
    {
        UseProgress::Active(receipt) => receipt.capability,
        UseProgress::Closed => panic!("use unexpectedly closed"),
    };
    drop(use_scope);
    let context = SourceUse {
        state: Arc::downgrade(&f.registry.state),
        caller: f.peer.caller.clone(),
        member: f.member.clone(),
        nonce: f.request(1).nonce,
        capability,
    };
    let before = context.namespace(&f.peer.caller, f._home.path()).unwrap();
    assert!(before.origin().is_none());
    let reservation = f.reservation();
    let origin = before.revalidated(f._home.path()).unwrap();
    assert!(origin.origin().is_some());
    reservation
        .cell
        .lock()
        .unwrap()
        .scope()
        .unwrap()
        .cancellation()
        .cancel();
    assert!(
        origin.current().is_ok(),
        "a cancelled proposed switch is not cancellation of the independent A use"
    );
    let wrong = aterm_tempfile::tempdir().unwrap();
    assert!(context.namespace(&f.peer.caller, wrong.path()).is_err());
    assert!(!wrong.path().join(".aterm").exists());
    let mut state = f.registry.state.lock().unwrap();
    let member = state
        .members
        .get_mut(&member_key(&f.peer.caller, &f.member).unwrap())
        .unwrap();
    member.source = Arc::new(metadata_fixture::with_authority(
        metadata_fixture::source(
            metadata_fixture::binding("source"),
            f._home.path().join("auth.json"),
            1,
        ),
        super::super::super::codex_assignment::SourceAuthority::Startup {
            nonce: "9".repeat(32),
        },
    ));
    drop(state);
    assert!(
        origin.current().is_err(),
        "startup authority cannot be collapsed into generation zero"
    );
}

#[test]
fn reader_namespace_retains_the_actual_shared_claim_without_reacquiring_it() {
    let f = Fixture::new();
    let mut readiness =
        SourceReadiness::preload(SourceWorkLease::for_home(f._home.path()).unwrap(), None).unwrap();
    readiness.complete(ReadyRole::ReaderLoaded).unwrap();
    let original = match readiness.gate.as_ref().unwrap() {
        ReaderGate::Global(lease) => lease.clone(),
        _ => panic!("wrong gate class"),
    };
    f.registry
        .state
        .lock()
        .unwrap()
        .members
        .get_mut(&member_key(&f.peer.caller, &f.member).unwrap())
        .unwrap()
        .readiness = readiness;
    let capability = f.begin(1);
    let context = SourceUse {
        state: Arc::downgrade(&f.registry.state),
        caller: f.peer.caller.clone(),
        member: f.member.clone(),
        nonce: f.request(1).nonce,
        capability,
    };
    let namespace = context.namespace(&f.peer.caller, f._home.path()).unwrap();
    let borrowed = namespace.source_lease().unwrap().unwrap();
    assert!(Arc::ptr_eq(&original, &borrowed));
    assert!(borrowed.current().is_ok());
    assert!(
        crate::harness::codex_source_gate::PublicationLock::try_for_home(f._home.path())
            .unwrap()
            .is_none()
    );
}

#[test]
fn target_open_retirement_binds_actual_recorded_context_before_and_after_closed() {
    // Ledger-only target injection; real socket/process, Scope and cleanup
    // transitions, without minting an InstalledTarget or credential permission.
    let f = Fixture::new();
    let reservation = f.reservation();
    let assignment = reservation
        .cell
        .lock()
        .unwrap()
        .target
        .operation()
        .to_owned();
    let removed = f
        .registry
        .state
        .lock()
        .unwrap()
        .members
        .remove(&member_key(&f.peer.caller, &f.member).unwrap());
    drop(removed);
    target::open_cleanup_fixture(
        &f.registry,
        &reservation.cell,
        &f.peer,
        &f.connection,
        (&f.member, &f.operation),
        f.scope.as_ref().unwrap(),
        false,
    );
    let context = "c".repeat(64);
    let cleanup = "e".repeat(32);
    let wrong_assignment = "f".repeat(32);
    let wrong_context = "d".repeat(64);
    let prepare = |assignment: &str, context: &str| {
        f.registry.prepare_target_open_retirement(
            &f.peer.caller,
            &f.connection,
            target::TargetOpenRetirement {
                operation: &cleanup,
                open_operation: &f.operation,
                assignment,
                context,
                member: None,
            },
        )
    };
    assert!(prepare(&wrong_assignment, &context).is_err());
    assert!(prepare(&assignment, &wrong_context).is_err());
    let command = prepare(&assignment, &context).unwrap();
    f.registry.retire_target_open(command).unwrap();
    let _pause = f.lifetime.pause().unwrap();
    assert!(f.lifetime.enter().is_none());
    assert!(prepare(&wrong_assignment, &context).is_err());
    assert!(prepare(&assignment, &wrong_context).is_err());
    f.registry
        .retire_target_open(prepare(&assignment, &context).unwrap())
        .unwrap();
}

#[test]
fn target_open_cleanup_keeps_exact_lost_reply_fence_after_pause_and_context_loss() {
    let f = Fixture::new();
    let reservation = f.reservation();
    let removed = f
        .registry
        .state
        .lock()
        .unwrap()
        .members
        .remove(&member_key(&f.peer.caller, &f.member).unwrap());
    drop(removed);
    target::open_cleanup_fixture(
        &f.registry,
        &reservation.cell,
        &f.peer,
        &f.connection,
        (&f.member, &f.operation),
        f.scope.as_ref().unwrap(),
        false,
    );
    let capture = target::captured_open_fixture(&reservation.cell, &f.operation);
    let release = f.registry.fixture_connection();
    f.registry.connection_closed(f.connection.id);
    let cleanup_operation = "e".repeat(32);
    let cleanup = f
        .registry
        .prepare_target_open_cleanup(
            &f.peer.caller,
            &release,
            &cleanup_operation,
            &f.operation,
            None,
        )
        .unwrap();
    let _paused = f.lifetime.pause();
    assert!(matches!(
        f.registry.retire_target_open(cleanup),
        Err(Error::Busy)
    ));
    drop(capture);
    let cleanup = f
        .registry
        .prepare_target_open_cleanup(
            &f.peer.caller,
            &release,
            &cleanup_operation,
            &f.operation,
            None,
        )
        .unwrap();
    f.registry.retire_target_open(cleanup).unwrap();
    // Exact accepted cleanup remains correlatable without context/Ready or an
    // ordinary App enter. Unknown operations never become Closed by absence.
    let replay = f
        .registry
        .prepare_target_open_cleanup(
            &f.peer.caller,
            &release,
            &cleanup_operation,
            &f.operation,
            None,
        )
        .unwrap();
    f.registry.retire_target_open(replay).unwrap();
    assert!(
        f.registry
            .prepare_target_open_cleanup(
                &f.peer.caller,
                &release,
                &cleanup_operation,
                &"f".repeat(32),
                None
            )
            .is_err()
    );
    assert!(
        f.registry
            .prepare_target_open_cleanup(
                &f.peer.caller,
                &release,
                &cleanup_operation,
                &f.operation,
                Some(&f.member)
            )
            .is_err()
    );
    assert!(
        !f.registry.state.lock().unwrap().actors[&actor_key(&f.peer.caller)]
            .issued
            .contains_key(&f.member)
    );
}

#[test]
fn target_open_cleanup_cannot_discard_an_accepted_preload_or_loaded_member() {
    let f = Fixture::new();
    let reservation = f.reservation();
    target::open_cleanup_fixture(
        &f.registry,
        &reservation.cell,
        &f.peer,
        &f.connection,
        (&f.member, &f.operation),
        f.scope.as_ref().unwrap(),
        true,
    );
    assert!(
        f.registry
            .prepare_target_open_cleanup(
                &f.peer.caller,
                &f.connection,
                &"e".repeat(32),
                &f.operation,
                None
            )
            .is_err()
    );
    assert!(
        f.registry
            .state
            .lock()
            .unwrap()
            .members
            .contains_key(&member_key(&f.peer.caller, &f.member).unwrap())
    );
}

#[test]
fn distinct_target_retains_actual_global_share_without_retaining_a_exclusive() {
    // Explicit synthetic phase/target injection tests only actual kernel
    // lifetime separation, not installation, owner, image or release authority.
    let fixture = Fixture::new();
    let reservation = fixture.reservation();
    let sealed = seal::kernel_fixture(&reservation);
    let kernel = sealed.retained_kernel().unwrap();
    let intent = reservation.cell.lock().unwrap().target.intent();
    let resource = kernel.target_resource(&intent).unwrap();
    let source = fixture
        .registry
        .ready_source(&fixture.peer.caller, &fixture.member)
        .unwrap();
    assert!(
        crate::harness::codex_source_gate::FamilyWork::acquire(
            crate::harness::codex_source_gate::FamilySource::Manager(&source)
        )
        .is_err()
    );
    fixture
        .registry
        .state
        .lock()
        .unwrap()
        .reservations
        .remove(&actor_key(&fixture.peer.caller));
    drop(reservation);
    drop(sealed);
    drop(kernel); // synthetic test-only teardown
    resource.current().unwrap();
    let a = crate::harness::codex_source_gate::FamilyWork::acquire(
        crate::harness::codex_source_gate::FamilySource::Manager(&source),
    )
    .unwrap();
    assert!(
        crate::harness::codex_source_gate::PublicationLock::for_home(fixture._home.path()).is_err()
    );
    drop(a);
    drop(resource);
    crate::harness::codex_source_gate::PublicationLock::for_home(fixture._home.path()).unwrap();
}

#[test]
fn same_grant_target_borrows_actual_family_without_relocking_or_drop_unfreeze() {
    let fixture = Fixture::new();
    let reservation = fixture.reservation();
    reservation.cell.lock().unwrap().target = prepare::reservation_metadata_fixture(
        fixture.peer.caller.clone(),
        metadata_fixture::binding("source"),
        metadata_fixture::binding("source"),
    );
    let sealed = seal::kernel_fixture(&reservation);
    let kernel = sealed.retained_kernel().unwrap();
    let intent = reservation.cell.lock().unwrap().target.intent();
    let resource = kernel.target_resource(&intent).unwrap();
    resource.current().unwrap();
    kernel.current().unwrap();
    let source = fixture
        .registry
        .ready_source(&fixture.peer.caller, &fixture.member)
        .unwrap();
    fixture
        .registry
        .state
        .lock()
        .unwrap()
        .reservations
        .remove(&actor_key(&fixture.peer.caller));
    drop(reservation);
    drop(sealed);
    drop(kernel);
    assert!(
        crate::harness::codex_source_gate::FamilyWork::acquire(
            crate::harness::codex_source_gate::FamilySource::Manager(&source)
        )
        .is_err(),
        "before a verified ACK/handoff, dropping A handles cannot assert unlock"
    );
    drop(resource);
    crate::harness::codex_source_gate::FamilyWork::acquire(
        crate::harness::codex_source_gate::FamilySource::Manager(&source),
    )
    .unwrap();
}

#[test]
fn source_seal_or_operation_string_cannot_prepare_target_factory() {
    let fixture = Fixture::new();
    let reservation = fixture.reservation();
    let _sealed = seal::kernel_fixture(&reservation);
    let before = {
        let state = fixture.registry.state.lock().unwrap();
        let actor = &state.actors[&actor_key(&fixture.peer.caller)];
        (actor.highwater, actor.issued.len(), state.version)
    };
    assert!(matches!(
        fixture.registry.prepare_target_constructor(
            &fixture.peer,
            &fixture.connection,
            &target::absent_role_fixture(),
            &"a".repeat(32)
        ),
        Err(Error::Stale)
    ));
    let state = fixture.registry.state.lock().unwrap();
    let actor = &state.actors[&actor_key(&fixture.peer.caller)];
    assert_eq!((actor.highwater, actor.issued.len(), state.version), before);
    assert!(
        reservation
            .cell
            .lock()
            .unwrap()
            .target_constructors
            .is_empty()
    );
}

#[test]
fn settled_history_capacity_refuses_new_use_before_highwater_and_preserves_exact_cleanup() {
    // Actual counted Begin/End occurrences; synthetic source/image fixture.
    // Bounded refusal is not authenticated indefinite epoch compaction.
    let fixture = Fixture::new();
    let mut first = None;
    for counter in 1..=MAX_MEMBER_HISTORY as u64 {
        let capability = fixture.begin(counter);
        if counter == 1 {
            first = Some(capability.clone());
        }
        fixture
            .registry
            .end_use(
                &fixture.peer,
                &fixture.connection,
                &fixture.member,
                fixture.request(counter).nonce,
                &capability,
            )
            .unwrap();
    }
    let next = MAX_MEMBER_HISTORY as u64 + 1;
    assert!(matches!(
        fixture.registry.begin_use(
            peer(&fixture.peer),
            &fixture.connection,
            fixture.request(next),
            &fixture.lifetime.enter().unwrap()
        ),
        Err(Error::Busy)
    ));
    let state = fixture.registry.state.lock().unwrap();
    let uses = &state.uses[&member_key(&fixture.peer.caller, &fixture.member).unwrap()];
    assert_eq!(uses.highwater, MAX_MEMBER_HISTORY as u64);
    assert_eq!(uses.ending.len(), MAX_MEMBER_HISTORY);
    assert!(uses.active.is_empty() && uses.pending.is_empty());
    drop(state);
    fixture
        .registry
        .end_use(
            &fixture.peer,
            &fixture.connection,
            &fixture.member,
            fixture.request(1).nonce,
            first.as_deref().unwrap(),
        )
        .unwrap();
}

#[test]
fn contextual_preload_owns_independent_scope_after_parent_end() {
    // Synthetic source/peer only. Genuine process, gate, App operation and
    // End lifecycle; this does not qualify the supported owner/image path.
    let f = Fixture::new();
    let capability = f.begin(1);
    let context = f
        .registry
        .use_context(
            &f.peer,
            &f.connection,
            &f.member,
            f.request(1).nonce,
            &capability,
        )
        .unwrap();
    let parent_identity = context
        .namespace(&f.peer.caller, f._home.path())
        .unwrap()
        .scope()
        .operation_identity();
    let sequence = PreloadSequence::parse("1234567890abcdef0000000000000001").unwrap();
    assert!(
        f.registry
            .prepare_preload_use(
                &f.peer.caller,
                &f.member,
                &"c".repeat(32),
                sequence,
                &f.connection,
                PreloadUseIdentity {
                    member: &f.member,
                    nonce: f.request(1).nonce,
                    capability: &"0".repeat(64)
                }
            )
            .is_err()
    );
    let command = f
        .registry
        .prepare_preload_use(
            &f.peer.caller,
            &f.member,
            &"c".repeat(32),
            sequence,
            &f.connection,
            PreloadUseIdentity {
                member: &f.member,
                nonce: f.request(1).nonce,
                capability: &capability,
            },
        )
        .unwrap();
    assert!(command.scope().operation_identity() != parent_identity);
    f.registry
        .end_use(
            &f.peer,
            &f.connection,
            &f.member,
            f.request(1).nonce,
            &capability,
        )
        .unwrap();
    assert!(!context.current());
    assert!(command.current().is_ok());
    drop(command);
    let replay = f
        .registry
        .prepare_preload_use(
            &f.peer.caller,
            &f.member,
            &"c".repeat(32),
            sequence,
            &f.connection,
            PreloadUseIdentity {
                member: &f.member,
                nonce: f.request(1).nonce,
                capability: &capability,
            },
        )
        .unwrap();
    assert!(replay.current().is_ok());
    drop(replay);
    assert!(
        f.registry
            .prepare_preload_use(
                &f.peer.caller,
                &f.member,
                &"d".repeat(32),
                sequence,
                &f.connection,
                PreloadUseIdentity {
                    member: &f.member,
                    nonce: f.request(1).nonce,
                    capability: &capability
                }
            )
            .is_err()
    );
    let next = PreloadSequence::parse("1234567890abcdef0000000000000002").unwrap();
    assert!(
        f.registry
            .prepare_preload_use(
                &f.peer.caller,
                &f.member,
                &"d".repeat(32),
                next,
                &f.connection,
                PreloadUseIdentity {
                    member: &f.member,
                    nonce: f.request(1).nonce,
                    capability: &capability
                }
            )
            .is_err()
    );
}

#[test]
fn contextual_preload_cancel_keeps_actual_worker_counted_until_drop() {
    let f = Fixture::new();
    let capability = f.begin(1);
    let context = f
        .registry
        .use_context(
            &f.peer,
            &f.connection,
            &f.member,
            f.request(1).nonce,
            &capability,
        )
        .unwrap();
    let sequence = PreloadSequence::parse("1234567890abcdef0000000000000001").unwrap();
    let command = f
        .registry
        .prepare_preload_for_use(
            &f.peer.caller,
            &f.member,
            &"c".repeat(32),
            sequence,
            &f.connection,
            &context,
        )
        .unwrap();
    let key = member_key(&f.peer.caller, &f.member).unwrap();
    let pending = f.registry.state.lock().unwrap().preloads[&key].clone();
    pending.revoke_departed();
    assert!(command.current().is_err());
    assert!(!pending.retired());
    drop(command);
    assert!(pending.retired());
}

#[test]
fn work_rebind_transfers_exact_pending_scope_without_restoring_ready() {
    // Metadata injection only; actual kernel process, connection EOF, App
    // Scope and original native Pending transfer are exercised here.
    let f = Fixture::new();
    let capability = f.begin(1);
    let context = f
        .registry
        .use_context(
            &f.peer,
            &f.connection,
            &f.member,
            f.request(1).nonce,
            &capability,
        )
        .unwrap();
    let sequence = PreloadSequence::parse("1234567890abcdef0000000000000001").unwrap();
    let command = f
        .registry
        .prepare_preload_for_use(
            &f.peer.caller,
            &f.member,
            &"c".repeat(32),
            sequence,
            &f.connection,
            &context,
        )
        .unwrap();
    let identity = command.scope().operation_identity();
    let key = member_key(&f.peer.caller, &f.member).unwrap();
    let original = f.registry.state.lock().unwrap().preloads[&key].clone();
    f.registry.connection_closed(f.connection.id);
    let replacement = f.registry.fixture_connection();
    let request = || target::WorkRebindRequest {
        operation: &f.operation,
        member: &f.member,
        kind: target::WorkRebindKind::Ordinary,
    };
    assert!(matches!(
        f.registry
            .rebind_member_work(&f.peer.caller, &replacement, request()),
        Err(Error::Busy)
    ));
    assert!(
        f.registry.state.lock().unwrap().actors[&actor_key(&f.peer.caller)].issued[&f.member]
            .connection
            .same(&f.connection)
    );
    drop(command);
    f.registry
        .rebind_member_work(&f.peer.caller, &replacement, request())
        .unwrap();
    let resumed = f
        .registry
        .prepare_preload_use(
            &f.peer.caller,
            &f.member,
            &"c".repeat(32),
            sequence,
            &replacement,
            namespace::PreloadUseIdentity {
                member: &f.member,
                nonce: f.request(1).nonce,
                capability: &capability,
            },
        )
        .unwrap();
    assert!(resumed.scope().operation_identity() == identity);
    assert!(resumed.current().is_ok());
    assert!(
        !original.retired(),
        "the replacement owns the same real occurrence"
    );
    let state = f.registry.state.lock().unwrap();
    assert!(state.members[&key].unknown, "Bound is not Ready");
    assert!(state.preloads[&key].connection.same(&replacement));
}

#[test]
fn work_rebind_checks_original_body_and_never_mutates_after_app_seal() {
    let mut f = Fixture::new();
    f.registry.connection_closed(f.connection.id);
    let replacement = f.registry.fixture_connection();
    assert!(
        f.registry
            .rebind_member_work(
                &f.peer.caller,
                &replacement,
                target::WorkRebindRequest {
                    operation: &f.operation,
                    member: &f.member,
                    kind: target::WorkRebindKind::Derived {
                        use_member: &f.member,
                        use_operation: "1234567890abcdef0000000000000001",
                        capability: &"f".repeat(64)
                    }
                }
            )
            .is_err()
    );
    let pause = f.lifetime.pause().unwrap();
    drop(f.scope.take());
    let _drained = match pause.try_wait() {
        Ok(drained) => drained,
        Err(_) => panic!("fixture has no pending native operation"),
    };
    assert!(matches!(
        f.registry.rebind_member_work(
            &f.peer.caller,
            &replacement,
            target::WorkRebindRequest {
                operation: &f.operation,
                member: &f.member,
                kind: target::WorkRebindKind::Ordinary
            }
        ),
        Err(Error::Busy)
    ));
    let state = f.registry.state.lock().unwrap();
    assert!(
        state.actors[&actor_key(&f.peer.caller)].issued[&f.member]
            .connection
            .same(&f.connection)
    );
}

#[test]
fn reader_use_keeps_read_permission_without_manager_refresh_authority() {
    let f = Fixture::new();
    let key = member_key(&f.peer.caller, &f.member).unwrap();
    let mut readiness =
        SourceReadiness::preload(SourceWorkLease::for_home(f._home.path()).unwrap(), None).unwrap();
    readiness.complete(ReadyRole::ReaderLoaded).unwrap();
    f.registry
        .state
        .lock()
        .unwrap()
        .members
        .get_mut(&key)
        .unwrap()
        .readiness = readiness;
    let capability = f.begin(1);
    let context = f
        .registry
        .use_context(
            &f.peer,
            &f.connection,
            &f.member,
            f.request(1).nonce,
            &capability,
        )
        .unwrap();
    let proof = context.namespace(&f.peer.caller, f._home.path()).unwrap();
    assert!(proof.current().is_ok());
    assert!(matches!(proof.refresh_allowed(), Err(Error::Denied)));
    assert!(matches!(
        context.prepare_refresh(
            &f.peer.caller,
            f._home.path(),
            slot::RefreshRequest::new(
                proof.source().slot().into(),
                "e".repeat(32),
                proof.source().digest().into()
            )
        ),
        Err(Error::Denied)
    ));
}

#[test]
fn admitted_manager_refresh_role_survives_same_scope_ready_reset() {
    let f = Fixture::new();
    let capability = f.begin(1);
    let context = f
        .registry
        .use_context(
            &f.peer,
            &f.connection,
            &f.member,
            f.request(1).nonce,
            &capability,
        )
        .unwrap();
    assert!(context.current());
    let key = member_key(&f.peer.caller, &f.member).unwrap();
    f.registry
        .state
        .lock()
        .unwrap()
        .members
        .get_mut(&key)
        .unwrap()
        .readiness
        .role = None;
    let recovered = f
        .registry
        .registered_source_use(
            &f.peer.caller,
            &f.connection,
            &f.member,
            f.request(1).nonce,
            &capability,
        )
        .unwrap();
    let proof = recovered.namespace(&f.peer.caller, f._home.path()).unwrap();
    assert!(proof.refresh_allowed().is_ok());
    assert!(
        recovered
            .prepare_refresh(
                &f.peer.caller,
                f._home.path(),
                slot::RefreshRequest::new(
                    proof.source().slot().into(),
                    "e".repeat(32),
                    proof.source().digest().into()
                )
            )
            .is_ok()
    );
    assert!(matches!(
        recovered.prepare_refresh(
            &f.peer.caller,
            f._home.path(),
            slot::RefreshRequest::new(
                "666f726569676e".into(),
                "e".repeat(32),
                proof.source().digest().into()
            )
        ),
        Err(Error::Denied)
    ));
    let foreign = f.registry.fixture_connection();
    assert!(
        f.registry
            .registered_source_use(
                &f.peer.caller,
                &foreign,
                &f.member,
                f.request(1).nonce,
                &capability
            )
            .is_err()
    );
    assert!(matches!(
        f.registry.end_use(
            &f.peer,
            &f.connection,
            &f.member,
            f.request(1).nonce,
            &capability,
        ),
        Err(Error::Busy)
    ));
    // The actual read namespace owns a counted Scope. SourceUse handles are
    // only lookup metadata; keep both alive while releasing the real loan.
    drop(proof);
    f.registry
        .end_use(
            &f.peer,
            &f.connection,
            &f.member,
            f.request(1).nonce,
            &capability,
        )
        .unwrap();
    assert!(
        f.registry
            .registered_source_use(
                &f.peer.caller,
                &f.connection,
                &f.member,
                f.request(1).nonce,
                &capability
            )
            .is_err()
    );
}

#[test]
fn ordinary_resample_keeps_the_actual_accepted_read_operation() {
    let mut f = Fixture::new();
    let sequence = PreloadSequence::parse("1234567890abcdef0000000000000001").unwrap();
    let attempt = preload_sequence::Attempt {
        sequence,
        operation: f.operation.clone(),
        origin: preload_sequence::Origin::Ordinary,
    };
    let worker = preload_custody::enter_sequenced(
        &f.registry,
        &f.peer,
        &f.member,
        &f.operation,
        &f.connection,
        &attempt,
    )
    .unwrap();
    let identity = worker.scope().operation_identity();
    let key = member_key(&f.peer.caller, &f.member).unwrap();
    let source = f.registry.state.lock().unwrap().members[&key]
        .source
        .clone();
    f.registry
        .state
        .lock()
        .unwrap()
        .preload_sequences
        .get_mut(&key)
        .unwrap()
        .loaded(
            &attempt,
            &f.registration,
            source.credential_generation(),
            source.digest(),
        )
        .unwrap();
    drop(worker);
    let previous = PreloadPredecessor {
        registration: f.registration.clone(),
        credential_generation: source.credential_generation(),
        digest: source.digest().into(),
    };
    let command = match f
        .registry
        .prepare_ordinary_preload_resample(
            &f.peer.caller,
            &f.member,
            &f.operation,
            sequence,
            &f.connection,
            &previous,
        )
        .unwrap()
    {
        namespace::ResamplePreparation::Pending(command) => command,
        _ => panic!("an actual unresolved read was expected"),
    };
    assert!(command.scope().operation_identity() == identity);
    let pending = f.registry.state.lock().unwrap().preloads[&key].clone();
    let pause = f.lifetime.pause().unwrap();
    drop(f.scope.take());
    assert!(command.current().is_err());
    let pause = match pause.try_wait() {
        Err(pause) => pause,
        Ok(_) => panic!("native read still owns its count"),
    };
    pending.revoke_departed();
    drop(command);
    assert!(pause.try_wait().is_ok());
}

#[test]
fn owned_ordinary_ready_freezes_report_before_any_source_validation() {
    let f = Fixture::new();
    let sequence = PreloadSequence::parse("1234567890abcdef0000000000000001").unwrap();
    let attempt = preload_sequence::Attempt {
        sequence,
        operation: f.operation.clone(),
        origin: preload_sequence::Origin::Ordinary,
    };
    let worker = preload_custody::enter_sequenced(
        &f.registry,
        &f.peer,
        &f.member,
        &f.operation,
        &f.connection,
        &attempt,
    )
    .unwrap();
    let key = member_key(&f.peer.caller, &f.member).unwrap();
    let source = f.registry.state.lock().unwrap().members[&key]
        .source
        .clone();
    f.registry
        .state
        .lock()
        .unwrap()
        .preload_sequences
        .get_mut(&key)
        .unwrap()
        .loaded(
            &attempt,
            &f.registration,
            source.credential_generation(),
            source.digest(),
        )
        .unwrap();
    drop(worker);
    let report = |auth| Ready::Loaded {
        credential_generation: source.credential_generation(),
        digest: source.digest().into(),
        account: source.account().into(),
        user: source.user().into(),
        auth,
    };
    let pending = f
        .registry
        .prepare_ordinary_ready(
            &f.peer.caller,
            &f.connection,
            &f.member,
            &f.operation,
            sequence,
            &f.registration,
            report(auth()),
        )
        .unwrap();
    assert!(matches!(
        pending,
        namespace::CapturedReadyPreparation::Pending(_)
    ));
    drop(pending); // Simulate accepted but unstarted worker removal.
    let mut changed = auth();
    changed.generation += 1;
    assert!(matches!(
        f.registry.prepare_ordinary_ready(
            &f.peer.caller,
            &f.connection,
            &f.member,
            &f.operation,
            sequence,
            &f.registration,
            report(changed)
        ),
        Err(Error::Stale)
    ));
}

#[test]
fn historical_context_without_actual_applied_normalizer_cannot_mint_a_read_scope() {
    let mut f = Fixture::new();
    let sequence = PreloadSequence::parse("1234567890abcdef0000000000000001").unwrap();
    assert!(
        f.registry
            .prepare_normalized_preload(
                &f.peer.caller,
                &f.connection,
                target::normalize::NormalizedMemberRequest {
                    assignment: &"a".repeat(32),
                    context: &"b".repeat(64),
                    member: &f.member,
                    operation: &"c".repeat(32)
                },
                sequence
            )
            .is_err()
    );
    assert!(f.registry.state.lock().unwrap().preloads.is_empty());
    let pause = f.lifetime.pause().unwrap();
    drop(f.scope.take());
    assert!(pause.try_wait().is_ok());
}

#[test]
fn contextual_resample_ready_hint_requires_exact_recorded_attempt_even_after_parent_end() {
    // Synthetic source metadata, real process/gate/App/capability lifecycle.
    // This qualifies correlation/admission only, not a physical owner refresh.
    let f = Fixture::new();
    let capability = f.begin(1);
    let sequence = PreloadSequence::parse("1234567890abcdef0000000000000001").unwrap();
    let operation = "c".repeat(32);
    let command = f
        .registry
        .prepare_preload_use(
            &f.peer.caller,
            &f.member,
            &operation,
            sequence,
            &f.connection,
            PreloadUseIdentity {
                member: &f.member,
                nonce: f.request(1).nonce,
                capability: &capability,
            },
        )
        .unwrap();
    let key = member_key(&f.peer.caller, &f.member).unwrap();
    let previous = {
        let mut state = f.registry.state.lock().unwrap();
        let source = state.members[&key].source.clone();
        let attempt = preload_sequence::Attempt {
            sequence,
            operation: operation.clone(),
            origin: preload_sequence::Origin::Use {
                member: f.member.clone(),
                nonce: format!(
                    "{:016x}{:016x}",
                    f.request(1).nonce.prefix,
                    f.request(1).nonce.counter
                ),
                capability: capability.clone(),
            },
        };
        let ledger = state.preload_sequences.get_mut(&key).unwrap();
        ledger
            .loaded(
                &attempt,
                &f.registration,
                source.credential_generation(),
                source.digest(),
            )
            .unwrap();
        ledger
            .accept_ready(&attempt, &f.registration, ReadyRole::Manager(auth()))
            .unwrap();
        ledger.complete(&attempt, &f.registration).unwrap();
        PreloadPredecessor {
            registration: f.registration.clone(),
            credential_generation: source.credential_generation(),
            digest: source.digest().into(),
        }
    };
    drop(command);
    f.registry
        .end_use(
            &f.peer,
            &f.connection,
            &f.member,
            f.request(1).nonce,
            &capability,
        )
        .unwrap();
    let result = f
        .registry
        .prepare_contextual_preload_resample(
            &f.peer.caller,
            &f.member,
            &operation,
            sequence,
            &f.connection,
            &previous,
            PreloadUseIdentity {
                member: &f.member,
                nonce: f.request(1).nonce,
                capability: &capability,
            },
        )
        .unwrap();
    assert!(
        matches!(result,ResamplePreparation::Settled(PreloadResample::ReadyCommitted { previous: actual }) if actual==previous)
    );
    let mut foreign = previous.clone();
    foreign.digest = "0".repeat(64);
    assert!(
        f.registry
            .prepare_contextual_preload_resample(
                &f.peer.caller,
                &f.member,
                &operation,
                sequence,
                &f.connection,
                &foreign,
                PreloadUseIdentity {
                    member: &f.member,
                    nonce: f.request(1).nonce,
                    capability: &capability
                }
            )
            .is_err()
    );
    assert!(
        f.registry
            .prepare_contextual_preload_resample(
                &f.peer.caller,
                &f.member,
                &operation,
                sequence,
                &f.connection,
                &previous,
                PreloadUseIdentity {
                    member: &f.member,
                    nonce: f.request(1).nonce,
                    capability: &"0".repeat(64)
                }
            )
            .is_err()
    );
}

#[test]
fn existing_derived_work_rebind_after_parent_end_is_metadata_only_not_new_read_or_use() {
    // Synthetic source metadata; actual process, native connection/EOF and use state.
    let f = Fixture::new();
    let capability = f.begin(1);
    let child_connection = f.registry.fixture_connection();
    let open = "d".repeat(32);
    let child = f
        .registry
        .open_registered_derived(
            &f.peer.caller,
            &child_connection,
            &open,
            UseIdentity {
                member: &f.member,
                nonce: f.request(1).nonce,
                capability: &capability,
            },
        )
        .unwrap();
    f.registry.connection_closed(child_connection.id);
    f.registry
        .end_use(
            &f.peer,
            &f.connection,
            &f.member,
            f.request(1).nonce,
            &capability,
        )
        .unwrap();
    let replacement = f.registry.fixture_connection();
    let nonce = format!(
        "{:016x}{:016x}",
        f.request(1).nonce.prefix,
        f.request(1).nonce.counter
    );
    assert!(
        f.registry
            .rebind_member_work(
                &f.peer.caller,
                &replacement,
                target::WorkRebindRequest {
                    operation: &open,
                    member: &child.member,
                    kind: target::WorkRebindKind::Derived {
                        use_member: &f.member,
                        use_operation: &nonce,
                        capability: &"0".repeat(64)
                    },
                }
            )
            .is_err()
    );
    f.registry
        .rebind_member_work(
            &f.peer.caller,
            &replacement,
            target::WorkRebindRequest {
                operation: &open,
                member: &child.member,
                kind: target::WorkRebindKind::Derived {
                    use_member: &f.member,
                    use_operation: &nonce,
                    capability: &capability,
                },
            },
        )
        .unwrap();
    assert!(
        f.registry
            .registered_source_use(
                &f.peer.caller,
                &f.connection,
                &f.member,
                f.request(1).nonce,
                &capability
            )
            .is_err()
    );
    assert!(
        f.registry
            .prepare_preload_use(
                &f.peer.caller,
                &child.member,
                &"e".repeat(32),
                PreloadSequence::parse("11111111111111110000000000000001").unwrap(),
                &replacement,
                PreloadUseIdentity {
                    member: &f.member,
                    nonce: f.request(1).nonce,
                    capability: &capability
                }
            )
            .is_err()
    );
    let state = f.registry.state.lock().unwrap();
    assert!(
        !state
            .members
            .contains_key(&member_key(&f.peer.caller, &child.member).unwrap())
    );
    assert!(
        state.actors[&actor_key(&f.peer.caller)].issued[&child.member]
            .connection
            .same(&replacement)
    );
}

#[test]
fn derived_constructor_continues_actual_parent_during_owned_preload_ready_reset() {
    // Synthetic source row, real native Process/gates/counted PRELOAD/App/use.
    // No physical owner, HTTP or fresh Ready is supplied by this fixture.
    let f = Fixture::new();
    let capability = f.begin(1);
    let sequence = PreloadSequence::parse("1234567890abcdef0000000000000001").unwrap();
    let pending = f
        .registry
        .prepare_preload_use(
            &f.peer.caller,
            &f.member,
            &"c".repeat(32),
            sequence,
            &f.connection,
            PreloadUseIdentity {
                member: &f.member,
                nonce: f.request(1).nonce,
                capability: &capability,
            },
        )
        .unwrap();
    // Descriptive Ready resets on the original admitted source's publication.
    let key = member_key(&f.peer.caller, &f.member).unwrap();
    f.registry
        .state
        .lock()
        .unwrap()
        .members
        .get_mut(&key)
        .unwrap()
        .readiness
        .role = None;
    let new_scope = f.lifetime.enter().unwrap();
    assert!(
        f.registry
            .begin_use(peer(&f.peer), &f.connection, f.request(2), &new_scope)
            .is_err(),
        "new root/use still requires actual Ready"
    );
    let child_connection = f.registry.fixture_connection();
    let open = "d".repeat(32);
    assert!(
        f.registry
            .open_registered_derived(
                &f.peer.caller,
                &child_connection,
                &open,
                UseIdentity {
                    member: &f.member,
                    nonce: f.request(1).nonce,
                    capability: &"0".repeat(64)
                }
            )
            .is_err()
    );
    let child = f
        .registry
        .open_registered_derived(
            &f.peer.caller,
            &child_connection,
            &open,
            UseIdentity {
                member: &f.member,
                nonce: f.request(1).nonce,
                capability: &capability,
            },
        )
        .unwrap();
    let child_read = f
        .registry
        .prepare_preload_use(
            &f.peer.caller,
            &child.member,
            &"e".repeat(32),
            sequence,
            &child_connection,
            PreloadUseIdentity {
                member: &f.member,
                nonce: f.request(1).nonce,
                capability: &capability,
            },
        )
        .unwrap();
    assert!(child_read.scope().operation_identity() != pending.scope().operation_identity());
    assert!(
        f.registry
            .registered_source_use(
                &f.peer.caller,
                &child_connection,
                &f.member,
                f.request(1).nonce,
                &capability
            )
            .is_err(),
        "child connection cannot become original parent work"
    );
    f.registry
        .end_use(
            &f.peer,
            &f.connection,
            &f.member,
            f.request(1).nonce,
            &capability,
        )
        .unwrap();
    let another = f.registry.fixture_connection();
    assert!(
        f.registry
            .open_registered_derived(
                &f.peer.caller,
                &another,
                &"f".repeat(32),
                UseIdentity {
                    member: &f.member,
                    nonce: f.request(1).nonce,
                    capability: &capability
                }
            )
            .is_err(),
        "Closed capability cannot mint another child"
    );
    assert!(
        child_read.current().is_ok(),
        "accepted child's own counted completion survives parent End"
    );
    drop(child_read);
    drop(pending);
}

#[test]
fn derived_continuation_after_ready_reset_refuses_foreign_source_and_parent_socket_eof() {
    let f = Fixture::new();
    let capability = f.begin(1);
    let key = member_key(&f.peer.caller, &f.member).unwrap();
    let original = {
        let mut state = f.registry.state.lock().unwrap();
        let member = state.members.get_mut(&key).unwrap();
        member.readiness.role = None;
        member.source.clone()
    };
    let child = f.registry.fixture_connection();
    {
        let mut state = f.registry.state.lock().unwrap();
        state.members.get_mut(&key).unwrap().source = Arc::new(metadata_fixture::source(
            metadata_fixture::binding("foreign"),
            f._home.path().join("foreign.json"),
            1,
        ));
    }
    assert!(
        f.registry
            .open_registered_derived(
                &f.peer.caller,
                &child,
                &"e".repeat(32),
                UseIdentity {
                    member: &f.member,
                    nonce: f.request(1).nonce,
                    capability: &capability
                }
            )
            .is_err()
    );
    f.registry
        .state
        .lock()
        .unwrap()
        .members
        .get_mut(&key)
        .unwrap()
        .source = original;
    f.registry.connection_closed(f.connection.id);
    assert!(
        f.registry
            .open_registered_derived(
                &f.peer.caller,
                &child,
                &"e".repeat(32),
                UseIdentity {
                    member: &f.member,
                    nonce: f.request(1).nonce,
                    capability: &capability
                }
            )
            .is_err()
    );
}

#[test]
fn release_only_plain_retirement_cannot_bypass_native_use_or_managed_reader() {
    let fixture = Fixture::new();
    let cleanup = fixture.registry.fixture_connection();
    let capability = fixture.begin(1);
    assert_eq!(
        fixture.registry.retire_nonpreloaded_member(
            &fixture.peer.caller,
            &cleanup,
            &fixture.member,
            None
        ),
        Err(Error::Busy)
    );
    fixture
        .registry
        .end_use(
            &fixture.peer,
            &cleanup,
            &fixture.member,
            fixture.request(1).nonce,
            &capability,
        )
        .unwrap();
    assert_eq!(
        fixture.registry.retire_nonpreloaded_member(
            &fixture.peer.caller,
            &cleanup,
            &fixture.member,
            None
        ),
        Err(Error::Denied)
    );
    let key = member_key(&fixture.peer.caller, &fixture.member).unwrap();
    fixture
        .registry
        .state
        .lock()
        .unwrap()
        .members
        .get_mut(&key)
        .unwrap()
        .readiness
        .role = Some(ReadyRole::ReaderLoaded);
    assert_eq!(
        fixture.registry.retire_nonpreloaded_member(
            &fixture.peer.caller,
            &cleanup,
            &fixture.member,
            Some(&fixture.registration)
        ),
        Err(Error::Denied)
    );
    assert!(
        fixture
            .registry
            .state
            .lock()
            .unwrap()
            .members
            .contains_key(&key)
    );
}

fn rebound_manager_fixture() -> (Fixture, PreloadSequence, FixtureConnection) {
    // Explicit synthetic source/ledger injection, not a positive physical Slot
    // or image qualification. Process, native EOF/rebind, App scopes and gate
    // are real. Deliberately no Begin/Use exists before this recovery.
    let f = Fixture::new();
    let sequence = PreloadSequence::parse("1234567890abcdef0000000000000001").unwrap();
    let attempt = preload_sequence::Attempt {
        sequence,
        operation: f.operation.clone(),
        origin: preload_sequence::Origin::Ordinary,
    };
    let key = member_key(&f.peer.caller, &f.member).unwrap();
    {
        let mut state = f.registry.state.lock().unwrap();
        let source = state.members[&key].source.clone();
        let mut ledger = preload_sequence::Ledger::new(f.peer.process.clone(), attempt.clone());
        ledger
            .loaded(
                &attempt,
                &f.registration,
                source.credential_generation(),
                source.digest(),
            )
            .unwrap();
        ledger
            .accept_ready(&attempt, &f.registration, ReadyRole::Manager(auth()))
            .unwrap();
        ledger.complete(&attempt, &f.registration).unwrap();
        state.preload_sequences.insert(key, ledger);
    }
    f.registry.connection_closed(f.connection.id);
    let replacement = f.registry.fixture_connection();
    f.registry
        .rebind_member_work(
            &f.peer.caller,
            &replacement,
            target::WorkRebindRequest {
                operation: &f.operation,
                member: &f.member,
                kind: target::WorkRebindKind::Ordinary,
            },
        )
        .unwrap();
    (f, sequence, replacement)
}

#[test]
fn rebound_read_before_first_native_use_has_counted_custody_and_keeps_http_closed() {
    let (f, _, replacement) = rebound_manager_fixture();
    let operation = "e".repeat(32);
    let sequence = PreloadSequence::parse("1234567890abcdef0000000000000002").unwrap();
    let mut command = f
        .registry
        .prepare_rebound_preload(
            &f.peer.caller,
            &replacement,
            &f.member,
            &operation,
            sequence,
        )
        .unwrap();
    let key = member_key(&f.peer.caller, &f.member).unwrap();
    assert!(
        !f.registry.state.lock().unwrap().uses.contains_key(&key),
        "read recovery invents no native HTTP use"
    );
    assert!(f.registry.state.lock().unwrap().members[&key].unknown);
    assert!(
        f.registry
            .begin_use(
                peer(&f.peer),
                &replacement,
                f.request(1),
                f.scope.as_ref().unwrap()
            )
            .is_err()
    );
    assert!(command.current().is_ok());
    let env = Env {
        state: f._home.path().join("state"),
        aterm_state: None,
        cwd: f._home.path().into(),
        home: Some(f._home.path().into()),
        claude_dir: None,
        claude_config_unusable: None,
        claude_profile: None,
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        codex_home: None,
        xdg_config_home: None,
        codex_login: None,
        agy_conversation: None,
        now: 0,
        sid: f.peer.caller.sid.clone(),
        sock: None,
        config: None,
        claude_tmp: None,
        claude_running: None,
        tz_s: 0,
    };
    // The synthetic row has no signed physical namespace. Actual read must fail
    // rather than promoting it from same account/counter metadata.
    assert!(command.preload(&env).is_err());
    assert!(f.registry.state.lock().unwrap().members[&key].unknown);
    f.registry.connection_closed(replacement.id);
    assert!(command.current().is_err());
    drop(command);
    assert!(
        f.registry.state.lock().unwrap().preloads.contains_key(&key),
        "observer/worker drop does not close the owned read"
    );
}

#[test]
fn rebound_ready_requires_exact_manager_report_and_native_rebound_occurrence() {
    let (f, sequence, replacement) = rebound_manager_fixture();
    let key = member_key(&f.peer.caller, &f.member).unwrap();
    let report = || {
        let state = f.registry.state.lock().unwrap();
        let source = &state.members[&key].source;
        Ready::Loaded {
            credential_generation: source.credential_generation(),
            digest: source.digest().into(),
            account: source.account().into(),
            user: source.user().into(),
            auth: auth(),
        }
    };
    assert!(
        f.registry
            .prepare_rebound_ready(
                &f.peer.caller,
                &replacement,
                ReboundReadyRequest {
                    member: &f.member,
                    operation: &f.operation,
                    sequence,
                    registration: &"0".repeat(32),
                    ready: report()
                }
            )
            .is_err()
    );
    assert!(
        f.registry
            .prepare_rebound_ready(
                &f.peer.caller,
                &f.connection,
                ReboundReadyRequest {
                    member: &f.member,
                    operation: &f.operation,
                    sequence,
                    registration: &f.registration,
                    ready: report()
                }
            )
            .is_err()
    );
    let command = f
        .registry
        .prepare_rebound_ready(
            &f.peer.caller,
            &replacement,
            ReboundReadyRequest {
                member: &f.member,
                operation: &f.operation,
                sequence,
                registration: &f.registration,
                ready: report(),
            },
        )
        .unwrap();
    assert!(
        f.registry.state.lock().unwrap().members[&key].unknown,
        "prepared command is not a completion"
    );
    drop(command);
    let command = f
        .registry
        .prepare_rebound_ready(
            &f.peer.caller,
            &replacement,
            ReboundReadyRequest {
                member: &f.member,
                operation: &f.operation,
                sequence,
                registration: &f.registration,
                ready: report(),
            },
        )
        .unwrap();
    assert!(
        f.registry.state.lock().unwrap().members[&key].unknown,
        "lost observer does not clear Unknown"
    );
    drop(command);
    f.registry
        .state
        .lock()
        .unwrap()
        .members
        .get_mut(&key)
        .unwrap()
        .readiness
        .role = Some(ReadyRole::ReaderLoaded);
    assert!(
        f.registry
            .prepare_rebound_preload(
                &f.peer.caller,
                &replacement,
                &f.member,
                &"e".repeat(32),
                PreloadSequence::parse("1234567890abcdef0000000000000002").unwrap()
            )
            .is_err()
    );
}

#[test]
fn rebound_preload_does_not_replace_ordinary_or_sealed_constructor_admission() {
    let f = Fixture::new();
    let sequence = PreloadSequence::parse("1234567890abcdef0000000000000001").unwrap();
    assert!(
        f.registry
            .prepare_rebound_preload(
                &f.peer.caller,
                &f.connection,
                &f.member,
                &"c".repeat(32),
                sequence
            )
            .is_err()
    );
    let (f, _, replacement) = rebound_manager_fixture();
    let _pause = f.lifetime.pause().unwrap();
    assert!(
        f.registry
            .prepare_rebound_preload(
                &f.peer.caller,
                &replacement,
                &f.member,
                &"c".repeat(32),
                PreloadSequence::parse("1234567890abcdef0000000000000002").unwrap()
            )
            .is_err()
    );
    let key = member_key(&f.peer.caller, &f.member).unwrap();
    assert!(
        !f.registry.state.lock().unwrap().preloads.contains_key(&key),
        "closed App cannot acquire recovery read custody"
    );
}
