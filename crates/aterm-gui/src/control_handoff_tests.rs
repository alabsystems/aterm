// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An artifact's handoff rides its connection's outbox (`control_lanes.rs`):
//! the parker delivers a guarded answer and its challenge, awaits the one line
//! that acknowledges it, and fails the handoff closed at its deadline however
//! the connection ends. Before, the answer was written and its acknowledgement
//! read on the request lane, for up to `ARTIFACT_HANDOFF_TIMEOUT` each.
//!
//! These drive the genuine [`Lanes`] over real socket pairs with a stub whose
//! answers are real [`ControlReply`] handoffs. Every wait for something that
//! must happen is a hang detector ([`aterm_spec::wait::MUST_HAPPEN`]), never a
//! latency budget.

use std::io::{BufReader, Read as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_types::control_verbs::{ARTIFACT_REPLY_ACK_PREFIX, ARTIFACT_REPLY_CHALLENGE_PREFIX};
use aterm_uds::CtlStream;

use super::control_lanes::{Census, LaneLimits, LaneReader, LaneService, Lanes, Outbox};
use super::{
    ArtifactBudgets, ControlReply, Principal, ReplyHandoff, ReplyRetention, ReplyWire, Request,
    Scope, ServeDisposition, WireRetention, read_request_line,
};

/// The hang detector.
const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The size of a big answer: far past what a socket pair buffers.
const BIG: usize = 4 << 20;

/// The write-only permits `bytes-big` holds: this file's own budgets.
static PERMITS: std::sync::LazyLock<ArtifactBudgets> =
    std::sync::LazyLock::new(ArtifactBudgets::shipping);

/// A guard whose life the test reads: alive until released.
struct Probe(Arc<AtomicBool>);

impl WireRetention for Probe {
    fn prepare_write(&mut self) -> Result<(), String> {
        Ok(())
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Where the stub's held requests wait until the test opens it (at most a
/// hang detector's length).
#[derive(Default)]
struct Gate {
    open: Mutex<bool>,
    opened: Condvar,
}

impl Gate {
    fn pass(&self) {
        let deadline = Instant::now() + HANG;
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        while !*open {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            open = self
                .opened
                .wait_timeout(open, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn open(&self) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.opened.notify_all();
    }
}

/// The stub. `AUTH ok` authenticates. `guarded` answers `OK guarded` with a
/// guard ([`Self::guard_alive`]), `guarded-big` a [`BIG`] one; `bytes-big` a
/// [`BIG`] in-memory body holding a write-only permit of [`PERMITS`]; `latched`
/// a [`BIG`] plain body whose last byte releases a latch ([`Self::wire`]).
/// Each handoff fails `timeout` after its answer is queued. `hold` is a wait,
/// held at the gate. Anything else is answered `OK <line>`. With `lane_acks` a
/// guarded answer is delivered and its acknowledgement read ON ITS LANE,
/// waiting: the shape before the outbox carried handoffs (a negative control's).
struct Captures {
    timeout: Duration,
    lane_acks: bool,
    guards: Mutex<Vec<Arc<AtomicBool>>>,
    wires: Mutex<Vec<ReplyWire>>,
    gate: Gate,
}

impl Captures {
    fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            lane_acks: false,
            guards: Mutex::default(),
            wires: Mutex::default(),
            gate: Gate::default(),
        }
    }

    fn guarded(&self, body: String) -> ControlReply {
        let alive = Arc::new(AtomicBool::new(true));
        self.guards
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::clone(&alive));
        ControlReply::guarded(body, Some(ReplyRetention::new(Probe(alive))))
    }

    /// Whether the `index`th guard handed out is still held.
    fn guard_alive(&self, index: usize) -> bool {
        self.guards.lock().unwrap_or_else(PoisonError::into_inner)[index].load(Ordering::SeqCst)
    }

    /// The `index`th latch handed out.
    fn wire(&self, index: usize) -> ReplyWire {
        self.wires.lock().unwrap_or_else(PoisonError::into_inner)[index].clone()
    }
}

fn big_line() -> String {
    let mut line = "x".repeat(BIG);
    line.push('\n');
    line
}

impl LaneService for Captures {
    fn authenticate(
        &self,
        _stream: &CtlStream,
        reader: &mut LaneReader<'_>,
    ) -> Option<(Scope, Principal, Option<String>)> {
        (read_request_line(reader)? == "AUTH ok").then_some((
            Scope::Owner,
            Principal::Instance,
            None,
        ))
    }

    fn serve_line(
        &self,
        request: Request,
        _scope: Scope,
        stream: &CtlStream,
        reader: &mut LaneReader<'_>,
        out: &mut Outbox,
    ) -> Option<ServeDisposition> {
        let line = request.line();
        let reply = match line {
            "guarded" => self.guarded("OK guarded\n".to_string()),
            "guarded-big" => self.guarded(big_line()),
            "bytes-big" => {
                let permit = PERMITS
                    .try_reserve_in_memory()
                    .expect("the write-only permit is free");
                let retention = permit.release_path_and_retain_in_memory(());
                ControlReply::with_handoff(big_line(), Some(ReplyHandoff::WriteOnly(retention)))
            }
            "latched" => {
                let (wire, written) = ReplyWire::new();
                self.wires
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(wire);
                ControlReply::from(big_line()).releasing_on_write(written)
            }
            "hold" => {
                self.gate.pass();
                ControlReply::from(format!("OK {line}\n"))
            }
            _ => ControlReply::from(format!("OK {line}\n")),
        };
        if self.lane_acks && line == "guarded" {
            let mut outbox = Outbox::default();
            outbox.attach_within(reply, self.timeout);
            let _ = outbox.finish_waiting(stream, reader);
            return Some(ServeDisposition::Close);
        }
        out.attach_within(reply, self.timeout);
        None
    }

    fn is_wait(&self, request: &Request, _scope: Scope) -> bool {
        request.line() == "hold"
    }

    fn subscribe(&self, _request: Request, _scope: Scope, _stream: CtlStream) {}

    fn release(&self, _id: u64) {}

    fn has_live_claim(&self, _id: u64) -> bool {
        false
    }
}

/// Lanes with `limits`, serving with `stub`.
fn lanes(limits: LaneLimits, stub: &Arc<Captures>) -> Arc<Lanes> {
    let lanes = Lanes::new(limits);
    assert_eq!(lanes.start().rpc, limits.rpc, "every request lane started");
    lanes.publish(Arc::clone(stub) as Arc<dyn LaneService>);
    lanes
}

/// A parker, and more request lanes than a test has connections: a client
/// dialled as the last one's lane lets go of it is admitted, never handed
/// back to a lane not yet free.
fn parked_lanes(stub: &Arc<Captures>) -> Arc<Lanes> {
    lanes(
        LaneLimits {
            rpc: 4,
            wait: 0,
            inline: 0,
            open: 16,
            subscriptions: 1,
        },
        stub,
    )
}

/// The client end of an admitted connection, or `None` when the lanes handed
/// it back (the listener's busy reply).
fn dial(lanes: &Lanes) -> Option<CtlStream> {
    let (client, server) = CtlStream::pair().expect("socket pair");
    client
        .set_read_timeout(Some(HANG))
        .expect("bound client reads");
    lanes.admit(server).ok()?;
    Some(client)
}

/// Wait (a hang detector) until the lanes are in the shape `want` names.
#[track_caller]
fn settle(lanes: &Lanes, what: &str, want: impl Fn(Census) -> bool) {
    let deadline = Instant::now() + HANG;
    loop {
        let census = lanes.census();
        if want(census) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the lanes never settled ({what}): {census:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// One line the server sent, without its newline.
fn line_of(reader: &mut impl std::io::BufRead) -> String {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        // Linux resets a Unix socket closed with bytes it never read (an
        // acknowledgement a stopped parker did not take): closed all the same.
        Ok(0) => "<EOF>".to_string(),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => "<EOF>".to_string(),
        Ok(_) => line.trim_end().to_string(),
        Err(error) => format!("<{error}>"),
    }
}

/// Send `requests` (the handshake first) and read a guarded answer: its
/// `OK guarded` line, then its challenge. Returns the reader and the nonce.
fn read_guarded<'a>(client: &'a CtlStream, requests: &[u8]) -> (BufReader<&'a CtlStream>, String) {
    let mut writer = client;
    writer.write_all(requests).expect("requests");
    let mut reader = BufReader::new(client);
    assert_eq!(line_of(&mut reader), "OK guarded");
    let challenge = line_of(&mut reader);
    let nonce = challenge
        .strip_prefix(ARTIFACT_REPLY_CHALLENGE_PREFIX)
        .unwrap_or_else(|| panic!("a challenge follows the answer: {challenge:?}"))
        .to_string();
    (reader, nonce)
}

/// Four guarded captures whose clients read the challenge and never answer
/// it, then four waits served on request lanes (no wait lane takes them), on
/// eight lanes; then a fresh client. Its answer, or `None` when it was refused.
fn unacked_captures_then_waits(lane_acks: bool) -> Option<String> {
    const CAPTURES: usize = 4;
    const WAITS: usize = 4;
    let stub = Arc::new(Captures {
        lane_acks,
        ..Captures::new(HANG)
    });
    let lanes = lanes(
        LaneLimits {
            rpc: CAPTURES + WAITS,
            wait: 0,
            inline: WAITS,
            open: 64,
            subscriptions: 1,
        },
        &stub,
    );
    let captures: Vec<CtlStream> = (0..CAPTURES)
        .map(|_| {
            let client = dial(&lanes).expect("a capture is admitted");
            read_guarded(&client, b"AUTH ok\nguarded\n");
            client
        })
        .collect();
    settle(&lanes, "each acknowledgement awaited", |census| {
        if lane_acks {
            census.rpc == CAPTURES
        } else {
            census.parked == CAPTURES && census.rpc == 0
        }
    });
    let waits: Vec<CtlStream> = (0..WAITS)
        .map(|served| {
            let client = dial(&lanes).expect("a wait is admitted");
            (&client).write_all(b"AUTH ok\nhold\n").expect("the wait");
            settle(&lanes, "the wait served on its request lane", |census| {
                census.inline == served + 1
            });
            client
        })
        .collect();
    let fresh = dial(&lanes).map(|fresh| {
        (&fresh)
            .write_all(b"AUTH ok\nversion\n")
            .expect("the request");
        line_of(&mut BufReader::new(&fresh))
    });
    stub.gate.open();
    drop((captures, waits));
    fresh
}

/// FOUR CAPTURES NOBODY ACKNOWLEDGES, beside the waits the request lanes
/// serve. A guarded artifact's acknowledgement was read on its request lane
/// for up to `ARTIFACT_HANDOFF_TIMEOUT`: four captures whose clients read the
/// challenge and never answered held four lanes, four waits served there
/// (every wait lane taken) held the rest, and every other client was refused.
/// Now the parker awaits the line, and a fresh client is answered.
///
/// NEGATIVE CONTROL: the same drive against the acknowledgement awaited on
/// the lane (`lane_acks`, the old shape) refuses the fresh client.
#[test]
fn eight_unacked_captures_leave_lanes_free() {
    assert_eq!(
        unacked_captures_then_waits(false).as_deref(),
        Some("OK version"),
        "unacknowledged captures and inline waits must leave a lane for a fresh client"
    );
    assert_eq!(
        unacked_captures_then_waits(true),
        None,
        "the negative control: acknowledgements awaited on their lanes take every lane"
    );
}

/// THE CHALLENGE IS ONE-SHOT. A guess pipelined behind the request, before
/// the nonce exists, fails the handoff; so does a wrong line after the
/// challenge, though the right one follows it: one line decides, and the
/// connection closes. Each guard goes to quarantine, never released.
///
/// POSITIVE CONTROL: the echo of the challenge releases its guard.
#[test]
fn a_pipelined_ack_cannot_be_guessed() {
    let stub = Arc::new(Captures::new(HANG));
    let lanes = parked_lanes(&stub);

    let guesser = dial(&lanes).expect("admitted");
    let guess = format!(
        "AUTH ok\nguarded\n{ARTIFACT_REPLY_ACK_PREFIX}{}\n",
        "0f".repeat(16)
    );
    let (mut reader, _) = read_guarded(&guesser, guess.as_bytes());
    assert_eq!(line_of(&mut reader), "<EOF>", "one line decided: closed");
    assert!(
        stub.guard_alive(0),
        "a guess made before the challenge must not release the guard"
    );

    let second = dial(&lanes).expect("admitted");
    let (mut reader, nonce) = read_guarded(&second, b"AUTH ok\nguarded\n");
    (&second)
        .write_all(format!("ACK wrong\n{ARTIFACT_REPLY_ACK_PREFIX}{nonce}\n").as_bytes())
        .expect("two lines");
    assert_eq!(line_of(&mut reader), "<EOF>", "one line decided: closed");
    assert!(
        stub.guard_alive(1),
        "a wrong line decides: the right one after it must not release the guard"
    );

    let honest = dial(&lanes).expect("admitted");
    let (mut reader, nonce) = read_guarded(&honest, b"AUTH ok\nguarded\n");
    (&honest)
        .write_all(format!("{ARTIFACT_REPLY_ACK_PREFIX}{nonce}\n").as_bytes())
        .expect("the echo");
    assert_eq!(line_of(&mut reader), "<EOF>", "acknowledged: closed");
    aterm_spec::wait::until_within(HANG, "the echo to release its guard", || {
        (!stub.guard_alive(2)).then_some(())
    });
}

/// A HANDOFF ITS PEER DOES NOT FINISH fails closed at its deadline, where the
/// parker holds it: a guarded answer nobody reads, and one read but never
/// acknowledged, each close with their guard in quarantine; an in-memory body
/// nobody reads closes and gives its permit back. Before, a write-only answer
/// parked unread held its permit for as long as its peer stayed.
#[test]
fn an_unfinished_handoff_fails_closed_at_its_deadline() {
    let stub = Arc::new(Captures::new(Duration::from_millis(300)));
    let lanes = parked_lanes(&stub);
    let unread = dial(&lanes).expect("admitted");
    (&unread)
        .write_all(b"AUTH ok\nguarded-big\n")
        .expect("request");
    let unanswered = dial(&lanes).expect("admitted");
    let _ = read_guarded(&unanswered, b"AUTH ok\nguarded\n");
    let unread_bytes = dial(&lanes).expect("admitted");
    (&unread_bytes)
        .write_all(b"AUTH ok\nbytes-big\n")
        .expect("request");
    settle(&lanes, "every handoff closed at its deadline", |census| {
        census.open == 0
    });
    assert!(
        stub.guard_alive(0) && stub.guard_alive(1),
        "a guard whose handoff ran out goes to quarantine, never released"
    );
    assert_eq!(
        PERMITS.in_flight.used(),
        0,
        "a write-only permit is given back when its handoff runs out"
    );
    drop((unread, unanswered, unread_bytes));
}

/// A reply's latch (what a seamless update's Commit waits on) holds while the
/// peer has not taken the reply's last byte, and is released once it has.
/// Before, it was released as the reply was copied into the lane's buffer,
/// before a byte left.
#[test]
fn a_latch_is_released_once_its_last_byte_is_taken() {
    let stub = Arc::new(Captures::new(HANG));
    let lanes = parked_lanes(&stub);
    let client = dial(&lanes).expect("admitted");
    (&client).write_all(b"AUTH ok\nlatched\n").expect("request");
    settle(&lanes, "the unread answer parked", |census| {
        census.parked == 1 && census.rpc == 0
    });
    let wire = stub.wire(0);
    assert!(
        !wire.is_released(),
        "the latch holds while the peer has not taken the answer"
    );
    let mut answer = vec![0u8; BIG + 1];
    (&client).read_exact(&mut answer).expect("the answer");
    assert!(
        wire.wait_released(Instant::now() + HANG),
        "the latch is released once the last byte is out"
    );
}

/// A PARKER THAT STOPS while a guarded answer awaits its acknowledgement
/// closes the connection, and the guard goes to quarantine: the right echo,
/// sent as it stopped and never read, proves nothing.
#[test]
fn a_stopped_parker_quarantines_the_guard_it_held() {
    let stub = Arc::new(Captures::new(HANG));
    let lanes = parked_lanes(&stub);
    let client = dial(&lanes).expect("admitted");
    let (mut reader, nonce) = read_guarded(&client, b"AUTH ok\nguarded\n");
    settle(
        &lanes,
        "the acknowledgement awaited in the parker",
        |census| census.parked == 1 && census.rpc == 0,
    );
    lanes.inject_parker_panics(usize::MAX);
    (&client)
        .write_all(format!("{ARTIFACT_REPLY_ACK_PREFIX}{nonce}\n").as_bytes())
        .expect("the echo");
    assert_eq!(
        line_of(&mut reader),
        "<EOF>",
        "the stopped parker closed it"
    );
    settle(&lanes, "the parker stopped, nothing open", |census| {
        !census.parking && census.open == 0
    });
    assert!(
        stub.guard_alive(0),
        "a guard its stopped parker held goes to quarantine, never released"
    );
}

/// Without a parker the lane sees a guarded answer through itself, waiting:
/// its challenge is answered and the guard released, and a wrong line
/// quarantines it, the connection closing either way.
#[test]
fn without_a_parker_the_lane_awaits_the_acknowledgement() {
    let stub = Arc::new(Captures::new(HANG));
    // Two lanes: the second client is dialled as the first one's lets go.
    let lanes = lanes(
        LaneLimits {
            rpc: 2,
            wait: 0,
            inline: 0,
            open: 0,
            subscriptions: 1,
        },
        &stub,
    );
    let honest = dial(&lanes).expect("admitted");
    let (mut reader, nonce) = read_guarded(&honest, b"AUTH ok\nguarded\n");
    (&honest)
        .write_all(format!("{ARTIFACT_REPLY_ACK_PREFIX}{nonce}\n").as_bytes())
        .expect("the echo");
    assert_eq!(line_of(&mut reader), "<EOF>", "acknowledged: closed");
    aterm_spec::wait::until_within(HANG, "the echo to release its guard", || {
        (!stub.guard_alive(0)).then_some(())
    });

    let wrong = dial(&lanes).expect("admitted");
    let (mut reader, _) = read_guarded(&wrong, b"AUTH ok\nguarded\n");
    (&wrong).write_all(b"ACK wrong\n").expect("a wrong line");
    assert_eq!(line_of(&mut reader), "<EOF>", "failed: closed");
    assert!(
        stub.guard_alive(1),
        "a wrong line quarantines the guard, never releases it"
    );
}
