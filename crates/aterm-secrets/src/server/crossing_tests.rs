// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The crossed fence over HTTP ([`crate::crossing`]): a server whose vault crossed serves and
//! takes records only from a client or peer that states its records bound, and one that has not
//! crossed serves a build before the fence as it always did.
//!
//! The vault is [`fixture::Batch`]: a base log three records short of
//! [`crate::membership_log::LEGACY_RECORDS`] and a removal batch past it. A server's connection
//! threads and peer pulls hold the production bound. Mark-order tests use a
//! matching-capacity source and a retained prefix ending at F's wrap, the dangerous history a
//! legacy build reads as its own revocation. A larger crossed source is refused whole in the
//! numeric-compatibility tests. Explicit budget fixtures use the budgets seam
//! ([`fixture::RAISED`], `RecordBudgets::within`) and the test adapter
//! ([`post_records_stating`]); a refusal fixture preloads a smaller-budget store before
//! starting the production-capacity connection threads.

use super::*;
use crate::crossing::fixture::{self, Batch, Legacy, VAULT};
use crate::crossing::{Faults, Mark, RecordsBound};
use crate::membership_log::{
    LEGACY_RECORDS, RecordBudgets, RecordId, decode_records, encode_records,
};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;

const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

fn serve(store: &Path) -> Running {
    serve_holding(store, Duration::from_millis(200))
}

/// [`serve`], holding each long poll up to `hold`.
fn serve_holding(store: &Path, hold: Duration) -> Running {
    serve_tuned(store, hold, Tuning::default())
}

/// [`serve_holding`] under `tuning`.
fn serve_tuned(store: &Path, hold: Duration, tuning: Tuning) -> Running {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    launch(
        listener,
        &Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: store.to_path_buf(),
            advertise: None,
            pins: Vec::new(),
            hold,
            keep_awake: false,
        },
        tuning,
    )
    .expect("started")
}

/// Wait (a hang detector, not a budget) until `done` holds.
fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + HANG;
    while !done() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn url(running: &Running) -> String {
    format!("http://{}/v1/{VAULT}", running.addr)
}

/// The vault's crossed mark in `store`.
fn mark_in(store: &Path) -> PathBuf {
    store.join(format!("{VAULT}.crossed"))
}

/// A marked poster with the server's capacity. Larger sources are tested separately.
fn crossed() -> RecordsBound {
    RecordsBound::here(true)
}

/// One answer, read whole off the wire.
struct Answer {
    status: u16,
    head: String,
    body: Vec<u8>,
}

impl Answer {
    /// The records bound its head states, if any.
    fn bound(&self) -> Option<RecordsBound> {
        self.head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case(crate::transport::RECORDS_BOUND_HEADER)
                .then(|| RecordsBound::parse(value.trim()))
                .flatten()
        })
    }

    /// The records its body carries; none for a body that is not a records stream.
    fn records(&self) -> usize {
        decode_records(&self.body).map_or(0, |records| records.len())
    }
}

/// Send `request` as is and read the whole answer.
fn exchange(running: &Running, request: &[u8]) -> Answer {
    exchange_at(running.addr, request)
}

/// [`exchange`] with the server at `addr`.
fn exchange_at(addr: SocketAddr, request: &[u8]) -> Answer {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.set_read_timeout(Some(HANG)).expect("timeout");
    stream.set_write_timeout(Some(HANG)).expect("timeout");
    stream.write_all(request).expect("write");
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let split = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("an answer's head");
    let head = String::from_utf8_lossy(&bytes[..split]).into_owned();
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status");
    Answer {
        status,
        head,
        body: bytes[split + 4..].to_vec(),
    }
}

