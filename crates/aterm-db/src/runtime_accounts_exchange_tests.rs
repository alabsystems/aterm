// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::replica::{InventoryObservation, InventorySummary, MatchingInventory};
use aterm_secrets::commons::roster::Role;

fn matching(observed: InventoryObservation) -> MatchingInventory {
    let report = crate::replica::Report {
        table: Some(observed.scope.digest()),
        local_known: Some(observed.inventory),
        peer_known: Some(observed.inventory),
        ..Default::default()
    };
    MatchingInventory::from_observations(&report, observed, observed).unwrap()
}

fn round(exchange: MatchingInventory) -> RepairRound {
    let mut round = RepairRound::panicked(None);
    round.actor = Some(exchange.peer_actor());
    round.exchange_started = true;
    round.control = Ok(Default::default());
    round.application = Some(crate::replica::Report {
        table: Some(exchange.scope().digest()),
        local_known: Some(exchange.inventory()),
        peer_known: Some(exchange.inventory()),
        matching_inventory: Some(exchange),
        ..Default::default()
    });
    round
}

fn sample(bootstrap: &Bootstrap, peer: &Bootstrap) -> InventoryObservation {
    let session = crate::table::Session::observe(bootstrap).unwrap();
    InventoryObservation {
        scope: crate::catalog::Descriptor::application(
            bootstrap.trust(),
            "accounts",
            "proposals",
            1,
        )
        .unwrap()
        .scope,
        local_actor: session.actor(),
        peer_actor: peer.request().unwrap().binding().actor(),
        control_observation: session.observation().unwrap(),
        inventory: InventorySummary::of(&Default::default()),
        observed_at_ms: 1000,
        sampled_at: Instant::now(),
    }
}

#[test]
fn account_exchange_remapping_requires_exact_scope_actor_context_and_peer_binding() {
    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let peer = lab.bootstrap(1);
    let journal = Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
    let access = bootstrap.access_in(&journal).unwrap().unwrap();
    let observed = sample(&bootstrap, &peer);
    let candidate =
        accounts_exchange_candidate(&bootstrap, &journal, &access, matching(observed)).unwrap();
    assert_eq!(
        candidate.observation.peer,
        machine_identity(peer.request().unwrap().binding())
    );
    assert_eq!(candidate.observation.sampled_at_ms, observed.observed_at_ms);
    assert_eq!(candidate.sampled_at, observed.sampled_at);
    let mut wrong_table = observed;
    wrong_table.scope =
        crate::catalog::Descriptor::application(bootstrap.trust(), "notes", "pages", 1)
            .unwrap()
            .scope;
    let mut wrong_actor = observed;
    wrong_actor.local_actor = observed.peer_actor;
    let mut wrong_control = observed;
    wrong_control.control_observation[0] ^= 1;
    let mut unknown_peer = observed;
    unknown_peer.peer_actor = [9; 48];
    let mut own_binding = observed;
    own_binding.peer_actor = observed.local_actor;
    for changed in [
        wrong_table,
        wrong_actor,
        wrong_control,
        unknown_peer,
        own_binding,
    ] {
        assert!(
            accounts_exchange_candidate(&bootstrap, &journal, &access, matching(changed)).is_none()
        );
    }
}

