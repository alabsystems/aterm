// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A member's record passes against a records server a test writes (`docs/DESIGN-fluid-swarm-
//! 2026-10-28.md` §4.1 F13).

use super::fan_out::held::HeldReads;
use super::*;
use crate::membership_log::untaken::chain::{Chain, carries, have_named};
use crate::membership_log::{
    Body, Draft, Entrant, Evidence, Log, MemberId, Record, RecordId, decode_records,
    encode_records, with_kind_code,
};
use crate::server::RECORDS_PULLS_AT_ONCE;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

const HANG_DETECTOR: Duration = aterm_spec::wait::MUST_HAPPEN;

/// What a [`RecordsHost`] answers a `GET` of `/records` naming `have`: the records it sends and
/// whether it says `X-Aterm-Records-Need: 1`.
type Answer = dyn Fn(&[RecordId]) -> (Vec<u8>, bool) + Send + Sync;

/// What a [`RecordsHost`] answers a `POST` of `/records` carrying a body: its status.
type Posted = dyn Fn(&[u8]) -> u16 + Send + Sync;

/// One `GET` of `/records` a [`RecordsHost`] answered: the ids its `have=` named, the body it
/// sent and whether it said `X-Aterm-Records-Need: 1`.
#[derive(Clone)]
struct Answered {
    have: Vec<RecordId>,
    body: Vec<u8>,
    need: bool,
}

/// The boot id a [`RecordsHost`] puts on each `GET` of `/records` it answers
/// (`X-Aterm-Records-Boot`, F23 rule 8).
enum Boot {
    /// None, as a server before F23 answers.
    Silent,
    /// This one, until a test changes it (a restart).
    Id(String),
    /// Another on every reply: this count, then the next.
    EveryReply(u64),
}

/// A records server on loopback whose answers a test writes: `get` answers each `GET` of
/// `/records`, and `post` each `POST`. It counts the POSTs, the records they carried, and the
/// records its GETs sent, and keeps every `GET` it answered. It states no records bound unless a
/// test sets one ([`RecordsHost::set_bound`]).
struct RecordsHost {
    addr: SocketAddr,
    posts: Arc<AtomicUsize>,
    posted: Arc<AtomicUsize>,
    sent: Arc<AtomicUsize>,
    answers: Arc<std::sync::Mutex<Vec<Answered>>>,
    boot: Arc<std::sync::Mutex<Boot>>,
    bound: Arc<std::sync::Mutex<Option<crate::crossing::RecordsBound>>>,
    stop: Arc<AtomicBool>,
    worker: std::thread::JoinHandle<()>,
}

impl RecordsHost {
    /// A host whose every `POST` gets `post_status`.
    fn start(get: Box<Answer>, post_status: u16) -> RecordsHost {
        Self::start_with(get, Box::new(move |_| post_status))
    }

    fn start_with(get: Box<Answer>, post: Box<Posted>) -> RecordsHost {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let posts = Arc::new(AtomicUsize::new(0));
        let posted = Arc::new(AtomicUsize::new(0));
        let sent = Arc::new(AtomicUsize::new(0));
        let answers = Arc::new(std::sync::Mutex::new(Vec::new()));
        let boot = Arc::new(std::sync::Mutex::new(Boot::Silent));
        let bound = Arc::new(std::sync::Mutex::new(None::<crate::crossing::RecordsBound>));
        let stop = Arc::new(AtomicBool::new(false));
        let (counted_posts, counted_posted, counted_sent, kept_answers, booted, stopping) = (
            Arc::clone(&posts),
            Arc::clone(&posted),
            Arc::clone(&sent),
            Arc::clone(&answers),
            Arc::clone(&boot),
            Arc::clone(&stop),
        );
        let stating = Arc::clone(&bound);
        let worker = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(mut stream) = stream else {
                    continue;
                };
                let Some((line, body)) = read_request(&mut stream) else {
                    continue;
                };
                let reply = if line.starts_with("POST ") {
                    counted_posts.fetch_add(1, Ordering::SeqCst);
                    counted_posted.fetch_add(
                        decode_records(&body).map_or(0, |records| records.len()),
                        Ordering::SeqCst,
                    );
                    let empty = "Content-Length: 0\r\nConnection: close\r\n\r\n";
                    let status = post(&body);
                    format!("HTTP/1.1 {status} Answer\r\n{empty}").into_bytes()
                } else if line.starts_with("GET ") && line.contains("/records") {
                    let have = have_named(&line);
                    let (body, need) = get(&have);
                    counted_sent.fetch_add(
                        decode_records(&body).map_or(0, |records| records.len()),
                        Ordering::SeqCst,
                    );
                    kept_answers
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(Answered {
                            have,
                            body: body.clone(),
                            need,
                        });
                    let mut need = if need {
                        format!("{}: 1\r\n", transport::RECORDS_NEED_HEADER)
                    } else {
                        String::new()
                    };
                    let boot_id = match &mut *booted
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                    {
                        Boot::Silent => None,
                        Boot::Id(id) => Some(id.clone()),
                        Boot::EveryReply(count) => {
                            *count += 1;
                            Some(format!("{:032x}", *count))
                        }
                    };
                    if let Some(id) = boot_id {
                        need.push_str(&format!("{}: {id}\r\n", transport::RECORDS_BOOT_HEADER));
                    }
                    if let Some(bound) = *stating
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                    {
                        need.push_str(&format!(
                            "{}: {}\r\n",
                            transport::RECORDS_BOUND_HEADER,
                            bound.value()
                        ));
                    }
                    let status = if body.is_empty() {
                        "204 No Content"
                    } else {
                        "200 OK"
                    };
                    let length = body.len();
                    let head = format!("HTTP/1.1 {status}\r\n{need}Content-Length: {length}\r\n");
                    let mut reply = format!("{head}Connection: close\r\n\r\n").into_bytes();
                    reply.extend_from_slice(&body);
                    reply
                } else {
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_vec()
                };
                let _ = stream.write_all(&reply);
            }
        });
        RecordsHost {
            addr,
            posts,
            posted,
            sent,
            answers,
            boot,
            bound,
            stop,
            worker,
        }
    }

    /// State `bound` of its records on every `GET` of `/records` answered from now on, as a
    /// server of the fence's build does; `None`, the default, states none, as one before it.
    fn set_bound(&self, bound: Option<crate::crossing::RecordsBound>) {
        *self
            .bound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = bound;
    }

    /// Put `boot` on every `GET` of `/records` answered from now on.
    fn set_boot(&self, boot: Boot) {
        *self
            .boot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = boot;
    }

    /// Vault `vault`'s URL here.
    fn url(&self, vault: &str) -> String {
        format!("http://{}/v1/{vault}", self.addr)
    }

    fn posts(&self) -> usize {
        self.posts.load(Ordering::SeqCst)
    }

    /// How many records the POSTs carried in all.
    fn posted(&self) -> usize {
        self.posted.load(Ordering::SeqCst)
    }

    fn sent(&self) -> usize {
        self.sent.load(Ordering::SeqCst)
    }

    /// Every `GET` of `/records` answered so far, in order.
    fn answers(&self) -> Vec<Answered> {
        self.answers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn end(self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        self.worker.join().expect("the host ends");
    }
}

/// Read one request (its head, and the body its `Content-Length` names) from `stream`: its
/// request line and its body.
fn read_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    stream.set_read_timeout(Some(HANG_DETECTOR)).ok()?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).ok()?;
    head.lines().next().map(|line| (line.to_owned(), body))
}

/// A founder or entrant `tag` names: its writer seed, incarnation, member id and entrant.
fn entrant(tag: u8, label: &str) -> ([u8; 32], [u8; 16], MemberId, Entrant) {
    let writer = [tag; 32];
    let machine = [tag.wrapping_add(0x20); 16];
    let incarnation = [tag; 16];
    let identity = crate::age::Identity::from_secret_bytes([tag.wrapping_add(0x40); 32]);
    let request = crate::commons::binding::Request::create(
        [0x31; 16],
        [0x42; 32],
        machine,
        incarnation,
        &identity,
        &writer,
    )
    .expect("request");
    let member = request.binding().actor();
    (
        writer,
        incarnation,
        member,
        Entrant {
            request,
            node_key: [tag.wrapping_add(0x80); 32],
            name: format!("mac-{tag}"),
            label: label.to_owned(),
        },
    )
}

/// `log` with one more record, a `Name` its founder `m1` signs.
fn named(log: &mut Log, writer: &[u8; 32], incarnation: [u8; 16], m1: &MemberId, n: u64) -> Record {
    let record = log
        .draft(
            m1,
            2_000 + n,
            Body::Name {
                name: format!("mac-one-{n}"),
            },
        )
        .sign(writer, incarnation)
        .expect("signed");
    log.insert(record.clone()).expect("held");
    record
}

/// A Mac configured, with no envelope, to read vault `team` at `host`.
fn member_of(root: &Path, host: &RecordsHost) -> Paths {
    member_pinned(root, host, &[0x5a; 32])
}

/// [`member_of`], pinning the vault key `key`.
fn member_pinned(root: &Path, host: &RecordsHost, key: &[u8; 32]) -> Paths {
    let member = Paths::for_home(&root.join("m2"));
    let pin = crate::b64::encode_padded(key);
    configure_log_member(&member, &format!("{}#{pin}", host.url("team")), "m2")
        .expect("configured");
    member
}

#[test]
fn unused_peer_selection_does_not_advance_after_a_record_long_poll() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let primary = RecordsHost::start(Box::new(|_| (Vec::new(), false)), 400);
    let peer_reads = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&peer_reads);
    let peer = RecordsHost::start(
        Box::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            (Vec::new(), false)
        }),
        400,
    );
    let member = member_of(root.path(), &primary);
    let selections = AtomicUsize::new(0);
    let found = |_: &Config| {
        selections.fetch_add(1, Ordering::SeqCst);
        vec![crate::discover::DiscoveredVault {
            url: peer.url("team"),
            txt: None,
        }]
    };
    let found = PeerDiscovery::supplied(&found);
    sync_records_inner(&member, Some(Duration::ZERO), &[], Some(&found), None)
        .expect("the primary answered its long poll");
    let after_poll = selections.load(Ordering::SeqCst);
    let reads_after_poll = peer_reads.load(Ordering::SeqCst);
    sync_records_inner(&member, None, &[], Some(&found), None)
        .expect("a non-waiting pass asks peers");
    let after_probe = selections.load(Ordering::SeqCst);
    let reads_after_probe = peer_reads.load(Ordering::SeqCst);
    peer.end();
    primary.end();
    assert_eq!(after_poll, 0, "an unused batch must not advance discovery");
    assert_eq!(reads_after_poll, 0);
    assert_eq!(after_probe, 1);
    assert_eq!(reads_after_probe, 1);
}

/// What [`lacking`] sets up: a records host whose store lacks the member's newest record and
/// says so, the member, and the founder's log and keys to mint more records with.
struct Lacking {
    host: RecordsHost,
    member: Paths,
    log: Log,
    writer: [u8; 32],
    incarnation: [u8; 16],
    founder: MemberId,
}

