// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Requests on real independent queues, with signed vault identity checks.

use super::*;

/// A queue server whose join requests may change at once.
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
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
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
    let recipient = line.split_whitespace().next().unwrap();
    assert_eq!(approve_request(&admin, recipient).unwrap(), "pending");
    primary.stop();
    mirror.stop();
    foreign.stop();
}

/// A listing names the address of the server that answered it, which an approver continues its
/// long-poll from and `aterm secrets requests --json` names: here the loopback server, so this
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
    let inspected = inspect_requests(&paths, None).unwrap();
    assert_eq!(inspected.requests.len(), 1);
    assert_eq!(
        inspected.queue_server,
        Some(std::net::Ipv4Addr::LOCALHOST.into())
    );
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

/// What a mirror serves that does not verify as the vault, each a different way: another vault,
/// something that is no vault, no vault at all, and an answer no vault server gives.
fn not_the_vault(foreign: &[u8]) -> [transport::Answer; 4] {
    use transport::Answer;
    [
        Answer::Fetched(Fetched::Body(foreign.to_vec())),
        Answer::Fetched(Fetched::Body(b"no envelope".to_vec())),
        Answer::Fetched(Fetched::Missing),
        Answer::Foreign("a redirect".to_owned()),
    ]
}

/// The URL of the mirror a browse named `mirror`th.
fn mirror_url(mirror: usize) -> String {
    format!("http://mirror-{mirror}.local:7484/v1/team")
}

/// A Mac joining posts its public request to every mirror a browse named whose vault verifies,
/// and to none whose vault does not, while it reads at most [`ENVELOPE_PULLS_AT_ONCE`] of their
/// vaults longer than [`ENVELOPE_PEEK`] whole at once, however many it names: each mirror's vault
/// is read first under the peek, all at once, and one longer than that is asked again and read
/// whole under a slot held until that mirror's vault was judged and the request posted to it
/// ([`fan_out::reads_within`]). Before, each mirror was read whole on a thread of its own, so a
/// LAN host that advertised [`MAX_CANDIDATES`] instances of itself, each answering with a 16 MiB
/// body, had a Mac joining the vault, which trusts none of them yet, hold 256 MiB of bodies
/// nothing had verified. Here every mirror's vault is longer than the peek and its whole read held
/// until the test lets it go ([`HeldReads`]), and the slots counted are the ones the shipped join
/// makes. A quarter of the mirrors serve something that is not the vault ([`not_the_vault`]). The
/// configured queue refuses the connection, so the join succeeds only through the mirrors that
/// took the request.
///
/// NEGATIVE CONTROL: `post_verified` reading each mirror whole on a thread of its own, as
/// `post_discovering` did before (`std::thread::scope` spawning, for each mirror, `look(url,
/// left, envelope::MAX_ENVELOPE)`, then `verified` and `post`): more whole reads in flight at
/// once, `left: 16, right: 2` (any count over 2 fails it), "whole reads in flight at once". Its
/// judge posting whether the vault verified or not (`let _ = verified(config, &pin, answer);`):
/// every mirror is posted, "every mirror whose vault verified, and only those, got the request".
///
/// [`ENVELOPE_PULLS_AT_ONCE`]: crate::server::ENVELOPE_PULLS_AT_ONCE
/// [`ENVELOPE_PEEK`]: crate::server::ENVELOPE_PEEK
/// [`MAX_CANDIDATES`]: crate::discover::MAX_CANDIDATES
/// [`HeldReads`]: super::fan_out::held::HeldReads
#[test]
fn a_join_reads_at_most_two_mirrors_whole_at_once_and_posts_each_verified_one() {
    use super::fan_out::held::HeldReads;
    use crate::discover::MAX_CANDIDATES;
    use crate::server::ENVELOPE_PULLS_AT_ONCE;
    let root = aterm_tempfile::tempdir().unwrap();
    let vault_server = server(&root.path().join("vault"));
    let foreign_server = server(&root.path().join("foreign"));
    let (admin, mut config) = create_vault(&root.path().join("admin"), &vault_server);
    let (foreign_admin, _) = create_vault(&root.path().join("foreign-admin"), &foreign_server);
    vault_server.stop();
    foreign_server.stop();
    let vault = std::fs::read(admin.cache_file()).unwrap();
    let foreign = std::fs::read(foreign_admin.cache_file()).unwrap();
    // Nothing listens on the discard port: the configured queue refuses the connection.
    config.source = Source::Url("http://127.0.0.1:9/v1/team".to_owned());
    let Source::Url(primary) = config.source.clone() else {
        unreachable!()
    };
    let line = request(&root.path().join("joining"));
    // Each mirror, what it serves, and whether that is the vault: every fourth one is not.
    let wrong = not_the_vault(&foreign);
    let mirrors: Vec<(String, transport::Answer, bool)> = (0..MAX_CANDIDATES)
        .map(|mirror| match mirror % 4 {
            3 => (mirror_url(mirror), wrong[mirror / 4].clone(), false),
            _ => (
                mirror_url(mirror),
                transport::Answer::Fetched(Fetched::Body(vault.clone())),
                true,
            ),
        })
        .collect();
    let held = HeldReads::new(super::fan_out::ENVELOPES);
    let look = |url: &str, _: Duration, limit: usize| {
        let (_, answer, _) = mirrors
            .iter()
            .find(|(mirror, _, _)| mirror == url)
            .expect("a mirror");
        held.read(limit, answer.clone())
    };
    let posted = std::sync::Mutex::new(Vec::new());
    let post = |url: &str, posted_line: &str, _: Instant| {
        posted
            .lock()
            .expect("the posts")
            .push((url.to_owned(), posted_line.to_owned()));
        Ok(())
    };
    // The browse names the configured queue and one mirror twice besides: neither is read again.
    let discover = |_: Instant| {
        std::iter::once(primary.clone())
            .chain(mirrors.iter().map(|(url, _, _)| url.clone()))
            .chain(std::iter::once(mirror_url(0)))
            .collect()
    };
    let counted = held.count(MAX_CANDIDATES, || {
        post_discovering_through(&config, &line, None, discover, look, post)
    });
    assert_eq!(
        counted.seen.in_flight, ENVELOPE_PULLS_AT_ONCE,
        "whole reads in flight at once"
    );
    assert_eq!(
        counted.seen.waiting,
        MAX_CANDIDATES - ENVELOPE_PULLS_AT_ONCE,
        "whole reads waiting for a slot"
    );
    assert_eq!(counted.most, ENVELOPE_PULLS_AT_ONCE);
    assert_eq!(
        held.peeks.load(std::sync::atomic::Ordering::SeqCst),
        MAX_CANDIDATES,
        "every mirror's vault was peeked once"
    );
    assert!(
        counted.result.is_ok(),
        "the mirrors took the request the configured queue refused: {:?}",
        counted.result
    );
    let mut posted = posted.into_inner().expect("the posts");
    posted.sort();
    let mut verified: Vec<(String, String)> = mirrors
        .iter()
        .filter(|(_, _, verifies)| *verifies)
        .map(|(url, _, _)| (url.clone(), line.clone()))
        .collect();
    verified.sort();
    assert_eq!(verified.len(), MAX_CANDIDATES - wrong.len());
    assert_eq!(
        posted, verified,
        "every mirror whose vault verified, and only those, got the request"
    );
}