/// A records GET as a build before the fence sends it: no records bound.
fn legacy_get(running: &Running) -> Answer {
    exchange(
        running,
        format!("GET /v1/{VAULT}/records HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
}

/// A records GET stating `bound`.
fn stated_get(running: &Running, bound: RecordsBound) -> Answer {
    exchange(
        running,
        format!(
            "GET /v1/{VAULT}/records HTTP/1.1\r\nHost: 127.0.0.1\r\n{}: {}\r\nConnection: \
             close\r\n\r\n",
            crate::transport::RECORDS_BOUND_HEADER,
            bound.value()
        )
        .as_bytes(),
    )
}

#[test]
fn numeric_incompatible_get_and_post_leave_the_server_unchanged() {
    let batch = Batch::get();
    let root = aterm_tempfile::tempdir().unwrap();
    let running = holding_base(root.path());
    let incompatible = RecordsBound::of(
        2 * crate::membership_log::RecordBudgets::PRODUCTION.held,
        true,
    );
    let get = stated_get(&running, incompatible);
    assert_eq!(get.status, 409);
    assert_eq!(get.records(), 0);
    let posted = crate::transport::post_records_answered_until(
        &url(&running),
        &encode_records(&batch.removal),
        incompatible,
        None,
    )
    .unwrap();
    assert!(posted.is_err());
    assert_eq!(held(&running), fixture::BASE);
    assert!(!Mark::at(mark_in(root.path())).stored().unwrap());
    assert_eq!(stated_get(&running, RecordsBound::uncrossed()).status, 200);
    running.stop();
}

#[test]
fn numeric_incompatible_post_is_refused_before_reading_its_body() {
    let root = aterm_tempfile::tempdir().unwrap();
    let running = serve_tuned(
        root.path(),
        Duration::from_millis(200),
        Tuning {
            body_stall: Duration::from_millis(100),
            ..Tuning::default()
        },
    );
    let request = format!(
        "POST /v1/{VAULT}/records?{} HTTP/1.1\r\nHost: 127.0.0.1\r\n{}: {}\r\nContent-Length: 1024\r\nConnection: close\r\n\r\n",
        crate::transport::CROSSED_POST_QUERY,
        crate::transport::RECORDS_BOUND_HEADER,
        RecordsBound::of(
            2 * crate::membership_log::RecordBudgets::PRODUCTION.held,
            true
        )
        .value(),
    );
    let response = exchange(&running, request.as_bytes());
    assert_eq!(response.status, 409);
    assert_eq!(response.records(), 0);
    assert!(!Mark::at(mark_in(root.path())).stored().unwrap());
    running.stop();
}

/// A records POST of `body` as a build before the fence sends it.
fn legacy_post(running: &Running, body: &[u8]) -> Answer {
    let mut request = format!(
        "POST /v1/{VAULT}/records HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(body);
    exchange(running, &request)
}

/// What [`fenced`] says.
fn fenced_text() -> Vec<u8> {
    match fenced().body {
        Body::Text(text) => text.into_bytes(),
        _ => unreachable!("a fenced reply is text"),
    }
}

/// How many of `ids` `running` holds.
fn holds_of(running: &Running, ids: &[RecordId]) -> usize {
    let store = running.stored_records(VAULT);
    ids.iter().filter(|id| store.holds(id)).count()
}

fn ids(records: &[crate::membership_log::Record]) -> Vec<RecordId> {
    records.iter().map(|record| *record.id()).collect()
}

/// The held records of `running`'s log.
fn held(running: &Running) -> usize {
    running
        .stored_records(VAULT)
        .log()
        .map_or(0, crate::membership_log::Log::held_count)
}

/// A server that holds the base log, posted as a build before the fence posts it.
fn holding_base(store: &Path) -> Running {
    let running = serve(store);
    crate::transport::post_records(&url(&running), &encode_records(&Batch::get().base))
        .expect("the base reaches the server");
    assert_eq!(held(&running), fixture::BASE);
    running
}

/// A server whose vault crossed (a poster that states its log crossed sent it records) answers a
/// records GET and a records POST that state no bound with `409` and no record, and takes nothing
/// the POST carried; a client that states its bound is served. A build before the fence takes a
/// `409` for a source that failed, so it keeps the log it held.
///
/// CONTROL: the same GET before the vault crossed is served the base; and the stated client's
/// answer, read by a build before the fence that holds the base, is the batch cut mid-Wraps (its
/// own store holds no more than the legacy bound), which it reads as its own revocation and A's
/// and B's keys out.
#[test]
fn a_crossed_server_answers_a_request_that_states_no_bound_with_409_and_no_record() {
    let batch = Batch::get();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = holding_base(root.path());
    let before = legacy_get(&running);
    assert_eq!(before.status, 200, "{}", before.head);
    assert_eq!(
        before.records(),
        fixture::BASE,
        "control: served before it crossed"
    );

    let posted = crate::transport::post_records_answered_until(
        &url(&running),
        &encode_records(&batch.removal[..3]),
        crossed(),
        None,
    );
    assert!(
        matches!(posted, Ok(Ok(()))),
        "a crossed poster's records: {posted:?}"
    );
    assert!(
        Mark::at(mark_in(root.path())).stored().expect("look"),
        "the mark is stored before a crossed log's records are taken"
    );
    assert_eq!(
        held(&running),
        crate::membership_log::LEGACY_RECORDS,
        "the server took the batch up to its bound"
    );

    let get = legacy_get(&running);
    assert_eq!(get.status, 409, "{}", get.head);
    assert_eq!(get.body, fenced_text(), "no record frame");
    assert_eq!(get.records(), 0);
    let post = legacy_post(&running, &encode_records(&batch.removal[3..]));
    assert_eq!(post.status, 409, "{}", post.head);
    assert_eq!(post.body, fenced_text(), "no record frame");
    assert_eq!(
        holds_of(&running, &ids(&batch.removal[3..])),
        0,
        "a fenced POST takes nothing"
    );
    // What a build before the fence keeps: the base, untouched by the 409.
    let mut legacy = Legacy::holding(&batch.base);
    assert!(!legacy.read(&get.body), "a 409's text is no records stream");
    assert!(!legacy.revoked(&batch.a.recipient));
    assert!(legacy.out().is_empty());

    let served = stated_get(&running, RecordsBound::uncrossed());
    assert_eq!(served.status, 200, "{}", served.head);
    assert_eq!(served.records(), crate::membership_log::LEGACY_RECORDS);
    assert_eq!(served.bound(), Some(RecordsBound::here(true)));
    // CONTROL: had it been served, the batch cut mid-Wraps revokes A.
    assert!(legacy.read(&served.body), "control: a stream it takes");
    assert!(
        legacy.revoked(&batch.a.recipient),
        "control: the cut batch revokes A"
    );
    assert_eq!(legacy.out(), batch.cut_out(), "control: A and B are out");
    running.stop();
}

/// A server that has not crossed answers a records GET and POST that state no bound as before:
/// the whole log, which every build before the fence takes whole, beside a bound header such a
/// build ignores; and a POST it takes.
///
/// CONTROL: the same GET once the vault's mark is stored gets `409`: only crossing fences it.
#[test]
fn a_server_that_has_not_crossed_serves_a_request_that_states_no_bound_as_before() {
    let batch = Batch::get();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = holding_base(root.path());
    let get = legacy_get(&running);
    assert_eq!(get.status, 200, "{}", get.head);
    assert!(
        get.head.contains("Content-Type: application/octet-stream"),
        "{}",
        get.head
    );
    let served = decode_records(&get.body).expect("a records stream");
    assert_eq!(ids(&served), ids(&batch.base), "the whole log, in order");
    assert_eq!(get.bound(), Some(RecordsBound::here(false)));
    let mut legacy = Legacy::holding(&[]);
    assert!(
        legacy.read(&get.body),
        "a build before the fence takes it whole"
    );
    assert_eq!(legacy.held(), fixture::BASE);
    assert!(!legacy.revoked(&batch.a.recipient));
    assert!(legacy.out().is_empty());
    let post = legacy_post(&running, &encode_records(&batch.removal[..1]));
    assert_eq!(post.status, 204, "{}", post.head);
    assert_eq!(holds_of(&running, &ids(&batch.removal[..1])), 1);

    Mark::at(mark_in(root.path())).store().expect("the mark");
    let fenced = legacy_get(&running);
    assert_eq!(fenced.status, 409, "control: {}", fenced.head);
    running.stop();
}

/// A server that dies between storing its mark and taking a crossed poster's records holds the
/// mark and none of them: started again, it fences a build before the fence, which keeps the log
/// it held.
///
/// CONTROL (MUTANT `Faults::mark_after_take`): a server that takes the records first and dies
/// before its mark holds the batch cut mid-Wraps with no mark; started again, it has not crossed
/// by its own count (it holds no more than the legacy bound) and serves the cut batch to a build
/// before the fence, which reads it as A's revocation and A's and B's keys out.
#[test]
fn a_server_that_dies_between_its_mark_and_the_records_holds_the_mark_and_none_of_them() {
    let batch = Batch::get();
    let removal = ids(&batch.removal);
    let after_crash = |mark_after_take: bool| {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = holding_base(root.path());
        let mark = mark_in(root.path());
        crate::crossing::inject(
            &mark,
            Faults {
                dies_between: true,
                mark_after_take,
                ..Faults::default()
            },
        );
        let posted = crate::transport::post_records_answered_until(
            &url(&running),
            &encode_records(&batch.removal[..3]),
            crossed(),
            None,
        );
        assert!(!matches!(posted, Ok(Ok(()))), "the server died: {posted:?}");
        running.stop();
        crate::crossing::inject(&mark, Faults::default());
        let restarted = serve(root.path());
        let stored = Mark::at(mark).stored().expect("look");
        let taken = holds_of(&restarted, &removal);
        let get = legacy_get(&restarted);
        restarted.stop();
        (stored, taken, get)
    };

    let (stored, taken, get) = after_crash(false);
    assert!(stored, "the mark is stored");
    assert_eq!(taken, 0, "no record of the batch is held");
    assert_eq!(get.status, 409, "{}", get.head);
    let mut legacy = Legacy::holding(&batch.base);
    assert!(!legacy.read(&get.body));
    assert!(!legacy.revoked(&batch.a.recipient));
    assert!(legacy.out().is_empty());

    let (stored, taken, get) = after_crash(true);
    assert!(!stored, "mutant: no mark");
    assert_eq!(taken, 3, "mutant: the batch up to F's wrap is held");
    assert_eq!(get.status, 200, "mutant: served: {}", get.head);
    let mut legacy = Legacy::holding(&batch.base);
    assert!(legacy.read(&get.body), "mutant: a stream it takes whole");
    assert!(
        legacy.revoked(&batch.a.recipient),
        "mutant: the cut batch revokes A"
    );
    assert_eq!(legacy.out(), batch.cut_out(), "mutant: A and B are out");
}

/// The records a build before the fence would take from a server that capped its reply at the
/// legacy bound (the raised-bound design's partial reply, which the judged plan replaced with the
/// fence): the log's first records, the batch cut mid-Wraps, which it takes whole. The whole log
/// it refuses whole, and keeps what it held.
///
/// This is the oracle the fence answers: no reply of a crossed log reaches such a build, capped
/// or whole.
#[test]
fn a_capped_prefix_served_to_a_build_before_the_fence_revokes_a_member() {
    let batch = Batch::get();
    let whole = batch.whole();
    let capped = &whole[..crate::membership_log::LEGACY_RECORDS];

    let mut fresh = Legacy::holding(&[]);
    assert!(fresh.read(&encode_records(capped)), "a capped reply fits");
    assert!(
        fresh.revoked(&batch.a.recipient),
        "the capped reply revokes A"
    );
    assert!(
        !fresh.revoked(&batch.f.recipient),
        "F's wrap lies before the cut"
    );
    assert_eq!(fresh.out(), batch.cut_out());

    // The whole log is past the legacy bound, so its stream is built and read at a raised one.
    let stream = fixture::RAISED.within(|| encode_records(&whole));
    let mut holding = Legacy::holding(&batch.base);
    assert!(!holding.read(&stream), "the whole log is refused whole");
    assert!(!holding.revoked(&batch.a.recipient));
    assert!(holding.out().is_empty());
    // A member at the raised bound reads the whole batch: C alone is out.
    let raised = Legacy::at(fixture::RAISED, &whole);
    assert!(!raised.revoked(&batch.a.recipient));
    assert!(raised.revoked(&batch.c.recipient));
    assert_eq!(raised.out(), batch.removed());
}

/// A server whose log would hold more than the legacy bound stores its mark before it holds the
/// record that takes it there: where the mark cannot be stored, none of the POST is held. Its log
/// holds the whole fixture, past the legacy bound, so the store is made and posted to on the test
/// thread at the raised held bound ([`fixture::RAISED`]) through the test adapter.
///
/// CONTROL (MUTANT `Faults::mark_after_take`): a server that holds the records first holds the
/// 4,097th record with no mark, where the mark then cannot be stored.
#[test]
fn a_server_stores_its_mark_before_it_holds_its_4097th_record() {
    let batch = Batch::get();
    let removal = ids(&batch.removal);
    let post = |faults: Faults| {
        fixture::RAISED.within(|| {
            let root = aterm_tempfile::tempdir().expect("tempdir");
            let running = serve(root.path());
            let base = post_records(&running.shared, VAULT, &encode_records(&batch.base), true);
            assert_eq!(base.status, 204);
            let mark = mark_in(root.path());
            crate::crossing::inject(&mark, faults);
            let reply = post_records_stating(
                &running.shared,
                VAULT,
                &encode_records(&batch.removal),
                true,
                Some(RecordsBound::uncrossed()),
            );
            crate::crossing::inject(&mark, Faults::default());
            let taken = holds_of(&running, &removal);
            let stored = Mark::at(mark).stored().expect("look");
            running.stop();
            (reply.status, taken, stored)
        })
    };

    assert_eq!(
        post(Faults::default()),
        (204, removal.len(), true),
        "the whole batch, past the legacy bound, with the mark"
    );
    assert_eq!(
        post(Faults {
            fail_store: true,
            ..Faults::default()
        }),
        (500, 0, false),
        "no mark, no record past the bound"
    );
    let (status, taken, stored) = post(Faults {
        fail_store: true,
        mark_after_take: true,
        ..Faults::default()
    });
    assert_eq!(status, 500);
    assert_eq!(
        (taken, stored),
        (removal.len(), false),
        "mutant: the 4,097th record is held with no mark"
    );
}

/// A peer of a build before the fence: it answers every records GET with `body` and no records
/// bound, and counts them.
struct LegacyPeer {
    addr: SocketAddr,
    gets: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}

impl LegacyPeer {
    fn start(body: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let gets = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (counted, stopping) = (Arc::clone(&gets), Arc::clone(&stop));
        let worker = std::thread::spawn(move || {
            loop {
                let stream = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .map(|(stream, _)| stream);
                if stopping.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(mut stream) = stream else {
                    continue;
                };
                let _ = stream.set_read_timeout(Some(HANG));
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => break,
                    }
                }
                let line = String::from_utf8_lossy(&head);
                let reply = if line.starts_with("GET ") && line.contains("/records") {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let mut reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    reply.extend_from_slice(&body);
                    reply
                } else {
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_vec()
                };
                let _ = stream.write_all(&reply);
            }
        });
        Self {
            addr,
            gets,
            stop,
            worker,
        }
    }

    fn end(self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.addr);
        self.worker.join().expect("the peer ends");
    }
}