impl Lacking {
    /// One more record in the founder's log.
    fn mint(&mut self, n: u64) -> Record {
        named(
            &mut self.log,
            &self.writer,
            self.incarnation,
            &self.founder,
            n,
        )
    }
}

/// A member under `root` holding a genesis and seven records, and a host holding the first six,
/// which cannot take the seventh: it answers each pull the records past the ids named that it
/// holds, says `X-Aterm-Records-Need` while it lacks one, and answers each `POST` `post_status`.
fn lacking(root: &Path, post_status: u16) -> Lacking {
    let (writer, incarnation, founder, entrant) = entrant(1, "m1");
    let genesis = Record::found(&[7; 32], vec![entrant], 1_000).expect("genesis");
    let mut log = Log::new(genesis.clone()).expect("log");
    let mut held = vec![genesis];
    for n in 0..6 {
        held.push(named(&mut log, &writer, incarnation, &founder, n));
    }
    let server = crate::membership_log::Store::decode("team", &encode_records(&held))
        .expect("the server's store");
    let host = RecordsHost::start(
        Box::new(move |have| {
            let need = !have.is_empty()
                && !server
                    .log()
                    .is_some_and(|log| have.iter().all(|id| log.get(id).is_some()));
            (encode_records(&server.missing(have)), need)
        }),
        post_status,
    );
    let member = member_of(root, &host);
    let mut lacking = Lacking {
        host,
        member,
        log,
        writer,
        incarnation,
        founder,
    };
    held.push(lacking.mint(6));
    save_records(&lacking.member, &encode_records(&held)).expect("the member's store");
    lacking
}

#[test]
fn bounded_member_records_pull_retains_verified_records_without_reposting() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut fixture = lacking(root.path(), 400);
    let late = fixture.mint(8);
    let wire = encode_records(fixture.log.records());
    let host = RecordsHost::start(Box::new(move |_| (wire.clone(), true)), 400);
    let config = configured(&fixture.member).unwrap();
    let mut moved = config.clone();
    moved.source = Source::Url(host.url("team"));
    save_config(&fixture.member, &moved).unwrap();
    let found = |_: &Config| Vec::new();
    let found = PeerDiscovery::supplied(&found);
    sync_records_until(
        &fixture.member,
        None,
        &[],
        Some(&found),
        Some(Instant::now() + HANG_DETECTOR),
        None,
    )
    .expect("bounded record pull");
    assert!(
        load_records(&fixture.member)
            .unwrap()
            .complete_bytes(late.id())
            .is_some()
    );
    assert_eq!(host.posts(), 0, "a bounded reader does not repost");
    host.end();
    fixture.host.end();
}

/// A different writer crosses after the request began but before its legacy answer arrives.
/// The unchanged control accepts the very same valid stream; crossing must preserve the file.
/// An empty answer must likewise never admit a fresh pass from the stale snapshot.
#[test]
fn crossing_during_a_records_pull_rejects_the_old_snapshot() {
    for (crosses, empty) in [(false, false), (true, false), (false, true), (true, true)] {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let mut fixture = lacking(root.path(), 400);
        let late = fixture.mint(8);
        let wire = if empty {
            Vec::new()
        } else {
            encode_records(fixture.log.records())
        };
        let mark = crossing_mark(&fixture.member);
        let host = RecordsHost::start(
            Box::new(move |_| {
                if crosses {
                    mark.store()
                        .expect("the other writer crosses before the reply");
                }
                (wire.clone(), false)
            }),
            400,
        );
        let mut moved = configured(&fixture.member).expect("config");
        moved.source = Source::Url(host.url("team"));
        save_config(&fixture.member, &moved).expect("new source");
        let before = std::fs::read(fixture.member.records_file()).expect("records before");
        let found = |_: &Config| Vec::new();
        let found = PeerDiscovery::supplied(&found);
        let result = sync_records_until(
            &fixture.member,
            None,
            &[],
            Some(&found),
            Some(Instant::now() + HANG_DETECTOR),
            None,
        );
        // Stop owned servers before an assertion can unwind.
        host.end();
        fixture.host.end();
        if crosses {
            assert!(
                result.is_err(),
                "a crossed snapshot cannot admit the old answer"
            );
            assert_eq!(
                std::fs::read(fixture.member.records_file()).expect("records after"),
                before,
            );
        } else {
            result.expect("the unchanged snapshot accepts the stream");
            assert_eq!(
                load_records(&fixture.member)
                    .expect("store")
                    .holds(late.id()),
                !empty,
            );
        }
    }
}

#[test]
fn an_empty_past_does_not_consume_another_servers_post_verdict() {
    struct StopHost(Option<RecordsHost>);
    impl std::ops::Deref for StopHost {
        type Target = RecordsHost;
        fn deref(&self) -> &Self::Target {
            self.0.as_ref().unwrap()
        }
    }
    impl Drop for StopHost {
        fn drop(&mut self) {
            if let Some(host) = self.0.take() {
                host.end();
            }
        }
    }

    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let Lacking { host, member, .. } = lacking(root.path(), 400);
    let primary = StopHost(Some(host));
    let status = Arc::new(AtomicUsize::new(500));
    let reply_status = Arc::clone(&status);
    let peer = StopHost(Some(RecordsHost::start_with(
        Box::new(|_| (Vec::new(), true)),
        Box::new(move |_| reply_status.load(Ordering::SeqCst) as u16),
    )));
    let primary_url = primary.url("team");
    let peer_url = peer.url("team");
    let store = load_records(&member).unwrap();
    let frontier = sha256(&store.frontier().concat());
    // This server previously judged every current record but a different frontier marker.
    // Its Past is empty, while the other server still needs the Whole selection.
    Reposted::posted(
        &member,
        &primary_url,
        &[0; 32],
        store.have(),
        Posting::Whole { boot_reset: false },
        None,
    );
    let peers = vec![peer_url.clone()];
    let found = PeerDiscovery::selected(&peers);
    let first = sync_records_inner(&member, None, &[], Some(&found), None);
    let primary_judged = Reposted::due(&member, &primary_url, &frontier, None).is_none();
    let peer_unjudged = matches!(
        Reposted::due(&member, &peer_url, &frontier, None),
        Some(Repost::Whole { .. })
    );
    let first_posts = (primary.posts(), peer.posts());

    // A transport/storage failure did not judge the peer's post. A definite400 does, even
    // though it refused the records, and the empty primary still consumes no answer.
    status.store(400, Ordering::SeqCst);
    Reposted::posted(
        &member,
        &primary_url,
        &[0; 32],
        store.have(),
        Posting::Whole { boot_reset: false },
        None,
    );
    let second = sync_records_inner(&member, None, &[], Some(&found), None);
    let peer_judged = Reposted::due(&member, &peer_url, &frontier, None).is_none();
    let final_posts = (primary.posts(), peer.posts());
    drop(peer);
    drop(primary);

    assert_eq!(first.unwrap(), 0);
    assert_eq!(second.unwrap(), 0);
    assert!(primary_judged, "the empty Past was not recorded");
    assert!(peer_unjudged, "a500 response judged the nonempty post");
    assert!(peer_judged, "a400 response did not judge the retry");
    assert_eq!(first_posts, (0, 1));
    assert_eq!(final_posts, (0, 2));
}

/// A records pass that waits on nothing asks the vault's peers beside its source, not after it:
/// a source that answers nothing must not spend a bounded pass's deadline (a member's freshness
/// read before it lets a Mac in, `readmit::FRESH_WITHIN`, inside the fleet listener's handshake
/// watchdog) before any peer is asked, or one machine's being down keeps every Mac out.
/// NEGATIVE CONTROL: asking the peers once the source's pull returned (the order before): the
/// source holds its answer until a peer is asked, its pull runs out first, and the record only it
/// sends is never taken.
#[test]
fn a_pass_that_waits_on_nothing_asks_the_peers_beside_its_source() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut fixture = lacking(root.path(), 400);
    let late = fixture.mint(8);
    let wire = encode_records(fixture.log.records());
    let asked = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let peer_rings = Arc::clone(&asked);
    let peer = RecordsHost::start(
        Box::new(move |_| {
            let (flag, bell) = &*peer_rings;
            *flag.lock().unwrap() = true;
            bell.notify_all();
            (Vec::new(), false)
        }),
        400,
    );
    let source_waits = Arc::clone(&asked);
    let source = RecordsHost::start(
        Box::new(move |_| {
            let (flag, bell) = &*source_waits;
            let held = flag.lock().unwrap();
            drop(bell.wait_timeout_while(held, HANG_DETECTOR, |asked| !*asked));
            (wire.clone(), true)
        }),
        400,
    );
    let mut moved = configured(&fixture.member).unwrap();
    moved.source = Source::Url(source.url("team"));
    save_config(&fixture.member, &moved).unwrap();
    let found = |_: &Config| {
        vec![crate::discover::DiscoveredVault {
            url: peer.url("team"),
            txt: None,
        }]
    };
    let found = PeerDiscovery::supplied(&found);
    let pass = sync_records_until(
        &fixture.member,
        None,
        &[],
        Some(&found),
        Some(Instant::now() + HANG_DETECTOR),
        None,
    );
    let taken = load_records(&fixture.member)
        .unwrap()
        .complete_bytes(late.id())
        .is_some();
    peer.end();
    source.end();
    fixture.host.end();
    pass.expect("the bounded pass");
    assert!(
        taken,
        "the source's answer, sent once a peer was asked, is taken"
    );
}

/// A server that cannot take a member's frontier (a record its build refuses, a store at its
/// bound) gets neither the member's whole store nor its own sent again on every pass: it answers
/// the records past a recent ancestor of the head it lacks, which the member names after that
/// head, and the member posts its store to it once for each frontier it holds. Before, the
/// server sent its whole store back on every pass (it found none of the heads named) and the
/// member re-posted its whole store, up to 64 MiB each way per pass, forever.
/// NEGATIVE CONTROL: naming the heads alone (`Store::frontier` for `Store::have` in
/// `sync_records_inner`) has the server send its whole log on both passes.
/// NEGATIVE CONTROL: re-posting on every pass that says `X-Aterm-Records-Need` (dropping
/// `Reposted::due`'s check) posts on both passes.
/// Once the member's frontier moves, it posts that server only the records past the store the
/// server judged, not its whole store again: in a busy swarm every record minted anywhere moves
/// every Mac's frontier, and each Mac re-posted its whole store, up to 64 MiB, to a server that
/// refused one of its records for good.
/// NEGATIVE CONTROL: posting the whole store on a moved frontier (`Repost::Past` treated as
/// `Repost::Whole`) posts nine records the second time, not one.
#[test]
fn a_server_that_cannot_take_a_members_frontier_gets_no_whole_store_each_pass() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut lacking = lacking(root.path(), 400);
    for pass in 0..2 {
        assert_eq!(
            sync_records(&lacking.member, None).expect("a pass"),
            0,
            "pass {pass}"
        );
    }
    assert_eq!(
        lacking.host.sent(),
        0,
        "the server sent records the member holds: it found none of the heads named"
    );
    assert_eq!(
        lacking.host.posts(),
        1,
        "the member re-posted a frontier the server could not take"
    );

    assert_eq!(
        lacking.host.posted(),
        8,
        "the first post is the whole store"
    );

    // A frontier that moved is posted again: the record past what the server judged.
    let eighth = lacking.mint(7);
    save_records(&lacking.member, eighth.bytes()).expect("one more record");
    sync_records(&lacking.member, None).expect("a pass");
    assert_eq!(lacking.host.posts(), 2, "a new frontier was not posted");
    assert_eq!(
        lacking.host.posted(),
        8 + 1,
        "a moved frontier re-posted the store the server judged"
    );
    sync_records(&lacking.member, None).expect("a pass");
    assert_eq!(
        lacking.host.posts(),
        2,
        "a frontier the server judged went again"
    );
    lacking.host.end();
}

