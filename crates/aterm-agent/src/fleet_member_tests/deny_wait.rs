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

struct FleetPair {
    _issuer_serving: Listener,
    member_serving: Listener,
    issuer: World,
    member: World,
}

fn fleet_pair(tag: &str) -> FleetPair {
    let (issuer, code, serving) = issuing(&format!("{tag}-issuer"));
    let member = World::new(&format!("{tag}-member"));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    ok(join(
        &member.places,
        &Opts {
            host: Some("127.0.0.1".to_owned()),
            port: Some(listener.local_addr().unwrap().port()),
            ..Opts::default()
        },
        &code,
    ));
    let member_serving = Listener::serve(listener, &member.places.state);
    ok(list_peers(&member.places, HANG));
    FleetPair {
        _issuer_serving: serving,
        member_serving,
        issuer,
        member,
    }
}

#[test]
fn a_drive_retains_its_local_denial_read_error() {
    use std::os::unix::fs::PermissionsExt as _;

    let pair = fleet_pair("drive-denial-cause");
    let node = member::node_id(&pair.member.places.state).unwrap();
    let listener = pair.member_serving.take();
    let paths = vault_beside(&pair.issuer);
    let source = paths.dir.join("source.toml");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
    let cause = client::swarm_denied_nodes(&paths).unwrap_err().to_string();
    let refused = drive_fleet_in(&pair.issuer.places, &node, HANG)
        .err()
        .expect("an unread local deny list must refuse the drive");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();

    assert!(
        refused.again,
        "an unavailable local read is not a peer denial"
    );
    assert!(matches!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock
    ));
    assert!(
        refused.why.contains(&cause),
        "the refusal lost the error from the actual local read: {refused:?}; cause={cause}"
    );
}

#[test]
fn a_drive_waits_for_its_local_writer_before_dialing_once() {
    let pair = fleet_pair("drive-busy-vault");
    let node = member::node_id(&pair.member.places.state).unwrap();
    // An issuer has no issuer-copy lookup to observe the lock before Drive's preflight.
    assert!(
        read_peers(&pair.issuer.places.state)
            .unwrap()
            .iter()
            .all(|peer| peer.via != Via::Issuer)
    );
    let paths = vault_beside(&pair.issuer);
    let writer = Rc::new(RefCell::new(Some(
        client::test_hold_state_lock(&paths).unwrap(),
    )));
    let release = writer.clone();
    let observed = Rc::new(Cell::new(0));
    let count = observed.clone();
    let refused = client::test_with_denied_nodes_busy(
        move || {
            count.set(count.get() + 1);
            drop(release.borrow_mut().take());
        },
        || {
            drive_fleet_in(&pair.issuer.places, &node, HANG)
                .err()
                .expect("the fixture serves no Drive")
        },
    );
    drop(writer.borrow_mut().take());
    assert_eq!(observed.get(), 1);
    assert!(
        refused.ungranted,
        "the same call must reach the peer: {refused:?}"
    );
}

#[test]
fn a_book_refresh_waits_for_its_local_writer() {
    let pair = fleet_pair("book-busy-vault");
    let paths = vault_beside(&pair.member);
    let writer = Rc::new(RefCell::new(Some(
        client::test_hold_state_lock(&paths).unwrap(),
    )));
    let release = writer.clone();
    let secret = read_key_file(&pair.member.places.state).unwrap();
    let me = own_identity(&pair.member.places).unwrap();
    let refreshed = client::test_with_denied_nodes_busy(
        move || drop(release.borrow_mut().take()),
        || {
            copy_again(
                &pair.member.places,
                &secret,
                Some(&me),
                (after_now(HANG), HANG),
            )
        },
    );
    drop(writer.borrow_mut().take());
    assert!(matches!(refreshed, Ok(true)), "{refreshed:?}");
}

#[test]
fn a_drive_enforces_the_denial_committed_while_it_waited() {
    let pair = fleet_pair("drive-denied-after-writer");
    let node = member::node_id(&pair.member.places.state).unwrap();
    let peer_key = own_identity(&pair.member.places).unwrap().key().hex();
    let listener = pair.member_serving.take();
    let paths = vault_beside(&pair.issuer);
    let writer = Rc::new(RefCell::new(Some(
        client::test_hold_state_lock(&paths).unwrap(),
    )));
    let release = writer.clone();
    let apply_paths = paths.clone();
    let refused = client::test_with_denied_nodes_busy(
        move || {
            drop(release.borrow_mut().take());
            let admin = client::admin_open(&apply_paths).unwrap();
            let mut doc = admin.doc.clone();
            doc.set_entry(
                &aterm_secrets::text::Audience::Everyone,
                client::SWARM_ROSTER_ENTRY,
                format!("deny_nodes = [\"{peer_key}\"]\n").as_bytes(),
            )
            .unwrap();
            client::admin_publish(&apply_paths, &admin, &doc).unwrap();
        },
        || drive_fleet_in(&pair.issuer.places, &node, HANG).is_err(),
    );
    drop(writer.borrow_mut().take());
    assert!(refused);
    assert!(matches!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock
    ));
}