/// `addr`'s vault as a peer pull finds it.
fn peer(addr: SocketAddr) -> crate::discover::DiscoveredVault {
    crate::discover::DiscoveredVault {
        url: format!("http://{addr}/v1/{VAULT}"),
        txt: Some(VaultTxt {
            vault: VAULT.to_owned(),
            signer: "0000-0000-0000-0000-0000".to_owned(),
            seq: 1,
            head: [0; 32],
            label: Some("m2".to_owned()),
        }),
    }
}

/// A server whose vault crossed takes nothing a peer that states no records bound sends.
///
/// CONTROL: a server that has not crossed takes the same answer from the same peer.
#[test]
fn a_crossed_server_takes_nothing_from_a_peer_that_states_no_bound() {
    let batch = Batch::get();
    let legacy = LegacyPeer::start(encode_records(&batch.base));
    let pulled = |cross: bool| {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = serve(root.path());
        crate::transport::post_records(&url(&running), batch.base[0].bytes()).expect("the genesis");
        if cross {
            Mark::at(mark_in(root.path())).store().expect("the mark");
        }
        let _ = running.pull_from_peers(&[peer(legacy.addr)]);
        let held = held(&running);
        running.stop();
        held
    };
    assert_eq!(pulled(true), 1, "a crossed server takes nothing");
    assert_eq!(
        pulled(false),
        fixture::BASE,
        "control: one that has not crossed takes it"
    );
    assert_eq!(legacy.gets.load(Ordering::SeqCst), 2);
    legacy.end();
}

