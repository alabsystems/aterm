// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A vault command posts records to every server it reaches at once ([`fan_out`]): once its
//! version has landed, servers that take a post and never read it hold the command for one
//! exchange, not one each, and every post after a publish lands shares that one exchange.
//!
//! On 2026-11-02 `aterm secrets approve` went on for minutes after the server applied its
//! version: one 120 s records post after another, each to the server of a Mac whose aterm had
//! wedged (it took the connection and never read the 130 KB sent). Most tests here stand up
//! servers that hold a records post, or an envelope read, until all of them hold one: one after
//! another, the first holds its request until the hang detector and the rest hear nothing, so
//! the test fails; at once, every one of them holds one and the test passes at once.

use super::log_entries_tests::two_macs;
use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

/// How many held servers each test stands up: the Macs whose aterm had wedged at once.
const HELD: usize = 8;

/// The requests [`HeldServers`] hold while a hold is on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Holds {
    /// `POST <url>/records`.
    RecordsPosts,
    /// `GET <url>`, the envelope.
    EnvelopeReads,
}

/// What the held servers saw.
#[derive(Default)]
struct Held {
    /// Whether a request is held now until [`HeldServers::release`].
    holding: bool,
    /// The servers have been dropped: nothing is held any longer.
    ended: bool,
    /// The servers holding a request now.
    holding_requests: BTreeSet<usize>,
    /// How many records posts reached the servers, held or not.
    records_posts: usize,
    /// How many envelope PUTs reached the servers.
    puts: usize,
}

struct Shared {
    held: Mutex<Held>,
    changed: Condvar,
    /// Whether a records pull is answered with `X-Aterm-Records-Need: 1`.
    need: bool,
    /// What a hold holds.
    holds: Holds,
}

impl Shared {
    fn held(&self) -> std::sync::MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hold server `index`'s request while a hold is on.
    fn hold_request(&self, index: usize) {
        let mut held = self.held();
        if !held.holding || held.ended {
            return;
        }
        held.holding_requests.insert(index);
        self.changed.notify_all();
        let mut held = self
            .changed
            .wait_while(held, |held| held.holding && !held.ended)
            .unwrap_or_else(PoisonError::into_inner);
        held.holding_requests.remove(&index);
    }
}

/// Loopback servers of the vault `alab` that answer every request at once (a records pull with a
/// 404, or with a 204 saying they lack the asker's frontier where `need`; a records post with a
/// 200, the post judged; any other read with a 404; any other write, an envelope PUT among them,
/// with a 503) but the requests a hold holds ([`Holds`]) while [`HeldServers::hold`] is on: one
/// of those each takes and reads none of until [`HeldServers::release`], and then answers as
/// any other.
struct HeldServers {
    urls: Vec<String>,
    shared: Arc<Shared>,
    listeners: Vec<std::thread::JoinHandle<()>>,
}

impl HeldServers {
    /// `count` servers whose hold holds records posts.
    fn start(count: usize, need: bool) -> Self {
        Self::holding(count, need, Holds::RecordsPosts)
    }

    /// `count` servers whose hold holds `holds`.
    fn holding(count: usize, need: bool, holds: Holds) -> Self {
        let shared = Arc::new(Shared {
            held: Mutex::new(Held::default()),
            changed: Condvar::new(),
            need,
            holds,
        });
        let mut urls = Vec::new();
        let mut listeners = Vec::new();
        for index in 0..count {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            urls.push(format!(
                "http://{}/v1/alab",
                listener.local_addr().expect("addr")
            ));
            let shared = Arc::clone(&shared);
            listeners.push(std::thread::spawn(move || {
                loop {
                    let stream = aterm_uds::accept(
                        &listener,
                        aterm_uds::AcceptWait::Forever,
                        aterm_uds::AcceptedMode::Blocking,
                    )
                    .map(|(stream, _)| stream);
                    if shared.held().ended {
                        return;
                    }
                    if let Ok(stream) = stream {
                        let shared = Arc::clone(&shared);
                        std::thread::spawn(move || answer(&shared, index, stream));
                    }
                }
            }));
        }
        Self {
            urls,
            shared,
            listeners,
        }
    }

    /// Hold every request a hold holds from now until [`Self::release`].
    fn hold(&self) {
        self.shared.held().holding = true;
    }

    /// Answer the requests held and every one after.
    fn release(&self) {
        self.shared.held().holding = false;
        self.shared.changed.notify_all();
    }

