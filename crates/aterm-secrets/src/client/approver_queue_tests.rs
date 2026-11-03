// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Approvers read independently stored queues through the vault's signed mirror policy.

use super::*;

#[test]
fn request_debug_omits_admission_proofs_even_inside_an_inspection() {
    let request = Request {
        recipient: "public-recipient".into(),
        host: "new-machine".into(),
        code: "public-code".into(),
        at: Some(42),
        allowed: Some("private-member-authorization".into()),
        proof: Some("private-password-proof".into()),
    };
    let inspection = RequestInspection {
        requests: vec![request.clone()],
        ..Default::default()
    };
    let wire = transport::JoinRequest {
        recipient: request.recipient.clone(),
        host: request.host.clone(),
        code: request.code.clone(),
        time: request.at,
        allowed: request.allowed.clone(),
        proof: request.proof.clone(),
    };
    for output in [
        format!("{request:?}"),
        format!("{inspection:?}"),
        format!("{wire:?}"),
    ] {
        assert!(output.contains("new-machine"));
        assert!(output.contains("member_allow_present: true"));
        assert!(output.contains("password_proof_present: true"));
        assert!(!output.contains("private-member-authorization"));
        assert!(!output.contains("private-password-proof"));
    }
    let absent = Request {
        allowed: None,
        proof: None,
        ..request
    };
    let wire_absent = transport::JoinRequest {
        allowed: None,
        proof: None,
        ..wire
    };
    for output in [format!("{absent:?}"), format!("{wire_absent:?}")] {
        assert!(output.contains("member_allow_present: false"));
        assert!(output.contains("password_proof_present: false"));
    }
}

#[test]
fn inspection_origin_keeps_the_observed_address_without_url_credentials() {
    let address = "192.0.2.7".parse().unwrap();
    let inspection = RequestInspection::from_queue(
        Vec::new(),
        true,
        Heard {
            url: "https://queue.example/v1/team?access=private-query#private-fragment".into(),
            from: Some(address),
            version: Some(42),
            ..Heard::default()
        },
    );
    assert_eq!(
        inspection.queue_source.as_deref(),
        Some("https://queue.example/v1/team")
    );
    assert_eq!(inspection.queue_server, Some(address));
    assert_eq!(inspection.queue_version, Some(42));
    assert!(inspection.known_members_filtered);
    let absent = RequestInspection::from_queue(Vec::new(), false, Heard::default());
    assert!(absent.queue_source.is_none());
    assert!(absent.queue_server.is_none());
    assert!(absent.queue_version.is_none());
}

fn server(root: &Path) -> crate::server::Running {
    crate::server::start_with_join_change_gap(
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

pub(crate) fn mirror_vault(
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

pub(crate) fn request(root: &Path, host: &str) -> (String, String) {
    let identity = machine_identity(&Paths::for_home(&root.join(host))).unwrap();
    let recipient = identity.recipient();
    let line = format!("{} {host} {}", recipient.to_bech32(), join_code(&recipient));
    (recipient.to_bech32(), line)
}

/// `line`, the request of `recipient` for `host`, with its proof of the join password of the
/// vault `paths` administers: one its approver lets in by itself.
fn proved(paths: &Paths, recipient: &str, host: &str, line: &str) -> String {
    let password = admin_open(paths)
        .unwrap()
        .doc
        .join_password()
        .expect("a join password");
    let proof = join::Key::derive("team", password.as_slice()).prove("team", recipient, host);
    format!("{line} proof={proof}")
}

#[test]
fn an_approver_reads_and_approves_a_verified_mirror_when_its_primary_is_down() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), true);
    let (recipient, line) = request(root.path(), "newcomer");
    let line = proved(&paths, &recipient, "newcomer", &line);
    transport::post_join(&mirror_url, &line).unwrap();
    assert_eq!(transport::list_joins(&mirror_url).unwrap().len(), 1);
    primary.stop();

    let inspected = inspect_requests_until(&paths, Instant::now() + aterm_spec::wait::MUST_HAPPEN)
        .expect("bounded inspection identifies the mirror that answered");
    assert_eq!(inspected.requests[0].recipient, recipient);
    assert_eq!(inspected.queue_source.as_deref(), Some(mirror_url.as_str()));
    assert_eq!(
        inspected.queue_server,
        Some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
    );
    assert!(inspected.queue_version.is_some());

    let approved =
        auto_approve(&paths).expect("the verified policy mirror still serves the request");
    assert_eq!(approved.approved, ["newcomer"]);
    assert!(
        admin_open(&paths)
            .unwrap()
            .doc
            .machines()
            .iter()
            .any(|machine| machine.recipient == recipient)
    );
    assert!(
        request_members(&paths, &config(&paths).unwrap().unwrap(), None)
            .unwrap()
            .is_none_or(|members| !members.contains(&recipient)),
        "approval has not completed membership enrollment"
    );
    let inspected = inspect_requests_until(&paths, Instant::now() + aterm_spec::wait::MUST_HAPPEN)
        .expect("bounded diagnostics can read the locally applied approval");
    assert!(inspected.requests.is_empty());
    mirror.stop();
}