/// Where no mirror's vault verifies, a join gives what the configured queue answered, its error
/// as the post gave it, and posts to no mirror: a mirror's answer, read under the peek or read
/// whole past it, counts only once the request was posted to a mirror that proved the vault.
///
/// NEGATIVE CONTROL: `post_verified` counting a mirror that answered at all as one that took the
/// request (`.any(|posted| posted.is_some())`): the join succeeds where the only queue that could
/// take it refused it, "the configured queue's answer".
#[test]
fn a_join_no_verified_mirror_took_gives_the_configured_queues_answer() {
    let root = aterm_tempfile::tempdir().unwrap();
    let vault_server = server(&root.path().join("vault"));
    let foreign_server = server(&root.path().join("foreign"));
    let (_, mut config) = create_vault(&root.path().join("admin"), &vault_server);
    let (foreign_admin, _) = create_vault(&root.path().join("foreign-admin"), &foreign_server);
    vault_server.stop();
    foreign_server.stop();
    let foreign = std::fs::read(foreign_admin.cache_file()).unwrap();
    // Nothing listens on the discard port: the configured queue refuses the connection.
    config.source = Source::Url("http://127.0.0.1:9/v1/team".to_owned());
    let Source::Url(primary) = config.source.clone() else {
        unreachable!()
    };
    let line = request(&root.path().join("joining"));
    // Each way of not being the vault twice: once read under the peek, once whole past it.
    let mirrors: Vec<(String, transport::Answer)> = not_the_vault(&foreign)
        .into_iter()
        .flat_map(|answer| [answer.clone(), answer])
        .enumerate()
        .map(|(mirror, answer)| (mirror_url(mirror), answer))
        .collect();
    let look = |url: &str, _: Duration, limit: usize| -> Looked {
        let (mirror, (_, answer)) = mirrors
            .iter()
            .enumerate()
            .find(|(_, (mirror, _))| mirror == url)
            .expect("a mirror");
        if mirror % 2 == 1 && limit == crate::server::ENVELOPE_PEEK {
            return Ok(transport::Pulled::Larger);
        }
        Ok(transport::Pulled::Fetched(answer.clone()))
    };
    let posted = std::sync::Mutex::new(Vec::new());
    let post = |url: &str, _: &str, _: Instant| {
        posted.lock().expect("the posts").push(url.to_owned());
        Ok(())
    };
    let joined = post_discovering_through(
        &config,
        &line,
        None,
        |_| mirrors.iter().map(|(url, _)| url.clone()).collect(),
        look,
        post,
    );
    let configured = transport::post_join(&primary, &line);
    assert!(configured.is_err(), "the configured queue refuses it");
    assert_eq!(joined, configured, "the configured queue's answer");
    assert!(
        posted.into_inner().expect("the posts").is_empty(),
        "no mirror got the request"
    );
}
