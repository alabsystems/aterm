// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A lane never waits on its peer (`control_lanes.rs`). It is given a
//! connection only with a whole request in hand, and it writes the answer
//! without waiting for the peer to read it; the parker waits for the rest of a
//! handshake, line or frame a peer began, and delivers the part of an answer
//! it has not read yet. Before, a client that dribbled its `AUTH` line (no
//! token needed), stopped mid-line or mid-frame, or did not read a large
//! answer held its lane for as long as it liked, and eight of them answered
//! every other client `ERR control server busy; retry`; a deadline on each wait
//! only made such peers reconnect when it ran out. The one wait that IS the
//! request, a relay, runs on a wait lane: a relayed `subscribe` held a request
//! lane for the whole subscription.
//!
//! These drive the genuine [`Lanes`] — one request lane, so a lane held is a
//! fresh client refused — over real socket pairs, with a stub service (the
//! relay with the genuine classifier and forward, to a stand-in sibling). Every
//! wait for something that must happen is a hang detector
//! ([`aterm_spec::wait::MUST_HAPPEN`]), never a latency budget.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aterm_uds::CtlStream;

use super::control_lanes::{Census, LaneLimits, LaneReader, LaneService, Lanes, lane_reader};
use super::{
    AUTH_READ_TIMEOUT, ActiveHandle, Scope, ServeDisposition, authenticate_connection,
    control_denials, read_request_line,
};

/// The instance token the handshake tests present.
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The hang detector.
const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The size of `big`'s answer: far past what a socket pair buffers.
const BIG: usize = 4 << 20;