#[test]
fn a_healthy_primary_does_not_hide_a_request_retained_only_by_a_mirror() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), true);
    let (recipient, line) = request(root.path(), "mirror-only");
    let line = proved(&paths, &recipient, "mirror-only", &line);
    transport::post_join(&mirror_url, &line).unwrap();
    let Source::Url(primary_url) = config(&paths).unwrap().unwrap().source else {
        unreachable!()
    };
    assert!(transport::list_joins(&primary_url).unwrap().is_empty());

    let inspected = inspect_requests(&paths, None)
        .expect("an empty primary does not identify the nonempty mirror as itself");
    assert_eq!(inspected.requests[0].recipient, recipient);
    assert_eq!(inspected.queue_source.as_deref(), Some(mirror_url.as_str()));
    assert_eq!(
        inspected.queue_server,
        Some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
    );
    assert!(inspected.queue_version.is_some());

    let approved = auto_approve(&paths).unwrap();
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
fn an_admin_service_approval_keeps_its_request_across_queue_turns() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), false);
    let Source::Url(primary_url) = config(&paths).unwrap().unwrap().source else {
        unreachable!()
    };
    let (primary_key, primary_line) = request(root.path(), "primary-only");
    let (mirror_key, mirror_line) = request(root.path(), "mirror-only");
    transport::post_join(&primary_url, &primary_line).unwrap();
    transport::post_join(&mirror_url, &mirror_line).unwrap();
    let admin = admin_open(&paths).unwrap();
    let listed = inspect_requests(&paths, Some(&admin.doc)).unwrap();
    assert_eq!(listed.requests.len(), 1);
    // The next turn starts at the other queue, so the service's first lookup
    // finds this key; its nested approval lookup must not lose it on another turn.
    let selected = if listed.requests[0].recipient == primary_key {
        mirror_key
    } else {
        assert_eq!(listed.requests[0].recipient, mirror_key);
        primary_key
    };
    let request = admin_service::Request {
        code: join_code(&Recipient::parse(&selected).unwrap()),
        nonce: "0123456789abcdef0123456789abcdef".into(),
    };
    let approved = admin_service::approve(&paths, "fixture:operator", &request);
    let after = admin_open(&paths);
    primary.stop();
    mirror.stop();
    let wire = approved.unwrap();
    let after = after.unwrap();
    let receipt = admin_service::verify_for_request(
        &wire,
        &after.config.signer.unwrap(),
        "team",
        "fixture:operator",
        &request,
    )
    .unwrap();
    assert_eq!(receipt["recipient"].as_str(), Some(selected.as_str()));
    assert!(
        after
            .doc
            .machines()
            .iter()
            .any(|machine| machine.recipient == selected)
    );
    assert!(
        !after
            .doc
            .machines()
            .iter()
            .any(|machine| machine.recipient == listed.requests[0].recipient)
    );
}

#[test]
fn a_public_recipient_approval_finds_the_request_after_the_queue_rotates() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), false);
    let Source::Url(primary_url) = config(&paths).unwrap().unwrap().source else {
        unreachable!()
    };
    let (primary_key, primary_line) = request(root.path(), "primary-only");
    let (mirror_key, mirror_line) = request(root.path(), "mirror-only");
    transport::post_join(&primary_url, &primary_line).unwrap();
    transport::post_join(&mirror_url, &mirror_line).unwrap();
    let before = admin_open(&paths).unwrap();
    let listed = inspect_requests(&paths, Some(&before.doc)).unwrap();
    assert_eq!(listed.requests.len(), 1);
    let selected = &listed.requests[0];
    let other = if selected.recipient == primary_key {
        mirror_key
    } else {
        assert_eq!(selected.recipient, mirror_key);
        primary_key
    };

    let approved = approve_request(&paths, &selected.recipient);
    let after = admin_open(&paths);
    primary.stop();
    mirror.stop();

    assert_eq!(approved.unwrap(), selected.host);
    let after = after.unwrap();
    assert_manual_queued_recipient(&after, &selected.recipient, &selected.host);
    assert!(
        after
            .doc
            .machines()
            .iter()
            .all(|machine| machine.recipient != other)
    );
}