    /// Wait until every server holds a request at once; the servers that held one when the hang
    /// detector ended the wait, if not.
    fn all_holding(&self) -> Result<(), BTreeSet<usize>> {
        let (held, waited) = self
            .shared
            .changed
            .wait_timeout_while(self.shared.held(), aterm_spec::wait::MUST_HAPPEN, |held| {
                held.holding_requests.len() < self.urls.len()
            })
            .unwrap_or_else(PoisonError::into_inner);
        if waited.timed_out() {
            Err(held.holding_requests.clone())
        } else {
            Ok(())
        }
    }
}

impl Drop for HeldServers {
    fn drop(&mut self) {
        {
            let mut held = self.shared.held();
            held.holding = false;
            held.ended = true;
        }
        self.shared.changed.notify_all();
        for url in &self.urls {
            // Wakes a listener, which then ends.
            let _ = std::net::TcpStream::connect(
                url.trim_start_matches("http://")
                    .trim_end_matches("/v1/alab"),
            );
        }
        for listener in self.listeners.drain(..) {
            let _ = listener.join();
        }
    }
}

/// One request to held server `index`, answered as [`HeldServers`] says.
fn answer(shared: &Shared, index: usize, stream: std::net::TcpStream) {
    let _ = stream.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN));
    let Ok(read) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut words = line.split_whitespace();
    let method = words.next().unwrap_or_default().to_owned();
    let target = words.next().unwrap_or_default().to_owned();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        match reader.read_line(&mut header) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let path = target.split('?').next().unwrap_or_default();
    let records = path.ends_with("/records");
    let request = match (method.as_str(), records) {
        ("POST", true) => Some(Holds::RecordsPosts),
        ("GET", false) if path.ends_with("/v1/alab") => Some(Holds::EnvelopeReads),
        _ => None,
    };
    {
        let mut held = shared.held();
        if request == Some(Holds::RecordsPosts) {
            held.records_posts += 1;
        }
        if method == "PUT" {
            held.puts += 1;
        }
    }
    if request == Some(shared.holds) {
        shared.hold_request(index);
    }
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let reply: &[u8] = match (method.as_str(), records) {
        ("POST", true) => b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        ("GET", true) if shared.need => {
            b"HTTP/1.1 204 No Content\r\nX-Aterm-Records-Need: 1\r\nConnection: close\r\n\r\n"
        }
        ("GET", _) => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        _ => b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    };
    let mut stream = stream;
    let _ = stream.write_all(reply);
}

/// A publish whose version a server stored keeps its records and posts them to every candidate
/// server at once (`log_entries::keep_drafted_until`), the CLI's path (`publish_as` with no
/// deadline): servers that take the post and never answer hold the command for one exchange.
///
/// NEGATIVE CONTROL: post to one server after another in `keep_drafted_until` (`for url in urls
/// { let _ = transport::post_records(url, &wire); }`, main before this fix): the first held server
/// holds its post until the hang detector, no other hears one, and the test fails "never: every
/// server held its records post at once".
#[test]
fn a_landed_publish_posts_its_records_to_every_server_at_once() {
    let macs = two_macs(&[(Audience::Everyone, "gh-token", b"token-v1".as_slice())]);
    let servers = HeldServers::start(HELD, false);
    servers.hold();
    let publisher = std::thread::spawn({
        let m22 = macs.m22.clone();
        let urls = servers.urls.clone();
        move || {
            let admin = admin_open(&m22).expect("open");
            let mut doc = admin.doc.clone();
            doc.set_entry(&Audience::Everyone, "gh-token", b"token-v2")
                .expect("set");
            admin_publish_with_candidates(&m22, &admin, &doc, &urls)
        }
    });
    let at_once = servers.all_holding();
    servers.release();
    let published = publisher
        .join()
        .expect("the publish ran")
        .expect("the source took the publish");
    assert_eq!(
        at_once,
        Ok(()),
        "never: every server held its records post at once"
    );
    assert_eq!(macs.server.held_seq("alab"), Some(published.seq));
    drop(servers);
    macs.server.stop();
    drop(macs.root);
}

/// A records pass posts the store to every server that said it lacks this Mac's frontier, all at
/// once: the pass a publish runs once its version landed (`sync_without_reconcile`) and every
/// subscriber pass.
///
/// NEGATIVE CONTROL: post to them one after another in `sync_records_until` (`for url in
/// &push_urls { … transport::post_records_answered(url, body) … }`, main before this fix): fails
/// "never: every server lacking the frontier held its post at once".
#[test]
fn a_records_pass_posts_to_every_server_lacking_its_frontier_at_once() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let servers = HeldServers::start(HELD, true);
    servers.hold();
    let pass = std::thread::spawn({
        let m22 = macs.m22.clone();
        let urls = servers.urls.clone();
        move || {
            // Every server this pass reads is one on loopback that answers.
            let _answering = ServersAnswering::set();
            sync_records_inner(&m22, None, &urls, None, None).map(drop)
        }
    });
    let at_once = servers.all_holding();
    servers.release();
    let _ = pass.join().expect("the pass ran");
    assert_eq!(
        at_once,
        Ok(()),
        "never: every server lacking the frontier held its post at once"
    );
    drop(servers);
    macs.server.stop();
    drop(macs.root);
}