/// A server that answers a member's POST without judging the records (`503`: it is reading its
/// limit of bodies already, or one from this address; `500`: it could not write its store;
/// `408`; `429`) has not refused the frontier, and the next pass that hears it lacks that
/// frontier posts it again. Before, any answer counted as a server that could not take it, and
/// a laptop that rejoined into a busy server kept a record it minted while away to itself for
/// an hour of awake time (`REPOST_AGAIN`): servers pull only from servers, so its POST is how the
/// record leaves the Mac that minted it.
/// NEGATIVE CONTROL: taking every answer as a verdict (`post_records_answered` answering `Ok` for
/// a `503`) posts once over both passes.
#[test]
fn a_server_too_busy_to_judge_a_post_is_posted_the_frontier_again() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    for status in [503, 500, 408, 429] {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let lacking = lacking(root.path(), status);
        for pass in 0..2 {
            assert_eq!(
                sync_records(&lacking.member, None).expect("a pass"),
                0,
                "{status}: pass {pass}"
            );
        }
        assert_eq!(
            lacking.host.posts(),
            2,
            "{status}: a server that judged nothing was not posted the frontier again"
        );
        lacking.host.end();
    }
}

/// A store with no genesis keeps nothing a pull hands it without the genesis that roots it: any
/// LAN host can answer `/v1/<name>/records`, and none of what one sends can be checked before a
/// genesis. Before, a keyless host filled such a store (a new Mac's, or every ALab Mac's until
/// a Genesis v2 lands) with up to `MAX_WAITING - 1` records, about 63 MiB, rewritten whole each
/// time one arrived; an honest server sends a store with no genesis its log from the genesis.
/// NEGATIVE CONTROL: keeping what a pull added to a store left with no genesis (main before this
/// fix) keeps the junk.
#[test]
fn a_keyless_host_leaves_nothing_in_a_store_with_no_genesis() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let junk: Vec<Record> = (0..3u64)
        .map(|i| {
            let mut parent = [0x66; 32];
            parent[..8].copy_from_slice(&i.to_le_bytes());
            crate::membership_log::Draft {
                network: [0xab; 32],
                sequence: 1,
                previous: None,
                parents: vec![parent],
                authored_ms: 3_000 + i,
                body: Body::Name {
                    name: format!("junk-{i}"),
                },
            }
            .sign(&[0xee; 32], [0xee; 16])
            .expect("signed")
        })
        .collect();
    let wire = encode_records(&junk);
    let host = RecordsHost::start(Box::new(move |_| (wire.clone(), false)), 204);
    let member = member_of(root.path(), &host);

    let _ = sync_records(&member, None);
    assert_eq!(host.sent(), junk.len(), "the pull reached the host");
    assert!(
        load_records(&member)
            .expect("the member's store")
            .is_empty(),
        "a store with no genesis kept records a keyless host sent"
    );
    assert!(!member.records_file().exists());
    assert_eq!(host.posts(), 0);
    host.end();
}

/// A store with no genesis keeps a pulled genesis only when the vault's key line signed it (the
/// key `source.toml` pins, a key it reached that pin through, a key of the vault's cached
/// envelope), as a genesis Commons offers must be: a store for a vault other than ALab checks no
/// genesis itself, and the first one it keeps binds it to that network for good. Before, a
/// keyless LAN host answering for vault `team` with a genesis it signed itself, and records under
/// it, filled the store, which then refused the vault's own genesis as another network's.
/// NEGATIVE CONTROL: keeping any genesis a pull brings (dropping the key-line check in
/// `sync_records_inner`) keeps the host's six records, and the vault's genesis is refused.
#[test]
fn a_keyless_host_cannot_root_a_store_with_a_genesis_it_signed() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let vault_seed = [0x77; 32];
    let ours = Record::found(&vault_seed, vec![entrant(1, "m1").3], 1_000).expect("genesis");
    let Body::Genesis {
        signer: vault_key, ..
    } = ours.body()
    else {
        panic!("a genesis");
    };
    let (writer, incarnation, member_id, founder) = entrant(2, "m9");
    let made_up = Record::found(&[0x66; 32], vec![founder], 1_000).expect("its own genesis");
    let mut theirs = Log::new(made_up.clone()).expect("log");
    let mut junk = vec![made_up];
    for n in 0..5 {
        junk.push(named(&mut theirs, &writer, incarnation, &member_id, n));
    }
    let answer = Arc::new(std::sync::Mutex::new(encode_records(&junk)));
    let answering = Arc::clone(&answer);
    let host = RecordsHost::start(
        Box::new(move |_| {
            let wire = answering.lock().expect("the answer").clone();
            (wire, false)
        }),
        204,
    );
    let member = member_pinned(root.path(), &host, vault_key);

    let _ = sync_records(&member, None);
    assert_eq!(host.sent(), junk.len(), "the pull reached the host");
    assert!(
        load_records(&member)
            .expect("the member's store")
            .is_empty(),
        "a store with no genesis kept a genesis no key of the vault signed"
    );
    assert!(!member.records_file().exists());

    // The vault's own genesis, pulled, still roots the store.
    *answer.lock().expect("the answer") = ours.bytes().to_vec();
    assert_eq!(sync_records(&member, None).expect("a pass"), 1);
    let store = load_records(&member).expect("the member's store");
    assert_eq!(store.log().map(|log| *log.genesis().id()), Some(*ours.id()));
    assert_eq!(host.posts(), 0);
    host.end();
}

/// A kind code no build writes yet: a test's stand-in for a newer build's record.
const NEWER_KIND: u8 = 0xee;

/// A founder's log holding a genesis and `Name`s it signed, those records in order, and its keys.
struct Founded {
    log: Log,
    records: Vec<Record>,
    writer: [u8; 32],
    incarnation: [u8; 16],
    founder: MemberId,
}

impl Founded {
    /// A genesis and `n` `Name`s, `n0` to `n{n-1}`, each after the one before.
    fn new(n: u64) -> Self {
        let (writer, incarnation, founder, entrant) = entrant(1, "m1");
        let genesis = Record::found(&[7; 32], vec![entrant], 1_000).expect("genesis");
        let mut founded = Self {
            log: Log::new(genesis.clone()).expect("log"),
            records: vec![genesis],
            writer,
            incarnation,
            founder,
        };
        for i in 0..n {
            founded.mint(i);
        }
        founded
    }

    /// One more `Name` in the founder's log, after its heads.
    fn mint(&mut self, n: u64) -> Record {
        let record = named(
            &mut self.log,
            &self.writer,
            self.incarnation,
            &self.founder,
            n,
        );
        self.records.push(record.clone());
        record
    }

    /// The bytes of the founder's next record, held nowhere, rewritten to a kind this build does
    /// not know (a newer build's record), and the parents it names.
    fn unread(&self, n: u64) -> (Vec<u8>, Vec<RecordId>) {
        let record = self
            .log
            .draft(
                &self.founder,
                5_000 + n,
                Body::Name {
                    name: format!("newer-{n}"),
                },
            )
            .sign(&self.writer, self.incarnation)
            .expect("signed");
        (
            with_kind_code(&record, NEWER_KIND),
            record.parents().to_vec(),
        )
    }

    /// A `Name` the founder signs at `sequence` after `parent` alone, its record at the sequence
    /// before (one this build cannot read, if the test says so).
    fn after(&self, sequence: u64, parent: RecordId, name: &str) -> Record {
        Draft {
            network: *self.records[0].id(),
            sequence,
            previous: Some(parent),
            parents: vec![parent],
            authored_ms: 6_000 + sequence,
            body: Body::Name {
                name: name.to_owned(),
            },
        }
        .sign(&self.writer, self.incarnation)
        .expect("signed")
    }
}

/// A records host answering each pull from `chain` as `Store::missing` does (the frames past
/// the closure of the ids named that it holds), saying it lacks a record while `have` names one
/// it does not hold, and answering every `POST` `post_status`.
fn chain_host(chain: &Arc<std::sync::Mutex<Chain>>, post_status: u16) -> RecordsHost {
    let serving = Arc::clone(chain);
    RecordsHost::start(
        Box::new(move |have| {
            let chain = serving
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (chain.past(have), chain.lacks_any(have))
        }),
        post_status,
    )
}

/// [`chain_host`], which also takes each record a `POST` carries that it does not hold.
fn chain_host_taking_posts(chain: &Arc<std::sync::Mutex<Chain>>) -> RecordsHost {
    let (serving, taking) = (Arc::clone(chain), Arc::clone(chain));
    RecordsHost::start_with(
        Box::new(move |have| {
            let chain = serving
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (chain.past(have), chain.lacks_any(have))
        }),
        Box::new(move |body| {
            let Ok(records) = decode_records(body) else {
                return 400;
            };
            let mut chain = taking
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for record in &records {
                if !chain.holds(record.id()) {
                    chain.push(record);
                }
            }
            204
        }),
    )
}

/// A record this build cannot read (a kind only a newer build writes) travels to a Mac once, not
/// on every pass: the next pull to that server names what it could not take first, so the server
/// sends only what is new past it. Before, the server's stream past the Mac's `have` held it on
/// every pass, refused whole each time, up to 64 MiB from each server.
/// NEGATIVE CONTROL: main's `sync_records_until` (every pull names `local_store.have()`) is sent
/// both records again on pass 2 and refuses them again ("a pass: Error(\"the vault server returned an invalid membership record stream\", Other)").
/// NEGATIVE CONTROL: noting only inside the lock, not before the error a stream with no peers
/// returns, keeps no memo, with the same failure on pass 2.
/// NEGATIVE CONTROL: framing through the kind check (`Record::frame_prefix` calling
/// `Kind::from_code`, as `Record::wire_len_prefix` does) frames nothing, keeps no memo, with the
/// same failure on pass 2.
#[test]
fn a_record_this_build_cannot_read_is_pulled_once_not_on_every_pass() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let founded = Founded::new(6);
    let (newer, parents) = founded.unread(6);
    let mut chain = Chain::of(&founded.records);
    let newer_id = chain.push_raw(newer.clone(), parents);
    let child = founded.after(8, newer_id, "child");
    chain.push(&child);
    let chain = Arc::new(std::sync::Mutex::new(chain));
    let host = chain_host(&chain, 400);
    let member = member_of(root.path(), &host);
    save_records(&member, &encode_records(&founded.records)).expect("the member's store");

    assert!(
        sync_records(&member, None).is_err(),
        "a stream of a record this build cannot read and its child is refused"
    );
    for pass in 2..=3 {
        assert_eq!(
            sync_records(&member, None).expect("a pass"),
            0,
            "pass {pass}"
        );
    }
    let grandchild = founded.after(9, *child.id(), "grandchild");
    chain
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(&grandchild);
    assert_eq!(
        sync_records(&member, None).expect("a pass"),
        1,
        "the record past them came, and waits on them"
    );
    assert_eq!(sync_records(&member, None).expect("a pass"), 0);
    let answers = host.answers();
    host.end();

    assert_eq!(answers.len(), 5);
    let mut both = newer.clone();
    both.extend_from_slice(child.bytes());
    assert_eq!(answers[0].body, both, "pass 1 brings both");
    assert!(
        answers[1].body.is_empty() && answers[2].body.is_empty(),
        "the records this Mac could not take came again"
    );
    assert_eq!(
        answers[1].have.first(),
        Some(child.id()),
        "the pull names what it could not take first"
    );
    assert_eq!(
        answers[3].body,
        grandchild.bytes(),
        "only what is new past them comes"
    );
    assert!(
        answers[4].body.is_empty(),
        "the record past them came again"
    );
    assert_eq!(answers[4].have.first(), Some(grandchild.id()));
}

