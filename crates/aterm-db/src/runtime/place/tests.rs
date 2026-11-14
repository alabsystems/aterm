// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::runtime::{AuthenticatedConnection, observe_contacts};
use aterm_netplace::{Book, BookMember, Facts, Record, Where, heard, step};
use aterm_secrets::{
    age::Identity,
    commons::{binding::Request, roster::Role},
};

fn request(machine: u8, incarnation: u8) -> Request {
    Request::create(
        [9; 16],
        crate::crypto::public(&[99; 32]).unwrap(),
        [machine; 16],
        [incarnation; 16],
        &Identity::generate().unwrap(),
        &[incarnation; 32],
    )
    .unwrap()
}

fn round(peer: &Binding) -> RepairRound {
    let mut round = RepairRound::panicked(None);
    round.actor = Some(peer.actor());
    round.exchange_started = true;
    round.connection = Some(AuthenticatedConnection {
        local: "192.168.4.31:40000".parse().unwrap(),
        peer: "192.168.4.40:50000".parse().unwrap(),
        began_ms: 2000,
        authenticated_ms: 2100,
    });
    round.completed_at_ms = 2200;
    round
}

fn facts() -> Facts {
    let place = aterm_netplace::sample::home(1000, 0);
    Facts {
        route: place.route,
        tunnel: None,
        path: place.path,
    }
}

fn book() -> Book {
    Book {
        members: (0..3)
            .map(|n| BookMember {
                member: format!("{n:016x}"),
                addr: None,
                answered_ms: None,
                network: None,
            })
            .collect(),
    }
}

#[test]
fn three_incarnations_count_as_one_machine_and_three_machines_can_identify_home() {
    let root = aterm_tempfile::tempdir().unwrap();
    let dir = aterm_netplace::record::dir(root.path());
    let publisher = Publisher {
        dir: Some(dir.clone()),
    };
    let local = request(1, 1);
    let peers: Vec<_> = (2..5).map(|n| request(2, n)).collect();
    publisher.note(
        peers
            .iter()
            .map(|peer| answer(&round(peer.binding()), local.binding(), peer.binding()).unwrap())
            .collect(),
    );
    let evidence = heard::read(&dir);
    assert_eq!(
        evidence.answers.len(),
        1,
        "writer incarnations share one machine key"
    );
    let facts = facts();
    let book = book();
    let (initial, _) = step(
        &Record::default(),
        &facts,
        &Default::default(),
        Some(&book),
        1000,
        None,
    );
    let (observed, place) = step(&initial, &facts, &evidence, Some(&book), 3000, None);
    assert_eq!(place.heard, 1);
    assert_ne!(place.at, Where::AlabLan);
    assert!(observed.remembered.is_empty());

    publisher.note(
        (3..5)
            .map(|n| {
                let peer = request(n, n);
                answer(&round(peer.binding()), local.binding(), peer.binding()).unwrap()
            })
            .collect(),
    );
    let evidence = heard::read(&dir);
    assert_eq!(evidence.answers.len(), 3);
    let (observed, place) = step(&initial, &facts, &evidence, Some(&book), 3000, None);
    assert_eq!(place.heard, 3);
    assert_eq!(place.at, Where::AlabLan);
    assert_eq!(observed.remembered.len(), 1);
}

#[test]
fn captured_socket_route_and_original_time_cannot_identify_a_later_network() {
    let local = request(1, 1);
    let facts = facts();
    let book = book();
    let (initial, _) = step(
        &Record::default(),
        &facts,
        &Default::default(),
        Some(&book),
        1000,
        None,
    );
    let evidence = aterm_netplace::Evidence {
        answers: (2..5)
            .map(|n| {
                let peer = request(n, n);
                let note = answer(&round(peer.binding()), local.binding(), peer.binding()).unwrap();
                assert_eq!((note.began_ms, note.at_ms), (2000, 2100));
                assert_eq!(note.via, Some("192.168.4.31".parse().unwrap()));
                note
            })
            .collect(),
        ..Default::default()
    };
    // The next router reuses the same subnet and local IP. Collection must not
    // turn the previous stay's authentications into new contact on this stay.
    let mut moved = facts.clone();
    moved.route.as_mut().unwrap().gateway_hw = Some(vec![2, 0, 0, 0, 0, 2]);
    let (away, _) = step(
        &initial,
        &moved,
        &Default::default(),
        Some(&book),
        4000,
        None,
    );
    let (away, place) = step(&away, &moved, &evidence, Some(&book), 5000, None);
    assert_eq!(place.heard, 0);
    assert_ne!(place.at, Where::AlabLan);
    assert!(away.remembered.is_empty());

    let mut tunnel = evidence;
    for answer in &mut tunnel.answers {
        answer.via = Some("10.0.0.2".parse().unwrap());
    }
    let (_, place) = step(&initial, &facts, &tunnel, Some(&book), 3000, None);
    assert_eq!(
        place.heard, 0,
        "on-subnet peer addresses do not hide a different socket route"
    );
}