fn assert_manual_queued_recipient(admin: &Admin, recipient: &str, host: &str) {
    let matching: Vec<_> = admin
        .doc
        .machines()
        .into_iter()
        .filter(|machine| machine.recipient == recipient)
        .collect();
    assert_eq!(matching.len(), 1, "one logical recipient was approved");
    assert_eq!(matching[0].name, host);
    assert_eq!(
        admin.commons_admission(host).unwrap().method,
        AdmissionMethod::Manual
    );
}

fn assert_approval_left_document_unchanged(before: &Admin, after: &Admin, paths: &Paths) {
    assert_eq!(after.seq, before.seq, "a refused lookup did not publish");
    assert!(
        after.doc.render().as_slice() == before.doc.render().as_slice(),
        "a refused lookup left membership and admission evidence unchanged"
    );
    assert!(read_approved(paths).is_empty());
}

#[test]
fn a_replicated_identity_is_approved_once_and_keeps_both_queue_observations() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), false);
    let Source::Url(primary_url) = config(&paths).unwrap().unwrap().source else {
        unreachable!()
    };
    let (recipient, line) = request(root.path(), "replicated");
    let proof_a =
        join::Key::derive("team", b"first observed proof").prove("team", &recipient, "replicated");
    let proof_b =
        join::Key::derive("team", b"second observed proof").prove("team", &recipient, "replicated");
    transport::post_join(&primary_url, &format!("{line} proof={proof_a}")).unwrap();
    transport::post_join(&mirror_url, &format!("{line} proof={proof_b}")).unwrap();
    let mut lines = transport::list_joins(&primary_url).unwrap();
    lines.extend(transport::list_joins(&mirror_url).unwrap());
    let expected: BTreeSet<_> = requests_in(lines, &[])
        .iter()
        .filter_map(Answered::of)
        .collect();
    assert_eq!(
        expected.len(),
        2,
        "both observed proof variants are distinct"
    );

    let approved = approve_request(&paths, &recipient);
    let after = admin_open(&paths);
    let recorded = read_approved(&paths);
    primary.stop();
    mirror.stop();

    assert_eq!(approved.unwrap(), "replicated");
    assert_manual_queued_recipient(&after.unwrap(), &recipient, "replicated");
    assert_eq!(
        recorded, expected,
        "neither queue observation was discarded"
    );
}

#[test]
fn a_public_recipient_approval_refuses_conflicting_names_across_queues() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), false);
    let Source::Url(primary_url) = config(&paths).unwrap().unwrap().source else {
        unreachable!()
    };
    let (recipient, line) = request(root.path(), "first-name");
    let code = join_code(&Recipient::parse(&recipient).unwrap());
    transport::post_join(&primary_url, &line).unwrap();
    transport::post_join(&mirror_url, &format!("{recipient} second-name {code}")).unwrap();
    let before = admin_open(&paths).unwrap();

    let approved = approve_request(&paths, &recipient);
    let after = admin_open(&paths);
    primary.stop();
    mirror.stop();

    assert!(approved.is_err(), "a queued key had two distinct names");
    assert_approval_left_document_unchanged(&before, &after.unwrap(), &paths);
}

#[test]
fn a_public_recipient_approval_rejects_a_foreign_signer_mirror() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, _) = mirror_vault(root.path(), false);
    let foreign = server(&root.path().join("foreign-approval"));
    let foreign_url = format!("http://{}/v1/team", foreign.addr);
    let foreign_paths = Paths::for_home(&root.path().join("foreign-approval-admin"));
    create(&foreign_paths, &foreign_url, None, Some("foreign")).unwrap();
    let admin = admin_open(&paths).unwrap();
    assert_ne!(
        config(&foreign_paths).unwrap().unwrap().signer,
        admin.config.signer,
        "the policy destination holds a different signed vault"
    );
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_POLICY_ENTRY,
        crate::swarm_roster::SwarmPolicy {
            servers: vec![foreign_url.clone()],
            ..Default::default()
        }
        .to_toml()
        .as_bytes(),
    )
    .unwrap();
    admin_publish(&paths, &admin, &doc).unwrap();
    let (recipient, line) = request(root.path(), "foreign-only");
    transport::post_join(&foreign_url, &line).unwrap();
    assert_eq!(transport::list_joins(&foreign_url).unwrap().len(), 1);
    let before = admin_open(&paths).unwrap();

    let approved = approve_request(&paths, &recipient);
    let after = admin_open(&paths);
    primary.stop();
    mirror.stop();
    foreign.stop();

    assert!(
        approved.is_err(),
        "an untrusted queue cannot identify an approval"
    );
    assert_approval_left_document_unchanged(&before, &after.unwrap(), &paths);
}