/// The stub service: `AUTH ok` authenticates; `big` answers [`BIG`] bytes of
/// `x` and a newline; `feed-bin <n>` (the wire's own frame grammar) answers
/// `OK <n> <body>` once its body is read; `quit` closes; anything else is
/// answered `OK <line>`. Frame headers it is shown are recorded, and one
/// addressed `@0` is refused.
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
    ) -> Option<(Scope, Option<String>)> {
        if read_request_line(reader)? == "AUTH ok" {
            return Some((Scope::Owner, None));
        }
        let _ = (&*stream).write_all(b"ERR auth\n");
        None
    }

    fn serve_line(
        &self,
        line: String,
        _scope: Scope,
        _stream: &CtlStream,
        reader: &mut LaneReader<'_>,
        out: &mut Vec<u8>,
    ) -> Option<ServeDisposition> {
        if line == "quit" {
            return Some(ServeDisposition::Close);
        }
        if line == "big" {
            out.resize(out.len() + BIG, b'x');
            out.push(b'\n');
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

    fn frame_header(&self, line: &str, _scope: Scope, out: &mut Vec<u8>) -> bool {
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

    fn is_wait(&self, _line: &str, _scope: Scope) -> bool {
        false
    }

    fn subscribe(&self, _line: String, _scope: Scope, _stream: CtlStream) {}

    fn release(&self, _id: u64) {}
}

/// One request lane (so a lane held is a fresh client refused) and a parker.
struct Rig {
    lanes: Arc<Lanes>,
    stub: Arc<Stub>,
}

impl Rig {
    fn new() -> Self {
        Self::with_open(16)
    }

    /// `open: 0` starts no parker: the lanes as without one.
    fn with_open(open: usize) -> Self {
        let lanes = Lanes::new(LaneLimits {
            rpc: 1,
            wait: 0,
            inline: 0,
            open,
        });
        assert_eq!(lanes.start(), 1, "the request lane started");
        let stub = Arc::new(Stub::default());
        lanes.publish(Arc::clone(&stub) as Arc<dyn LaneService>);
        Self { lanes, stub }
    }

    /// Dial: the client end of an admitted connection; `None` when the lanes
    /// handed it back (the listener's busy reply).
    fn dial(&self) -> Option<CtlStream> {
        let (client, server) = CtlStream::pair().expect("socket pair");
        client
            .set_read_timeout(Some(HANG))
            .expect("bound client reads");
        self.lanes.admit(server).ok()?;
        Some(client)
    }

    /// Wait (a hang detector) until the lanes are in the shape `want` names.
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

    /// The request lane is free while `parked` connections wait in the parker,
    /// and a fresh client is admitted and answered.
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

/// One line the server sent, without its newline.
fn line_of(client: &CtlStream) -> String {
    let mut reply = String::new();
    match BufReader::new(client).read_line(&mut reply) {
        Ok(0) => "<EOF>".to_string(),
        Ok(_) => reply.trim_end().to_string(),
        Err(error) => format!("<{error}>"),
    }
}

/// THE UNREAD ANSWER (a pager, a consumer that paused). A client asks for a
/// large answer and reads none of it for now: the lane wrote on until the peer
/// read, holding it — and a write bound cut the reader off instead. Now the
/// lane delivers what fits and the parker the rest, as the peer reads; the
/// answer arrives whole and in order, and the request pipelined behind it is
/// answered after it.
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

/// THE BYTE-AT-A-TIME READER. A client that reads its answer a byte at a time
/// keeps any per-write bound from running out, and so kept its lane however
/// the write was bounded. The lane is free while it reads.
#[test]
fn an_answer_read_a_byte_at_a_time_keeps_no_lane() {
    let rig = Rig::new();
    let slow = rig.dial().expect("admitted");
    (&slow).write_all(b"AUTH ok\nbig\n").expect("request");
    let stop = Arc::new(AtomicBool::new(false));
    let read = Arc::new(AtomicUsize::new(0));
    let reader = {
        let (stop, read) = (Arc::clone(&stop), Arc::clone(&read));
        std::thread::spawn(move || {
            let mut byte = [0u8; 1];
            while !stop.load(Ordering::SeqCst) && (&slow).read_exact(&mut byte).is_ok() {
                read.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };
    aterm_spec::wait::until_within(HANG, "the slow reader to read", || {
        (read.load(Ordering::SeqCst) > 0).then_some(())
    });
    rig.answers_a_fresh_client_while(1, "the slowly read answer parked");
    assert!(
        !reader.is_finished(),
        "the slow reader is still reading: the answer is far from delivered"
    );
    stop.store(true, Ordering::SeqCst);
    reader.join().expect("the slow reader stops");
}

/// An answer whose peer hangs up without reading it is dropped with its
/// connection: the parker holds nothing for a peer that is gone.
#[test]
fn an_answer_whose_peer_hangs_up_unread_is_dropped() {
    let rig = Rig::new();
    let gone = rig.dial().expect("admitted");
    (&gone).write_all(b"AUTH ok\nbig\n").expect("request");
    rig.settle("the unread answer parked", |census| census.parked == 1);
    drop(gone);
    rig.settle("the connection closed", |census| census.open == 0);
}

/// THE ABANDONED LINE. An authenticated peer sends half a request (`vers`) and
/// stops: the lane waited for the newline (a deadline on it only made such
/// peers reconnect). Now the parker holds what came, and the request is
/// served once the rest arrives.
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

/// THE ABANDONED FRAME, and its header. `feed-bin <n>` announces a body; a peer
/// that sends part of it held the lane. Now the header is acted on as soon as
/// it is whole — the input boundary it is — and the parker waits for the body;
/// the frame is served once it is whole.
#[test]
fn a_frame_header_is_acted_on_at_once_and_its_body_keeps_no_lane() {
    let rig = Rig::new();
    let framing = rig.dial().expect("admitted");
    (&framing)
        .write_all(b"AUTH ok\nfeed-bin 6\nabc")
        .expect("a header and half a body");
    rig.answers_a_fresh_client_while(1, "the begun frame parked");
    assert_eq!(
        rig.stub.headers(),
        ["feed-bin 6"],
        "the header was acted on before its body came"
    );
    (&framing).write_all(b"def").expect("the rest of the body");
    assert_eq!(line_of(&framing), "OK 6 abcdef");
    assert_eq!(
        rig.stub.headers(),
        ["feed-bin 6"],
        "a header is acted on once"
    );
}

/// A frame refused on its header is answered and closed without its body
/// being waited for.
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

/// The forwarding service: the genuine classifier ([`super::is_wait_request`])
/// and forward ([`super::try_proxy_forward`]) over a session store and socket
/// dir of its own. `AUTH ok` authenticates as the owner; a line nothing
/// forwards is answered `OK <line>`.
struct Forwarder {
    store: super::Store,
    dir: std::path::PathBuf,
}

impl LaneService for Forwarder {
    fn authenticate(
        &self,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
    ) -> Option<(Scope, Option<String>)> {
        Stub::default().authenticate(stream, reader)
    }

    fn serve_line(
        &self,
        line: String,
        scope: Scope,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
        out: &mut Vec<u8>,
    ) -> Option<ServeDisposition> {
        if super::try_proxy_forward(&line, scope, &self.store, &self.dir, stream, reader) {
            return Some(ServeDisposition::Close);
        }
        out.extend_from_slice(format!("OK {line}\n").as_bytes());
        None
    }

    fn is_wait(&self, line: &str, scope: Scope) -> bool {
        super::is_wait_request(line, scope, &self.store)
    }

    fn subscribe(&self, _line: String, _scope: Scope, _stream: CtlStream) {}

    fn release(&self, _id: u64) {}
}

/// A stand-in sibling instance on `listener`: it answers a forward's connect
/// probe (`TOKEN <token> version`) and the subscription the forward hands it,
/// and holds that subscription until its peer hangs up. A connection that
/// says nothing (the forward's liveness probe) is dropped. Returns the
/// subscriptions it served.
fn stand_in_sibling(listener: &std::os::unix::net::UnixListener) -> usize {
    let mut served = 0;
    while served == 0 {
        let Ok((conn, _)) = listener.accept() else {
            return served;
        };
        let _ = conn.set_read_timeout(Some(HANG));
        let mut reader = BufReader::new(&conn);
        let mut line = String::new();
        while reader.read_line(&mut line).is_ok_and(|read| read > 0) {
            let answer = if line.starts_with("TOKEN ") && line.trim_end().ends_with(" version") {
                "OK version\n"
            } else if line.starts_with("subscribe ") {
                served += 1;
                "OK subscribe 1\n"
            } else {
                "ERR unexpected\n"
            };
            if (&conn).write_all(answer.as_bytes()).is_err() {
                break;
            }
            line.clear();
        }
    }
    served
}

/// A scratch directory, removed when the test ends, failed or not.
struct ScratchDir(std::path::PathBuf);

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// THE RELAYED SUBSCRIPTION. `subscribe @<sid>` for a session another
/// instance hosts is relayed to it for as long as the subscription lives. Its
/// selector comes second, so it was classed request-lane work, and eight
/// watchers of another window's sessions held every request lane: every other
/// client was answered `ERR control server busy; retry` while they watched.
/// Here the genuine lanes, classifier, forward and relay carry one such
/// subscription to a stand-in sibling, with ONE request lane: the relay runs on
/// a wait lane, and a fresh client is answered while it lives. So does a path
/// spelling of the sid, which reaches the same `graph/<sid>` entry.
#[test]
fn a_relayed_subscription_keeps_no_request_lane() {
    relay_one_subscription(0, |sid| format!("subscribe @{sid} events"));
    relay_one_subscription(1, |sid| format!("subscribe @./{sid} events"));
}

/// One run of [`a_relayed_subscription_keeps_no_request_lane`]: `spell` writes
/// the subscription for the sibling's session id.
fn relay_one_subscription(run: u32, spell: fn(&str) -> String) {
    use aterm_session::{LaunchNonce, SessionId};
    // Short: the socket path, canonicalized under `/private`, must fit a
    // `sockaddr_un`.
    let dir = std::env::temp_dir().join(format!("aterm-rl{run}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the socket dir");
    let _scratch = ScratchDir(dir.clone());
    let sock = dir.join("aterm-77101.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind the sibling");
    std::fs::write(dir.join("aterm-77101.token"), "cafe0123\n").expect("its token");
    let sid = SessionId::generate();
    crate::proxy::write_graph_entry(
        &dir,
        &sid,
        &sock.to_string_lossy(),
        &LaunchNonce::generate(),
    );
    let sibling = std::thread::spawn(move || stand_in_sibling(&listener));

    let lanes = Lanes::new(LaneLimits {
        rpc: 1,
        wait: 1,
        inline: 0,
        open: 16,
    });
    assert_eq!(lanes.start(), 1, "the request lane started");
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

/// A peer that writes `head`, then one more byte every 10 ms, never a newline,
/// until told to stop or its socket fails. Counts the bytes it got in.
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

    /// Whether its socket failed (the server closed the connection).
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

/// THE TOKENLESS DRIBBLE. Peers with no token send `AUTH ` and then a byte
/// every 10 ms: each held a lane while its handshake was read, and with a
/// deadline on it reconnected when it ran out. Now the parker holds them, and
/// a fresh client is answered while they dribble.
#[test]
fn dribbled_handshakes_keep_no_lane() {
    let rig = Rig::new();
    let dribblers: Vec<Dribbler> = (0..3)
        .map(|parked| {
            // Each is admitted once the one before it has left the lane.
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

/// A handshake that never comes whole is dropped when its lifetime from
/// admission runs out, however many bytes still arrive.
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

/// Without a parker the lane serves as it always did, waiting on its peer: a
/// large answer is written whole to a client that reads it.
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

/// THE TOKENLESS DRIBBLE on a lane with no parker. Each read came back long
/// before a per-read timeout, so the lane read on until the 64 KiB line cap
/// (about eleven minutes at a byte every 10 ms; 22 days at one every 29 s). The
/// handshake's deadline is the whole line's, so the lane gives up at it however
/// many bytes still arrive.
#[test]
fn a_dribbled_handshake_ends_at_its_deadline() {
    let (client, server) = CtlStream::pair().expect("socket pair");
    let dribbler = Dribbler::start(client, b"AUTH ");
    let handshake = std::thread::spawn(move || {
        let active: ActiveHandle = Arc::new(Mutex::new(None));
        let denials = control_denials::AuthDenials::new();
        let mut reader = lane_reader(&server, Vec::new());
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
    let sent = dribbler.sent();
    assert!(
        sent > b"AUTH ".len(),
        "the peer really dribbled ({sent} bytes): bytes kept arriving, and the deadline held"
    );
}

/// The positive control: a whole line that arrives in time authenticates.
#[test]
fn a_handshake_in_time_authenticates() {
    let (client, server) = CtlStream::pair().expect("socket pair");
    (&client)
        .write_all(format!("AUTH {TOKEN}\n").as_bytes())
        .expect("handshake");
    let active: ActiveHandle = Arc::new(Mutex::new(None));
    let denials = control_denials::AuthDenials::new();
    let mut reader = lane_reader(&server, Vec::new());
    let deadline = Instant::now() + AUTH_READ_TIMEOUT;
    let got = authenticate_connection(&server, &mut reader, TOKEN, &active, &denials, deadline);
    assert!(got.is_some(), "the instance token authenticates");
}
