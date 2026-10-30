// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Non-blocking request-lane and parker tests for [`Lanes`].

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;

use super::control_lanes::{Census, LaneLimits, LaneReader, LaneService, Lanes, Outbox, Unread};
use super::{
    ActiveHandle, MAX_REQUEST_LINE, Request, Scope, ServeDisposition, authenticate_connection,
    control_denials, read_request_line,
};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

const BIG: usize = 4 << 20;

#[derive(Default)]
struct Stub {
    headers: Mutex<Vec<String>>,
}

impl Stub {
    fn headers(&self) -> Vec<String> {
        self.headers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl LaneService for Stub {
    fn authenticate(
        &self,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
    ) -> Option<(Scope, crate::control::Principal, Option<String>)> {
        if read_request_line(reader)? == "AUTH ok" {
            return Some((Scope::Owner, crate::control::Principal::Instance, None));
        }
        let _ = (&*stream).write_all(b"ERR auth\n");
        None
    }

    fn serve_line(
        &self,
        request: Request,
        _scope: Scope,
        _stream: &CtlStream,
        reader: &mut LaneReader<'_>,
        out: &mut Outbox,
    ) -> Option<ServeDisposition> {
        let line = request.line();
        if line == "quit" {
            return Some(ServeDisposition::Close);
        }
        if line == "big" {
            let mut big = vec![b'x'; BIG];
            big.push(b'\n');
            out.push_owned(big);
            return None;
        }
        if let Some(n) = line.strip_prefix("feed-bin ") {
            let n: usize = n.parse().expect("the tests send a length");
            let mut body = vec![0u8; n];
            if reader.read_exact(&mut body).is_err() {
                return Some(ServeDisposition::Close);
            }
            out.extend_from_slice(
                format!("OK {n} {}\n", String::from_utf8_lossy(&body)).as_bytes(),
            );
            return None;
        }
        out.extend_from_slice(format!("OK {line}\n").as_bytes());
        None
    }

    fn frame_header(&self, request: &Request, _scope: Scope, out: &mut Outbox) -> bool {
        let line = request.line();
        self.headers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(line.to_string());
        if line.starts_with("@0 ") {
            out.extend_from_slice(b"ERR refused\n");
            return false;
        }
        true
    }

    fn is_wait(&self, _request: &Request, _scope: Scope) -> bool {
        false
    }

    fn subscribe(&self, _request: Request, _scope: Scope, _stream: CtlStream) {}

    fn release(&self, _id: u64) {}

    fn has_live_claim(&self, _id: u64) -> bool {
        false
    }
}

struct Rig {
    lanes: Arc<Lanes>,
    stub: Arc<Stub>,
}

impl Rig {
    fn new() -> Self {
        Self::with_open(16)
    }

    fn with_open(open: usize) -> Self {
        let lanes = Lanes::new(LaneLimits {
            rpc: 1,
            wait: 0,
            inline: 0,
            open,
            subscriptions: 1,
        });
        assert_eq!(lanes.start().rpc, 1, "the request lane started");
        let stub = Arc::new(Stub::default());
        lanes.publish(Arc::clone(&stub) as Arc<dyn LaneService>);
        Self { lanes, stub }
    }

    fn dial(&self) -> Option<CtlStream> {
        let (client, server) = CtlStream::pair().expect("socket pair");
        client
            .set_read_timeout(Some(HANG))
            .expect("bound client reads");
        self.lanes.admit(server).ok()?;
        Some(client)
    }

    #[track_caller]
    fn settle(&self, what: &str, want: impl Fn(Census) -> bool) -> Census {
        let deadline = Instant::now() + HANG;
        loop {
            let census = self.lanes.census();
            if want(census) {
                return census;
            }
            assert!(
                Instant::now() < deadline,
                "the lanes never settled ({what}): {census:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[track_caller]
    fn answers_a_fresh_client_while(&self, parked: usize, what: &str) {
        self.settle(what, |census| census.rpc == 0 && census.parked == parked);
        let fresh = self
            .dial()
            .expect("a fresh client is admitted: the lane is free");
        (&fresh).write_all(b"AUTH ok\nping\n").expect("request");
        assert_eq!(line_of(&fresh), "OK ping", "a fresh client is answered");
    }
}

fn line_of(client: &CtlStream) -> String {
    next_line(&mut BufReader::new(client))
}

fn next_line(reader: &mut impl std::io::BufRead) -> String {
    let mut reply = String::new();
    match reader.read_line(&mut reply) {
        Ok(0) => "<EOF>".to_string(),
        Ok(_) => reply.trim_end().to_string(),
        Err(error) => format!("<{error}>"),
    }
}

#[test]
fn an_unread_answer_keeps_no_lane_and_arrives_whole_in_order() {
    let rig = Rig::new();
    let paused = rig.dial().expect("admitted");
    (&paused)
        .write_all(b"AUTH ok\nbig\nping\n")
        .expect("requests");
    rig.answers_a_fresh_client_while(1, "the unread answer parked");

    let mut reader = BufReader::new(&paused);
    let mut answer = Vec::new();
    reader
        .read_until(b'\n', &mut answer)
        .expect("the paused reader reads its answer");
    assert_eq!(answer.len(), BIG + 1, "the whole answer arrives");
    assert!(answer[..BIG].iter().all(|byte| *byte == b'x'));
    let mut next = String::new();
    reader.read_line(&mut next).expect("the next answer");
    assert_eq!(
        next, "OK ping\n",
        "the pipelined request is answered after it"
    );
}

#[test]
fn an_answer_whose_peer_hangs_up_unread_is_dropped() {
    let rig = Rig::new();
    let gone = rig.dial().expect("admitted");
    (&gone).write_all(b"AUTH ok\nbig\n").expect("request");
    rig.settle("the unread answer parked", |census| census.parked == 1);
    drop(gone);
    rig.settle("the connection closed", |census| census.open == 0);
}

#[test]
fn a_begun_request_keeps_no_lane_and_is_served_once_whole() {
    let rig = Rig::new();
    let halfway = rig.dial().expect("admitted");
    (&halfway)
        .write_all(b"AUTH ok\nvers")
        .expect("half a request");
    rig.answers_a_fresh_client_while(1, "the begun request parked");
    (&halfway).write_all(b"ion\n").expect("the rest");
    assert_eq!(line_of(&halfway), "OK version", "nothing begun was lost");
}

#[test]
fn a_frame_header_is_acted_on_at_once_and_its_body_keeps_no_lane() {
    let rig = Rig::new();
    let framing = rig.dial().expect("admitted");
    (&framing)
        .write_all(b"AUTH ok\nfeed-bin 6\nabc")
        .expect("a header and half a body");
    let deadline = Instant::now() + HANG;
    while rig.stub.headers().is_empty() {
        assert!(
            Instant::now() < deadline,
            "the header was never acted on before its body came"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        rig.stub.headers(),
        ["feed-bin 6"],
        "the header was acted on before its body came"
    );
    rig.answers_a_fresh_client_while(1, "the begun frame parked");
    (&framing).write_all(b"def").expect("the rest of the body");
    assert_eq!(line_of(&framing), "OK 6 abcdef");
    assert_eq!(
        rig.stub.headers(),
        ["feed-bin 6"],
        "a header is acted on once"
    );
}

#[test]
fn a_large_request_arriving_slowly_is_held_off_the_heap() {
    const BODY: usize = 200_000;
    let rig = Rig::new();
    let client = rig.dial().expect("admitted");
    let header = format!("feed-bin {BODY}\n");
    (&client)
        .write_all(format!("AUTH ok\n{header}").as_bytes())
        .expect("the header");
    (&client)
        .write_all(&vec![b'z'; BODY / 2])
        .expect("half the body");
    rig.answers_a_fresh_client_while(1, "the half-sent frame parked");
    rig.settle("half the body gathered", |census| {
        census.held == header.len() + BODY
    });
    let mut rest = vec![b'z'; BODY - BODY / 2];
    rest.extend_from_slice(b"ping\n");
    (&client)
        .write_all(&rest)
        .expect("the rest, and a request behind it");
    let mut reader = BufReader::new(&client);
    let answer = next_line(&mut reader);
    assert_eq!(
        answer.len(),
        format!("OK {BODY} ").len() + BODY,
        "the frame served whole"
    );
    assert_eq!(
        next_line(&mut reader),
        "OK ping",
        "and the request behind it"
    );

    let line = vec![b'y'; 40 * 1024];
    (&client)
        .write_all(&line)
        .expect("a line with no newline yet");
    rig.settle("the begun line gathered", |census| {
        census.parked == 1 && census.held == MAX_REQUEST_LINE + 1
    });
    (&client).write_all(b"\n").expect("its newline");
    assert_eq!(
        next_line(&mut reader).len(),
        "OK ".len() + line.len(),
        "the line served whole"
    );
}

#[test]
fn a_frame_refused_on_its_header_closes_without_waiting_for_its_body() {
    let rig = Rig::new();
    let refused = rig.dial().expect("admitted");
    (&refused)
        .write_all(b"AUTH ok\n@0 feed-bin 6\nab")
        .expect("a header and part of a body");
    assert_eq!(line_of(&refused), "ERR refused");
    assert_eq!(line_of(&refused), "<EOF>", "and the connection is closed");
    rig.settle("the connection closed", |census| census.open == 0);
}

#[test]
fn an_over_long_line_is_answered_why_then_closed() {
    let refusal = "ERR request line too long (max 65536 bytes)";
    let over_long = |head: &[u8]| {
        let mut wire = head.to_vec();
        wire.resize(head.len() + MAX_REQUEST_LINE + 1, b'x');
        wire.extend_from_slice(b"\nping\n");
        wire
    };
    for open in [16, 0] {
        let rig = Rig::with_open(open);
        let client = rig.dial().expect("admitted");
        let wire = over_long(b"AUTH ok\n");
        std::thread::scope(|scope| {
            scope.spawn(|| (&client).write_all(&wire));
            let mut reader = BufReader::new(&client);
            assert_eq!(next_line(&mut reader), refusal, "open={open}");
            let after = next_line(&mut reader);
            assert!(
                after.starts_with('<'),
                "open={open}: `ping` was served: {after}"
            );
        });
        rig.settle("the connection closed", |census| census.open == 0);
    }
    let (client, server) = CtlStream::pair().expect("socket pair");
    client
        .set_read_timeout(Some(HANG))
        .expect("bound client reads");
    let wire = over_long(b"AUTH ");
    std::thread::scope(|scope| {
        scope.spawn(|| (&client).write_all(&wire));
        let handshake = scope.spawn(move || {
            let active: ActiveHandle = Arc::new(Mutex::new(None));
            let denials = control_denials::AuthDenials::new();
            let mut unread = Unread::default();
            let mut reader = LaneReader::new(&server, &mut unread);
            let deadline = Instant::now() + HANG;
            authenticate_connection(&server, &mut reader, TOKEN, &active, &denials, deadline)
                .is_some()
        });
        assert_eq!(line_of(&client), refusal, "the handshake's line");
        assert!(!handshake.join().expect("the handshake returns"));
    });
}

struct FramedWaits;

impl LaneService for FramedWaits {
    fn authenticate(
        &self,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
    ) -> Option<(Scope, crate::control::Principal, Option<String>)> {
        Stub::default().authenticate(stream, reader)
    }

    fn serve_line(
        &self,
        request: Request,
        scope: Scope,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
        out: &mut Outbox,
    ) -> Option<ServeDisposition> {
        let request = match request.line().split_once(" paste-bin ") {
            Some((_, n)) => Request::parse(format!("feed-bin {n}")),
            None => request,
        };
        Stub::default().serve_line(request, scope, stream, reader, out)
    }

    fn is_wait(&self, request: &Request, _scope: Scope) -> bool {
        request.line().contains(" paste-bin ")
    }

    fn subscribe(&self, _request: Request, _scope: Scope, _stream: CtlStream) {}

    fn release(&self, _id: u64) {}

    fn has_live_claim(&self, _id: u64) -> bool {
        false
    }
}

#[test]
fn a_framed_wait_refused_busy_closes_and_serves_none_of_its_body() {
    for open in [16, 0] {
        let lanes = Lanes::new(LaneLimits {
            rpc: 1,
            wait: 0,
            inline: 0,
            open,
            subscriptions: 1,
        });
        assert_eq!(lanes.start().rpc, 1, "the request lane started");
        lanes.publish(Arc::new(FramedWaits) as Arc<dyn LaneService>);
        let (client, server) = CtlStream::pair().expect("socket pair");
        client
            .set_read_timeout(Some(HANG))
            .expect("bound client reads");
        assert!(lanes.admit(server).is_ok(), "admitted");
        let body = b"signal kill\n@1 send rm -rf ~\n";
        let mut wire = format!("AUTH ok\n@2 paste-bin {}\n", body.len()).into_bytes();
        wire.extend_from_slice(body);
        wire.extend_from_slice(b"ping\n");
        (&client).write_all(&wire).expect("the frame");
        let mut reader = BufReader::new(&client);
        let mut next = || {
            let mut answer = String::new();
            match reader.read_line(&mut answer) {
                Ok(0) => "<EOF>".to_string(),
                Ok(_) => answer.trim_end().to_string(),
                Err(error) => format!("<{error}>"),
            }
        };
        assert_eq!(
            next(),
            aterm_types::control_verbs::CONTROL_BUSY_LINE.trim_end(),
            "open={open}"
        );
        assert_eq!(
            next(),
            "<EOF>",
            "open={open}: the refused frame's body was served as requests"
        );
    }
}

struct Forwarder {
    store: super::Store,
    dir: std::path::PathBuf,
}

impl LaneService for Forwarder {
    fn authenticate(
        &self,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
    ) -> Option<(Scope, crate::control::Principal, Option<String>)> {
        Stub::default().authenticate(stream, reader)
    }

    fn serve_line(
        &self,
        request: Request,
        scope: Scope,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
        out: &mut Outbox,
    ) -> Option<ServeDisposition> {
        if super::try_proxy_forward(&request, scope, &self.store, &self.dir, stream, reader) {
            return Some(ServeDisposition::Close);
        }
        out.extend_from_slice(format!("OK {}\n", request.line()).as_bytes());
        None
    }

    fn is_wait(&self, request: &Request, scope: Scope) -> bool {
        super::is_wait_request(request, scope, &self.store)
    }

    fn subscribe(&self, _request: Request, _scope: Scope, _stream: CtlStream) {}

    fn release(&self, _id: u64) {}

    fn has_live_claim(&self, id: u64) -> bool {
        super::connection_has_live_claim(&self.store, id, crate::metrics::now_us())
    }
}

fn stand_in_sibling(listener: &std::os::unix::net::UnixListener, store: &super::Store) -> usize {
    let active: ActiveHandle = Arc::new(Mutex::new(None));
    let mut served = 0;
    while served == 0 {
        let Ok((mut conn, _)) = listener.accept() else {
            return served;
        };
        let _ = conn.set_read_timeout(Some(HANG));
        let Some(line) = line_after_probe(&conn) else {
            continue;
        };
        served += 1;
        let head = aterm_types::control_request::head(&line);
        super::run_subscribe_socket(head, &active, store, Scope::Owner, &mut conn);
    }
    served
}

fn line_after_probe(conn: &CtlStream) -> Option<String> {
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    loop {
        line.clear();
        if !reader.read_line(&mut line).is_ok_and(|read| read > 0) {
            return None;
        }
        let request = line.strip_suffix('\n').unwrap_or(&line);
        if !(request.starts_with("TOKEN ") && request.ends_with(" version")) {
            return Some(request.to_string());
        }
        (&*conn).write_all(b"OK version\n").ok()?;
    }
}

#[test]
fn a_relayed_subscription_keeps_no_request_lane() {
    relay_one_subscription(0, |sid| format!("subscribe @{sid} events"));
    relay_one_subscription(1, |sid| format!("subscribe\t@{sid}\tevents"));
}

fn relay_one_subscription(run: u32, spell: fn(&str) -> String) {
    use aterm_session::LaunchNonce;
    let scratch = crate::test_scratch::short_dir(&format!("run {run}'s sibling socket"));
    let dir = scratch.path().to_path_buf();
    let sock = dir.join("aterm-77101.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind the sibling");
    std::fs::write(dir.join("aterm-77101.token"), "cafe0123\n").expect("its token");
    let hosted = crate::session_store::test_handle(0);
    let sid = hosted.sid.clone();
    let sibling_store = crate::session_store::new_store();
    sibling_store
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .register(hosted);
    crate::proxy::write_graph_entry(
        &dir,
        &sid,
        &sock.to_string_lossy(),
        &LaunchNonce::generate(),
    );
    let sibling = std::thread::spawn(move || stand_in_sibling(&listener, &sibling_store));

    let lanes = Lanes::new(LaneLimits {
        rpc: 1,
        wait: 1,
        inline: 0,
        open: 16,
        subscriptions: 1,
    });
    assert_eq!(lanes.start().rpc, 1, "the request lane started");
    lanes.publish(Arc::new(Forwarder {
        store: crate::session_store::new_store(),
        dir: dir.clone(),
    }) as Arc<dyn LaneService>);
    let dial = || {
        let (client, server) = CtlStream::pair().expect("socket pair");
        client
            .set_read_timeout(Some(HANG))
            .expect("bound client reads");
        lanes.admit(server).ok().map(|()| client)
    };

    let watcher = dial().expect("the watcher is admitted");
    (&watcher)
        .write_all(format!("AUTH ok\n{}\n", spell(sid.as_str())).as_bytes())
        .expect("the subscription");
    assert_eq!(
        line_of(&watcher),
        "OK subscribe 1",
        "the sibling answered through the relay"
    );
    let census = lanes.census();
    assert_eq!(
        census.waits, 1,
        "the live relay holds a wait lane, not the request lane: {census:?}"
    );
    aterm_spec::wait::until_within(HANG, "the request lane free", || {
        (lanes.census().rpc == 0).then_some(())
    });
    let fresh = dial().expect("a fresh client is admitted while the relay lives");
    (&fresh).write_all(b"AUTH ok\nping\n").expect("request");
    assert_eq!(line_of(&fresh), "OK ping", "a fresh client is answered");

    drop(watcher);
    aterm_spec::wait::until_within(HANG, "the relay to end with its watcher", || {
        sibling.is_finished().then_some(())
    });
    assert_eq!(
        sibling.join().expect("the stand-in sibling returns"),
        1,
        "one subscription was relayed"
    );
}

struct Dribbler {
    stop: Arc<AtomicBool>,
    sent: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl Dribbler {
    fn start(client: CtlStream, head: &'static [u8]) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicUsize::new(0));
        let (stopping, counting) = (Arc::clone(&stop), Arc::clone(&sent));
        let thread = std::thread::spawn(move || {
            if (&client).write_all(head).is_err() {
                return;
            }
            counting.fetch_add(head.len(), Ordering::SeqCst);
            while !stopping.load(Ordering::SeqCst) {
                if (&client).write_all(b"0").is_err() {
                    return;
                }
                counting.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        Self {
            stop,
            sent,
            thread: Some(thread),
        }
    }

    fn sent(&self) -> usize {
        self.sent.load(Ordering::SeqCst)
    }

    fn ended(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for Dribbler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[test]
fn dribbled_handshakes_keep_no_lane() {
    let rig = Rig::new();
    let dribblers: Vec<Dribbler> = (0..3)
        .map(|parked| {
            rig.settle("the lane free", |census| {
                census.rpc == 0 && census.parked == parked
            });
            Dribbler::start(rig.dial().expect("admitted"), b"AUTH ")
        })
        .collect();
    rig.answers_a_fresh_client_while(3, "the dribbled handshakes parked");
    assert!(
        dribblers.iter().all(|dribbler| !dribbler.ended()),
        "every dribbler is still connected and dribbling"
    );
}

#[test]
fn a_handshake_that_never_ends_is_dropped_at_its_lifetime() {
    let rig = Rig::new();
    rig.lanes.set_handshake_lifetime(Duration::from_millis(200));
    let dribbler = Dribbler::start(rig.dial().expect("admitted"), b"AUTH ");
    rig.settle("the handshake dropped at its lifetime", |census| {
        census.open == 0
    });
    aterm_spec::wait::until_within(HANG, "the dribbler to see its connection end", || {
        dribbler.ended().then_some(())
    });
}

#[test]
fn without_a_parker_the_lane_writes_and_reads_waiting() {
    let rig = Rig::with_open(0);
    let client = rig.dial().expect("admitted");
    (&client)
        .write_all(b"AUTH ok\nbig\nvers")
        .expect("requests");
    let mut reader = BufReader::new(&client);
    let mut answer = Vec::new();
    reader.read_until(b'\n', &mut answer).expect("the answer");
    assert_eq!(answer.len(), BIG + 1, "the whole answer arrives");
    (&client).write_all(b"ion\n").expect("the rest");
    let mut next = String::new();
    reader.read_line(&mut next).expect("the next answer");
    assert_eq!(next, "OK version\n");
}

#[test]
fn a_dribbled_handshake_ends_at_its_deadline() {
    let (client, server) = CtlStream::pair().expect("socket pair");
    let dribbler = Dribbler::start(client, b"AUTH ");
    aterm_spec::wait::until_within(HANG, "the peer to dribble past its head", || {
        (dribbler.sent() > b"AUTH ".len()).then_some(())
    });
    let handshake = std::thread::spawn(move || {
        let active: ActiveHandle = Arc::new(Mutex::new(None));
        let denials = control_denials::AuthDenials::new();
        let mut unread = Unread::default();
        let mut reader = LaneReader::new(&server, &mut unread);
        let deadline = Instant::now() + Duration::from_millis(200);
        let got = authenticate_connection(&server, &mut reader, TOKEN, &active, &denials, deadline);
        (got.is_some(), Instant::now() >= deadline)
    });
    aterm_spec::wait::until_within(HANG, "the dribbled handshake to end", || {
        handshake.is_finished().then_some(())
    });
    let (authenticated, past_deadline) = handshake.join().expect("the handshake returns");
    assert!(
        !authenticated,
        "a line that never ends authenticates nothing"
    );
    assert!(
        past_deadline,
        "it ended at the deadline, not on a read error"
    );
}