/// A server that has not crossed stores its mark before it takes what a crossed peer sends, and
/// then fences a build before the fence.
///
/// CONTROL (MUTANT `Faults::mark_after_take`, dying between): it holds the crossed peer's
/// records, cut at its bound, with no mark, and serves them to a build before the fence.
#[test]
fn a_server_stores_its_mark_before_it_takes_a_crossed_peers_records() {
    let batch = Batch::get();
    let source_root = aterm_tempfile::tempdir().expect("tempdir");
    let source = holding_base(source_root.path());
    let posted = crate::transport::post_records_answered_until(
        &url(&source),
        &encode_records(&batch.removal[..3]),
        crossed(),
        None,
    );
    assert!(matches!(posted, Ok(Ok(()))), "{posted:?}");
    let pulled = |mark_after_take: bool| {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = serve(root.path());
        crate::transport::post_records(&url(&running), batch.base[0].bytes()).expect("the genesis");
        let mark = mark_in(root.path());
        crate::crossing::inject(
            &mark,
            Faults {
                dies_between: mark_after_take,
                mark_after_take,
                ..Faults::default()
            },
        );
        let _ = running.pull_from_peers(&[peer(source.addr)]);
        crate::crossing::inject(&mark, Faults::default());
        let get = legacy_get(&running);
        let stored = Mark::at(mark).stored().expect("look");
        running.stop();
        (stored, get)
    };

    let (stored, get) = pulled(false);
    assert!(stored, "the mark is stored");
    assert_eq!(get.status, 409, "{}", get.head);

    let (stored, get) = pulled(true);
    assert!(!stored, "mutant: no mark");
    assert_eq!(get.status, 200, "mutant: served: {}", get.head);
    let mut legacy = Legacy::holding(&batch.base);
    assert!(legacy.read(&get.body));
    assert!(legacy.revoked(&batch.a.recipient), "mutant: A is revoked");
    source.stop();
}

