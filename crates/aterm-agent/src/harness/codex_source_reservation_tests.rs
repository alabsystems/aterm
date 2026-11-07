// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::super::{
    account_work, codex_assignment::prepare, codex_session_slot::metadata_fixture,
};
use super::*;

static CONNECTION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX - 1000);

struct Fixture {
    registry: Registry,
    peer: Peer,
    connection: Connection,
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
        let id = CONNECTION.fetch_sub(1, Ordering::SeqCst);
        let peer = Peer {
            caller: slot::Caller {
                sid: format!("s-use-fixture-{id}"),
                pid: process.pid,
                birth: process.birth,
            },
            process,
            launch: None,
        };
        let lifetime = account_work::Lifetime::default();
        let registry = Registry::for_app(&lifetime);
        let connection = registry.connection(id).unwrap();
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
            _scope: self.scope.as_ref().unwrap().clone(),
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
    let connection = fixture
        .registry
        .connection(CONNECTION.fetch_sub(1, Ordering::SeqCst))
        .unwrap();
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
    let fixture = Fixture::new();
    let capability = fixture.begin(1);
    let cleanup = fixture
        .registry
        .connection(CONNECTION.fetch_sub(1, Ordering::SeqCst))
        .unwrap();
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
            .is_err()
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
    let other_connection = fixture
        .registry
        .connection(CONNECTION.fetch_sub(1, Ordering::SeqCst))
        .unwrap();
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
    let connection = fixture
        .registry
        .connection(CONNECTION.fetch_sub(1, Ordering::SeqCst))
        .unwrap();
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
    let connection = fixture
        .registry
        .connection(CONNECTION.fetch_sub(1, Ordering::SeqCst))
        .unwrap();
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
    let cleanup_connection = fixture
        .registry
        .connection(CONNECTION.fetch_sub(1, Ordering::SeqCst))
        .unwrap();
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
    let connection = fixture
        .registry
        .connection(CONNECTION.fetch_sub(1, Ordering::SeqCst))
        .unwrap();
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
    let connection = fixture
        .registry
        .connection(CONNECTION.fetch_sub(1, Ordering::SeqCst))
        .unwrap();
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
    let connection = fixture
        .registry
        .connection(CONNECTION.fetch_sub(1, Ordering::SeqCst))
        .unwrap();
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
    let connection = fixture
        .registry
        .connection(CONNECTION.fetch_sub(1, Ordering::SeqCst))
        .unwrap();
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
    let state = fixture.registry.state.lock().unwrap();
    assert!(!state.uses.contains_key(&key));
    assert!(!state.preloads.contains_key(&key));
    assert!(!state.actors.contains_key(&actor_key(&fixture.peer.caller)));
}