/// [`post_records_to_servers`], which a removal, an undo, a closed wrap gap and a tended epoch
/// run (the last two inside the pass a landed publish runs), posts to the source and every
/// policy server at once, and counts each that took the records.
///
/// NEGATIVE CONTROL: post to one server after another in `post_records_to_servers` (main before
/// this fix): fails "never: every policy server held its post at once".
#[test]
fn posting_the_records_reaches_every_server_at_once() {
    let servers = HeldServers::start(HELD, false);
    let policy = crate::swarm_roster::SwarmPolicy {
        servers: servers.urls.clone(),
        ..crate::swarm_roster::SwarmPolicy::default()
    };
    let toml = policy.to_toml();
    let macs = two_macs(&[(
        Audience::Everyone,
        crate::SWARM_POLICY_ENTRY,
        toml.as_bytes(),
    )]);
    servers.hold();
    let posting = std::thread::spawn({
        let m22 = macs.m22.clone();
        move || post_records_to_servers(&m22)
    });
    let at_once = servers.all_holding();
    servers.release();
    let posted = posting
        .join()
        .expect("the post ran")
        .expect("a server took the records");
    assert_eq!(
        at_once,
        Ok(()),
        "never: every policy server held its post at once"
    );
    assert_eq!(
        posted,
        HELD + 1,
        "the source and every policy server took the records"
    );
    drop(servers);
    macs.server.stop();
    drop(macs.root);
}

/// Every records post that follows a landed publish ends by one deadline, set as the version
/// lands: the store's post to every server (`log_entries::keep_drafted_until`) and the push of the
/// pass that ends the publish to each server that said it lacks the frontier
/// (`sync_without_reconcile`). So however many rounds of posts follow, servers that never answer
/// hold the command for that one exchange. With the deadline already past as the version lands,
/// the publish still stores its version and PUTs it to every server, and posts records to none.
///
/// NEGATIVE CONTROL: give either round its own deadline (`None` in place of `Some(posts_by)` at
/// `publish_as_with`'s `keep_drafted_until` or `sync_without_reconcile`): servers are posted
/// records, and the test fails "a records post past the publish's one deadline went out".
#[test]
fn every_records_post_after_a_landed_publish_shares_one_deadline() {
    let macs = two_macs(&[(Audience::Everyone, "gh-token", b"token-v1".as_slice())]);
    let servers = HeldServers::start(HELD, true);
    let admin = admin_open(&macs.m22).expect("open");
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "gh-token", b"token-v2")
        .expect("set");
    let published = {
        // Every server the pass reads is one on loopback that answers.
        let _answering = ServersAnswering::set();
        LANDED_POSTS_BY.set(Some(Instant::now()));
        admin_publish_with_candidates(&macs.m22, &admin, &doc, &servers.urls)
            .expect("the source took the publish")
    };
    assert_eq!(macs.server.held_seq("alab"), Some(published.seq));
    {
        let held = servers.shared.held();
        assert_eq!(
            held.puts, HELD,
            "the publish PUT its version to every server"
        );
        assert_eq!(
            held.records_posts, 0,
            "a records post past the publish's one deadline went out"
        );
    }
    drop(servers);
    macs.server.stop();
    drop(macs.root);
}

/// The store goes to every server that answered the publish's PUT, or failed it fast, and not to
/// one that let the PUT run out its deadline: such a server answered nothing in an exchange (a
/// Mac whose aterm hung took the connection and read nothing), and would most likely hold a
/// records post the same way. It may still have stored the envelope, read after the deadline;
/// it takes the records from a later records pass that hears it lack the frontier.
#[test]
fn a_server_whose_put_ran_out_is_not_posted_the_store() {
    let outcomes = [
        ("http://stored".to_owned(), Ok(())),
        (
            "http://hung".to_owned(),
            Err(Error::of_kind(crate::ErrorKind::TimedOut, "ran out")),
        ),
        (
            "http://raced".to_owned(),
            Err(Error::of_kind(crate::ErrorKind::NotStored, "409")),
        ),
        (
            "http://down".to_owned(),
            Err(Error::new("connection refused")),
        ),
    ];
    assert_eq!(
        answered_puts(&outcomes),
        ["http://stored", "http://raced", "http://down"]
    );
}