#[test]
fn a_leave_during_the_vault_wait_cancels_the_pending_drive() {
    let pair = fleet_pair("drive-left-during-writer");
    let node = member::node_id(&pair.member.places.state).unwrap();
    let paths = vault_beside(&pair.issuer);
    let writer = Rc::new(RefCell::new(Some(
        client::test_hold_state_lock(&paths).unwrap(),
    )));
    let release = writer.clone();
    let left_at = Places {
        state: pair.issuer.places.state.clone(),
        config: pair.issuer.places.config.clone(),
    };
    let observed = Rc::new(Cell::new(false));
    let seen = observed.clone();
    let refused = client::test_with_denied_nodes_busy(
        move || {
            seen.set(true);
            drop(release.borrow_mut().take());
            ok(leave(&left_at, &[]));
        },
        || {
            drive_fleet_in(&pair.issuer.places, &node, HANG)
                .err()
                .expect("the drive was cancelled")
        },
    );
    drop(writer.borrow_mut().take());
    assert!(observed.get());
    assert!(
        !refused.again,
        "a leave cancels rather than retries: {refused:?}"
    );
    assert!(
        !refused.ungranted,
        "the cancelled drive must not reach peer admission: {refused:?}"
    );
    assert!(read_key_file(&pair.issuer.places.state).is_err());
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
        || {
            converge_under_elapsed(
                &member.places,
                &code,
                vault_now(&member.places).as_ref(),
                &sync_here(),
                Duration::from_secs(11),
            )
        },
    );
    drop(writer.borrow_mut().take());
    serving.stop();

    assert_eq!(
        observed.get(),
        1,
        "the required join observed the actual vault writer"
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
    let member = World::new("join-expired-vault-member");
    let paths = vault_beside(&member);
    let writer = client::test_hold_state_lock(&paths).unwrap();
    let joined =
        join_with_denial_elapsed(&member.places, &sync_here(), &code, Duration::from_secs(61));
    drop(writer);

    let words = failure_words(joined.unwrap_err());
    assert!(words.contains(DENY_LIST_UNREAD));
    assert!(words.contains(&client::SwarmDenialUnavailable::WriterBusy.to_string()));
    assert!(matches!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock
    ));
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

/// The issuer's vault, beside its fleet state, whose roster labels the member `m77`: a word no
/// host, book entry or node id of the fixture answers to, so only the roster names it.
fn labelled_member(pair: &FleetPair) -> Paths {
    use aterm_secrets::swarm_roster::{ROSTER_ENTRY, SwarmMember, SwarmRoster};
    let paths = vault_beside(&pair.issuer);
    let member = own_identity(&pair.member.places).unwrap();
    let roster = SwarmRoster {
        members: vec![SwarmMember {
            label: "m77".to_owned(),
            aliases: Vec::new(),
            vault_name: "studio-of-m77".to_owned(),
            recipient: "age1member".to_owned(),
            hint: None,
            hw_hint: None,
            admitted_ms: 1_760_000_000,
            claimed_label: None,
            claimed_at_ms: None,
            node_id: Some(member.node().to_owned()),
            node_key: Some(member.key().hex()),
        }],
        deny_labels: Vec::new(),
        deny_nodes: Vec::new(),
    };
    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .unwrap();
    client::admin_publish(&paths, &admin, &doc).unwrap();
    paths
}

/// A key-bound drive waits out a vault writer before using its denial view. The legacy roster
/// still supplies denial metadata, while the node id makes this test independent of registry
/// label enrollment. The unwaited read is the negative control for losing that metadata.
#[test]
fn a_drive_by_a_key_bound_node_waits_out_a_vault_writer() {
    let pair = fleet_pair("label-busy-vault");
    let paths = labelled_member(&pair);
    let node = own_identity(&pair.member.places).unwrap();
    let reached = drive_fleet_in(&pair.issuer.places, node.node(), HANG)
        .err()
        .expect("the fixture serves no Drive");
    assert!(reached.ungranted, "the key names the member: {reached:?}");

    let writer = Rc::new(RefCell::new(Some(
        client::test_hold_state_lock(&paths).unwrap(),
    )));
    assert!(
        client::test_without_roster_wait(|| roster_identities(&pair.issuer.places))
            .unwrap()
            .is_empty(),
        "an unwaited read takes the writer's moment as no roster"
    );
    let release = writer.clone();
    let observed = Rc::new(Cell::new(0));
    let count = observed.clone();
    let refused = client::test_with_denied_nodes_busy(
        move || {
            count.set(count.get() + 1);
            drop(release.borrow_mut().take());
        },
        || {
            drive_fleet_in(&pair.issuer.places, node.node(), HANG)
                .err()
                .expect("the fixture serves no Drive")
        },
    );
    drop(writer.borrow_mut().take());
    assert_eq!(observed.get(), 1, "the drive met the writer and waited");
    assert!(
        refused.ungranted,
        "the key named the member while the writer held the vault: {refused:?}"
    );
}

/// A drive by a roster label whose deadline passes while a vault writer holds the state lock is
/// refused to try again, saying the roster is unread, and dials nothing: never "no Mac this
/// machine's book records", which names a current member as unknown.
#[test]
fn a_drive_whose_roster_stays_unread_is_refused_to_try_again() {
    let pair = fleet_pair("label-expired-vault");
    let paths = labelled_member(&pair);
    let listener = pair.member_serving.take();
    let writer = client::test_hold_state_lock(&paths).unwrap();
    let refused = drive_fleet_in(&pair.issuer.places, "m77", Duration::ZERO)
        .err()
        .expect("the roster is unread");
    drop(writer);
    assert!(refused.again, "a writer's moment is retryable: {refused:?}");
    assert!(refused.left.is_none(), "{refused:?}");
    assert!(refused.why.contains("swarm roster"), "{}", refused.why);
    assert!(
        !refused.why.contains("no Mac this machine's book records"),
        "{}",
        refused.why
    );
    assert!(matches!(
        aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Ready,
            aterm_uds::AcceptedMode::Blocking,
        ),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock
    ));
}
