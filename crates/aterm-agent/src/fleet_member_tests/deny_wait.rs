// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::client::{self, Paths};

fn vault_beside(member: &World) -> Paths {
    let home = member.places.state.parent().unwrap();
    let paths = Paths::for_home(home);
    let source = home.join("fixture.vault");
    client::create(&paths, source.to_str().unwrap(), None, Some("m2")).unwrap();
    assert_eq!(vault_paths_for(&member.places).unwrap().dir, paths.dir);
    paths
}

#[test]
fn a_fleet_join_waits_for_its_own_vault_apply_to_finish() {
    let (issuer, code, mut serving) = issuing("join-busy-vault-issuer");
    let member = World::new("join-busy-vault-member");
    let home = member.places.state.parent().unwrap();
    let paths = Paths::for_home(home);
    let source = home.join("fixture.vault");
    client::create(&paths, source.to_str().unwrap(), None, Some("m2")).unwrap();
    assert_eq!(vault_paths_for(&member.places).unwrap().dir, paths.dir);
    assert_eq!(
        client::swarm_denied_nodes(&paths).unwrap(),
        Some(Vec::new())
    );

    let writer = Rc::new(RefCell::new(Some(
        client::test_hold_state_lock(&paths).unwrap(),
    )));
    let release = writer.clone();
    let observed = Rc::new(Cell::new(0));
    let count = observed.clone();
    let joined = client::test_with_denied_nodes_busy(
        move || {
            count.set(count.get() + 1);
            drop(release.borrow_mut().take());
        },
        || converge_on(&member.places, &code, &sync_here()),
    );
    drop(writer.borrow_mut().take());
    serving.stop();

    assert_eq!(
        observed.get(),
        1,
        "the join read the vault while its apply held state"
    );
    assert!(
        matches!(joined, Ok(FleetSync::Joined(_))),
        "the same join must continue after the apply: {joined:?}"
    );
    assert_eq!(
        read_key_file(&member.places.state).unwrap(),
        read_key_file(&issuer.places.state).unwrap()
    );
    let table = fleet_table(&member.config()).unwrap();
    assert_eq!(table.level(), Some(aterm_net::fleet::DRIVE_LEVEL));
    assert_eq!(table.drive_from, [aterm_net::fleet::ALL_MEMBERS]);
}

#[test]
fn an_expired_vault_wait_refuses_before_dialing_the_fleet() {
    let (issuer, _code, mut serving) = issuing("join-expired-vault-issuer");
    serving.stop();
    // A fresh socket has never been dialled by init's own-listener probe.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let code = placed(
        &issuer,
        ("127.0.0.1", listener.local_addr().unwrap().port()),
    );
    listener.set_nonblocking(true).unwrap();
    let member = World::new("join-expired-vault-member");
    let paths = vault_beside(&member);
    let writer = client::test_hold_state_lock(&paths).unwrap();
    let joined = join_with_denial_deadline(&member.places, &sync_here(), &code, Instant::now());
    drop(writer);

    assert!(joined.is_err_and(|error| failure_words(error).contains(DENY_LIST_UNREAD)));
    assert!(matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    assert!(!member.places.state.join(KEY_FILE).exists());
    assert_eq!(member.config(), "");
}

#[test]
fn a_leave_while_the_vault_is_busy_prevents_the_automatic_join() {
    let (_issuer, code, mut serving) = issuing("join-left-vault-issuer");
    let member = World::new("join-left-vault-member");
    let paths = vault_beside(&member);
    let writer = Rc::new(RefCell::new(Some(
        client::test_hold_state_lock(&paths).unwrap(),
    )));
    let release = writer.clone();
    let elsewhere = Places {
        state: member.places.state.clone(),
        config: member.places.config.clone(),
    };
    let left = Rc::new(Cell::new(false));
    let record = left.clone();
    let joined = client::test_with_denied_nodes_busy(
        move || {
            if !record.replace(true) {
                ok(leave(&elsewhere, &[]));
                drop(release.borrow_mut().take());
            }
        },
        || converge_on(&member.places, &code, &sync_here()),
    );
    drop(writer.borrow_mut().take());
    serving.stop();

    assert!(
        left.get(),
        "the leave occurred during the actual busy observation"
    );
    assert!(joined.is_err());
    assert!(member.places.state.join(member::OFF_FILE).is_file());
    assert!(!member.places.state.join(KEY_FILE).exists());
    assert!(fleet_table(&member.config()).unwrap().listen.is_none());
    assert!(!member.places.state.join(GRANT_OWED_FILE).exists());
}