#[test]
fn a_connection_requires_authenticated_identity_original_clocks_and_a_remote_machine() {
    let local = request(1, 1);
    let peer = request(2, 2);
    let connected = round(peer.binding());
    assert!(answer(&connected, local.binding(), peer.binding()).is_some());
    let changes: [fn(&mut RepairRound); 8] = [
        |round| round.actor = None,
        |round| round.actor = Some([42; 48]),
        |round| round.exchange_started = false,
        |round| round.connection = None,
        |round| round.connection.as_mut().unwrap().began_ms = 0,
        |round| round.connection.as_mut().unwrap().authenticated_ms = 1999,
        |round| round.connection.as_mut().unwrap().authenticated_ms = 2201,
        |round| round.connection.as_mut().unwrap().local = "127.0.0.1:40000".parse().unwrap(),
    ];
    for change in changes {
        let mut changed = connected.clone();
        change(&mut changed);
        assert!(answer(&changed, local.binding(), peer.binding()).is_none());
    }
    let same_machine = request(1, 2);
    assert!(
        answer(
            &round(same_machine.binding()),
            local.binding(),
            same_machine.binding()
        )
        .is_none()
    );
}

#[test]
fn current_membership_and_local_custody_gate_shared_evidence_but_table_failure_does_not() {
    let lab = crate::table::tests::Lab::new(&[Role::Writer, Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let peer = lab.bootstrap(1);
    let dir = aterm_netplace::record::dir(lab._home.path());
    let publisher = Publisher {
        dir: Some(dir.clone()),
    };
    let config = lab._home.path().join("network/config");
    aterm_secrets::fsx::ensure_private_dir(config.parent().unwrap()).unwrap();
    let mut history = crate::network_state::Observer::open(&config, Default::default()).unwrap();
    history
        .local(
            super::super::machine_identity(bootstrap.request().unwrap().binding()),
            bootstrap.trust(),
            Vec::new,
        )
        .unwrap();
    let connected = round(peer.request().unwrap().binding());
    let mut unknown = connected.clone();
    unknown.actor = Some([42; 48]);
    let mut own = connected.clone();
    own.actor = Some(bootstrap.request().unwrap().binding().actor());
    let mut unproven = connected.clone();
    unproven.actor = None;
    observe_contacts(
        &bootstrap,
        &std::cell::OnceCell::new(),
        &mut history,
        Vec::new(),
        &[unknown, own, unproven],
        &publisher,
        || panic!("no contact timestamp was requested"),
    );
    assert!(heard::read(&dir).answers.is_empty());

    let mut failed = connected.clone();
    failed.control = Err(crate::control::Failure::new(
        aterm_types::network_state::RepairCause::Io,
        "authenticated, but control repair failed",
    ));
    failed.application_error = Some(crate::control::Failure::new(
        aterm_types::network_state::RepairCause::TableUndeclared,
        "no table repair",
    ));
    observe_contacts(
        &bootstrap,
        &std::cell::OnceCell::new(),
        &mut history,
        Vec::new(),
        &[failed],
        &publisher,
        || panic!("retained evidence must use its original clocks"),
    );
    let evidence = heard::read(&dir);
    assert_eq!(
        evidence.answers,
        [answer(
            &connected,
            bootstrap.request().unwrap().binding(),
            peer.request().unwrap().binding()
        )
        .unwrap()]
    );

    let held = lab.paths[0].dir.join("identity-held");
    std::fs::rename(lab.paths[0].dir.join("identity"), &held).unwrap();
    let unavailable_dir = dir.with_file_name("unavailable-place");
    let unavailable = Publisher {
        dir: Some(unavailable_dir.clone()),
    };
    observe_contacts(
        &bootstrap,
        &std::cell::OnceCell::new(),
        &mut history,
        Vec::new(),
        &[connected],
        &unavailable,
        || panic!("missing custody may not become a new contact"),
    );
    assert!(heard::read(&unavailable_dir).answers.is_empty());
    std::fs::rename(held, lab.paths[0].dir.join("identity")).unwrap();
}