/// What a server sent that the store refused (here a record by a key no record seats, standing
/// in for a store at its bound or another refusal that holds for good) stays remembered while
/// the store only grows, a record this Mac minted included; once the store loses records (a
/// leave and rejoin, a records file replaced) the pull names its own ids again and brings them.
/// NEGATIVE CONTROL: main's `sync_records_until` is sent the refused record again on pass 2 and
/// fails on it ("pass 2: Error(\"a membership record's signer has no seat in its causal past\",
/// Other)").
/// NEGATIVE CONTROL: dropping the memo whenever the store's frontier moves (`Gap::holds` also
/// asking for the frontier it was written at) is sent the refused record again on pass 3 ("pass
/// 3: Error(\"a membership record's signer has no seat in its causal past\", Other)").
/// NEGATIVE CONTROL: dropping the floor check from `Gap::holds` names the refused record first on
/// pass 4, the host sends nothing, and the store never gets back what it lost ("a store that
/// lost records pulls them again: left: 0, right: 5").
#[test]
fn what_a_server_sent_that_the_store_refused_stays_remembered_while_the_store_only_grows() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut founded = Founded::new(6);
    let held = encode_records(&founded.records[..4]);
    let shrunk = encode_records(&founded.records[..2]);
    let (unseated_writer, unseated_incarnation, _, _) = entrant(2, "m2");
    let head = *founded.records.last().expect("records").id();
    let refused = Draft {
        network: *founded.records[0].id(),
        sequence: 1,
        previous: None,
        parents: vec![head],
        authored_ms: 3_000,
        body: Body::Name {
            name: "unseated".to_owned(),
        },
    }
    .sign(&unseated_writer, unseated_incarnation)
    .expect("signed");
    let mut chain = Chain::of(&founded.records);
    chain.push(&refused);
    let chain = Arc::new(std::sync::Mutex::new(chain));
    let host = chain_host(&chain, 400);
    let member = member_of(root.path(), &host);
    save_records(&member, &held).expect("the member's store");

    assert_eq!(sync_records(&member, None).expect("pass 1"), 3);
    assert_eq!(sync_records(&member, None).expect("pass 2"), 0);
    let minted = founded.mint(6);
    save_records(&member, minted.bytes()).expect("a record this Mac minted");
    assert_eq!(sync_records(&member, None).expect("pass 3"), 0);
    fsx::write_atomic(&member.records_file(), &shrunk, 0o600).expect("a smaller store");
    let pulled = sync_records(&member, None).expect("pass 4");
    let answers = host.answers();
    host.end();

    assert!(carries(&answers[0].body, refused.bytes()));
    assert_eq!(answers[1].body, Vec::<u8>::new(), "pass 2");
    assert_eq!(answers[2].body, Vec::<u8>::new(), "pass 3");
    assert_eq!(pulled, 5, "a store that lost records pulls them again");
}

/// A Mac whose `have` fills the pull's URL (a seated key's two hundred forks at sequence one,
/// each a head) still names what a server sent that it could not take: the memo's heads go
/// before the store's own ids, which the URL cuts at about 125. The forks the URL leaves out
/// still travel, as the forked-heads fix accepted.
/// NEGATIVE CONTROL: naming the memo's heads after `Store::have`'s ids has the URL cut them, and
/// the record goes on every pass ("sent once over three passes: left: 3, right: 1").
/// NEGATIVE CONTROL: main's `sync_records_until` refuses the whole stream on pass 1 ("a pass:
/// Error(\"the vault server returned an invalid membership record stream\", Other)").
#[test]
fn a_member_whose_frontier_fills_the_pull_url_still_names_what_it_could_not_take() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (w1, inc1, m1, e1) = entrant(1, "m1");
    let (w2, inc2, _, e2) = entrant(2, "m2");
    let genesis = Record::found(&[7; 32], vec![e1], 1_000).expect("genesis");
    let mut log = Log::new(genesis.clone()).expect("log");
    let admit = log
        .draft(
            &m1,
            1_001,
            Body::Admit {
                entrant: e2,
                evidence: Evidence::Phrase,
            },
        )
        .sign(&w1, inc1)
        .expect("admit");
    log.insert(admit.clone()).expect("admit enters");
    let forks: Vec<Record> = (0..200u64)
        .map(|i| {
            Draft {
                network: *genesis.id(),
                sequence: 1,
                previous: None,
                parents: vec![*admit.id()],
                authored_ms: 2_000 + i,
                body: Body::Name {
                    name: format!("fork-{i}"),
                },
            }
            .sign(&w2, inc2)
            .expect("fork")
        })
        .collect();
    let next = log
        .draft(
            &m1,
            3_000,
            Body::Name {
                name: "newer".to_owned(),
            },
        )
        .sign(&w1, inc1)
        .expect("signed");
    let newer = with_kind_code(&next, NEWER_KIND);
    let held: Vec<&Record> = [&genesis, &admit].into_iter().chain(&forks).collect();
    let mut chain = Chain::of(held.iter().copied());
    chain.push_raw(newer.clone(), next.parents().to_vec());
    let chain = Arc::new(std::sync::Mutex::new(chain));
    let host = chain_host(&chain, 400);
    let member = member_of(root.path(), &host);
    save_records(&member, &encode_records(held.iter().copied())).expect("the member's store");
    assert!(
        load_records(&member).expect("store").have().len() > 125,
        "the store's own ids fill the URL"
    );

    for pass in 1..=3 {
        assert_eq!(
            sync_records(&member, None).expect("a pass"),
            0,
            "pass {pass}"
        );
    }
    let answers = host.answers();
    host.end();
    assert_eq!(answers.len(), 3);
    assert_eq!(
        answers
            .iter()
            .filter(|answer| carries(&answer.body, &newer))
            .count(),
        1,
        "sent once over three passes"
    );
}

/// A records file replaced by one holding fewer records while a pull is out (a leave and
/// rejoin) leaves no memo: the pass notes only against a store that holds every id of the
/// frontier its pull named, so what the server sent past that frontier, which waits in the
/// smaller store, does not cover the records the store lost.
/// NEGATIVE CONTROL: noting without that check keeps the waiting record as a memo head over a
/// floor the smaller store holds, the host sends nothing on pass 2, and the lost records never
/// come back ("pass 2 brings the records the file lost: left: 0, right: 5"). Main keeps no memo
/// and passes; this guards the memo.
#[test]
fn a_store_replaced_with_fewer_records_mid_pull_still_pulls_what_it_lacks() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut founded = Founded::new(6);
    let held = encode_records(&founded.records);
    let shrunk = encode_records(&founded.records[..2]);
    founded.mint(6);
    let chain = Chain::of(&founded.records);
    let shrink: Arc<std::sync::Mutex<Option<PathBuf>>> = Arc::new(std::sync::Mutex::new(None));
    let shrinking = Arc::clone(&shrink);
    let host = RecordsHost::start(
        Box::new(move |have| {
            let file = shrinking
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(file) = file {
                fsx::write_atomic(&file, &shrunk, 0o600).expect("the file shrinks");
            }
            (chain.past(have), chain.lacks_any(have))
        }),
        400,
    );
    let member = member_of(root.path(), &host);
    save_records(&member, &held).expect("the member's store");
    *shrink
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(member.records_file());

    assert_eq!(
        sync_records(&member, None).expect("pass 1"),
        1,
        "the record past the frontier waits on what the file lost"
    );
    let pulled = sync_records(&member, None).expect("pass 2");
    host.end();
    assert_eq!(pulled, 5, "pass 2 brings the records the file lost");
    let store = load_records(&member).expect("the member's store");
    for record in &founded.records {
        assert!(store.complete_bytes(record.id()).is_some());
    }
}

/// A server of a newer build sends a record this build cannot read beside records it reads (its
/// parent, and a sibling): the pass takes those, and the next pulls name what it could not take,
/// so the server sends neither it nor its child again. Before, the whole stream was refused on
/// every pass, and a memo that named the unreadable record's heads without taking the rest
/// would cover its readable parent, which then came from no server that holds both.
/// NEGATIVE CONTROL: taking none of a stream that does not verify whole (`verified` without the
/// readable part) errs on pass 1 ("pass 1 takes what it reads: Err(Error(\"the vault server returned an invalid membership record stream\", Other))"), and the
/// memo's heads then cover the readable parent for good. Main's `sync_records_until` fails the
/// same way.
#[test]
fn a_readable_record_beside_one_this_build_cannot_read_is_taken_and_the_rest_not_pulled_again() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut founded = Founded::new(6);
    let held = encode_records(&founded.records);
    let parent = founded.mint(6);
    let (newer, newer_parents) = founded.unread(7);
    let sibling = founded.after(8, *parent.id(), "sibling");
    let mut chain = Chain::of(&founded.records);
    let newer_id = chain.push_raw(newer.clone(), newer_parents);
    chain.push(&sibling);
    let child = founded.after(9, newer_id, "child");
    chain.push(&child);
    let chain = Arc::new(std::sync::Mutex::new(chain));
    let host = chain_host(&chain, 400);
    let member = member_of(root.path(), &host);
    save_records(&member, &held).expect("the member's store");

    let first = sync_records(&member, None);
    assert!(
        matches!(first, Ok(2)),
        "pass 1 takes what it reads: {first:?}"
    );
    let store = load_records(&member).expect("the member's store");
    assert!(
        store.complete_bytes(parent.id()).is_some(),
        "the readable parent"
    );
    assert!(
        store.complete_bytes(sibling.id()).is_some(),
        "the readable sibling"
    );
    for pass in 2..=3 {
        assert_eq!(
            sync_records(&member, None).expect("a pass"),
            0,
            "pass {pass}"
        );
    }
    let answers = host.answers();
    host.end();
    assert!(carries(&answers[0].body, &newer) && carries(&answers[0].body, child.bytes()));
    assert!(
        answers[1..].iter().all(|answer| answer.body.is_empty()),
        "what this Mac could not take came again"
    );
}

