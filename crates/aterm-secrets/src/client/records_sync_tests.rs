// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A member's record passes against a records server a test writes (`docs/DESIGN-fluid-swarm-
//! 2026-10-28.md` §4.1 F13).

use super::*;
use crate::membership_log::{
    Body, Entrant, Log, MemberId, Record, RecordId, decode_records, encode_records,
};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

const HANG_DETECTOR: Duration = aterm_spec::wait::MUST_HAPPEN;

/// What a [`RecordsHost`] answers a `GET` of `/records` naming `have`: the records it sends and
/// whether it says `X-Aterm-Records-Need: 1`.
type Answer = dyn Fn(&[RecordId]) -> (Vec<u8>, bool) + Send + Sync;

/// A records server on loopback whose answers a test writes: `get` answers each `GET` of
/// `/records`, and every `POST` gets `post_status`. It counts the POSTs, the records they
/// carried, and the records its GETs sent.
struct RecordsHost {
    addr: SocketAddr,
    posts: Arc<AtomicUsize>,
    posted: Arc<AtomicUsize>,
    sent: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: std::thread::JoinHandle<()>,
}

impl RecordsHost {
    fn start(get: Box<Answer>, post_status: u16) -> RecordsHost {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let posts = Arc::new(AtomicUsize::new(0));
        let posted = Arc::new(AtomicUsize::new(0));
        let sent = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (counted_posts, counted_posted, counted_sent, stopping) = (
            Arc::clone(&posts),
            Arc::clone(&posted),
            Arc::clone(&sent),
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
                    format!("HTTP/1.1 {post_status} Answer\r\n{empty}").into_bytes()
                } else if line.starts_with("GET ") && line.contains("/records") {
                    let (body, need) = get(&have_named(&line));
                    counted_sent.fetch_add(
                        decode_records(&body).map_or(0, |records| records.len()),
                        Ordering::SeqCst,
                    );
                    let need = if need {
                        format!("{}: 1\r\n", transport::RECORDS_NEED_HEADER)
                    } else {
                        String::new()
                    };
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
            stop,
            worker,
        }
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

/// The record ids a request line's `have=` names.
fn have_named(line: &str) -> Vec<RecordId> {
    let target = line.split(' ').nth(1).unwrap_or("");
    let Some((_, query)) = target.split_once("have=") else {
        return Vec::new();
    };
    query
        .split(',')
        .filter_map(|hex| {
            let mut id = [0u8; 32];
            aterm_codec::hex::decode_into(hex.as_bytes(), &mut id).then_some(id)
        })
        .collect()
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
