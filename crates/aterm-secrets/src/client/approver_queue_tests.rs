// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Approvers read independently stored queues through the vault's signed mirror policy.

use super::*;

fn server(root: &Path) -> crate::server::Running {
    crate::server::start_at_home(
        std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: root.to_owned(),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
        Duration::ZERO,
    )
    .unwrap()
}

fn mirror_vault(
    root: &Path,
    password: bool,
) -> (
    Paths,
    crate::server::Running,
    crate::server::Running,
    String,
) {
    let primary = server(&root.join("primary"));
    let mirror = server(&root.join("mirror"));
    let primary_url = format!("http://{}/v1/team", primary.addr);
    let mirror_url = format!("http://{}/v1/team", mirror.addr);
    let paths = Paths::for_home(&root.join("admin"));
    if password {
        test_create_with_password(&paths, &primary_url, None, Some("admin")).unwrap();
    } else {
        create(&paths, &primary_url, None, Some("admin")).unwrap();
    }
    let admin = admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    let policy = crate::swarm_roster::SwarmPolicy {
        servers: vec![mirror_url.clone()],
        ..Default::default()
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_POLICY_ENTRY,
        policy.to_toml().as_bytes(),
    )
    .unwrap();
    admin_publish(&paths, &admin, &doc).unwrap();
    transport::publish(
        &Source::Url(mirror_url.clone()),
        &std::fs::read(paths.cache_file()).unwrap(),
    )
    .unwrap();
    (paths, primary, mirror, mirror_url)
}

fn request(root: &Path, host: &str) -> (String, String) {
    let identity = machine_identity(&Paths::for_home(&root.join(host))).unwrap();
    let recipient = identity.recipient();
    let line = format!("{} {host} {}", recipient.to_bech32(), join_code(&recipient));
    (recipient.to_bech32(), line)
}

#[test]
fn an_approver_reads_and_approves_a_verified_mirror_when_its_primary_is_down() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), false);
    let (recipient, line) = request(root.path(), "newcomer");
    transport::post_join(&mirror_url, &line).unwrap();
    assert_eq!(transport::list_joins(&mirror_url).unwrap().len(), 1);
    primary.stop();

    let approved = test_auto_approve_alab_lan(&paths)
        .expect("the verified policy mirror still serves the request");
    assert_eq!(approved.approved, ["newcomer"]);
    assert!(
        admin_open(&paths)
            .unwrap()
            .doc
            .machines()
            .iter()
            .any(|machine| machine.recipient == recipient)
    );
    mirror.stop();
}

#[test]
fn a_healthy_primary_does_not_hide_a_request_retained_only_by_a_mirror() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), false);
    let (recipient, line) = request(root.path(), "mirror-only");
    transport::post_join(&mirror_url, &line).unwrap();
    let Source::Url(primary_url) = config(&paths).unwrap().unwrap().source else {
        unreachable!()
    };
    assert!(transport::list_joins(&primary_url).unwrap().is_empty());

    let approved = test_auto_approve_alab_lan(&paths).unwrap();
    assert_eq!(approved.approved, ["mirror-only"]);
    assert!(
        admin_open(&paths)
            .unwrap()
            .doc
            .machines()
            .iter()
            .any(|machine| machine.recipient == recipient)
    );
    primary.stop();
    mirror.stop();
}

#[test]
fn nonempty_primary_and_mirror_queues_both_get_a_turn() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), false);
    let config = config(&paths).unwrap().unwrap();
    let Source::Url(primary_url) = &config.source else {
        unreachable!()
    };
    let (_, primary_line) = request(root.path(), "primary-only");
    let (_, mirror_line) = request(root.path(), "mirror-only");
    transport::post_join(primary_url, &primary_line).unwrap();
    transport::post_join(&mirror_url, &mirror_line).unwrap();
    for fresh in [false, true] {
        let mut heard = Heard::default();
        let mut seen = BTreeSet::new();
        for _ in 0..2 {
            if fresh {
                heard = Heard::default();
            }
            let (lines, _) =
                join_queues::hear(&paths, &config, &mut heard, Duration::ZERO).unwrap();
            assert_eq!(lines.len(), 1);
            seen.insert(heard.url.clone());
        }
        assert_eq!(
            seen,
            BTreeSet::from([primary_url.clone(), mirror_url.clone()])
        );
    }
    primary.stop();
    mirror.stop();
}