/// A server that no longer holds a memo's head (the URL reaches another server now, or one that
/// reset its store) says it lacks a record named on every pull that names it; the second such
/// answer in a row drops the memo, so the pull names the store's own ids, the server stops
/// saying it lacks one, and the member posted it its store once.
/// NEGATIVE CONTROL: keeping the memo whatever the server says (`Untaken::heard` never dropping
/// it) names the head on pass 4 ("the pull still names a head the server lacks").
/// NEGATIVE CONTROL: main's `sync_records_until` names no head, so the server never says it lacks
/// one ("the server lacks the memo's head").
#[test]
fn a_server_that_no_longer_holds_a_memo_head_stops_hearing_need() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let founded = Founded::new(6);
    let (newer, parents) = founded.unread(6);
    let mut chain = Chain::of(&founded.records);
    let newer_id = chain.push_raw(newer, parents);
    let chain = Arc::new(std::sync::Mutex::new(chain));
    let host = chain_host(&chain, 204);
    let member = member_of(root.path(), &host);
    save_records(&member, &encode_records(&founded.records)).expect("the member's store");

    assert!(sync_records(&member, None).is_err(), "pass 1");
    chain
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .drop_frame(&newer_id);
    for pass in 2..=4 {
        assert_eq!(
            sync_records(&member, None).expect("a pass"),
            0,
            "pass {pass}"
        );
    }
    let answers = host.answers();
    let posts = host.posts();
    host.end();
    assert!(
        answers[1].need && answers[2].need,
        "the server lacks the memo's head"
    );
    assert!(
        !answers[3].have.contains(&newer_id),
        "the pull still names a head the server lacks"
    );
    assert!(!answers[3].need, "the server still lacks a record named");
    assert_eq!(posts, 1, "the member posted its store more than once");
}

/// A server one pass behind this Mac (here a record this Mac minted) says it lacks a record
/// named until it takes the post that follows; the memo stays, so what that server sent that
/// this Mac could not take does not come again. In a busy swarm a Mac's frontier is ahead of a
/// server on most passes.
/// NEGATIVE CONTROL: dropping the memo at the first such answer (`Untaken::heard` forgetting on
/// `needs` alone) brings the record again on pass 4, which errs ("pass 4:
/// Err(Error(\"the vault server returned an invalid membership record stream\", Other))").
/// NEGATIVE CONTROL: main's `sync_records_until` is sent it again on pass 2 ("pass 2:
/// Error(\"the vault server returned an invalid membership record stream\", Other)").
#[test]
fn a_server_one_pass_behind_this_mac_keeps_what_it_could_not_take() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut founded = Founded::new(6);
    let held = encode_records(&founded.records);
    let (newer, parents) = founded.unread(6);
    let mut chain = Chain::of(&founded.records);
    chain.push_raw(newer.clone(), parents);
    let chain = Arc::new(std::sync::Mutex::new(chain));
    let host = chain_host_taking_posts(&chain);
    let member = member_of(root.path(), &host);
    save_records(&member, &held).expect("the member's store");

    assert!(sync_records(&member, None).is_err(), "pass 1");
    assert_eq!(sync_records(&member, None).expect("pass 2"), 0);
    let minted = founded.mint(6);
    save_records(&member, minted.bytes()).expect("a record this Mac minted");
    for pass in 3..=5 {
        let result = sync_records(&member, None);
        assert!(matches!(result, Ok(0)), "pass {pass}: {result:?}");
    }
    let answers = host.answers();
    let posts = host.posts();
    host.end();
    assert!(answers[2].need, "the server lacked the minted record");
    assert!(!answers[3].need, "the server took the post");
    assert_eq!(posts, 1);
    assert_eq!(
        answers
            .iter()
            .filter(|answer| carries(&answer.body, &newer))
            .count(),
        1,
        "what this Mac could not take came again"
    );
}

/// A member whose server holds a removal it can never take (past a record of a kind only a newer
/// build writes, from which a newer admin's `Remove` draws its parents) has not read the vault's
/// newest records, however its passes end: once a pass notes what it could not take, the next
/// pull names that memo's heads and the server sends nothing, a pass that ends `Ok(0)`. The
/// member then lets no key in by proof, since the removal it cannot read may be of that Mac.
/// Here m7 is shown its node key's proof under a fresh vault key while its server holds a
/// `Remove` of m7's seat past such a record (signed by m22's key, standing in for an admin on a
/// newer build; this Mac never takes either).
/// NEGATIVE CONTROL: `admit_by_proof` taking every pass that ends `Ok` as fresh (`.is_ok()`, as
/// before the pass said what it left untaken) moves m7's seat ("a member whose server holds a
/// removal it cannot take let the new key in: Let { bundle: SecretBytes(<8814 bytes redacted>),
/// how: Seat { label: \"m7\" }, signed: 4 }").
#[test]
fn a_member_whose_server_holds_records_it_cannot_take_lets_no_key_in_by_proof() {
    use ring::signature::{Ed25519KeyPair, KeyPair as _};

    let macs = super::log_entries_tests::two_macs(&[(
        Audience::Everyone,
        "shared",
        b"everyone-v1".as_slice(),
    )]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let pair = Ed25519KeyPair::from_seed_unchecked(&[0x0b; 32]).expect("node key");
    let n7: [u8; 32] = pair.public_key().as_ref().try_into().expect("32 bytes");
    publish_local_node_key(m7, n7, |network, actor| {
        pair.sign(&crate::membership_log::node_proof_message(network, actor))
            .as_ref()
            .try_into()
            .expect("64 bytes")
    })
    .expect("m7 publishes its node key")
    .expect("a Node record");
    post_records_to_servers(m7).expect("m7 posts its records");
    sync_records(m22, None).expect("m22 pulls m7's Node record");

    let (bootstrap, admin, incarnation) = super::log_entries_tests::member_key(m22);
    let (_, old, _) = super::log_entries_tests::member_key(m7);
    let store = load_records(m22).expect("m22's store");
    let log = store.log().expect("m22's log");
    let at = aterm_time::unix_millis().expect("clock");
    let (newer, newer_parents, removal) = bootstrap.device().with_seed(|seed| {
        let next = log
            .draft(
                &admin,
                at,
                Body::Name {
                    name: "newer".to_owned(),
                },
            )
            .sign(seed, incarnation)
            .expect("signed");
        let newer = with_kind_code(&next, NEWER_KIND);
        let newer_id = aterm_digest::Sha256::digest(&newer);
        let removal = Draft {
            network: *log.network(),
            sequence: next.sequence() + 1,
            previous: Some(newer_id),
            parents: vec![newer_id],
            authored_ms: at + 1,
            body: Body::Remove {
                member: old,
                cutoff: next.parents()[0],
            },
        }
        .sign(seed, incarnation)
        .expect("signed");
        (newer, next.parents().to_vec(), removal)
    });
    let mut chain = Chain::of(log.records());
    chain.push_raw(newer, newer_parents);
    chain.push(&removal);
    let chain = Arc::new(std::sync::Mutex::new(chain));
    let host = chain_host(&chain, 400);
    // A server of the fence's build, so the pass's freshness turns on what it left untaken alone.
    host.set_bound(Some(crate::crossing::RecordsBound::uncrossed()));
    // The vault's server moved to the host, and m22's applied state follows it.
    let mut moved = configured(m22).expect("m22's config");
    moved.source = Source::Url(host.url(crate::ALAB_VAULT));
    save_config(m22, &moved).expect("m22 reads the host");
    let mut state = load_state(m22).expect("m22's state");
    state.source_identity = Some(source_identity(&moved));
    save_state(m22, &state).expect("m22's state follows its source");
    let _answering = ServersAnswering::set();

    let first = sync_records_until(m22, None, &[], None, None, None);
    assert!(
        !first.as_ref().is_ok_and(|pass| !pass.untaken),
        "a periodic pass takes nothing of a stream of nothing this build reads: {first:?}"
    );
    assert_eq!(
        sync_records_until(m22, None, &[], None, None, None).expect("a periodic pass"),
        RecordsPass {
            added: 0,
            untaken: true,
            unstated: false,
        },
        "the next pass names what it could not take, and the server sends nothing"
    );
    let (_, request, _) = super::log_entries_tests::newcomer(macs.root.path(), m22, "m7-fresh");
    let proven = Proven {
        reader: crate::members::bundle::Reader::BoundedLog(
            crate::crossing::RecordsBound::uncrossed(),
        ),
        request: &request,
        node: n7,
        name: "m7",
        fleet: false,
        lan: false,
        fresh: false,
        book: Book {
            node_id_held_out: false,
            node_id_seated: false,
            node_elsewhere: false,
            moved_seats_read: true,
        },
    };
    let later = aterm_time::unix_millis().expect("clock") + super::readmit::DARK_FOR_MS + 1;
    let decided = admit_by_proof(m22, &proven, later).expect("decided");
    let answers = host.answers();
    host.end();
    macs.server.stop();
    let Readmit::Refused(why) = &decided else {
        panic!(
            "a member whose server holds a removal it cannot take let the new key in: {decided:?}"
        );
    };
    assert!(why.contains("newest records"), "{why}");
    let last = answers.last().expect("the proof's pass asked the host");
    assert!(
        last.body.is_empty() && last.have.first() == Some(removal.id()),
        "the proof's pass named the removal it could not take, and the host sent nothing"
    );
    drop(macs.root);
}

/// A server that sends records this Mac cannot take and cannot take a record of this Mac's own
/// (another network's, one its store refuses: here one it was never posted whole and refuses)
/// says it lacks a record named on every pull, whatever the memo names. Two such answers in a row
/// drop the memo at most once an hour per server, so the gap travels twice in that hour, not on
/// every third pass.
/// NEGATIVE CONTROL: dropping the memo on every second answer in a row (`Untaken::heard` without
/// the once-an-hour check) brings the gap again on pass 7 ("pass 7: Err(Error(\"the vault
/// server returned an invalid membership record stream\", Other))").
#[test]
fn a_server_that_cannot_take_this_macs_records_drops_the_memo_at_most_once_an_hour() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut founded = Founded::new(6);
    let (newer, parents) = founded.unread(6);
    let mut chain = Chain::of(&founded.records);
    chain.push_raw(newer.clone(), parents);
    let chain = Arc::new(std::sync::Mutex::new(chain));
    let host = chain_host(&chain, 400);
    let member = member_of(root.path(), &host);
    founded.mint(7);
    save_records(&member, &encode_records(&founded.records)).expect("the member's store");

    for pass in 1..=8 {
        let result = sync_records(&member, None);
        let brought = matches!(pass, 1 | 4);
        assert_eq!(result.is_err(), brought, "pass {pass}: {result:?}");
    }
    let answers = host.answers();
    host.end();
    assert!(
        answers.iter().all(|answer| answer.need),
        "the server lacks this Mac's record"
    );
    assert_eq!(
        answers
            .iter()
            .filter(|answer| carries(&answer.body, &newer))
            .count(),
        2,
        "the gap travelled once more after the memo was dropped"
    );
}

