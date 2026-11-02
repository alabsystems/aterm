// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A member's record passes against a records server a test writes (`docs/DESIGN-fluid-swarm-
//! 2026-10-28.md` §4.1 F13).

use super::*;
use crate::membership_log::untaken::chain::{Chain, carries, have_named};
use crate::membership_log::{
    Body, Draft, Entrant, Evidence, Log, MemberId, Record, RecordId, decode_records,
    encode_records, with_kind_code,
};
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
/// records its GETs sent, and keeps every `GET` it answered.
struct RecordsHost {
    addr: SocketAddr,
    posts: Arc<AtomicUsize>,
    posted: Arc<AtomicUsize>,
    sent: Arc<AtomicUsize>,
    answers: Arc<std::sync::Mutex<Vec<Answered>>>,
    boot: Arc<std::sync::Mutex<Boot>>,
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
        let stop = Arc::new(AtomicBool::new(false));
        let (counted_posts, counted_posted, counted_sent, kept_answers, booted, stopping) = (
            Arc::clone(&posts),
            Arc::clone(&posted),
            Arc::clone(&sent),
            Arc::clone(&answers),
            Arc::clone(&boot),
            Arc::clone(&stop),
        );
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
            stop,
            worker,
        }
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
    sync_records_inner(&member, Some(Duration::ZERO), &[], Some(&found))
        .expect("the primary answered its long poll");
    let after_poll = selections.load(Ordering::SeqCst);
    let reads_after_poll = peer_reads.load(Ordering::SeqCst);
    sync_records_inner(&member, None, &[], Some(&found)).expect("a non-waiting pass asks peers");
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
    // The vault's server moved to the host, and m22's applied state follows it.
    let mut moved = configured(m22).expect("m22's config");
    moved.source = Source::Url(host.url(crate::ALAB_VAULT));
    save_config(m22, &moved).expect("m22 reads the host");
    let mut state = load_state(m22).expect("m22's state");
    state.source_identity = Some(source_identity(&moved));
    save_state(m22, &state).expect("m22's state follows its source");
    let _answering = ServersAnswering::set();

    let first = sync_records_until(m22, None, &[], None, None);
    assert!(
        !first.as_ref().is_ok_and(|pass| !pass.untaken),
        "a periodic pass takes nothing of a stream of nothing this build reads: {first:?}"
    );
    assert_eq!(
        sync_records_until(m22, None, &[], None, None).expect("a periodic pass"),
        RecordsPass {
            added: 0,
            untaken: true,
        },
        "the next pass names what it could not take, and the server sends nothing"
    );
    let (_, request, _) = super::log_entries_tests::newcomer(macs.root.path(), m22, "m7-fresh");
    let proven = Proven {
        request: &request,
        node: n7,
        name: "m7",
        fleet: false,
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
        let result = sync_records_inner(&member, None, &[], Some(&found));
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