#[test]
fn fresh_readers_visit_every_busy_queue_across_multiple_batches() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), false);
    let mut mirrors = vec![mirror];
    let mut urls = vec![mirror_url];
    for index in 1..crate::discover::MAX_CANDIDATES * 2 {
        let mirror = server(&root.path().join(format!("mirror-{index}")));
        urls.push(format!("http://{}/v1/team", mirror.addr));
        mirrors.push(mirror);
    }
    let admin = admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    let policy = crate::swarm_roster::SwarmPolicy {
        servers: urls.clone(),
        ..Default::default()
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_POLICY_ENTRY,
        policy.to_toml().as_bytes(),
    )
    .unwrap();
    admin_publish(&paths, &admin, &doc).unwrap();
    let config = config(&paths).unwrap().unwrap();
    let Source::Url(primary_url) = &config.source else {
        unreachable!()
    };
    urls.insert(0, primary_url.clone());
    for (index, url) in urls.iter().enumerate() {
        let (_, line) = request(root.path(), &format!("queue-{index}"));
        transport::post_join(url, &line).unwrap();
    }
    let mut seen = BTreeSet::new();
    for _ in 0..urls.len() {
        let mut heard = Heard::default();
        let (lines, _) = join_queues::hear(&paths, &config, &mut heard, Duration::ZERO).unwrap();
        assert_eq!(lines.len(), 1);
        seen.insert(heard.url);
    }
    assert_eq!(seen, urls.into_iter().collect());
    primary.stop();
    for mirror in mirrors {
        mirror.stop();
    }
}

#[test]
fn empty_queues_keep_the_current_listing_and_continue_long_polling() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, _) = mirror_vault(root.path(), false);
    let config = config(&paths).unwrap().unwrap();
    let mut heard = Heard::default();
    let (lines, moved) = join_queues::hear(&paths, &config, &mut heard, Duration::ZERO).unwrap();
    assert!(lines.is_empty());
    assert!(moved);
    assert!(heard.version.is_some());
    let previous = heard.url.clone();
    let (lines, moved) = join_queues::hear(&paths, &config, &mut heard, Duration::ZERO).unwrap();
    assert!(lines.is_empty());
    assert_eq!(heard.url, previous);
    assert!(
        !moved,
        "keep the unchanged listing rather than rotating empty queues"
    );
    primary.stop();
    mirror.stop();
}

#[test]
fn a_mirror_must_serve_the_published_version_and_every_approved_recipient() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, _) = mirror_vault(root.path(), false);
    let admin = admin_open(&paths).unwrap();
    let member = machine_identity(&paths).unwrap().recipient().to_bech32();
    let (missing, _) = request(root.path(), "not-published");
    primary.stop();

    assert!(
        verify_approved_in_served_head(&paths, &admin, admin.seq, std::slice::from_ref(&member))
            .is_ok(),
        "the mirror can confirm an existing member"
    );
    assert!(
        verify_approved_in_served_head(
            &paths,
            &admin,
            admin.seq + 1,
            std::slice::from_ref(&member)
        )
        .is_err(),
        "a signed but stale mirror cannot confirm a later publication"
    );
    assert!(
        verify_approved_in_served_head(&paths, &admin, admin.seq, &[member, missing]).is_err(),
        "a current mirror must contain every approved recipient"
    );
    mirror.stop();
}