/// A peer that holds a record this build cannot read sends it once, not on every pass: the pull
/// to each peer names that peer's memo's heads first, as the pull to the vault's source does.
/// Here the source holds nothing new and the one peer a pass asks holds the record and its child.
/// NEGATIVE CONTROL: peers named `Store::have`'s ids alone (`Named { ids: have.clone(), heads: 0
/// }` for each peer) are sent both records on every pass ("sent once over three passes: left: 3,
/// right: 1").
#[test]
fn a_peer_that_holds_a_record_this_build_cannot_read_sends_it_once() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let founded = Founded::new(6);
    let (newer, parents) = founded.unread(6);
    let mut chain = Chain::of(&founded.records);
    let newer_id = chain.push_raw(newer.clone(), parents);
    let child = founded.after(8, newer_id, "child");
    chain.push(&child);
    let chain = Arc::new(std::sync::Mutex::new(chain));
    let primary = RecordsHost::start(Box::new(|_| (Vec::new(), false)), 400);
    let peer = chain_host(&chain, 400);
    let member = member_of(root.path(), &primary);
    save_records(&member, &encode_records(&founded.records)).expect("the member's store");
    let found = |_: &Config| {
        vec![crate::discover::DiscoveredVault {
            url: peer.url("team"),
            txt: None,
        }]
    };
    let found = PeerDiscovery::supplied(&found);

    for pass in 1..=3 {
        let result = sync_records_inner(&member, None, &[], Some(&found), None);
        assert!(matches!(result, Ok(0)), "pass {pass}: {result:?}");
    }
    let answers = peer.answers();
    peer.end();
    primary.end();
    assert_eq!(answers.len(), 3, "each pass asks the peer");
    assert_eq!(
        answers
            .iter()
            .filter(|answer| carries(&answer.body, &newer))
            .count(),
        1,
        "sent once over three passes"
    );
    assert_eq!(
        answers[1].have.first(),
        Some(child.id()),
        "the pull to the peer names what it could not take first"
    );
}

/// F23 (rule 8, failure case 1): a server that judged a member's post and still lacks its
/// frontier is posted nothing more while that frontier stands (`Reposted`). A restart loses the
/// records it held waiting (here the member's newest, held back while the server refused its
/// parent), and a server's boot id (`X-Aterm-Records-Boot`) tells the member it restarted: its
/// next pass, frontier unchanged, posts its whole store, and the lost record lands.
/// NEGATIVE CONTROL: with no boot id (main before F23) the pass after the restart posts nothing,
/// and the test fails "the member posted nothing to a server that restarted".
#[test]
fn a_member_posts_its_whole_store_to_a_server_that_restarted() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (writer, incarnation, founder, first) = entrant(1, "m1");
    let genesis = Record::found(&[7; 32], vec![first], 1_000).expect("genesis");
    let mut log = Log::new(genesis.clone()).expect("log");
    let parent = named(&mut log, &writer, incarnation, &founder, 0);
    let child = named(&mut log, &writer, incarnation, &founder, 1);
    let held = Arc::new(std::sync::Mutex::new(
        crate::membership_log::Store::decode("team", genesis.bytes()).expect("the server's store"),
    ));
    // While `refusing`, the server cannot take the parent (a store at its bound, say).
    let refusing = Arc::new(AtomicBool::new(true));
    let (answering, taking, refused) =
        (Arc::clone(&held), Arc::clone(&held), Arc::clone(&refusing));
    let parent_id = *parent.id();
    let host = RecordsHost::start_with(
        Box::new(move |have| {
            let server = answering
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let need = !have.is_empty()
                && !server
                    .log()
                    .is_some_and(|log| have.iter().all(|id| log.get(id).is_some()));
            (encode_records(&server.missing(have)), need)
        }),
        Box::new(move |body| {
            let records: Vec<Record> = decode_records(body)
                .unwrap_or_default()
                .into_iter()
                .filter(|r| !(refused.load(Ordering::SeqCst) && *r.id() == parent_id))
                .collect();
            taking
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert_many(records);
            204
        }),
    );
    host.set_boot(Boot::Id(format!("{:032x}", 1)));
    let member = member_of(root.path(), &host);
    save_records(&member, &encode_records([&genesis, &parent, &child]))
        .expect("the member's store");

    sync_records(&member, None).expect("a pass");
    assert_eq!(host.posts(), 1, "the first pass posts the whole store");
    assert!(
        held.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .log()
            .is_some_and(|log| log.get(child.id()).is_none()),
        "the child waits on the server"
    );
    sync_records(&member, None).expect("a pass");
    assert_eq!(host.posts(), 1, "a frontier the server judged went again");

    // A restart: the server keeps its complete records, loses the waiting child, can take the
    // parent now, and draws a new boot id.
    {
        let mut server = held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let complete = encode_records(&server.missing(&[]));
        *server = crate::membership_log::Store::decode("team", &complete).expect("restarted");
    }
    refusing.store(false, Ordering::SeqCst);
    host.set_boot(Boot::Id(format!("{:032x}", 2)));
    sync_records(&member, None).expect("a pass");
    let posts = host.posts();
    let landed = held
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .log()
        .is_some_and(|log| log.get(child.id()).is_some());
    host.end();
    assert_eq!(
        posts, 2,
        "the member posted nothing to a server that restarted"
    );
    assert!(landed, "the child did not land");
}

/// F23 (rule 8): a host whose boot id changes on every reply draws one whole store per
/// `REPOST_AGAIN`, not one on every pass, which round 3's fix stopped: in the first hour the
/// first post and one boot id's, and in each hour after only the one its last whole post's age
/// draws, which serves the new boot id too. The hour passes on `Reposted`'s injected clock.
/// NEGATIVE CONTROLS: no limit on boot resets posts the whole store on both passes, and the test
/// fails "assertion `left == right` failed: a boot id that changes on every reply drew a whole
/// store on each pass" (left 2, right 1). Each of these
/// fails "assertion `left == right` failed: a boot id that changes on every reply drew two whole
/// stores in an hour after the first" (left 2, right 1): a whole post of the hour that leaves
/// the boot reset as it was; and entries kept for `REPOST_AGAIN` alone, so another server's
/// pass drops the entry before the hour's whole post finds its boot id (the branch before
/// review finding 6 had both).
#[test]
fn a_boot_id_that_changes_on_every_reply_draws_one_whole_store() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let lacking = lacking(root.path(), 400);
    lacking.host.set_boot(Boot::EveryReply(0));
    sync_records(&lacking.member, None).expect("a pass");
    assert_eq!(
        lacking.host.posts(),
        1,
        "the first pass posts the whole store"
    );
    for pass in 0..2 {
        sync_records(&lacking.member, None).unwrap_or_else(|e| panic!("pass {pass}: {e}"));
    }
    let first_hour = lacking.host.posts();
    Reposted::advance(&lacking.member, REPOST_AGAIN);
    // Another server's pass judges a post first, as passes to several servers do.
    Reposted::posted(
        &lacking.member,
        "http://another.invalid/v1/team",
        &[0; 32],
        Vec::new(),
        Posting::Whole { boot_reset: false },
        None,
    );
    for pass in 0..3 {
        sync_records(&lacking.member, None).unwrap_or_else(|e| panic!("pass {pass}: {e}"));
    }
    let second_hour = lacking.host.posts() - first_hour;
    lacking.host.end();
    assert_eq!(
        first_hour - 1,
        1,
        "a boot id that changes on every reply drew a whole store on each pass"
    );
    assert_eq!(
        second_hour, 1,
        "a boot id that changes on every reply drew two whole stores in an hour after the first"
    );
}

