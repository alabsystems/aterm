// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Requests on real independent queues, with signed vault identity checks.

use super::*;

/// A queue on the home ALab LAN, where members answered: the default ALab policy reads its
/// requests as a home server's.
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

fn create_vault(root: &Path, server: &crate::server::Running) -> (Paths, Config) {
    let paths = Paths::for_home(root);
    let url = format!("http://{}/v1/team", server.addr);
    create(&paths, &url, None, Some("source")).unwrap();
    let config = configured(&paths).unwrap();
    (paths, config)
}

fn request(root: &Path) -> String {
    let paths = Paths::for_home(root);
    let identity = machine_identity(&paths).unwrap();
    let recipient = identity.recipient();
    format!(
        "{} pending {}",
        recipient.to_bech32(),
        join_code(&recipient)
    )
}

fn queued(url: &str, line: &str) -> bool {
    let recipient = line.split_whitespace().next().unwrap();
    transport::list_joins(url)
        .unwrap()
        .iter()
        .any(|queued| queued.split_whitespace().next() == Some(recipient))
}

#[test]
fn a_join_reaches_the_verified_mirror_even_when_the_configured_queue_accepted_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let primary = server(&root.path().join("primary"));
    let mirror = server(&root.path().join("mirror"));
    let foreign = server(&root.path().join("foreign"));
    let (admin, mut config) = create_vault(&root.path().join("admin"), &mirror);
    let (_, foreign_config) = create_vault(&root.path().join("foreign-admin"), &foreign);
    let mirror_url = format!("http://{}/v1/team", mirror.addr);
    config.source = Source::Url(format!("http://{}/v1/team", primary.addr));
    transport::publish(&config.source, &std::fs::read(admin.cache_file()).unwrap()).unwrap();
    let Source::Url(primary_url) = &config.source else {
        unreachable!()
    };
    let Source::Url(foreign_url) = &foreign_config.source else {
        unreachable!()
    };
    let line = request(&root.path().join("joining"));
    post_discovering(&config, &line, None, |_| {
        vec![
            primary_url.clone(),
            mirror_url.clone(),
            foreign_url.clone(),
            mirror_url.clone(),
        ]
    })
    .unwrap();
    assert!(queued(primary_url, &line));
    assert!(
        queued(&mirror_url, &line),
        "the approver's independent queue received the request"
    );
    assert!(
        !queued(foreign_url, &line),
        "a different signed vault receives nothing"
    );
    assert_eq!(transport::list_joins(&mirror_url).unwrap().len(), 1);
    let approved = test_auto_approve_alab_lan(&admin).unwrap();
    assert_eq!(approved.approved, ["pending"]);
    primary.stop();
    mirror.stop();
    foreign.stop();
}

/// A listing names the address of the server that answered it, which an approver judges the
/// listing's `home=` words by (`home_lan::Place::serving`): here the loopback server, so this
/// Mac itself.
///
/// NEGATIVE CONTROL: with `PeerNoting::connect` keeping no address, the listing names none and
/// this test fails.
#[test]
fn a_listing_names_the_server_that_answered_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let queue = server(&root.path().join("queue"));
    let (paths, config) = create_vault(&root.path().join("admin"), &queue);
    let line = request(&root.path().join("joining"));
    post_discovering(&config, &line, None, |_| Vec::new()).unwrap();
    let (pending, from, _) = requests_from(&paths, &config).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(from, Some(std::net::Ipv4Addr::LOCALHOST.into()));
    let Source::Url(url) = &config.source else {
        unreachable!()
    };
    let (joins, from) = transport::wait_joins_from(url, None, Duration::from_secs(5)).unwrap();
    assert!(matches!(joins, transport::Joins::Listed(lines, _) if lines.len() == 1));
    assert_eq!(from, Some(std::net::Ipv4Addr::LOCALHOST.into()));
    queue.stop();
}

#[test]
fn a_verified_mirror_can_accept_when_the_configured_queue_is_unavailable() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mirror = server(&root.path().join("mirror"));
    let (_, mut config) = create_vault(&root.path().join("admin"), &mirror);
    let Source::Url(mirror_url) = config.source.clone() else {
        unreachable!()
    };
    let absent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    config.source = Source::Url(format!("http://{}/v1/team", absent.local_addr().unwrap()));
    drop(absent);
    let line = request(&root.path().join("joining"));
    post_discovering(
        &config,
        &line,
        Some(Instant::now() + Duration::from_secs(60)),
        |_| vec![mirror_url.clone()],
    )
    .unwrap();
    assert!(queued(&mirror_url, &line));
    mirror.stop();
}

#[test]
fn proofs_and_member_allows_never_leave_the_configured_route() {
    let root = aterm_tempfile::tempdir().unwrap();
    let primary = server(&root.path().join("primary"));
    let (_, config) = create_vault(&root.path().join("admin"), &primary);
    let base = request(&root.path().join("joining"));
    for suffix in [
        format!(" proof={}", crate::b64::encode_padded(&[7; 32])),
        " allowed=YWJj".into(),
    ] {
        let line = format!("{base}{suffix}");
        assert!(transport::parse_join_line(&line).is_ok());
        assert!(!public_request(&line));
        post_discovering(&config, &line, None, |_| {
            panic!("private admission evidence must not start mirror discovery")
        })
        .unwrap();
    }
    assert!(
        !public_alab_request(&config, &base),
        "an ordinary vault keeps its route"
    );
    primary.stop();
}