#[test]
fn contact_projection_rechecks_real_custody_and_never_retimes_an_exchange() {
    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let peer = lab.bootstrap(1);
    let observed = sample(&bootstrap, &peer);
    let completed = round(matching(observed));
    let local = machine_identity(bootstrap.request().unwrap().binding());
    for dir in ["network-one", "network-two"] {
        aterm_secrets::fsx::ensure_private_dir(&lab._home.path().join(dir)).unwrap();
    }
    let handle = ObservationHandle::default();
    let mut history =
        Observer::open(&lab._home.path().join("network-one/config"), handle.clone()).unwrap();
    history.local(local.clone(), bootstrap.trust()).unwrap();
    observe_contacts(
        &bootstrap,
        &std::cell::OnceCell::new(),
        &mut history,
        Vec::new(),
        std::slice::from_ref(&completed),
        &place::Publisher::default(),
        || panic!("historical evidence must use its own sample clock"),
    );
    history.observe(
        Visibility::Active,
        0,
        "observing",
        2000,
        observed.sampled_at + Duration::from_secs(1),
    );
    assert_eq!(
        handle.snapshot().accounts_exchange.unwrap().sampled_at_ms,
        1000
    );
    std::fs::remove_file(lab.paths[0].dir.join("identity")).unwrap();
    let handle = ObservationHandle::default();
    let mut refused =
        Observer::open(&lab._home.path().join("network-two/config"), handle.clone()).unwrap();
    refused.local(local, bootstrap.trust()).unwrap();
    observe_contacts(
        &bootstrap,
        &std::cell::OnceCell::new(),
        &mut refused,
        Vec::new(),
        &[completed],
        &place::Publisher::default(),
        || panic!("custody refusal cannot become a new sample"),
    );
    refused.observe(
        Visibility::Active,
        0,
        "observing",
        3000,
        observed.sampled_at + Duration::from_secs(2),
    );
    assert!(handle.snapshot().accounts_exchange.is_none());
}

#[test]
fn authenticated_application_failure_is_published_separately_from_peer_contact() {
    use aterm_types::network_state::{RepairOutcome, RepairStage};
    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let peer = lab.bootstrap(1);
    let actor = peer.request().unwrap().binding().actor();
    let local = machine_identity(bootstrap.request().unwrap().binding());
    let at = Instant::now();
    let config = lab._home.path().join("repair-observer/config");
    aterm_secrets::fsx::ensure_private_dir(config.parent().unwrap()).unwrap();
    let handle = ObservationHandle::default();
    let mut history = Observer::open(&config, handle.clone()).unwrap();
    history.local(local, bootstrap.trust()).unwrap();
    let mut failed = RepairRound::panicked(Some("127.0.0.1:41384".parse().unwrap()));
    failed.actor = Some(actor);
    failed.exchange_started = true;
    failed.phase = Some(crate::control::Phase::Application);
    failed.control = Ok(Default::default());
    failed.application_error = Some(crate::control::Failure::new(
        aterm_types::network_state::RepairCause::Decode,
        "private fixture error",
    ));
    failed.completed_at_ms = 1500;
    observe_contacts(
        &bootstrap,
        &std::cell::OnceCell::new(),
        &mut history,
        vec![(actor, at)],
        std::slice::from_ref(&failed),
        &place::Publisher::default(),
        || (2000, at + Duration::from_secs(1)),
    );
    history.observe(
        Visibility::Active,
        0,
        "observing",
        2000,
        at + Duration::from_secs(1),
    );
    let state = handle.snapshot();
    assert_eq!(state.peers.len(), 1);
    assert_eq!(state.peers[0].last_seen_ms, 1000);
    assert_eq!(
        state.peers[0].reachability,
        aterm_types::network_state::Reachability::Reachable
    );
    let attempt = state.peers[0].last_repair.as_ref().unwrap();
    assert_eq!(attempt.completed_at_ms, 1500);
    assert_eq!(
        attempt.outcome,
        RepairOutcome::Failed(aterm_types::network_state::RepairCause::Decode)
    );
    assert_eq!(attempt.stage, Some(RepairStage::Application));
    assert!(
        !crate::network_state::value(&state)
            .to_string()
            .contains("private fixture")
    );

    failed.actor = Some([9; 48]);
    failed.completed_at_ms = 2500;
    observe_contacts(
        &bootstrap,
        &std::cell::OnceCell::new(),
        &mut history,
        Vec::new(),
        &[failed.clone()],
        &place::Publisher::default(),
        || panic!("an unbound peer must not establish a contact"),
    );
    std::fs::remove_file(lab.paths[0].dir.join("identity")).unwrap();
    failed.actor = Some(actor);
    observe_contacts(
        &bootstrap,
        &std::cell::OnceCell::new(),
        &mut history,
        Vec::new(),
        &[failed],
        &place::Publisher::default(),
        || panic!("unavailable custody must not establish a contact"),
    );
    history.observe(
        Visibility::Active,
        0,
        "observing",
        3000,
        at + Duration::from_secs(2),
    );
    assert_eq!(
        handle.snapshot().peers[0].last_repair,
        state.peers[0].last_repair
    );
}