/// A crossed poster's POST carries the query every server before the fence refuses before it
/// reads the body, beside its bound; one that has not crossed carries neither the query nor a
/// crossed bound. A server of this build takes the query only beside a crossed bound.
#[test]
fn a_crossed_post_carries_the_query_a_server_before_the_fence_refuses_unread() {
    let batch = Batch::get();
    let heads = |bound: RecordsBound| {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let reader = std::thread::spawn(move || {
            let (mut stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("a post");
            stream.set_read_timeout(Some(HANG)).expect("timeout");
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                assert_eq!(stream.read(&mut byte).expect("read"), 1, "a whole head");
                head.push(byte[0]);
            }
            let _ = stream.write_all(
                b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            String::from_utf8_lossy(&head).into_owned()
        });
        let _ = crate::transport::post_records_answered_until(
            &format!("http://{addr}/v1/{VAULT}"),
            batch.base[0].bytes(),
            bound,
            None,
        );
        reader.join().expect("the reader")
    };
    let crossed_head = heads(crossed());
    assert!(
        crossed_head.starts_with(&format!(
            "POST /v1/{VAULT}/records?{} ",
            crate::transport::CROSSED_POST_QUERY
        )),
        "{crossed_head}"
    );
    assert!(crossed_head.contains(&crossed().value()), "{crossed_head}");
    let plain_head = heads(RecordsBound::uncrossed());
    assert!(
        plain_head.starts_with(&format!("POST /v1/{VAULT}/records ")),
        "{plain_head}"
    );
    assert!(
        plain_head.contains(&RecordsBound::uncrossed().value()),
        "{plain_head}"
    );

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = serve(root.path());
    let body = batch.base[0].bytes();
    let mut request = format!(
        "POST /v1/{VAULT}/records?{} HTTP/1.1\r\nHost: 127.0.0.1\r\n{}: {}\r\nContent-Length: \
         {}\r\nConnection: close\r\n\r\n",
        crate::transport::CROSSED_POST_QUERY,
        crate::transport::RECORDS_BOUND_HEADER,
        RecordsBound::uncrossed().value(),
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(body);
    let refused = exchange(&running, &request);
    assert_eq!(
        refused.status, 400,
        "the query beside an uncrossed bound: {}",
        refused.head
    );
    running.stop();
}

/// The first fence accepts `crossed=1` without comparing numeric capacities. A newer crossed
/// sender must use a query that implementation rejects before reading any records.
#[test]
fn a_header_only_fence_refuses_a_new_crossed_post_without_reading_its_body() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("address");
    let (finished, finish) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut read_bodies = Vec::new();
        for turn in 0..2 {
            let (mut stream, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .expect("post");
            stream.set_read_timeout(Some(HANG)).expect("timeout");
            let mut head = Vec::new();
            let mut byte = [0; 1];
            while !head.ends_with(b"\r\n\r\n") {
                assert_eq!(stream.read(&mut byte).expect("head"), 1);
                head.push(byte[0]);
            }
            let head = String::from_utf8(head).expect("header text");
            let old_query =
                head.starts_with(&format!("POST /v1/{VAULT}/records?crossed=1 HTTP/1.1\r\n"));
            let mut body = Vec::new();
            if old_query {
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
                    .expect("length")
                    .parse::<usize>()
                    .expect("number");
                body.resize(length, 0);
                stream.read_exact(&mut body).expect("old query body");
            }
            read_bodies.push(body.len());
            let status = if old_query { 204 } else { 400 };
            let _ = write!(
                stream,
                "HTTP/1.1 {status} Answer\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            if turn == 1 {
                // Keep unread bytes from resetting the TCP connection before the client has
                // read the refusal. No record body is read while waiting for that proof.
                finish.recv_timeout(HANG).expect("client read the refusal");
            }
        }
        read_bodies
    });
    let body = Batch::get().base[0].bytes();
    let mut control = format!(
        "POST /v1/{VAULT}/records?crossed=1 HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ).into_bytes();
    control.extend_from_slice(body);
    assert_eq!(exchange_at(addr, &control).status, 204);
    let posted = crate::transport::post_records_answered_until(
        &format!("http://{addr}/v1/{VAULT}"),
        body,
        RecordsBound::of(fixture::RAISED.held, true),
        None,
    );
    finished.send(()).expect("client finished");
    let bodies = reader.join().expect("reader");
    assert!(matches!(posted, Ok(Err(_))), "{posted:?}");
    assert_eq!(bodies, [body.len(), 0]);
}

/// A crossed poster's records that a store that has not crossed takes none of store no mark: a
/// batch a store with no genesis cannot root (`422`), one a store at its bound refuses (`400`),
/// and records it holds (`204`). Any LAN host may post such a batch, stating whatever bound it
/// likes; were the mark stored on receipt, every build before the fence would get `409` from
/// this server for good, though its log never held a crossed log's record. The store at its
/// bound still serves a build before the fence.
///
/// CONTROL (MUTANT `Faults::mark_on_receipt`, the order before this fix): the `422` and the
/// `400` each leave the mark. CONTROL: a crossed poster's batch the store takes part of stores it
/// ([`a_crossed_server_answers_a_request_that_states_no_bound_with_409_and_no_record`]).
#[test]
fn a_crossed_posters_batch_a_store_takes_none_of_stores_no_mark() {
    let batch = Batch::get();
    let run = |mark_on_receipt: bool| {
        let faults = Faults {
            mark_on_receipt,
            ..Faults::default()
        };
        // A store with no genesis, posted a crossed log's batch past its genesis.
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = serve(root.path());
        let mark = mark_in(root.path());
        crate::crossing::inject(&mark, faults);
        let unrooted = post_records_stating(
            &running.shared,
            VAULT,
            &encode_records(&batch.removal),
            true,
            Some(crossed()),
        );
        crate::crossing::inject(&mark, Faults::default());
        let unrooted = (unrooted.status, Mark::at(mark).stored().expect("look"));
        running.stop();

        // A store at its explicit smaller bound: load the base under that budget, then fill
        // it through the real HTTP path up to F's wrap from a build before the fence.
        let root = aterm_tempfile::tempdir().expect("tempdir");
        fsx::write_atomic(
            &root.path().join(format!("{VAULT}.records")),
            &encode_records(&batch.base),
            0o600,
        )
        .unwrap();
        let running = fixture::LEGACY.within(|| serve(root.path()));
        let filled = legacy_post(&running, &encode_records(&batch.removal[..3]));
        assert_eq!(filled.status, 204, "{}", filled.head);
        assert_eq!(held(&running), crate::membership_log::LEGACY_RECORDS);
        let mark = mark_in(root.path());
        crate::crossing::inject(&mark, faults);
        let refused = post_records_stating(
            &running.shared,
            VAULT,
            &encode_records(&batch.removal[3..]),
            true,
            Some(crossed()),
        );
        let held_already = post_records_stating(
            &running.shared,
            VAULT,
            &encode_records(&batch.base[..2]),
            true,
            Some(crossed()),
        );
        crate::crossing::inject(&mark, Faults::default());
        let refused = (
            refused.status,
            held_already.status,
            Mark::at(mark).stored().expect("look"),
        );
        let served = legacy_get(&running).status;
        running.stop();
        (unrooted, refused, served)
    };

    assert_eq!(
        run(false),
        ((422, false), (400, 204, false), 200),
        "no mark from a batch the store takes none of, and a build before the fence is served"
    );
    let (unrooted, refused, served) = run(true);
    assert_eq!(unrooted, (422, true), "mutant: the 422 crossed the store");
    assert_eq!(
        refused,
        (400, 204, true),
        "mutant: the 400 crossed the store"
    );
    assert_eq!(served, 409, "mutant: a build before the fence is fenced");
}

/// A crossed poster's batch that brings a store that has not crossed only records waiting on a
/// parent is not taken and stores no mark. A record waits for any key that ever held a seat (a
/// removed Mac's among them, `Log::insert`), naming a parent that may never come: taken, one such
/// record would cross the vault for good, every build before the fence then refused, though the
/// store would serve none of it. The poster's whole store, which brings the parent, is taken
/// after the mark.
///
/// CONTROL (in the test): the same record posted by a build before the fence is held waiting, so
/// what the store leaves out turns on the crossed bound alone.
/// CONTROL (MUTANT `Faults::mark_on_waiting`, the rule before this fix): the crossed POST stores
/// the mark and the store holds the record waiting; a build before the fence then gets `409`.
#[test]
fn a_crossed_posters_records_that_only_wait_on_a_parent_store_no_mark() {
    let batch = Batch::get();
    // The second `[everyone]` epoch, which waits on F's `Remove C`.
    let waiting = &batch.removal[1..2];
    let id = *waiting[0].id();
    // What a store that holds the base makes of a POST of the waiting epoch alone, stating
    // `bound`, under `faults`: its answer, whether it holds the epoch at all, whether its mark
    // is stored, and what a build before the fence is answered then.
    let run = |bound: Option<RecordsBound>, faults: Faults| {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let running = holding_base(root.path());
        let mark = mark_in(root.path());
        crate::crossing::inject(&mark, faults);
        let posted = post_records_stating(
            &running.shared,
            VAULT,
            &encode_records(waiting),
            true,
            bound,
        );
        crate::crossing::inject(&mark, Faults::default());
        let kept = running.stored_records(VAULT).contains(&id);
        let marked = Mark::at(mark).stored().expect("look");
        let served = legacy_get(&running).status;
        (posted.status, kept, marked, served, running, root)
    };

    let (posted, kept, marked, served, running, root) = run(Some(crossed()), Faults::default());
    assert_eq!(
        (posted, kept, marked, served),
        (204, false, false, 200),
        "the crossed poster's waiting record is not taken, and stores no mark"
    );
    // The poster's whole store comes again: the epoch with its parent and F's wrap, which fill
    // the store to the legacy bound.
    let whole = post_records_stating(
        &running.shared,
        VAULT,
        &encode_records(&batch.removal[..3]),
        true,
        Some(crossed()),
    );
    assert_eq!(whole.status, 204);
    assert_eq!(held(&running), crate::membership_log::LEGACY_RECORDS);
    assert!(
        running.stored_records(VAULT).holds(&id),
        "the epoch, complete"
    );
    assert!(
        Mark::at(mark_in(root.path())).stored().expect("look"),
        "the mark, with the parent"
    );
    assert_eq!(legacy_get(&running).status, 409);
    running.stop();

    let (posted, kept, marked, served, running, _root) = run(None, Faults::default());
    running.stop();
    assert_eq!(
        (posted, kept, marked, served),
        (204, true, false, 200),
        "control: a build before the fence's waiting record is held waiting"
    );

    let (posted, kept, marked, served, running, _root) = run(
        Some(crossed()),
        Faults {
            mark_on_waiting: true,
            ..Faults::default()
        },
    );
    running.stop();
    assert_eq!(
        (posted, kept, marked, served),
        (204, true, true, 409),
        "mutant: one waiting record crossed the vault"
    );
}

/// A records long poll that states no bound, parked on a store that has not crossed, gets `409`
/// and no record when a crossed poster's records cross the store while it waits: it would
/// otherwise be served the batch the store took, cut mid-Wraps. A poll beside it that states its
/// bound is served those records.
///
/// CONTROL (in the test): the stated poll's answer, read by a build before the fence that holds
/// the base, revokes A and puts A and B out: what the fence kept from the poll that stated none.
#[test]
fn a_long_poll_that_states_no_bound_gets_409_when_the_store_crosses_while_it_waits() {
    let batch = Batch::get();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = serve_holding(root.path(), HANG);
    crate::transport::post_records(&url(&running), &encode_records(&batch.base))
        .expect("the base reaches the server");
    let have = running
        .stored_records(VAULT)
        .frontier()
        .iter()
        .map(|id| aterm_codec::hex::encode_lower(id))
        .collect::<Vec<_>>()
        .join(",");
    let poll = |stating: Option<RecordsBound>| {
        let bound = stating.map_or_else(String::new, |bound| {
            format!(
                "{}: {}\r\n",
                crate::transport::RECORDS_BOUND_HEADER,
                bound.value()
            )
        });
        format!(
            "GET /v1/{VAULT}/records?have={have} HTTP/1.1\r\nHost: 127.0.0.1\r\n{bound}\
             Connection: close\r\n\r\n"
        )
        .into_bytes()
    };
    let (legacy, stated) = (poll(None), poll(Some(RecordsBound::uncrossed())));
    let addr = running.addr;
    let legacy = std::thread::spawn(move || exchange_at(addr, &legacy));
    let stated = std::thread::spawn(move || exchange_at(addr, &stated));
    until("both polls are parked", || running.waiting() == 2);
    let posted = crate::transport::post_records_answered_until(
        &url(&running),
        &encode_records(&batch.removal[..3]),
        crossed(),
        None,
    );
    assert!(matches!(posted, Ok(Ok(()))), "{posted:?}");
    let legacy = legacy.join().expect("the poll that states no bound");
    let stated = stated.join().expect("the poll that states its bound");
    running.stop();

    assert_eq!(legacy.status, 409, "{}", legacy.head);
    assert_eq!(legacy.body, fenced_text(), "no record frame");
    assert_eq!(stated.status, 200, "{}", stated.head);
    assert_eq!(stated.records(), 3, "the batch up to F's wrap");
    let mut view = Legacy::holding(&batch.base);
    assert!(view.read(&stated.body), "control: a stream it takes");
    assert!(view.revoked(&batch.a.recipient), "control: A is revoked");
    assert_eq!(view.out(), batch.cut_out(), "control: A and B are out");
}

/// A crossed server answers a records POST that states no bound `409` before it reads the body:
/// a POST whose `Content-Length` names more bytes than it sends is answered at once.
///
/// CONTROL (in the test): the same short POST to the server before it crossed is not answered
/// `409`: it reads on for the rest of the body, which never comes, until the body stalls (cut
/// short here so the control ends soon). Without the fence before the body, the crossed server
/// would read on the same way, and the fence after the body is read, in `apply_records`, is
/// never reached.
#[test]
fn a_crossed_server_refuses_a_post_that_states_no_bound_before_it_reads_the_body() {
    let batch = Batch::get();
    let body = encode_records(&batch.removal[..1]);
    let short = |running: &Running| {
        let mut request = format!(
            "POST /v1/{VAULT}/records HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body.len() + 1_024
        )
        .into_bytes();
        request.extend_from_slice(&body);
        exchange(running, &request)
    };
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = serve_tuned(
        root.path(),
        Duration::from_millis(200),
        Tuning {
            body_stall: Duration::from_millis(100),
            ..Tuning::default()
        },
    );
    crate::transport::post_records(&url(&running), &encode_records(&batch.base))
        .expect("the base reaches the server");
    let open = short(&running);
    assert_ne!(open.status, 409, "control: {}", open.head);
    Mark::at(mark_in(root.path())).store().expect("the mark");
    let fenced = short(&running);
    assert_eq!(fenced.status, 409, "{}", fenced.head);
    assert_eq!(fenced.body, fenced_text(), "no record frame");
    assert_eq!(holds_of(&running, &ids(&batch.removal[..1])), 0);
    running.stop();
}

/// A records GET that states its bound twice states none: a crossed server answers it `409`.
///
/// CONTROL (in the test): the same GET stating it once is served.
#[test]
fn a_records_get_that_states_its_bound_twice_states_none() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = holding_base(root.path());
    Mark::at(mark_in(root.path())).store().expect("the mark");
    let get = |times: usize| {
        let bound = format!(
            "{}: {}\r\n",
            crate::transport::RECORDS_BOUND_HEADER,
            RecordsBound::uncrossed().value()
        )
        .repeat(times);
        exchange(
            &running,
            format!(
                "GET /v1/{VAULT}/records HTTP/1.1\r\nHost: 127.0.0.1\r\n{bound}Connection: \
                 close\r\n\r\n"
            )
            .as_bytes(),
        )
    };
    let twice = get(2);
    assert_eq!(twice.status, 409, "{}", twice.head);
    assert_eq!(twice.records(), 0);
    let once = get(1);
    assert_eq!(once.status, 200, "control: {}", once.head);
    assert_eq!(once.records(), fixture::BASE);
    running.stop();
}