/// F23 (rule 7): a member's whole re-post carries its complete records only. A server keeps
/// waiting records in memory only and serves none, so they bought nothing on the wire.
/// NEGATIVE CONTROL: posting `Store::encode()` (main before F23) fails "the whole re-post
/// carried a waiting record".
#[test]
fn a_whole_repost_carries_complete_records_only() {
    // Every server it reads is its own on loopback, which answers.
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (writer, incarnation, founder, first) = entrant(1, "m1");
    let genesis = Record::found(&[7; 32], vec![first], 1_000).expect("genesis");
    let mut log = Log::new(genesis.clone()).expect("log");
    let complete = named(&mut log, &writer, incarnation, &founder, 0);
    let waiting = Draft {
        network: *genesis.id(),
        sequence: 1,
        previous: None,
        parents: vec![[0x66; 32]],
        authored_ms: 3_000,
        body: Body::Name {
            name: "waiting".to_owned(),
        },
    }
    .sign(&writer, incarnation)
    .expect("signed");
    let bodies = Arc::new(std::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    let kept = Arc::clone(&bodies);
    let host = RecordsHost::start_with(
        Box::new(|have| (Vec::new(), !have.is_empty())),
        Box::new(move |body| {
            kept.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(body.to_vec());
            204
        }),
    );
    let member = member_of(root.path(), &host);
    save_records(&member, &encode_records([&genesis, &complete, &waiting]))
        .expect("the member's store");
    assert!(
        load_records(&member).expect("store").contains(waiting.id()),
        "the member keeps the record waiting"
    );
    sync_records(&member, None).expect("a pass");
    host.end();
    let bodies = bodies
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(bodies.len(), 1, "one whole re-post");
    let posted: Vec<RecordId> = decode_records(&bodies[0])
        .expect("records")
        .iter()
        .map(|r| *r.id())
        .collect();
    assert!(
        !posted.contains(waiting.id()),
        "the whole re-post carried a waiting record"
    );
    assert_eq!(posted, vec![*genesis.id(), *complete.id()]);
}

/// The two `GET`s of `/records` a pass makes of a peer whose answer is longer than its peek,
/// each answered `200` with `body`: the first at once, which the pass hangs up on once it sees
/// the length, and the second, the whole read, once `held` lets it go. Each request line, in
/// order.
fn serve_twice(held: &HeldReads, listener: TcpListener, body: &[u8]) -> Vec<String> {
    listener.set_nonblocking(true).expect("nonblocking");
    let mut lines = Vec::new();
    for read in 0..2 {
        let mut stream = aterm_spec::wait::until_within(HANG_DETECTOR, "the pass's GET", || {
            listener.accept().ok().map(|(stream, _)| stream)
        });
        stream.set_nonblocking(false).expect("blocking");
        let (line, _) = read_request(&mut stream).expect("a request");
        lines.push(line);
        if read == 0 {
            held.peeks.fetch_add(1, Ordering::SeqCst);
        } else {
            held.hold();
        }
        let mut reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        reply.extend_from_slice(body);
        // The pass hangs up on the first answer once it sees the length: the rest goes nowhere.
        let _ = stream.write_all(&reply);
    }
    lines
}

/// A records pass reads at most [`RECORDS_PULLS_AT_ONCE`] of its peers' answers longer than its
/// peek ([`fan_out::RECORDS`]) whole at once, however many peers it asks, and still takes the
/// records such an answer brings: every answer is read first under the peek, all at once, and
/// only one longer than that is asked again and read whole, under a slot held until the pass has
/// judged its records. Before, every peer's answer was read whole at once, on a thread of its
/// own, and held until all of them had ended, so a LAN host that advertised
/// [`crate::discover::MAX_CANDIDATES`] instances of itself, each answering with 64 MiB, could have
/// a pass hold up to a gigabyte of records nothing had verified. Here each of 16 peers answers its first read
/// with more than the peek and holds its whole read until the test lets it go; one of them sends
/// the records the member lacks, the rest bytes that are no records.
///
/// NEGATIVE CONTROL: the peers read with as many slots as a browse names (`fan_out::Reads {
/// at_once: crate::discover::MAX_CANDIDATES, ..fan_out::RECORDS }` in `sync_records_until`'s
/// `fetch_peers`): more whole reads in flight at once than 2, "whole reads in flight at once"
/// (`left: 7, right: 2` in one run; any count over 2 fails it). The whole read held to the peek
/// (`read_by(url, reads.peek)` in `fan_out::reads_within`'s slotted arm): the long answer is lost,
/// "the records the long answer brought".
#[test]
fn a_records_pass_reads_at_most_two_peers_long_answers_whole_at_once() {
    const PEERS: usize = crate::discover::MAX_CANDIDATES;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut fixture = lacking(root.path(), 400);
    // Records past the member's store, more of them than the peek reads.
    let mut lacked = Vec::new();
    let mut n = 7;
    while encode_records(&lacked).len() <= fan_out::RECORDS.peek {
        for _ in 0..64 {
            lacked.push(fixture.mint(n));
            n += 1;
        }
    }
    let bodies: Vec<Vec<u8>> = (0..PEERS)
        .map(|peer| {
            if peer == 0 {
                encode_records(&lacked)
            } else {
                vec![0xa5; fan_out::RECORDS.peek + 1]
            }
        })
        .collect();
    let listeners: Vec<TcpListener> = (0..PEERS)
        .map(|_| TcpListener::bind("127.0.0.1:0").expect("bind"))
        .collect();
    let urls: Vec<String> = listeners
        .iter()
        .map(|listener| format!("http://{}/v1/team", listener.local_addr().expect("addr")))
        .collect();
    let found = PeerDiscovery::selected(&urls);
    let held = HeldReads::new(fan_out::RECORDS);
    let (counted, asked) = std::thread::scope(|scope| {
        let held = &held;
        let peers: Vec<_> = listeners
            .into_iter()
            .zip(&bodies)
            .map(|(listener, body)| scope.spawn(move || serve_twice(held, listener, body)))
            .collect();
        let counted = held.count(PEERS, || {
            // Every server it reads is its own on loopback, which answers.
            let _answering = ServersAnswering::set();
            sync_records_until(&fixture.member, None, &[], Some(&found), None, None)
        });
        let asked: Vec<Vec<String>> = peers
            .into_iter()
            .map(|peer| peer.join().expect("a peer"))
            .collect();
        (counted, asked)
    });
    fixture.host.end();
    assert_eq!(
        counted.seen.in_flight, RECORDS_PULLS_AT_ONCE,
        "whole reads in flight at once"
    );
    assert_eq!(
        counted.seen.waiting,
        PEERS - RECORDS_PULLS_AT_ONCE,
        "whole reads waiting for a slot"
    );
    assert_eq!(counted.most, RECORDS_PULLS_AT_ONCE);
    assert_eq!(
        held.peeks.load(Ordering::SeqCst),
        PEERS,
        "every peer's answer read under the peek first"
    );
    for lines in &asked {
        assert_eq!(lines.len(), 2, "{lines:?}");
        for line in lines {
            assert!(
                line.starts_with("GET ") && line.contains("/records"),
                "{line}"
            );
        }
    }
    let pass = counted.result.expect("the pass");
    let store = load_records(&fixture.member).expect("the member's store");
    assert!(
        lacked
            .iter()
            .all(|record| store.complete_bytes(record.id()).is_some()),
        "the records the long answer brought"
    );
    assert_eq!(pass.added, lacked.len());
}

/// What `admit_by_proof` decides for m7's node key under a fresh key at m22 (the seat a Mac that
/// went dark moves to its new key), where m22's only answering server is a host serving m22's own
/// log that states `bound` (`None`: a server before the fence), m22's crossed mark stored where
/// `crossed`.
fn proof_through_host(crossed: bool, bound: Option<crate::crossing::RecordsBound>) -> Readmit {
    use ring::signature::{Ed25519KeyPair, KeyPair as _};

    let macs = super::log_entries_tests::two_macs(&[(
        Audience::Everyone,
        "shared",
        b"everyone-v1".as_slice(),
    )]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let pair = Ed25519KeyPair::from_seed_unchecked(&[0x0b; 32]).expect("node key");
    let n7: [u8; 32] = pair.public_key().as_ref().try_into().expect("32 bytes");
    publish_local_node_key(m7, n7, |network, actor| {
        pair.sign(&crate::membership_log::node_proof_message(network, actor))
            .as_ref()
            .try_into()
            .expect("64 bytes")
    })
    .expect("m7 publishes its node key")
    .expect("a Node record");
    post_records_to_servers(m7).expect("m7 posts its records");
    sync_records(m22, None).expect("m22 pulls m7's Node record");
    // The vault's server moved to the host, the only one that answers.
    macs.server.stop();
    let store = load_records(m22).expect("m22's store");
    let chain = Arc::new(std::sync::Mutex::new(Chain::of(
        store.log().expect("m22's log").records(),
    )));
    let host = chain_host(&chain, 204);
    host.set_bound(bound);
    let mut moved = configured(m22).expect("m22's config");
    moved.source = Source::Url(host.url(crate::ALAB_VAULT));
    save_config(m22, &moved).expect("m22 reads the host");
    let mut state = load_state(m22).expect("m22's state");
    state.source_identity = Some(source_identity(&moved));
    save_state(m22, &state).expect("m22's state follows its source");
    if crossed {
        crossing_mark(m22).store().expect("m22's mark");
    }
    let _answering = ServersAnswering::set();
    let (_, request, _) = super::log_entries_tests::newcomer(macs.root.path(), m22, "m7-fresh");
    let proven = Proven {
        reader: crate::members::bundle::Reader::BoundedLog(
            crate::crossing::RecordsBound::uncrossed(),
        ),
        request: &request,
        node: n7,
        name: "m7",
        fleet: false,
        lan: false,
        fresh: false,
        book: Book {
            node_id_held_out: false,
            node_id_seated: false,
            node_elsewhere: false,
            moved_seats_read: true,
        },
    };
    let later = aterm_time::unix_millis().expect("clock") + super::readmit::DARK_FOR_MS + 1;
    let decided = admit_by_proof(m22, &proven, later).expect("decided");
    host.end();
    drop(macs.root);
    decided
}

/// A member whose only server states no records bound did not read the vault's newest records,
/// crossed or not ([`crate::crossing`]): a server before the fence holds no more than the legacy
/// bound and takes nothing past it, and once any log of the vault crossed, every crossed node
/// refuses it, so it may not hold a removal; and a member whose own log has not crossed cannot
/// tell a vault that never crossed from such a server frozen past the crossing. A crossed
/// member's pass takes nothing from that server (it ends in an error); an uncrossed member's
/// pass takes its answer and ends `Ok`, but not fresh ([`RecordsPass::unstated`]). Neither lets
/// a key in by proof.
///
/// CONTROL: the same host stating its bound lets the key in, crossed or not, so the refusals
/// turn on the bound alone. An uncrossed member's refusal is the pass's freshness itself: with
/// `RecordsPass::fresh` ignoring `unstated` (the gate before this fix, which held only crossed
/// members), that member lets the key in.
#[test]
fn a_member_whose_only_server_states_no_bound_lets_no_key_in_by_proof() {
    for crossed in [true, false] {
        let decided = proof_through_host(crossed, None);
        let Readmit::Refused(why) = &decided else {
            panic!(
                "a member (crossed: {crossed}) read a server before the fence as fresh: \
                 {decided:?}"
            );
        };
        assert!(why.contains("newest records"), "{why}");

        let stated = proof_through_host(crossed, Some(crate::crossing::RecordsBound::uncrossed()));
        assert!(
            matches!(stated, Readmit::Let { .. }),
            "control: a host that states its bound is fresh (crossed: {crossed}): {stated:?}"
        );
    }
}

/// What [`crossing_pass`] saw: whether the pass ended `Ok`, whether this Mac's crossed mark is
/// stored after it, whether its store holds the founder's `next` and `after` records complete,
/// and whether it holds `after` waiting on a parent.
#[derive(Debug, PartialEq, Eq)]
struct Crossing {
    ok: bool,
    marked: bool,
    next: bool,
    after: bool,
    waiting: bool,
}

/// One records pass of a Mac holding [`lacking`]'s eight records, crossed (its mark stored first)
/// where `crossed`, under `faults`: its source states `source` of its records and sends the
/// founder's two next records (`next`, then `after`, which descends from it) in the range
/// `from_source`, or records the Mac holds where `from_source` is `None`; a peer beside it, where
/// `peer` is `Some`, states what `peer` holds and sends both.
fn crossing_pass(
    crossed: bool,
    faults: crate::crossing::Faults,
    source: Option<crate::crossing::RecordsBound>,
    from_source: Option<std::ops::Range<usize>>,
    peer: Option<Option<crate::crossing::RecordsBound>>,
) -> Crossing {
    let _answering = ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let mut fixture = lacking(root.path(), 400);
    let next = fixture.mint(7);
    let after = fixture.mint(8);
    let held = load_records(&fixture.member)
        .expect("the member's store")
        .missing(&[]);
    let sent = encode_records(
        &from_source.map_or(held, |range| [next.clone(), after.clone()][range].to_vec()),
    );
    let primary = RecordsHost::start(Box::new(move |_| (sent.clone(), false)), 400);
    primary.set_bound(source);
    let both = encode_records(&[next.clone(), after.clone()]);
    let peer = peer.map(|bound| {
        let host = RecordsHost::start(Box::new(move |_| (both.clone(), false)), 400);
        host.set_bound(bound);
        host
    });
    let mut moved = configured(&fixture.member).expect("the member's config");
    moved.source = Source::Url(primary.url("team"));
    save_config(&fixture.member, &moved).expect("the member reads the primary");
    let mark = crossing_mark(&fixture.member);
    if crossed {
        mark.store().expect("the member's mark");
    }
    let peers: Vec<String> = peer.iter().map(|host| host.url("team")).collect();
    let found = PeerDiscovery::selected(&peers);
    crate::crossing::inject(mark.path(), faults);
    let pass = sync_records_until(&fixture.member, None, &[], Some(&found), None, None);
    crate::crossing::inject(mark.path(), crate::crossing::Faults::default());
    let store = load_records(&fixture.member).expect("the member's store");
    let seen = Crossing {
        ok: pass.is_ok(),
        marked: mark.stored().expect("look"),
        next: store.complete_bytes(next.id()).is_some(),
        after: store.complete_bytes(after.id()).is_some(),
        waiting: store.contains(after.id()) && store.complete_bytes(after.id()).is_none(),
    };
    if let Some(peer) = peer {
        peer.end();
    }
    primary.end();
    fixture.host.end();
    seen
}

/// A marked server with this client's capacity. Numeric mismatches are tested separately.
fn crossed_bound() -> Option<crate::crossing::RecordsBound> {
    Some(crate::crossing::RecordsBound::here(true))
}

#[test]
fn numeric_incompatible_source_never_changes_records_or_crossing_state() {
    for initially_crossed in [false, true] {
        for range in [None, Some(0..2)] {
            let observed = crossing_pass(
                initially_crossed,
                crate::crossing::Faults::default(),
                Some(crate::crossing::RecordsBound::of(
                    crate::crossing::fixture::RAISED.held,
                    true,
                )),
                range,
                None,
            );
            assert_eq!(
                observed,
                Crossing {
                    ok: false,
                    marked: initially_crossed,
                    next: false,
                    after: false,
                    waiting: false,
                }
            );
        }
    }
}

/// Both branches fit an uncrossed server, but their union crosses the legacy bound.
/// Only a server that can follow that union can make the resulting pass fresh.
#[test]
fn merging_uncrossed_branches_requires_a_source_compatible_with_the_result() {
    use crate::crossing::{RecordsBound, fixture::RAISED};
    use crate::membership_log::LEGACY_RECORDS;

    let _answering = ServersAnswering::set();
    for peer_capacity in [LEGACY_RECORDS, RAISED.held] {
        RAISED.within(|| {
            let root = aterm_tempfile::tempdir().expect("tempdir");
            let mut fixture = lacking(root.path(), 400);
            while fixture.log.held_count() < LEGACY_RECORDS - 3 {
                fixture.mint(fixture.log.held_count() as u64);
            }
            let mut remote = fixture.log.clone();
            fixture.mint(10_000);
            fixture.mint(10_001);
            save_records(&fixture.member, &encode_records(fixture.log.records()))
                .expect("the local branch");
            let tail: Vec<_> = (20_000..20_003)
                .map(|n| {
                    named(
                        &mut remote,
                        &fixture.writer,
                        fixture.incarnation,
                        &fixture.founder,
                        n,
                    )
                })
                .collect();
            assert_eq!(remote.held_count(), LEGACY_RECORDS);
            let wire = encode_records(&tail);
            let source = RecordsHost::start(Box::new(move |_| (wire.clone(), true)), 400);
            source.set_bound(Some(RecordsBound::of(peer_capacity, false)));
            let mut config = configured(&fixture.member).expect("config");
            config.source = Source::Url(source.url("team"));
            save_config(&fixture.member, &config).expect("source");
            let found = PeerDiscovery::selected(&[]);
            let pass = sync_records_until(
                &fixture.member,
                None,
                &[],
                Some(&found),
                Some(Instant::now() + HANG_DETECTOR),
                None,
            )
            .expect("the valid branch is retained");
            // Inspect the durable union through the selected local storage format.
            assert_eq!(
                std::fs::read(fixture.member.records_data_file().unwrap())
                    .expect("persisted union"),
                encode_records(fixture.log.records().chain(&tail))
            );
            assert_eq!(pass.added, 3);
            assert!(!pass.untaken);
            assert!(crossing_mark(&fixture.member).stored().expect("mark"));
            assert_eq!(pass.fresh(), peer_capacity == RAISED.held);
            assert_eq!(source.posts(), 0, "a bounded freshness read does not post");
            source.end();
            fixture.host.end();
        });
    }
}

/// A Mac that has not crossed stores its crossed mark ([`crate::crossing`]) before its records
/// file holds a record a server that states its log crossed sent it; and from then on, in the
/// same pass, it takes nothing from a server that states no bound. A Mac that dies between the
/// mark and the write holds the mark and none of those records.
///
/// CONTROL (in the test): a peer that states its bound is taken in the same pass, before what the
/// crossed server sent, so the crossed server's record, which the peer sent too, brings the store
/// nothing new and stores no mark.
/// CONTROL (MUTANT `Faults::mark_after_take`, dying between): the Mac holds the crossed server's
/// record with no mark, which it would then post to a server before the fence as an uncrossed
/// Mac's.
#[test]
fn an_uncrossed_mac_stores_its_mark_before_it_holds_a_crossed_servers_records() {
    use crate::crossing::{Faults, RecordsBound};

    let unstated_peer = Some(None);
    assert_eq!(
        crossing_pass(
            false,
            Faults::default(),
            crossed_bound(),
            Some(0..1),
            unstated_peer
        ),
        Crossing {
            ok: true,
            marked: true,
            next: true,
            after: false,
            waiting: false
        },
        "the mark, the crossed server's record, and nothing of the peer that states no bound"
    );
    assert_eq!(
        crossing_pass(
            false,
            Faults::default(),
            crossed_bound(),
            Some(0..1),
            Some(Some(RecordsBound::uncrossed()))
        ),
        Crossing {
            ok: true,
            marked: false,
            next: true,
            after: true,
            waiting: false
        },
        "control: a peer that states its bound is taken first, the crossed server's record with it"
    );
    let dies = Faults {
        dies_between: true,
        ..Faults::default()
    };
    assert_eq!(
        crossing_pass(false, dies, crossed_bound(), Some(0..1), None),
        Crossing {
            ok: false,
            marked: true,
            next: false,
            after: false,
            waiting: false
        },
        "dying between: the mark and no record"
    );
    assert_eq!(
        crossing_pass(
            false,
            Faults {
                mark_after_take: true,
                ..dies
            },
            crossed_bound(),
            Some(0..1),
            None
        ),
        Crossing {
            ok: false,
            marked: false,
            next: true,
            after: false,
            waiting: false
        },
        "mutant: the crossed server's record is held with no mark"
    );
}

/// A server that states its log crossed and sends only records this Mac holds stores no mark:
/// any LAN host can answer a records pull stating a crossed bound, and a Mac it crossed would
/// take nothing from an old server, and count none fresh, for good.
///
/// CONTROL (MUTANT `Faults::mark_on_receipt`, the order before this fix): the same answer stores
/// the mark.
#[test]
fn a_crossed_servers_answer_of_records_the_mac_holds_stores_no_mark() {
    use crate::crossing::Faults;

    let seen = crossing_pass(false, Faults::default(), crossed_bound(), None, None);
    assert!(seen.ok && !seen.marked, "{seen:?}");
    let mutant = crossing_pass(
        false,
        Faults {
            mark_on_receipt: true,
            ..Faults::default()
        },
        crossed_bound(),
        None,
        None,
    );
    assert!(mutant.ok && mutant.marked, "mutant: {mutant:?}");
}

/// A Mac that has not crossed takes nothing of a crossed server's answer that brings it only
/// records waiting on a parent, and stores no mark: any key that ever held a seat (a removed
/// Mac's among them, `Log::insert`) can sign such a record, naming a parent that never comes,
/// and it would cross the Mac for good though it never completes.
///
/// CONTROL (in the test): the same answer from a server that states it has not crossed is held
/// waiting, so what the pass leaves out turns on the crossed bound alone; and a crossed server's
/// answer that brings the parent with it is taken, after the mark.
/// CONTROL (MUTANT `Faults::mark_on_waiting`, the rule before this fix): the crossed answer
/// stores the mark, and the Mac holds the record waiting.
#[test]
fn a_crossed_servers_answer_of_records_that_only_wait_stores_no_mark() {
    use crate::crossing::{Faults, RecordsBound};

    assert_eq!(
        crossing_pass(false, Faults::default(), crossed_bound(), Some(1..2), None),
        Crossing {
            ok: true,
            marked: false,
            next: false,
            after: false,
            waiting: false
        },
        "nothing taken, and no mark"
    );
    assert_eq!(
        crossing_pass(
            false,
            Faults::default(),
            Some(RecordsBound::uncrossed()),
            Some(1..2),
            None
        ),
        Crossing {
            ok: true,
            marked: false,
            next: false,
            after: false,
            waiting: true
        },
        "control: an uncrossed server's record is held waiting"
    );
    assert_eq!(
        crossing_pass(false, Faults::default(), crossed_bound(), Some(0..2), None),
        Crossing {
            ok: true,
            marked: true,
            next: true,
            after: true,
            waiting: false
        },
        "control: with its parent, the record is taken after the mark"
    );
    assert_eq!(
        crossing_pass(
            false,
            Faults {
                mark_on_waiting: true,
                ..Faults::default()
            },
            crossed_bound(),
            Some(1..2),
            None
        ),
        Crossing {
            ok: true,
            marked: true,
            next: false,
            after: false,
            waiting: true
        },
        "mutant: one waiting record crossed the Mac"
    );
}

/// A crossed Mac takes nothing from a server that states no records bound: its answer counts as
/// one that failed, and the pass, with no other source, fails.
///
/// CONTROL (in the test): the same server stating its bound is taken.
#[test]
fn a_crossed_mac_takes_nothing_from_a_server_that_states_no_bound() {
    use crate::crossing::{Faults, RecordsBound};

    assert_eq!(
        crossing_pass(true, Faults::default(), None, Some(0..2), None),
        Crossing {
            ok: false,
            marked: true,
            next: false,
            after: false,
            waiting: false
        },
        "nothing taken from a server that states no bound"
    );
    assert_eq!(
        crossing_pass(
            true,
            Faults::default(),
            Some(RecordsBound::uncrossed()),
            Some(0..2),
            None
        ),
        Crossing {
            ok: true,
            marked: true,
            next: true,
            after: true,
            waiting: false
        },
        "control: the same server stating its bound is taken"
    );
}

/// A Mac whose log crossed during a records pass (another pass of its own stored the mark while
/// this one pulled) posts nothing to a server that stated no records bound and said it lacks the
/// Mac's frontier: what it holds may now be a crossed log's.
///
/// CONTROL (in the test): the same pass, with no mark stored meanwhile, posts that server its
/// store, as an uncrossed Mac posts a server before the fence.
#[test]
fn a_mac_that_crossed_during_a_pass_posts_nothing_to_a_server_that_states_no_bound() {
    let posts = |crossing: bool| {
        let _answering = ServersAnswering::set();
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let fixture = lacking(root.path(), 204);
        let mark = crossing_mark(&fixture.member);
        let meanwhile = mark.clone();
        let host = RecordsHost::start(
            Box::new(move |_| {
                if crossing {
                    meanwhile.store().expect("the mark, meanwhile");
                }
                (Vec::new(), true)
            }),
            204,
        );
        let mut moved = configured(&fixture.member).expect("the member's config");
        moved.source = Source::Url(host.url("team"));
        save_config(&fixture.member, &moved).expect("the member reads the host");
        let found = PeerDiscovery::selected(&[]);
        let pass = sync_records_until(&fixture.member, None, &[], Some(&found), None, None);
        let posted = host.posts();
        let marked = mark.stored().expect("look");
        host.end();
        fixture.host.end();
        assert_eq!(
            pass.is_err(),
            crossing,
            "a changed crossing rejects the stale pass"
        );
        (posted, marked)
    };
    assert_eq!(posts(true), (0, true), "no post once the Mac crossed");
    assert_eq!(posts(false), (1, false), "control: an uncrossed Mac posts");
}