#[test]
fn mirror_queue_versions_and_addresses_cannot_validate_another_listing() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), false);
    let (_, line) = request(root.path(), "newcomer");
    let (_, stale) = request(root.path(), "stale");
    transport::post_join(&mirror_url, &line).unwrap();
    let (transport::Joins::Listed(_, version), actual) =
        transport::wait_joins_from(&mirror_url, None, Duration::ZERO).unwrap()
    else {
        panic!("the mirror has a fresh listing");
    };
    let config = config(&paths).unwrap().unwrap();
    primary.stop();
    for (url, from) in [
        (mirror_url.clone(), Some("192.0.2.33".parse().unwrap())),
        ("http://retired.invalid:7484/v1/team".to_owned(), actual),
    ] {
        let mut heard = Heard {
            url,
            version,
            lines: vec![stale.clone()],
            from,
        };
        let (lines, _) = join_queues::hear(&paths, &config, &mut heard, Duration::ZERO).unwrap();
        assert_eq!(heard.url, mirror_url);
        assert_eq!(heard.from, actual);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with(&line));
    }
    mirror.stop();
}

#[test]
fn a_policy_url_with_another_vaults_signature_cannot_supply_join_requests() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, _) = mirror_vault(root.path(), false);
    let foreign = server(&root.path().join("foreign"));
    let foreign_url = format!("http://{}/v1/team", foreign.addr);
    let foreign_paths = Paths::for_home(&root.path().join("foreign-admin"));
    create(&foreign_paths, &foreign_url, None, Some("foreign")).unwrap();
    let foreign_config = config(&foreign_paths).unwrap().unwrap();
    let admin = admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    let policy = crate::swarm_roster::SwarmPolicy {
        servers: vec![foreign_url.clone()],
        ..Default::default()
    };
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_POLICY_ENTRY,
        policy.to_toml().as_bytes(),
    )
    .unwrap();
    admin_publish(&paths, &admin, &doc).unwrap();
    let (_, line) = request(root.path(), "untrusted");
    transport::post_join(&foreign_url, &line).unwrap();
    let body = transport::get_within(&foreign_url, Duration::from_secs(60)).unwrap();
    let transport::Answer::Fetched(Fetched::Body(body)) = body else {
        panic!("the other vault still serves its envelope");
    };
    assert_eq!(
        envelope::parse(&body).unwrap().signer,
        foreign_config.signer.unwrap()
    );
    primary.stop();
    let current = config(&paths).unwrap().unwrap();
    assert!(join_queues::hear(&paths, &current, &mut Heard::default(), Duration::ZERO).is_err());
    foreign.stop();
    mirror.stop();
}

#[test]
fn password_refusals_go_back_to_the_mirror_that_supplied_the_request() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), true);
    let (recipient, line) = request(root.path(), "wrong-password");
    let proof = join::Key::derive("team", b"not-the-right-password").prove(
        "team",
        &recipient,
        "wrong-password",
    );
    transport::post_join(&mirror_url, &format!("{line} proof={proof}")).unwrap();
    primary.stop();
    let done = auto_approve(&paths).unwrap();
    assert_eq!(done.refused.as_slice(), std::slice::from_ref(&recipient));
    assert!(
        transport::list_verdicts(&mirror_url)
            .unwrap()
            .contains(&(recipient, join::proof_tag(&proof)))
    );
    mirror.stop();
}

#[test]
fn the_background_approver_records_the_queue_it_actually_judged() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), true);
    let password = admin_open(&paths).unwrap().doc.join_password().unwrap();
    let (recipient, line) = request(root.path(), "member");
    let proof = join::Key::derive("team", password.as_slice()).prove("team", &recipient, "member");
    transport::post_join(&mirror_url, &format!("{line} proof={proof}")).unwrap();
    primary.stop();
    let mut approver = Approver::default();
    approver_pass(&paths, &|_| {}, &mut approver);
    assert_eq!(
        read_answered(&paths).begun,
        Some(format!("team {mirror_url}"))
    );
    assert!(
        admin_open(&paths)
            .unwrap()
            .doc
            .machines()
            .iter()
            .any(|machine| machine.recipient == recipient)
    );
    mirror.stop();
}