/// A server that cannot look for its vault's crossed mark answers every records request `500`,
/// stating no bound, and takes nothing: a bound it cannot vouch for would cross a client of this
/// build for good (it states crossed), or let one take a crossed log's records with no mark (it
/// states uncrossed), and a crossed poster's records taken without the mark would be served to a
/// build before the fence once the look works again.
///
/// CONTROL (in the test): once the look works, the same requests are served, stating the store
/// uncrossed, and the crossed POST is taken, with its mark.
#[test]
fn a_server_that_cannot_look_for_its_mark_states_no_bound_and_takes_nothing() {
    let batch = Batch::get();
    let removal = ids(&batch.removal);
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let running = holding_base(root.path());
    let mark = mark_in(root.path());
    crate::crossing::inject(
        &mark,
        Faults {
            fail_look: true,
            ..Faults::default()
        },
    );
    let stated = stated_get(&running, RecordsBound::uncrossed());
    let legacy = legacy_get(&running);
    let posted = crate::transport::post_records_answered_until(
        &url(&running),
        &encode_records(&batch.removal[..3]),
        crossed(),
        None,
    );
    crate::crossing::inject(&mark, Faults::default());
    assert_eq!(stated.status, 500, "{}", stated.head);
    assert_eq!(stated.bound(), None, "no bound it cannot vouch for");
    assert_eq!(stated.records(), 0);
    assert_eq!(legacy.status, 500, "{}", legacy.head);
    assert_eq!(legacy.records(), 0);
    assert!(!matches!(posted, Ok(Ok(()))), "{posted:?}");
    assert_eq!(holds_of(&running, &removal), 0, "nothing taken");
    assert!(!Mark::at(mark.clone()).stored().expect("look"), "no mark");

    let served = stated_get(&running, RecordsBound::uncrossed());
    assert_eq!(served.status, 200, "control: {}", served.head);
    assert_eq!(served.bound(), Some(RecordsBound::here(false)));
    let posted = crate::transport::post_records_answered_until(
        &url(&running),
        &encode_records(&batch.removal[..3]),
        crossed(),
        None,
    );
    assert!(matches!(posted, Ok(Ok(()))), "control: {posted:?}");
    assert_eq!(
        holds_of(&running, &removal),
        3,
        "control: taken up to the bound"
    );
    assert!(
        Mark::at(mark).stored().expect("look"),
        "control: with its mark"
    );
    running.stop();
}