/// An admin command opens the vault reading every candidate server past the source at once
/// ([`admin_open_with_candidates`]): servers that take the read and never answer hold it for
/// one probe deadline, not one each, before it publishes.
///
/// NEGATIVE CONTROL: read the candidates one after another (`for url in candidates {
/// transport::fetch_within_as(&url, None, servers_within(PROBE_DEADLINE), reader) … }`, main
/// before this fix): fails "never: every candidate held its read at once".
#[test]
fn an_admin_open_reads_every_candidate_at_once() {
    let macs = two_macs(&[]);
    let servers = HeldServers::holding(HELD, false, Holds::EnvelopeReads);
    servers.hold();
    let opening = std::thread::spawn({
        let m22 = macs.m22.clone();
        let urls = servers.urls.clone();
        move || {
            // Every candidate this open reads is one on loopback that answers.
            let _answering = ServersAnswering::set();
            admin_open_with_candidates(&m22, &urls).map(|admin| admin.seq())
        }
    });
    let at_once = servers.all_holding();
    servers.release();
    let opened = opening
        .join()
        .expect("the open ran")
        .expect("the source served the vault");
    assert_eq!(
        at_once,
        Ok(()),
        "never: every candidate held its read at once"
    );
    assert_eq!(macs.server.held_seq("alab"), Some(opened));
    drop(servers);
    macs.server.stop();
    drop(macs.root);
}

/// The servers' heads a mirror merges are read from every candidate at once
/// ([`ledger_publish::read_server_heads`]), each kept in the candidates' order.
///
/// NEGATIVE CONTROL: read them one after another (`for url in candidate_server_urls(..) {
/// transport::fetch_within_as(&url, None, servers_within(SERVER_HEAD_DEADLINE), ..) … }`, main
/// before this fix): fails "never: every server held its head read at once".
#[test]
fn the_server_heads_are_read_from_every_candidate_at_once() {
    let macs = two_macs(&[]);
    let servers = HeldServers::holding(HELD, false, Holds::EnvelopeReads);
    servers.hold();
    let reading = std::thread::spawn({
        let m22 = macs.m22.clone();
        let urls = servers.urls.clone();
        move || {
            let _answering = ServersAnswering::set();
            ledger_publish::read_server_heads(&m22, &urls)
        }
    });
    let at_once = servers.all_holding();
    servers.release();
    let heads = reading
        .join()
        .expect("the read ran")
        .expect("the vault is configured");
    assert_eq!(
        at_once,
        Ok(()),
        "never: every server held its head read at once"
    );
    let seqs: Vec<u64> = heads.iter().map(|head| head.envelope.seq).collect();
    assert_eq!(
        seqs,
        vec![
            macs.server
                .held_seq("alab")
                .expect("the source holds the vault")
        ],
        "the source's head alone: the held servers serve none"
    );
    drop(servers);
    macs.server.stop();
    drop(macs.root);
}

/// A Mac's mirrors are read and never posted records ([`mirrors::ReadOnly`]): a mirror lacking
/// the frontier takes what it lacks from the vault's servers, never from a member. The same
/// servers named as the pass's own peers are servers, and are posted what they lack.
///
/// NEGATIVE CONTROL: post to every peer that lacks the frontier (`let post = needs;` in
/// `sync_records_until`): fails "a mirror was posted records".
#[test]
fn a_mirror_lacking_the_frontier_is_never_posted_records() {
    let macs = two_macs(&[(Audience::Everyone, "shared", b"v1".as_slice())]);
    let servers = HeldServers::start(2, true);
    let config = configured(&macs.m22).unwrap();
    let config = set_mirrors(&macs.m22, &config, &servers.urls).expect("listed");
    assert_eq!(config.mirrors, servers.urls);
    {
        // Every server this pass reads is one on loopback that answers.
        let _answering = ServersAnswering::set();
        sync_records_inner(&macs.m22, None, &[], None, None).expect("the pass");
    }
    assert_eq!(
        servers.shared.held().records_posts,
        0,
        "a mirror was posted records"
    );
    {
        let _answering = ServersAnswering::set();
        sync_records_inner(&macs.m22, None, &servers.urls, None, None).expect("the pass");
    }
    assert!(
        servers.shared.held().records_posts > 0,
        "the pass's own peers are posted what they lack"
    );
    drop(servers);
    macs.server.stop();
    drop(macs.root);
}