#[test]
fn a_public_recipient_approval_uses_a_found_request_with_an_unavailable_mirror() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, _) = mirror_vault(root.path(), false);
    let Source::Url(primary_url) = config(&paths).unwrap().unwrap().source else {
        unreachable!()
    };
    let (recipient, line) = request(root.path(), "available-target");
    transport::post_join(&primary_url, &line).unwrap();
    mirror.stop();

    let approved = approve_request(&paths, &recipient);
    let after = admin_open(&paths);
    primary.stop();

    assert_eq!(approved.unwrap(), "available-target");
    assert_manual_queued_recipient(&after.unwrap(), &recipient, "available-target");
}

#[test]
fn approving_with_an_unavailable_queue_preserves_earlier_manual_answers() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), false);
    let Source::Url(primary_url) = config(&paths).unwrap().unwrap().source else {
        unreachable!()
    };
    let (first, first_line) = request(root.path(), "earlier-mirror-request");
    transport::post_join(&mirror_url, &first_line).unwrap();
    assert_eq!(
        approve_request(&paths, &first).unwrap(),
        "earlier-mirror-request"
    );
    let mut expected = read_approved(&paths);
    assert_eq!(expected.len(), 1, "the earlier approval is recorded");
    mirror.stop();

    let (second, second_line) = request(root.path(), "later-primary-request");
    transport::post_join(&primary_url, &second_line).unwrap();
    expected.extend(
        requests_in(transport::list_joins(&primary_url).unwrap(), &[])
            .iter()
            .filter_map(Answered::of),
    );
    assert_eq!(expected.len(), 2);
    let approved = approve_request(&paths, &second);
    let after = admin_open(&paths);
    let recorded = read_approved(&paths);
    primary.stop();

    assert_eq!(approved.unwrap(), "later-primary-request");
    let after = after.unwrap();
    assert_manual_queued_recipient(&after, &first, "earlier-mirror-request");
    assert_manual_queued_recipient(&after, &second, "later-primary-request");
    assert_eq!(
        recorded, expected,
        "an unobserved queue cannot withdraw an earlier manual answer"
    );
}

#[test]
fn a_missing_recipient_with_an_unavailable_queue_is_not_approved() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, _) = mirror_vault(root.path(), false);
    let Source::Url(primary_url) = config(&paths).unwrap().unwrap().source else {
        unreachable!()
    };
    let (_, other_line) = request(root.path(), "another-request");
    let (recipient, _) = request(root.path(), "unseen-target");
    transport::post_join(&primary_url, &other_line).unwrap();
    let before = admin_open(&paths).unwrap();
    mirror.stop();

    let approved = approve_request(&paths, &recipient);
    let after = admin_open(&paths);
    primary.stop();

    assert_eq!(
        approved.unwrap_err().kind(),
        crate::ErrorKind::JoinQueueIncomplete
    );
    assert_approval_left_document_unchanged(&before, &after.unwrap(), &paths);
}

#[test]
fn an_explicit_approval_reaches_a_policy_queue_beyond_the_first_batch() {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = mirror_vault(root.path(), false);
    let mut mirrors = vec![mirror];
    let mut urls = vec![mirror_url];
    for index in 1..=crate::discover::MAX_CANDIDATES {
        let mirror = server(&root.path().join(format!("approval-mirror-{index}")));
        urls.push(format!("http://{}/v1/team", mirror.addr));
        mirrors.push(mirror);
    }
    assert!(urls.len() > crate::discover::MAX_CANDIDATES);
    let target_url = urls.last().unwrap().clone();
    let admin = admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        crate::SWARM_POLICY_ENTRY,
        crate::swarm_roster::SwarmPolicy {
            servers: urls.clone(),
            ..Default::default()
        }
        .to_toml()
        .as_bytes(),
    )
    .unwrap();
    admin_publish(&paths, &admin, &doc).unwrap();
    let envelope = std::fs::read(paths.cache_file()).unwrap();
    for url in &urls {
        transport::publish(&Source::Url(url.clone()), &envelope).unwrap();
    }
    let Source::Url(primary_url) = config(&paths).unwrap().unwrap().source else {
        unreachable!()
    };
    let (_, other_line) = request(root.path(), "primary-distractor");
    transport::post_join(&primary_url, &other_line).unwrap();
    let (recipient, line) = request(root.path(), "last-policy-queue");
    transport::post_join(&target_url, &line).unwrap();

    let approved = approve_request(&paths, &recipient);
    let after = admin_open(&paths);
    primary.stop();
    for mirror in mirrors {
        mirror.stop();
    }

    assert_eq!(approved.unwrap(), "last-policy-queue");
    assert_manual_queued_recipient(&after.unwrap(), &recipient, "last-policy-queue");
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