/// A real server replaying `SwarmRecordsCapacity`'s `Crossing` actions on its own store folder,
/// on the test thread within the configuration's budgets: `Crossing` is a server that holds the
/// base log, each POST is the test adapter ([`post_records_stating`]) with the removal batch
/// whole (a crossed poster's, `RecordsBound::here(true)`, one that states it has not crossed, or
/// for `LegacyPost` one that states no bound: its `409` the fence's, its `204` a batch taken),
/// each `…Dies` the same POST with the server dying between its mark and its records
/// (`Faults::dies_between`) and a server started anew on the folder, and `LegacyGet` a records
/// GET that states no bound, over HTTP: its `409` the fence's, its `200` a log served.
struct CapacityRun {
    root: aterm_tempfile::TempDir,
    running: Option<Running>,
    /// What the run does wrong: the faults beside `dies_between` each POST suffers, and those
    /// each GET does.
    mutant: Faults,
    /// Whether the server took records a crossed poster sent (the model's `nx`).
    taught: bool,
    /// How many of the batch's records the last answer to a request that states no bound
    /// carried (`gs`): none for a POST's.
    served: usize,
    /// Whether the last request that states no bound was answered the fence's `409` (`fenced`).
    fenced: bool,
}

impl CapacityRun {
    fn start(mutant: Faults) -> Self {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        crate::crossing::inject(&mark_in(root.path()), mutant);
        let running = serve(root.path());
        let base = post_records(
            &running.shared,
            VAULT,
            &encode_records(&Batch::get().base),
            true,
        );
        assert_eq!(base.status, 204);
        let run = Self {
            root,
            running: Some(running),
            mutant,
            taught: false,
            served: 0,
            fenced: false,
        };
        assert_eq!(held(run.running()), fixture::BASE);
        run
    }

    fn running(&self) -> &Running {
        self.running.as_ref().expect("a running server")
    }

    fn mark(&self) -> PathBuf {
        mark_in(self.root.path())
    }

    fn batch_held(&self) -> usize {
        holds_of(self.running(), &ids(&Batch::get().removal))
    }

    /// The removal batch, POSTed by a poster that states `bound` (`None` for none), the server
    /// dying between its mark and its records where `dies`.
    fn post(&mut self, bound: Option<RecordsBound>, dies: bool) {
        let before = self.batch_held();
        crate::crossing::inject(
            &self.mark(),
            Faults {
                dies_between: dies,
                ..self.mutant
            },
        );
        let reply = post_records_stating(
            &self.running().shared,
            VAULT,
            &encode_records(&Batch::get().removal),
            true,
            bound,
        );
        crate::crossing::inject(&self.mark(), self.mutant);
        if dies {
            assert_eq!(reply.status, 500, "the server died");
            self.running.take().expect("a running server").stop();
            self.running = Some(serve(self.root.path()));
        } else if bound.is_some() {
            assert_eq!(reply.status, 204);
        } else {
            assert!(matches!(reply.status, 204 | 409), "{}", reply.status);
            self.fenced = reply.status == 409;
            self.served = 0;
        }
        if bound.is_some_and(|bound| bound.crossed) && self.batch_held() > before {
            self.taught = true;
        }
    }

    fn apply(&mut self, action: &str) {
        match action {
            "CrossedPost" => self.post(Some(RecordsBound::here(true)), false),
            "CrossedPostDies" => self.post(Some(RecordsBound::here(true)), true),
            "Grow" => self.post(Some(RecordsBound::uncrossed()), false),
            "GrowDies" => self.post(Some(RecordsBound::uncrossed()), true),
            "LegacyPost" => self.post(None, false),
            "LegacyGet" => {
                // The slot first, on this thread within its budgets: a GET's connection thread
                // would read a file past the legacy bound at the production one.
                let _ = self.batch_held();
                let get = legacy_get(self.running());
                self.fenced = get.status == 409;
                self.served = match get.status {
                    200 => {
                        let removal = ids(&Batch::get().removal);
                        decode_records(&get.body)
                            .expect("a records stream")
                            .iter()
                            .filter(|record| removal.contains(record.id()))
                            .count()
                    }
                    409 => 0,
                    other => panic!("LegacyGet answered {other}: {}", get.head),
                };
            }
            other => panic!("not a Crossing action: {other}"),
        }
    }

    fn stop(mut self) {
        crate::crossing::inject(&self.mark(), Faults::default());
        if let Some(running) = self.running.take() {
            running.stop();
        }
    }
}

/// `SwarmRecordsCapacity`'s variables, read off a real server in its `Crossing`: `nr` how many
/// of the batch's records it holds, `mark` whether its crossed mark is stored, `nx` whether
/// records a crossed poster sent were taken, and of the last records request that stated no
/// bound (a GET or a POST), `gs` how many of the batch's records its answer carried and `fenced`
/// whether it was the `409`.
fn records_capacity_crossing(run: &CapacityRun) -> aterm_spec::interp::State {
    let count = |n: usize| i64::try_from(n).expect("a count");
    aterm_spec::interp::State::from([
        ("sc", 2),
        ("w", 0),
        ("k", 0),
        ("last", 0),
        ("lw", 0),
        ("lk", 0),
        ("nr", count(run.batch_held())),
        ("nx", i64::from(run.taught)),
        (
            "mark",
            i64::from(Mark::at(run.mark()).stored().expect("look")),
        ),
        ("gs", count(run.served)),
        ("fenced", i64::from(run.fenced)),
    ])
}

/// The budgets a server holds `model`'s `Crossing` within: the explicit legacy bound where
/// `CX` past the base is `LX`, and production capacity where it takes the whole batch.
fn crossing_budgets(model: &aterm_spec::derive::Model) -> RecordBudgets {
    let constant = |name: &str| {
        model
            .consts
            .iter()
            .find(|(constant, _)| *constant == name)
            .map(|&(_, value)| usize::try_from(value).expect("a count"))
            .expect("the model's constant")
    };
    let batch = Batch::get();
    assert_eq!(constant("R"), batch.removal.len());
    assert_eq!(fixture::BASE + constant("LX"), LEGACY_RECORDS);
    if constant("CX") == constant("LX") {
        fixture::LEGACY
    } else {
        assert_eq!(constant("CX"), batch.removal.len());
        RecordBudgets::PRODUCTION
    }
}