#[test]
fn a_vault_removed_during_the_wait_cannot_authorize_the_pending_fleet_join() {
    let (issuer, code, mut serving) = issuing("join-removed-vault-issuer");
    let member = World::new("join-removed-vault-member");
    let paths = vault_beside(&member);
    let issuer_key = member::read_node_identity(&issuer.places.state)
        .unwrap()
        .unwrap()
        .key()
        .hex();
    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        client::SWARM_ROSTER_ENTRY,
        format!("deny_nodes = [\"{issuer_key}\"]\n").as_bytes(),
    )
    .unwrap();
    client::admin_publish(&paths, &admin, &doc).unwrap();
    assert_eq!(
        client::swarm_denied_nodes(&paths).unwrap(),
        Some(vec![issuer_key])
    );
    let writer = Rc::new(RefCell::new(Some(
        client::test_hold_state_lock(&paths).unwrap(),
    )));
    let release = writer.clone();
    let source_file = paths.dir.join("source.toml");
    let observed = Rc::new(Cell::new(0));
    let count = observed.clone();
    let joined = client::test_with_denied_nodes_busy(
        move || {
            count.set(count.get() + 1);
            std::fs::remove_file(&source_file).unwrap();
            drop(release.borrow_mut().take());
        },
        || converge_on(&member.places, &code, &sync_here()),
    );
    drop(writer.borrow_mut().take());
    serving.stop();

    assert_eq!(observed.get(), 1);
    assert!(
        joined.is_err(),
        "removing the vault cannot clear its issuer denial for this join: {joined:?}"
    );
    assert!(!member.places.state.join(KEY_FILE).exists());
    assert!(fleet_table(&member.config()).unwrap().listen.is_none());
    assert!(!member.places.state.join(GRANT_OWED_FILE).exists());
}

/// NEGATIVE CONTROL: when `aterm fleet join` starts while the initial vault admission still holds
/// `state.lock` before `source.toml` exists on disk, the join waits for that initial apply and
/// enforces the issuer denial it writes instead of treating the unconfigured state as `Some([])`.
#[test]
fn an_initial_vault_apply_before_source_toml_is_awaited_and_enforced() {
    let (issuer, code, mut serving) = issuing("join-initial-vault-issuer");
    let member = World::new("join-initial-vault-member");
    let home = member.places.state.parent().unwrap();
    let paths = Paths::for_home(home);
    let source = home.join("initial.vault");
    let issuer_key = member::read_node_identity(&issuer.places.state)
        .unwrap()
        .unwrap()
        .key()
        .hex();
    let writer = Rc::new(RefCell::new(Some(
        client::test_hold_state_lock(&paths).unwrap(),
    )));
    let release = writer.clone();
    let apply_paths = paths.clone();
    let observed = Rc::new(Cell::new(0));
    let count = observed.clone();
    let joined = client::test_with_denied_nodes_busy(
        move || {
            count.set(count.get() + 1);
            drop(release.borrow_mut().take());
            client::create(&apply_paths, source.to_str().unwrap(), None, Some("m2")).unwrap();
            let admin = client::admin_open(&apply_paths).unwrap();
            let mut doc = admin.doc.clone();
            doc.set_entry(
                &aterm_secrets::text::Audience::Everyone,
                client::SWARM_ROSTER_ENTRY,
                format!("deny_nodes = [\"{issuer_key}\"]\n").as_bytes(),
            )
            .unwrap();
            client::admin_publish(&apply_paths, &admin, &doc).unwrap();
        },
        || converge_on(&member.places, &code, &sync_here()),
    );
    drop(writer.borrow_mut().take());
    serving.stop();

    assert_eq!(observed.get(), 1);
    assert!(
        joined.is_err(),
        "an initial vault apply that denies the issuer must block the concurrent join: {joined:?}"
    );
    assert!(!member.places.state.join(KEY_FILE).exists());
    assert!(fleet_table(&member.config()).unwrap().listen.is_none());
    assert!(!member.places.state.join(GRANT_OWED_FILE).exists());
}