/// S8: a vault server refines `SwarmRecordsCapacity`'s `Crossing` in each configuration the
/// model declares (the legacy held total, where it takes the batch up to the bound; the
/// activation's, where it takes it all, past the bound): from every state the model reaches,
/// every action it enables, replayed on a real server down the walk's path to that state (a
/// fresh store folder each run, which goes on from the state an action took it to, by the walk's
/// path, to judge that state's next action), gives the state the model's walk found by that
/// path, or another successor the model allows there. At the legacy held total a POST from a
/// source that has not crossed (`Grow`, or `LegacyPost` before any mark) leaves the server
/// holding the batch cut mid-`Wrap`s with no mark, which a legacy GET is served whole: the
/// residual the model names, which the activation's room ends.
///
/// NEGATIVE CONTROLS: with `Faults::capped_prefix` (a crossed log's first records, capped at the
/// legacy bound, for a GET that states no bound) the legacy GET after a crossed POST at the
/// activation's held total, past the bound, carries the batch cut mid-`Wrap`s, which is no
/// successor of `LegacyGet` and breaks `LegacyNeverPartial`. With `Faults::mark_after_take`
/// (the records before the mark) a server that dies between them holds the cut batch with no
/// mark, which is no successor of `CrossedPostDies` and breaks `CrossedBeforeHeld`. A marked
/// server that takes a POST stating no bound as it takes one stating it has not crossed (`Grow`'s,
/// the POST without its fence) holds the batch's first records, which is no successor of
/// `LegacyPost`.
#[test]
fn the_crossed_fence_conforms_to_the_capacity_model() {
    let name = aterm_models_swarm::records_capacity_model().name;
    let configurations = aterm_spec::xref::model_configurations()
        .into_iter()
        .find(|configurations| configurations[0].name == name)
        .expect("SwarmRecordsCapacity is registered");
    let crossing = [
        "CrossedPost",
        "CrossedPostDies",
        "Grow",
        "GrowDies",
        "LegacyPost",
        "LegacyGet",
    ];
    for model in &configurations {
        let budgets = crossing_budgets(model);
        let start = model.successors("Crossing", &model.init_state());
        assert_eq!(start.len(), 1);
        let mut paths: BTreeMap<aterm_spec::interp::State, Vec<&'static str>> =
            BTreeMap::from([(start[0].clone(), Vec::new())]);
        let mut queue = std::collections::VecDeque::from([start[0].clone()]);
        while let Some(state) = queue.pop_front() {
            for action in &model.actions {
                if model.action_enabled(action.name, &state) {
                    assert!(crossing.contains(&action.name), "{}", action.name);
                }
                for next in model.successors(action.name, &state) {
                    if !paths.contains_key(&next) {
                        let mut path = paths[&state].clone();
                        path.push(action.name);
                        paths.insert(next.clone(), path);
                        queue.push_back(next);
                    }
                }
            }
        }
        let found_by: BTreeMap<&[&str], &aterm_spec::interp::State> = paths
            .iter()
            .map(|(state, path)| (path.as_slice(), state))
            .collect();
        budgets.within(|| {
            // The actions each state enables that no run has judged from it yet, by the walk's
            // path to the state.
            let mut pending: BTreeMap<&[&str], Vec<&'static str>> = paths
                .iter()
                .map(|(state, path)| {
                    let enabled = model
                        .actions
                        .iter()
                        .map(|action| action.name)
                        .filter(|action| model.action_enabled(action, state))
                        .collect();
                    (path.as_slice(), enabled)
                })
                .collect();
            let mut froms: Vec<&[&str]> = pending.keys().copied().collect();
            froms.sort_by_key(|path| path.len());
            let mut judged = BTreeSet::new();
            let mut runs = 0;
            for from in froms {
                while !pending[from].is_empty() {
                    runs += 1;
                    let mut run = CapacityRun::start(Faults::default());
                    assert_eq!(records_capacity_crossing(&run), start[0], "after Crossing");
                    judged.insert(start[0].clone());
                    for step in from {
                        run.apply(step);
                    }
                    let mut path = from.to_vec();
                    loop {
                        let state = found_by[path.as_slice()];
                        let action = pending.get_mut(path.as_slice()).expect("a state").remove(0);
                        run.apply(action);
                        let observed = records_capacity_crossing(&run);
                        path.push(action);
                        let Some(&next) = found_by.get(path.as_slice()) else {
                            assert!(
                                model.successors(action, state).contains(&observed),
                                "{action} after {:?} gave {observed:?}",
                                &path[..path.len() - 1]
                            );
                            break;
                        };
                        assert_eq!(&observed, next, "after {path:?}");
                        judged.insert(next.clone());
                        if pending[path.as_slice()].is_empty() {
                            break;
                        }
                    }
                    run.stop();
                }
            }
            assert_eq!(
                judged.len(),
                paths.len(),
                "every state was replayed and judged"
            );
            eprintln!(
                "SwarmRecordsCapacity Crossing (held {}): {} states judged on {runs} runs",
                budgets.held,
                judged.len()
            );
        });
    }

    let committed = &configurations[0];
    let legacy_budgets = crossing_budgets(committed);
    let start = committed.successors("Crossing", &committed.init_state())[0].clone();
    // Negative control: the capped reply, after a crossed POST past the bound.
    let activation = &configurations[1];
    let posted = activation.successors(
        "CrossedPost",
        &activation.successors("Crossing", &activation.init_state())[0],
    )[0]
    .clone();
    let capped = crossing_budgets(activation).within(|| {
        let mut run = CapacityRun::start(Faults {
            capped_prefix: true,
            ..Faults::default()
        });
        run.apply("CrossedPost");
        run.apply("LegacyGet");
        let observed = records_capacity_crossing(&run);
        run.stop();
        observed
    });
    assert_eq!(
        (capped["nr"], capped["gs"], capped["fenced"]),
        (5, 3, 0),
        "mutant: the batch cut mid-Wraps is served"
    );
    assert!(
        !activation
            .successors("LegacyGet", &posted)
            .contains(&capped)
    );
    assert!(!activation.check_invariant("LegacyNeverPartial", &capped));
    // Negative control: the records before the mark, the server dying between them.
    let unmarked = legacy_budgets.within(|| {
        let mut run = CapacityRun::start(Faults {
            mark_after_take: true,
            ..Faults::default()
        });
        run.apply("CrossedPostDies");
        let observed = records_capacity_crossing(&run);
        run.stop();
        observed
    });
    assert_eq!(
        (unmarked["nr"], unmarked["mark"]),
        (3, 0),
        "mutant: the cut batch is held with no mark"
    );
    assert!(
        !committed
            .successors("CrossedPostDies", &start)
            .contains(&unmarked)
    );
    assert!(!committed.check_invariant("CrossedBeforeHeld", &unmarked));
    // Negative control: the POST without its fence (one that states no bound, taken as one that
    // states it has not crossed), after a crossed POST died past its mark. The walk above
    // replayed the fenced `LegacyPost` from this state.
    let died = committed.successors("CrossedPostDies", &start)[0].clone();
    assert_eq!((died["nr"], died["mark"]), (0, 1));
    let unfenced = legacy_budgets.within(|| {
        let mut run = CapacityRun::start(Faults::default());
        run.apply("CrossedPostDies");
        run.post(Some(RecordsBound::uncrossed()), false);
        let observed = records_capacity_crossing(&run);
        run.stop();
        observed
    });
    assert_eq!(
        (unfenced["nr"], unfenced["mark"], unfenced["fenced"]),
        (3, 1, 0),
        "mutant: the batch's first records taken past the mark"
    );
    assert!(
        !committed
            .successors("LegacyPost", &died)
            .contains(&unfenced)
    );
}
