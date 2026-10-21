// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the control socket's lane tenure
//! (`aterm_models_native::control_lane_tenure_model`), and the lanes' scheduling
//! tests.
//!
//! Everything here drives the GENUINE shipping [`Lanes`] — its request lanes,
//! wait lanes and parker threads — over real `CtlStream` socket pairs. Only
//! the request handling is a stub ([`Stub`]): its requests block on a gate the
//! test opens, so the test knows how many requests each kind of lane is serving
//! at every quiescent point. The projection reads the lanes' own [`Census`]; an
//! idle connection kept on a lane shows there as request-lane work no request
//! explains (`held`), which the model forbids.
//!
//! The negative control is the old design run for real: the same lanes with no
//! parker keep every idle connection on its lane (the no-parker fallback IS the
//! pre-2026-09-26 behaviour), and the model rejects that trace and names the
//! refusal it causes.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_models_native::control_lane_tenure_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{State, admits};
use aterm_uds::CtlStream;

use super::super::{Request, Scope, ServeDisposition};
use super::{Census, LaneLimits, LaneReader, LaneService, Lanes, Outbox};

/// How long any settle may take before the test calls the lanes wedged: the
/// house hang detector, never a latency budget.
const SETTLE: Duration = aterm_spec::wait::MUST_HAPPEN;

#[derive(Default)]
struct Gate {
    /// Requests served on request lanes / wait lanes, blocked in the stub.
    rpc: usize,
    wait: usize,
    /// Releases handed out, per lane kind.
    rpc_open: usize,
    wait_open: usize,
    /// Push streams held on their subscription lanes, and their releases.
    push: usize,
    push_open: usize,
}

/// The stub service: `AUTH ok` authenticates, and so does `AUTH drain`, which
/// leaves the socket non-blocking as the handshake's detached-peer drain does;
/// `work…` blocks on a request lane and `wait…` (a wait request) blocks
/// wherever it is served, each until the test releases one of its kind;
/// `subscribe…` flips the connection to push, which answers `OK push <lane>`
/// and holds its subscription lane until the test ends one; `claim` binds a
/// claim to the connection, `unclaim` releases it without closing the
/// connection; `who` answers the serving connection's serial; `blocking`
/// answers whether the socket blocks as it is served; `drain` makes it
/// non-blocking, as the ACK's detached-peer drain does; `switches` answers the
/// descriptor mode switches the serving lane's thread has made; `quit` closes;
/// anything else is answered at once. Releasing the claims of a connection
/// named in `panic_release` panics instead (a defective release, for the
/// parker tests).
#[derive(Default)]
struct Stub {
    gate: Mutex<Gate>,
    moved: Condvar,
    released: Mutex<Vec<u64>>,
    claims: Mutex<BTreeSet<u64>>,
    panic_release: Mutex<Vec<u64>>,
}

/// The length of the stub's ` big` answer: 4 MiB, past any socket buffer.
const BIG_ANSWER: usize = 4 << 20;

/// The busy reply, as the lanes write it.
const BUSY: &str = aterm_types::control_verbs::CONTROL_BUSY_REPLY;

fn on_wait_lane() -> bool {
    std::thread::current()
        .name()
        .is_some_and(|name| name.starts_with("aterm-control-wait-"))
}

impl Stub {
    fn block(&self) {
        let wait = on_wait_lane();
        let mut gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        if wait {
            gate.wait += 1;
        } else {
            gate.rpc += 1;
        }
        self.moved.notify_all();
        loop {
            let open = if wait {
                &mut gate.wait_open
            } else {
                &mut gate.rpc_open
            };
            if *open > 0 {
                *open -= 1;
                break;
            }
            gate = self
                .moved
                .wait(gate)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if wait {
            gate.wait -= 1;
        } else {
            gate.rpc -= 1;
        }
        self.moved.notify_all();
    }

    /// Let one blocked request finish on a request lane (`wait = false`) or a
    /// wait lane.
    fn let_finish(&self, wait: bool) {
        let mut gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        if wait {
            gate.wait_open += 1;
        } else {
            gate.rpc_open += 1;
        }
        self.moved.notify_all();
    }

    /// End one push stream the stub holds.
    fn end_push(&self) {
        let mut gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        gate.push_open += 1;
        self.moved.notify_all();
    }

    /// Push streams the stub holds on their subscription lanes.
    fn pushes(&self) -> usize {
        self.gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push
    }

    fn in_flight(&self) -> (usize, usize) {
        let gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        (gate.rpc, gate.wait)
    }

    fn released(&self) -> Vec<u64> {
        self.released
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
        let first = super::super::read_request_line(reader)?;
        if first == "AUTH ok" {
            return Some((Scope::Owner, crate::control::Principal::Instance, None));
        }
        if first == "AUTH drain" {
            stream.set_nonblocking(true).expect("non-blocking");
            return Some((Scope::Owner, crate::control::Principal::Instance, None));
        }
        let mut writer = stream;
        let _ = writer.write_all(b"ERR auth\n");
        None
    }

    fn serve_line(
        &self,
        request: Request,
        scope: Scope,
        stream: &CtlStream,
        _reader: &mut LaneReader<'_>,
        out: &mut Outbox,
    ) -> Option<ServeDisposition> {
        if request.line().starts_with("subscribe") {
            return Some(ServeDisposition::Subscribe { request, scope });
        }
        let reply = match request.line() {
            "quit" => return Some(ServeDisposition::Close),
            "claim" => {
                super::super::note_connection_claim();
                self.claims
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(super::super::serving_connection().expect("serving a connection"));
                "OK claim".to_string()
            }
            "unclaim" => {
                self.claims
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&super::super::serving_connection().expect("serving a connection"));
                "OK unclaim".to_string()
            }
            "who" => format!("OK {}", super::super::serving_connection().unwrap_or(0)),
            "blocking" if blocks(stream) => "OK blocking".to_string(),
            "blocking" => "OK nonblocking".to_string(),
            "drain" => {
                stream.set_nonblocking(true).expect("non-blocking");
                "OK drain".to_string()
            }
            "switches" => format!("OK {}", super::mode_switches_here()),
            "lane" => format!("OK {}", if on_wait_lane() { "wait" } else { "rpc" }),
            other if other.starts_with("work") || other.starts_with("wait") => {
                self.block();
                format!("OK {other}")
            }
            // An answer larger than any socket buffer: the lane delivers what the
            // peer has room for, the parker the rest.
            other if other.ends_with(" big") => format!("OK {}", "x".repeat(BIG_ANSWER)),
            other => format!("OK {other}"),
        };
        out.extend_from_slice(format!("{reply}\n").as_bytes());
        None
    }

    fn is_wait(&self, request: &Request, _scope: Scope) -> bool {
        request.line().starts_with("wait")
    }

    fn subscribe(&self, _request: Request, _scope: Scope, stream: CtlStream) {
        let lane = std::thread::current()
            .name()
            .unwrap_or_default()
            .to_string();
        let _ = (&stream).write_all(format!("OK push {lane}\n").as_bytes());
        let mut gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        gate.push += 1;
        self.moved.notify_all();
        while gate.push_open == 0 {
            gate = self
                .moved
                .wait(gate)
                .unwrap_or_else(PoisonError::into_inner);
        }
        gate.push_open -= 1;
        gate.push -= 1;
        self.moved.notify_all();
    }

    fn release(&self, id: u64) {
        let defective = self
            .panic_release
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&id);
        if defective {
            panic!("injected: releasing connection {id}'s claims panics");
        }
        self.claims
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        self.released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(id);
    }

    fn has_live_claim(&self, id: u64) -> bool {
        self.claims
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&id)
    }
}

/// Whether `stream` blocks: its descriptor's `O_NONBLOCK` is clear.
fn blocks(stream: &CtlStream) -> bool {
    use std::os::fd::AsRawFd;

    // SAFETY: `F_GETFL` reads the status flags of a descriptor `stream` holds
    // open for the call.
    let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0, "F_GETFL: {}", std::io::Error::last_os_error());
    flags & libc::O_NONBLOCK == 0
}

/// A client end of an admitted connection.
struct Client {
    stream: CtlStream,
    reader: BufReader<CtlStream>,
}

impl Client {
    fn send(&mut self, text: &str) {
        self.stream
            .write_all(text.as_bytes())
            .expect("client write");
    }

    fn line(&mut self) -> String {
        let mut reply = String::new();
        match self.reader.read_line(&mut reply) {
            Ok(0) => "<EOF>".to_string(),
            Ok(_) => reply.trim_end().to_string(),
            Err(error) => format!("<{error}>"),
        }
    }

    fn request(&mut self, line: &str) -> String {
        self.send(&format!("{line}\n"));
        self.line()
    }
}

struct Rig {
    lanes: Arc<Lanes>,
    stub: Arc<Stub>,
}

impl Rig {
    fn new(limits: LaneLimits) -> Self {
        let lanes = Lanes::new(limits);
        let started = lanes.start();
        assert_eq!(started.rpc, limits.rpc, "every request lane started");
        assert_eq!(
            started.subscriptions,
            limits.subscriptions.min(1),
            "one subscription lane is started up front"
        );
        let stub = Arc::new(Stub::default());
        lanes.publish(Arc::clone(&stub) as Arc<dyn LaneService>);
        Self { lanes, stub }
    }

    /// Dial: `Some` when the lanes admitted the connection, `None` when they
    /// handed it back (the listener's busy reply).
    fn dial(&self) -> Option<Client> {
        let (client, server) = CtlStream::pair().expect("real control socket pair");
        client
            .set_read_timeout(Some(SETTLE))
            .expect("bound client reads");
        self.lanes.admit(server).ok()?;
        let reader = BufReader::new(client.try_clone().expect("clone the client end"));
        Some(Client {
            stream: client,
            reader,
        })
    }

    /// A connection that authenticated and is idle (served once, then parked
    /// or — without a parker — kept on its lane).
    fn idle(&self) -> Option<Client> {
        let mut client = self.dial()?;
        client.send("AUTH ok\nping\n");
        assert_eq!(client.line(), "OK ping");
        Some(client)
    }

    /// Wait until the lanes are quiescent in the shape `want` names.
    fn settle(&self, what: &str, want: impl Fn(Census, (usize, usize)) -> bool) -> Census {
        let deadline = Instant::now() + SETTLE;
        loop {
            let census = self.lanes.census();
            let flight = self.stub.in_flight();
            if want(census, flight) {
                return census;
            }
            assert!(
                Instant::now() < deadline,
                "the lanes never settled ({what}): {census:?}, in flight {flight:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// What the Tier-1 test knows that the lanes do not count: connections
/// admitted so far, whether a refusal came while a lane held an idle one or
/// beside a parked one whose place it could have taken, whether a wait was
/// answered busy, and whether a wait was kept on a request lane beside a free
/// wait lane.
#[derive(Clone, Copy, Default)]
struct Facts {
    arrivals: i64,
    refused_idle: i64,
    refused_parked: i64,
    busied: i64,
    bypassed: i64,
}

fn count(n: usize) -> i64 {
    i64::try_from(n).expect("small counts fit i64")
}

/// Project the genuine lanes onto the model at a QUIESCENT point (no resume in
/// the queue, no request between lanes). `held` is request-lane work no
/// request in flight explains: an idle connection kept on its lane. A request
/// lane serving a wait (the lanes' `inline` count) is `inline`, not `working`.
fn project(census: Census, flight: (usize, usize), facts: Facts) -> State {
    assert_eq!(
        census.waits, flight.1,
        "a wait lane holds a connection with no wait in flight: {census:?}"
    );
    let model = control_lane_tenure_model();
    let mut state = model.init_state();
    state.insert("working", count(flight.0) - count(census.inline));
    state.insert("held", count(census.rpc) - count(flight.0));
    state.insert("waiting", count(flight.1));
    state.insert("inline", count(census.inline));
    state.insert("parked", count(census.parked));
    state.insert("spared", count(census.spared));
    state.insert("placing", count(census.placing));
    state.insert("queued", 0);
    state.insert("open", count(census.open));
    state.insert("arrivals", facts.arrivals);
    state.insert("refused_idle", facts.refused_idle);
    state.insert("refused_parked", facts.refused_parked);
    state.insert("busied", facts.busied);
    state.insert("bypassed", facts.bypassed);
    state
}

/// The model reaches `after` from `before` through exactly `path`, each step
/// the one the model names for it, and every invariant holds at the end.
fn assert_path(model: &Model, before: &State, path: &[&'static str], after: &State) {
    let mut at = before.clone();
    for (index, action) in path.iter().enumerate() {
        let next = if index + 1 == path.len() {
            after.clone()
        } else {
            let successors = model.successors(action, &at);
            assert_eq!(
                successors.len(),
                1,
                "{action} is deterministic and enabled at {at:?}"
            );
            successors[0].clone()
        };
        assert_eq!(
            admits(model, &at, &next),
            Some(*action),
            "the shipping lanes' step {index} must be the model's {action}: {at:?} -> {next:?}"
        );
        at = next;
    }
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, after),
            "{}::{} fails at {after:?}",
            model.name,
            invariant.name
        );
    }
}

/// The model's constants, the lanes' limits.
fn model_limits() -> LaneLimits {
    LaneLimits {
        rpc: 2,
        wait: 1,
        inline: 1,
        open: 3,
        subscriptions: 1,
    }
}

#[test]
fn shipping_lanes_conform_to_the_lane_tenure_model() {
    let model = control_lane_tenure_model();
    let rig = Rig::new(model_limits());
    let mut facts = Facts::default();
    // A QUIESCENT point: besides the shape named, every lane's work is a
    // request in flight — no lane is still between its last reply and placing
    // its connection. (An idle connection kept on a lane never gets there, and
    // the settle names it.)
    let observe =
        |rig: &Rig, facts: Facts, what: &str, want: &dyn Fn(Census, (usize, usize)) -> bool| {
            let census = rig.settle(what, |c, f| want(c, f) && c.rpc == f.0 && c.waits == f.1);
            project(census, rig.stub.in_flight(), facts)
        };
    let s0 = observe(&rig, facts, "empty", &|c, _| {
        c == Census {
            parking: true,
            subscription_threads: 1,
            ..Census::default()
        }
    });
    assert_eq!(s0, model.init_state());

    // Three drivers, served once and left idle: each parks.
    let mut a = rig.idle().expect("A admitted");
    facts.arrivals += 1;
    let s1 = observe(&rig, facts, "A parked", &|c, _| c.parked == 1 && c.rpc == 0);
    assert_path(&model, &s0, &["Admit", "Finish"], &s1);
    let mut b = rig.idle().expect("B admitted");
    facts.arrivals += 1;
    let s2 = observe(&rig, facts, "B parked", &|c, _| c.parked == 2 && c.rpc == 0);
    assert_path(&model, &s1, &["Admit", "Finish"], &s2);
    let mut stale = rig.idle().expect("C admitted");
    facts.arrivals += 1;
    let s3 = observe(&rig, facts, "C parked", &|c, _| c.parked == 3 && c.rpc == 0);
    assert_path(&model, &s2, &["Admit", "Finish"], &s3);

    // A and B are served again, so C has gone longest without a byte.
    assert_eq!(a.request("ping"), "OK ping");
    let s3a = observe(&rig, facts, "A parked", &|c, _| c.parked == 3 && c.rpc == 0);
    assert_path(&model, &s3, &["Wake", "Pick", "Finish"], &s3a);
    assert_eq!(b.request("ping"), "OK ping");
    let s3b = observe(&rig, facts, "B parked", &|c, _| c.parked == 3 && c.rpc == 0);
    assert_path(&model, &s3a, &["Wake", "Pick", "Finish"], &s3b);

    // Every request lane is free and the open-connection bound is reached: a
    // fourth client, X, is not refused but handed to the parker, which parks it
    // in the place of the stalest parked connection, C, which reads the
    // hang-up. The bound stands; X, spared until its handshake is made, plays
    // C's part from here on.
    assert!(model.successors("Admit", &s3b).is_empty());
    assert!(
        model.successors("Refuse", &s3b).is_empty(),
        "the model refuses no client a parked connection can make room for"
    );
    let mut third = rig.dial().expect("X admitted at the cap");
    facts.arrivals += 1;
    // (Reading C, which is owed nothing, moves no byte, so cannot stir it.)
    assert_eq!(stale.line(), "<EOF>", "C, the stalest, closed for X");
    let s3c = observe(&rig, facts, "X parked in C's place", &|c, _| {
        c.parked == 3 && c.open == 3 && c.rpc == 0 && c.placing == 0 && c.spared == 1
    });
    assert_path(&model, &s3b, &["Hand", "Place"], &s3c);
    third.send("AUTH ok\nping\n");
    assert_eq!(third.line(), "OK ping", "X, served in C's place");
    let s3 = observe(&rig, facts, "X parked", &|c, _| {
        c.parked == 3 && c.rpc == 0 && c.spared == 0
    });
    assert_path(&model, &s3c, &["Unspare", "Wake", "Pick", "Finish"], &s3);

    // A asks for work: woken, taken back by a request lane.
    a.send("work\n");
    let s4 = observe(&rig, facts, "A working", &|c, f| {
        f == (1, 0) && c.parked == 2
    });
    assert_path(&model, &s3, &["Wake", "Pick"], &s4);
    // B asks for a wait: woken, taken back, moved to the wait lane.
    b.send("wait\n");
    let s5 = observe(&rig, facts, "B waiting", &|c, f| {
        f == (1, 1) && c.parked == 1 && c.rpc == 1
    });
    assert_path(&model, &s4, &["Wake", "Pick", "Defer"], &s5);

    // Each finishes and parks: the request lane's and the wait lane's finish.
    rig.stub.let_finish(false);
    assert_eq!(a.line(), "OK work");
    let s6 = observe(&rig, facts, "A parked again", &|c, f| {
        f == (0, 1) && c.parked == 2 && c.rpc == 0
    });
    assert_path(&model, &s5, &["Finish"], &s6);
    rig.stub.let_finish(true);
    assert_eq!(b.line(), "OK wait");
    let s7 = observe(&rig, facts, "B parked again", &|c, f| {
        f == (0, 0) && c.parked == 3
    });
    assert_path(&model, &s6, &["WaitFinish"], &s7);

    // A binds a claim to its connection, so gives way to no fresh client, and
    // quits: the close ends the tenure, and the claim with it.
    assert_eq!(a.request("claim"), "OK claim");
    let who = a.request("who");
    let a_id: u64 = who.trim_start_matches("OK ").parse().expect("a serial");
    let s8 = observe(&rig, facts, "A parked after claim", &|c, _| {
        c.parked == 3 && c.rpc == 0 && c.spared == 1
    });
    assert_path(&model, &s7, &["Wake", "Pick", "Finish", "Spare"], &s8);
    a.send("quit\n");
    assert_eq!(a.line(), "<EOF>");
    let s9 = observe(&rig, facts, "A closed", &|c, _| {
        c.open == 2 && c.parked == 2 && c.spared == 0
    });
    assert_path(&model, &s8, &["Unspare", "Wake", "Pick", "Close"], &s9);
    assert_eq!(rig.stub.released(), vec![a_id], "A's claim ended with it");

    // WAITS PAST THE WAIT LANES. D's wait takes the one wait lane, with the
    // request pipelined behind it (all three lines are one read): it moves
    // along, and is answered there after the wait.
    let mut d = rig.dial().expect("D admitted");
    facts.arrivals += 1;
    d.send("AUTH ok\nwait\nlane\n");
    let s10 = observe(&rig, facts, "D waiting", &|c, f| f == (0, 1) && c.rpc == 0);
    assert_path(&model, &s9, &["Admit", "Defer"], &s10);
    // B's wait finds it taken and is served on its request lane: the budget's
    // one place.
    b.send("wait\n");
    let s11 = observe(&rig, facts, "B waiting inline", &|c, f| {
        f == (1, 1) && c.rpc == 1 && c.inline == 1 && c.parked == 1
    });
    assert_path(&model, &s10, &["Wake", "Pick", "InlineWait"], &s11);
    // C's wait finds the wait lane taken AND the budget spent: the busy reply,
    // on a connection that stays open and parks again — the other request lane
    // is still free. "Without waiting" is an ORDER, not a stopwatch: a wait
    // that were held would answer only once `let_finish` below lets it.
    third.send("wait\n");
    assert_eq!(third.line(), BUSY, "a wait past every lane it may take");
    facts.busied = 1;
    let s12 = observe(&rig, facts, "C parked after its busy wait", &|c, f| {
        f == (1, 1) && c.rpc == 1 && c.inline == 1 && c.parked == 1
    });
    assert_path(&model, &s11, &["Wake", "Pick", "BusyWait"], &s12);
    // D's client says `quit` while its wait runs; the wait lane serves the
    // carried `lane`, then the `quit`.
    d.send("quit\n");
    rig.stub.let_finish(true);
    assert_eq!(d.line(), "OK wait");
    assert_eq!(
        d.line(),
        "OK wait",
        "the carried request, served where it landed"
    );
    assert_eq!(d.line(), "<EOF>");
    let s13 = observe(&rig, facts, "D closed", &|c, f| f == (1, 0) && c.open == 2);
    assert_path(&model, &s12, &["WaitClose"], &s13);
    // B's wait ends on its request lane, and B parks.
    rig.stub.let_finish(false);
    assert_eq!(b.line(), "OK wait");
    let s14 = observe(&rig, facts, "B parked", &|c, f| {
        f == (0, 0) && c.parked == 2 && c.inline == 0
    });
    assert_path(&model, &s13, &["InlineFinish"], &s14);
    // C, still connected after its busy reply, is served as ever.
    assert_eq!(third.request("ping"), "OK ping");
    let s15 = observe(&rig, facts, "C parked again", &|c, f| {
        f == (0, 0) && c.parked == 2
    });
    assert_path(&model, &s14, &["Wake", "Pick", "Finish"], &s15);

    // B's peer hangs up while parked: the parker closes it.
    drop(b);
    let s16 = observe(&rig, facts, "B closed", &|c, _| {
        c.open == 1 && c.parked == 1
    });
    assert_path(&model, &s15, &["ParkedClose"], &s16);
    drop(third);
}

/// A wait served on its request lane ends with its connection: the lane's
/// `InlineClose` (the client said `quit` behind it).
#[test]
fn an_inline_wait_whose_client_quits_closes_there() {
    let model = control_lane_tenure_model();
    let rig = Rig::new(model_limits());
    let mut facts = Facts::default();
    let mut d = rig.dial().expect("D admitted");
    facts.arrivals += 1;
    d.send("AUTH ok\nwait\n");
    let census = rig.settle("D waiting", |c, f| {
        f == (0, 1) && c.rpc == 0 && c.waits == f.1
    });
    let s1 = project(census, rig.stub.in_flight(), facts);
    // `wait` with `quit` pipelined behind it: the one wait lane is D's, so it
    // is served where it is, within the budget.
    let mut e = rig.dial().expect("E admitted");
    facts.arrivals += 1;
    e.send("AUTH ok\nwait\nquit\n");
    let census = rig.settle("E waiting inline", |c, f| {
        f == (1, 1) && c.rpc == 1 && c.inline == 1 && c.waits == f.1
    });
    let s2 = project(census, rig.stub.in_flight(), facts);
    assert_path(&model, &s1, &["Admit", "InlineWait"], &s2);
    rig.stub.let_finish(false);
    assert_eq!(e.line(), "OK wait");
    assert_eq!(e.line(), "<EOF>");
    let census = rig.settle("E closed", |c, f| {
        f == (0, 1) && c.rpc == 0 && c.inline == 0 && c.open == 1 && c.waits == f.1
    });
    let s3 = project(census, rig.stub.in_flight(), facts);
    assert_path(&model, &s2, &["InlineClose"], &s3);
    rig.stub.let_finish(true);
    assert_eq!(d.line(), "OK wait");
}

/// THE NEGATIVE CONTROL: the old design, run for real. Without a parker the
/// same lanes keep each idle connection on its lane (the pre-2026-09-26
/// behaviour). The model has no step to that state, `NoIdleHold` fails on it,
/// and the refusal it leads to is the one `NoRefusalByIdleDriver` names.
#[test]
fn the_old_lane_held_design_is_rejected_by_the_model() {
    let model = control_lane_tenure_model();
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 1,
        inline: 2,
        open: 0,
        subscriptions: 1,
    });
    let s0 = project(rig.lanes.census(), rig.stub.in_flight(), Facts::default());
    let mut facts = Facts::default();
    let _a = rig.idle().expect("A admitted");
    let _b = rig.idle().expect("B admitted");
    facts.arrivals = 2;
    let census = rig.settle("both idle on their lanes", |c, f| {
        c.rpc == 2 && f == (0, 0) && c.waits == f.1
    });
    let held = project(census, rig.stub.in_flight(), facts);
    assert_eq!(
        held["held"], 2,
        "the old design keeps both idle drivers on lanes"
    );
    let mid = model.successors("Admit", &s0)[0].clone();
    assert_eq!(
        admits(&model, &mid, &{
            let mut one = held.clone();
            one.insert("held", 1);
            one.insert("arrivals", 1);
            one.insert("open", 1);
            one
        }),
        None,
        "the model has no step that leaves an idle connection on its lane"
    );
    assert!(!model.check_invariant("NoIdleHold", &held));

    assert!(
        rig.dial().is_none(),
        "with both lanes held by idle drivers, a fresh client is refused"
    );
    facts.refused_idle = 1;
    let refused = project(rig.lanes.census(), rig.stub.in_flight(), facts);
    assert!(!model.check_invariant("NoRefusalByIdleDriver", &refused));
}

/// Twelve idle persistent drivers against two request lanes: a fresh client
/// is still admitted and served at once. The same drivers against the lanes
/// without a parker (the old design) refuse the third.
#[test]
fn idle_drivers_do_not_starve_a_fresh_client() {
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 1,
        inline: 1,
        open: 64,
        subscriptions: 1,
    });
    // Each is served, and parked before the next dials: a lane is busy until
    // its request is answered AND its connection is placed.
    let drivers: Vec<Client> = (0..12)
        .map(|index| {
            let driver = rig
                .idle()
                .unwrap_or_else(|| panic!("idle driver {index} refused"));
            rig.settle("parked", |c, _| c.parked == index + 1 && c.rpc == 0);
            driver
        })
        .collect();
    for attempt in 0..5 {
        let mut fresh = rig
            .dial()
            .unwrap_or_else(|| panic!("fresh client {attempt} refused with twelve idle drivers"));
        fresh.send("AUTH ok\nping\n");
        assert_eq!(fresh.line(), "OK ping");
        // One-shot, as `aterm ctl` is: it hangs up, and is gone from the lanes
        // before the next one dials (two lanes here, not eight).
        drop(fresh);
        rig.settle("the one-shot client gone", |c, _| {
            c.open == 12 && c.rpc == 0
        });
    }
    // Each driver is still served, in order, on the connection it holds.
    for mut driver in drivers {
        assert_eq!(driver.request("again"), "OK again");
    }

    let old = Rig::new(LaneLimits {
        rpc: 2,
        wait: 1,
        inline: 2,
        open: 0,
        subscriptions: 1,
    });
    let _held: Vec<Client> = (0..2).map(|_| old.idle().expect("two fit")).collect();
    old.settle("two held", |c, f| c.rpc == 2 && f == (0, 0));
    assert!(
        old.dial().is_none(),
        "negative control: without parking two idle drivers refuse the third"
    );
}

/// Drivers parked in waits move off the request lanes; with no wait lanes and
/// no bound on the waits a request lane serves (the old design) the same waits
/// take the request lanes and refuse a fresh client.
#[test]
fn waiting_drivers_do_not_starve_a_fresh_client() {
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 8,
        inline: 1,
        open: 64,
        subscriptions: 1,
    });
    let mut waiting = Vec::new();
    for index in 0..6 {
        let mut driver = rig.dial().expect("waiting driver admitted");
        driver.send("AUTH ok\nwait\n");
        waiting.push(driver);
        rig.settle("on a wait lane", |c, f| f == (0, index + 1) && c.rpc == 0);
    }
    let mut fresh = rig.dial().expect("fresh client admitted beside six waits");
    fresh.send("AUTH ok\nlane\n");
    assert_eq!(fresh.line(), "OK rpc");
    for _ in &waiting {
        rig.stub.let_finish(true);
    }
    for driver in &mut waiting {
        assert_eq!(driver.line(), "OK wait");
    }

    let old = Rig::new(LaneLimits {
        rpc: 2,
        wait: 0,
        inline: 2,
        open: 64,
        subscriptions: 1,
    });
    let mut held = Vec::new();
    for index in 0..2 {
        let mut driver = old.dial().expect("fits a lane");
        driver.send("AUTH ok\nwait\n");
        held.push(driver);
        old.settle("a wait on a request lane", |c, f| {
            f == (index + 1, 0) && c.rpc == index + 1
        });
    }
    assert!(
        old.dial().is_none(),
        "negative control: without wait lanes two waits refuse the third client"
    );
    for _ in &held {
        old.stub.let_finish(false);
    }
    for driver in &mut held {
        assert_eq!(driver.line(), "OK wait");
    }
}

/// The busy reply is kept for a socket truly saturated with work, and it does
/// not wait for a lane: two requests working on two lanes refuse a third client
/// while both still work, and the lane a finished request frees admits again.
#[test]
fn a_pool_saturated_with_work_still_refuses_promptly() {
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 1,
        inline: 1,
        open: 64,
        subscriptions: 1,
    });
    let mut workers = Vec::new();
    for index in 0..2 {
        let mut driver = rig.dial().expect("fits a lane");
        driver.send("AUTH ok\nwork\n");
        workers.push(driver);
        rig.settle("working", |c, f| f == (index + 1, 0) && c.rpc == index + 1);
    }
    // "Did not wait for a lane" is an ORDER: the admission is asked on a thread
    // of its own while this one keeps both lanes working, so its answer arriving
    // at all proves it did not wait for a lane to free — none can until
    // `let_finish` below. The minute is a hang detector for an admission that
    // parks the listener. What the order does NOT see is how long the refusal
    // took: an admission that retried a bounded while (under the minute) and
    // then refused would pass. That it does not is the code's shape — a
    // `Slot::take` and one `try_submit`, no wait — and no clock here judges it.
    // (It was a 500 ms stopwatch around a socketpair and two atomics, which a
    // loaded box's scheduler could cross on a correct tree.)
    let (answer_tx, answer_rx) = std::sync::mpsc::channel();
    let lanes = Arc::clone(&rig.lanes);
    let asker = std::thread::spawn(move || {
        let (_client, server) = CtlStream::pair().expect("real control socket pair");
        let _ = answer_tx.send(lanes.admit(server).is_ok());
    });
    let admitted = answer_rx
        .recv_timeout(SETTLE)
        .expect("the saturated lanes parked the admission instead of refusing it");
    assert!(!admitted, "saturated with work: refused");
    assert_eq!(
        rig.stub.in_flight(),
        (2, 0),
        "and refused while both lanes were still working"
    );
    asker.join().expect("the admission thread");
    rig.stub.let_finish(false);
    rig.settle("one lane free", |c, _| c.rpc == 1);
    let mut next = rig.dial().expect("a freed lane admits again");
    next.send("AUTH ok\nping\n");
    assert_eq!(next.line(), "OK ping");
    rig.stub.let_finish(false);
    for mut worker in workers {
        assert_eq!(worker.line(), "OK work");
    }
}

/// THE SUBSCRIPTION LANES (`subscription-capacity`). A connection that flips
/// to push is served on a subscription lane for its whole life: first the one
/// `start` reserved, then one started for each flip while fewer than the cap
/// run. The flip past the cap is answered busy at once and closed, never
/// queued behind a subscription that may never end; a subscription that ends
/// frees its lane, which the next flip takes with no new thread. Push streams
/// spend neither a request lane nor an open-connection slot: with every
/// subscription lane serving, a request connection is admitted and answered.
#[test]
fn subscriptions_grow_to_their_cap_and_one_past_it_is_refused_at_once() {
    const SUBSCRIPTIONS: usize = 2;
    let rig = Rig::new(LaneLimits {
        rpc: 1,
        wait: 1,
        inline: 0,
        open: 4,
        subscriptions: SUBSCRIPTIONS,
    });
    let reserved = rig.lanes.census();
    assert_eq!(
        (reserved.subscriptions, reserved.subscription_threads),
        (0, 1),
        "one subscription lane waits from the start, serving nothing"
    );
    let subscribe = || {
        let mut client = rig.dial().expect("a fresh connection is admitted");
        client.send("AUTH ok\nsubscribe @. events\n");
        let answer = client.line();
        (client, answer)
    };
    let mut held = Vec::new();
    for index in 0..SUBSCRIPTIONS {
        let (client, answer) = subscribe();
        assert!(
            answer.starts_with("OK push aterm-subscribe-"),
            "subscription {index} is served on a subscription lane: {answer:?}"
        );
        // A push stream keeps no open-connection slot and no request lane: the
        // request lane that flipped it is free once it has handed the stream
        // over and returned, which the stream's first line may beat.
        let census = rig.settle("the push stream held on its lane", |c, f| {
            rig.stub.pushes() == index + 1
                && c.subscriptions == index + 1
                && f == (0, 0)
                && (c.open, c.rpc) == (0, 0)
        });
        assert_eq!(
            census.subscription_threads,
            index + 1,
            "the reserved lane takes the first; each later one starts a lane"
        );
        held.push(client);
    }

    let mut request = rig
        .idle()
        .expect("a request connection is admitted with every subscription lane serving");
    assert_eq!(request.request("again"), "OK again");
    // Its lane is busy until it has parked the connection after the answer:
    // the next flip dials only then (one request lane here).
    rig.settle("the request connection parked", |c, _| {
        c.parked == 1 && c.rpc == 0
    });

    let (mut refused, answer) = subscribe();
    assert_eq!(
        answer, "ERR subscription capacity busy; retry",
        "the flip past the cap is answered at once"
    );
    assert_eq!(refused.line(), "<EOF>", "and closed");
    let census = rig.settle("the refused connection gone", |c, _| {
        c.open == 1 && c.rpc == 0
    });
    assert_eq!(
        (census.subscriptions, census.subscription_threads),
        (SUBSCRIPTIONS, SUBSCRIPTIONS),
        "no lane past the cap"
    );

    rig.stub.end_push();
    rig.settle("an ended push stream frees its lane", |c, _| {
        rig.stub.pushes() == SUBSCRIPTIONS - 1 && c.subscriptions == SUBSCRIPTIONS - 1
    });
    let (next, answer) = subscribe();
    assert!(
        answer.starts_with("OK push aterm-subscribe-"),
        "the freed lane serves the next flip: {answer:?}"
    );
    let census = rig.settle("the next push stream held", |c, _| {
        rig.stub.pushes() == SUBSCRIPTIONS && c.subscriptions == SUBSCRIPTIONS
    });
    assert_eq!(
        census.subscription_threads, SUBSCRIPTIONS,
        "the freed lane is reused, never a new thread"
    );
    held.push(next);
    for _ in 0..SUBSCRIPTIONS {
        rig.stub.end_push();
    }
    rig.settle("every push stream ended", |c, _| {
        rig.stub.pushes() == 0 && c.subscriptions == 0
    });
}

/// Everything `client` reads until its connection is closed: what it had been
/// sent of an answer, then the hang-up (a minute's read is the hang detector
/// for a connection never closed).
fn rest_until_closed(client: &mut Client) -> Vec<u8> {
    let mut rest = Vec::new();
    match client.reader.read_to_end(&mut rest) {
        Ok(_) => rest,
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => rest,
        Err(error) => panic!(
            "the connection was not closed: {error} after {} bytes",
            rest.len()
        ),
    }
}

/// ONE PROCESS HOLDING THE CAP (ATERM-KI-1015-13, until 2026-10-19): connections up to the cap —
/// one holding a claim, a pager owed a large answer, one about to send half a
/// request, then idle ones — and then fresh clients. The pager and the
/// half-sender were served before the idle ones, but stir after them: the
/// half-sender sends the first bytes of a request, the pager takes part of its
/// answer. Each fresh client is admitted and served in the place of the
/// stalest parked connection: the idle ones in the order they were last
/// served, then the half-sender, then the pager (what it had been sent of its
/// answer, then the hang-up), then the fresh ones in turn; the one holding a
/// claim outlasts them all, its claim kept. The cap is never passed. At the
/// cap with nothing parked (each connection on a lane) a fresh client is still
/// refused, with a request lane free. The negative control is the cap as it
/// was: the same connections refuse the fresh client with every request lane
/// free, which the model rejects (`NoRefusalBesideParked`).
#[test]
fn a_process_holding_the_cap_does_not_starve_a_fresh_client() {
    const CAP: usize = 6;
    let model = control_lane_tenure_model();
    for shipping in [true, false] {
        let rig = Rig::new(LaneLimits {
            rpc: 2,
            wait: 1,
            inline: 1,
            open: CAP,
            subscriptions: 1,
        });
        if !shipping {
            rig.lanes.refuse_at_the_cap();
        }
        let parked = |n: usize, what: &str| {
            rig.settle(what, |c, f| {
                c.parked == n && c.open == n && c.rpc == 0 && f == (0, 0)
            })
        };
        // The claim holder is served first, so it has gone longest without a
        // byte; its claim still keeps it to the last.
        let mut keeper = rig.idle().expect("the claim holder admitted");
        assert_eq!(keeper.request("claim"), "OK claim");
        let keeper_id = keeper.request("who");
        parked(1, "the claim holder parked");
        let mut pager = rig.idle().expect("the pager admitted");
        parked(2, "the pager parked");
        pager.send("answer big\n");
        rig.settle("the pager parked owing its answer", |c, _| {
            c.parked == 2 && c.rpc == 0 && c.held >= BIG_ANSWER / 2
        });
        let mut half = rig.idle().expect("the half-sender admitted");
        parked(3, "the half-sender parked");
        let mut victims: Vec<(&str, Client)> = Vec::new();
        for index in 0..3 {
            let idle = rig.idle().expect("an idle connection admitted");
            parked(4 + index, "an idle connection parked");
            victims.push(("an idle connection", idle));
        }
        // The half-sender sends its request's first bytes and no end to the
        // line: the parker holds them once it has read them.
        let before = rig.lanes.census().held;
        half.send("pin");
        rig.settle("the half-sent bytes held", |c, _| c.held > before);
        // Then the pager takes part of its answer: more than any socket
        // buffer, so the parker has delivered to it.
        let mut page = vec![0u8; 1 << 20];
        pager
            .reader
            .read_exact(&mut page)
            .expect("the pager's first page");
        victims.push(("the half-sender", half));
        victims.push(("the pager", pager));

        if !shipping {
            assert!(
                rig.dial().is_none(),
                "negative control: the cap refuses beside {CAP} parked connections"
            );
            let census = rig.lanes.census();
            assert_eq!(
                (census.rpc, census.parked),
                (0, CAP),
                "and every request lane is free"
            );
            let facts = Facts {
                arrivals: count(CAP),
                refused_parked: 1,
                ..Facts::default()
            };
            let state = project(census, rig.stub.in_flight(), facts);
            assert!(
                !model.check_invariant("NoRefusalBesideParked", &state),
                "negative control: the model rejects the refusal: {state:?}"
            );
            continue;
        }

        let mut fresh: Vec<Client> = Vec::new();
        for (index, (what, mut victim)) in victims.into_iter().enumerate() {
            let mut client = rig.dial().unwrap_or_else(|| {
                panic!("fresh client {index} refused at the cap with every request lane free")
            });
            // Served only once placed, so only after its victim was closed: a
            // victim read before then would take bytes, and be the stalest no
            // more.
            client.send("AUTH ok\nping\n");
            assert_eq!(
                client.line(),
                "OK ping",
                "fresh client {index}, in its place"
            );
            let rest = rest_until_closed(&mut victim);
            assert!(
                rest.len() < BIG_ANSWER && !rest.ends_with(b"\n"),
                "{what} was closed for fresh client {index} with no whole answer: {} bytes",
                rest.len()
            );
            parked(CAP, "the fresh client parked in its place");
            fresh.push(client);
        }
        // The fresh ones are the stalest now, the first first.
        let mut last = rig.dial().expect("one more fresh client admitted");
        last.send("AUTH ok\nping\n");
        assert_eq!(last.line(), "OK ping");
        assert!(
            rest_until_closed(&mut fresh[0]).is_empty(),
            "the first fresh client, now the stalest"
        );
        parked(CAP, "the last fresh client parked");
        // The claim holder is still connected and still claims.
        assert_eq!(
            keeper.request("who"),
            keeper_id,
            "the claim holder's connection kept"
        );
        assert!(rig.stub.released().is_empty(), "no claim released");
        for (index, client) in fresh.iter_mut().enumerate().skip(1) {
            assert_eq!(client.request("ping"), "OK ping", "fresh client {index}");
        }
    }

    // At the cap with nothing parked: a wait on the wait lane and one on a
    // request lane, and a request lane free.
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 1,
        inline: 1,
        open: 2,
        subscriptions: 1,
    });
    let mut waits = Vec::new();
    for index in 0..2 {
        let mut wait = rig.dial().expect("a wait admitted");
        wait.send("AUTH ok\nwait\n");
        waits.push(wait);
        rig.settle("placed", |c, f| {
            f == (index, 1) && c.inline == index && c.rpc == index
        });
    }
    assert!(rig.dial().is_none(), "nothing parked makes room at the cap");
    let census = rig.lanes.census();
    assert_eq!(
        (census.open, census.parked, census.rpc),
        (2, 0, 1),
        "refused at the cap, nothing parked, a request lane free"
    );
    rig.stub.let_finish(true);
    rig.stub.let_finish(false);
    for wait in &mut waits {
        assert_eq!(wait.line(), "OK wait");
    }
}

/// CLIENTS ARRIVING TOGETHER AT THE CAP: three fresh clients are handed to
/// the parker before its next pass — more than the two request lanes — and
/// each is placed in the place of one of the three parked idle connections,
/// stalest first, and served. The model binds each step (`Hand` three times,
/// then `Place` three times). Until 2026-10-19 the parker was handed no more
/// at once than there are request lanes, and the rest were turned away busy
/// beside parked connections (on a headless instance, 40 at once beside 1024
/// parked: 8 served, 32 busy); that bound back in place refuses the third
/// client here.
#[test]
fn clients_arriving_together_at_the_cap_are_each_served() {
    let model = control_lane_tenure_model();
    let rig = Rig::new(model_limits());
    let mut facts = Facts::default();
    let observe = |facts: Facts, what: &str, want: &dyn Fn(Census) -> bool| {
        let census = rig.settle(what, |c, f| want(c) && c.rpc == f.0 && c.waits == f.1);
        project(census, rig.stub.in_flight(), facts)
    };
    let mut at = model.init_state();
    let mut idle = Vec::new();
    for n in 1..=3 {
        idle.push(rig.idle().expect("an idle connection admitted"));
        facts.arrivals += 1;
        let next = observe(facts, "idle parked", &|c| c.parked == n && c.rpc == 0);
        assert_path(&model, &at, &["Admit", "Finish"], &next);
        at = next;
    }
    let pause = rig
        .lanes
        .parker_pause()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let mut fresh = Vec::new();
    for index in 0..3 {
        fresh.push(rig.dial().unwrap_or_else(|| {
            panic!("fresh client {index} refused at the cap beside 3 parked connections")
        }));
        facts.arrivals += 1;
    }
    let census = rig.lanes.census();
    assert_eq!(
        (census.placing, census.parked, census.open),
        (3, 3, 3),
        "three handed to the parker at once, beside three parked"
    );
    let handed = project(census, rig.stub.in_flight(), facts);
    assert_path(&model, &at, &["Hand", "Hand", "Hand"], &handed);
    drop(pause);
    let placed = observe(facts, "each placed", &|c| {
        c.placing == 0 && c.parked == 3 && c.open == 3 && c.spared == 3
    });
    assert_path(&model, &handed, &["Place", "Place", "Place"], &placed);
    for (index, victim) in idle.iter_mut().enumerate() {
        assert!(
            rest_until_closed(victim).is_empty(),
            "idle connection {index}, closed for a fresh client"
        );
    }
    for (index, client) in fresh.iter_mut().enumerate() {
        client.send("AUTH ok\nping\n");
        assert_eq!(
            client.line(),
            "OK ping",
            "fresh client {index}, in its place"
        );
    }
    rig.settle("each served and parked", |c, _| {
        c.parked == 3 && c.open == 3 && c.rpc == 0 && c.spared == 0
    });
}

/// A PLACE FREED SINCE ADMISSION: a fresh client handed to the parker at the
/// cap, and a connection ending on its request lane before the parker's next
/// pass. The fresh one is parked in the slots that one left, and no parked
/// connection is closed for it. The model binds each step (`Hand`, `Close`,
/// `PlaceFreed`).
#[test]
fn a_place_freed_since_admission_closes_no_one() {
    let model = control_lane_tenure_model();
    let rig = Rig::new(model_limits());
    let mut facts = Facts::default();
    let observe = |facts: Facts, what: &str, want: &dyn Fn(Census) -> bool| {
        let census = rig.settle(what, |c, f| want(c) && c.rpc == f.0 && c.waits == f.1);
        project(census, rig.stub.in_flight(), facts)
    };
    let mut at = model.init_state();
    let mut idle = Vec::new();
    for n in 1..=2 {
        idle.push(rig.idle().expect("an idle connection admitted"));
        facts.arrivals += 1;
        let next = observe(facts, "idle parked", &|c| c.parked == n && c.rpc == 0);
        assert_path(&model, &at, &["Admit", "Finish"], &next);
        at = next;
    }
    // A worker with its `quit` pipelined behind its work: the lane closes it
    // there once the work is done, never parking it.
    let mut worker = rig.dial().expect("the worker admitted");
    facts.arrivals += 1;
    worker.send("AUTH ok\nwork\nquit\n");
    let working = observe(facts, "the worker working", &|c| {
        c.open == 3 && c.parked == 2 && c.rpc == 1
    });
    assert_path(&model, &at, &["Admit"], &working);
    let pause = rig
        .lanes
        .parker_pause()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let mut fresh = rig.dial().expect("handed to the parker at the cap");
    facts.arrivals += 1;
    rig.stub.let_finish(false);
    assert_eq!(worker.line(), "OK work");
    assert_eq!(worker.line(), "<EOF>", "the worker closed on its lane");
    let freed = observe(facts, "the worker's place freed", &|c| {
        c.open == 2 && c.parked == 2 && c.placing == 1
    });
    assert_path(&model, &working, &["Hand", "Close"], &freed);
    drop(pause);
    let placed = observe(facts, "the fresh one placed", &|c| {
        c.open == 3 && c.parked == 3 && c.placing == 0 && c.spared == 1
    });
    assert_path(&model, &freed, &["PlaceFreed"], &placed);
    for (index, client) in idle.iter_mut().enumerate() {
        assert_eq!(
            client.request("ping"),
            "OK ping",
            "idle connection {index}, kept"
        );
    }
    fresh.send("AUTH ok\nping\n");
    assert_eq!(fresh.line(), "OK ping", "the fresh one, in the freed place");
}

/// WHAT DOES NOT GIVE WAY: at a cap held by connections that hold a claim
/// and one fresh client still making its handshake, the next fresh client is
/// answered busy — no claim released, and the one placed before it not closed
/// for it before it was served once. Once that one has made its handshake, it
/// gives way to the next like any other. The model binds each step.
#[test]
fn claim_holders_and_handshakes_in_progress_do_not_give_way() {
    let model = control_lane_tenure_model();
    let rig = Rig::new(model_limits());
    let mut facts = Facts::default();
    let observe = |facts: Facts, what: &str, want: &dyn Fn(Census) -> bool| {
        let census = rig.settle(what, |c, f| want(c) && c.rpc == f.0 && c.waits == f.1);
        project(census, rig.stub.in_flight(), facts)
    };
    let mut at = model.init_state();
    let mut keepers = Vec::new();
    for n in 1..=2 {
        let mut keeper = rig.idle().expect("a claim holder admitted");
        facts.arrivals += 1;
        let parked = observe(facts, "parked", &|c| c.parked == n && c.spared == n - 1);
        assert_path(&model, &at, &["Admit", "Finish"], &parked);
        assert_eq!(keeper.request("claim"), "OK claim");
        let id = keeper.request("who");
        at = observe(facts, "the claim holder parked", &|c| {
            c.parked == n && c.rpc == 0 && c.spared == n
        });
        // Its claim, then `who`: two requests, the claim's spared after it.
        let claimed = [
            "Wake", "Pick", "Finish", "Spare", "Unspare", "Wake", "Pick", "Finish", "Spare",
        ];
        assert_path(&model, &parked, &claimed, &at);
        keepers.push((keeper, id));
    }
    let mut idle = rig.idle().expect("an idle connection admitted");
    facts.arrivals += 1;
    let full = observe(facts, "the cap reached", &|c| {
        c.parked == 3 && c.spared == 2
    });
    assert_path(&model, &at, &["Admit", "Finish"], &full);

    // The first fresh client takes the idle connection's place.
    let mut first = rig
        .dial()
        .expect("the first fresh client handed to the parker");
    facts.arrivals += 1;
    assert!(
        rest_until_closed(&mut idle).is_empty(),
        "the idle one, closed"
    );
    let placed = observe(facts, "the first placed", &|c| {
        c.parked == 3 && c.placing == 0 && c.spared == 3
    });
    assert_path(&model, &full, &["Hand", "Place"], &placed);
    // The next is handed over too, and answered busy: nothing gives way.
    let mut second = rig
        .dial()
        .expect("the second fresh client handed to the parker");
    facts.arrivals += 1;
    assert_eq!(second.line(), BUSY, "nothing gives way to the second");
    assert_eq!(second.line(), "<EOF>", "and it is hung up");
    let turned = observe(facts, "the second turned away", &|c| {
        c.parked == 3 && c.placing == 0 && c.spared == 3
    });
    assert_path(&model, &placed, &["Hand", "TurnAway"], &turned);
    assert!(rig.stub.released().is_empty(), "no claim released");
    first.send("AUTH ok\nping\n");
    assert_eq!(first.line(), "OK ping", "the first, served in its place");
    let served = observe(facts, "the first served", &|c| {
        c.parked == 3 && c.rpc == 0 && c.spared == 2
    });
    assert_path(
        &model,
        &turned,
        &["Unspare", "Wake", "Pick", "Finish"],
        &served,
    );
    for (keeper, id) in &mut keepers {
        assert_eq!(&keeper.request("who"), id, "a claim holder, kept");
    }
    // A lane is busy until it has parked its connection after the answer, and
    // the cap hands a fresh client over only with a request lane free.
    rig.settle("the claim holders parked again", |c, _| {
        c.parked == 3 && c.rpc == 0
    });
    // Now made its handshake, the first gives way to the next.
    let mut third = rig
        .dial()
        .expect("the third fresh client handed to the parker");
    facts.arrivals += 1;
    assert!(
        rest_until_closed(&mut first).is_empty(),
        "the first, closed for the third"
    );
    third.send("AUTH ok\nping\n");
    assert_eq!(third.line(), "OK ping", "the third, in its place");
    let third_id = third.request("who");
    assert!(rig.stub.released().is_empty(), "no claim released");
    let third_served = observe(facts, "the third served", &|c| {
        c.parked == 3 && c.rpc == 0 && c.spared == 2
    });
    assert_path(
        &model,
        &served,
        &[
            "Hand", "Place", "Unspare", "Wake", "Pick", "Finish", "Wake", "Pick", "Finish",
        ],
        &third_served,
    );

    // A claim's cleanup flag stays set until this connection ends, but a
    // released claim must no longer spare its idle connection at the cap.
    assert_eq!(third.request("claim"), "OK claim");
    let all_claimed = observe(facts, "three live claim holders parked", &|c| {
        c.parked == 3 && c.rpc == 0 && c.spared == 3
    });
    assert_path(
        &model,
        &third_served,
        &["Wake", "Pick", "Finish", "Spare"],
        &all_claimed,
    );
    let released_id: u64 = keepers[0].1.strip_prefix("OK ").unwrap().parse().unwrap();
    assert_eq!(keepers[0].0.request("unclaim"), "OK unclaim");
    let unclaimed = rig.settle("the released claimant parked", |c, f| {
        c.rpc == f.0 && c.rpc == 0 && c.parked == 3 && c.spared == 2 && c.waits == f.1
    });
    let released = project(unclaimed, rig.stub.in_flight(), facts);
    assert_path(
        &model,
        &all_claimed,
        &["Unspare", "Wake", "Pick", "Finish"],
        &released,
    );
    let mut fourth = rig.dial().expect("a fresh client handed over at the cap");
    fourth.send("AUTH ok\nping\n");
    assert_eq!(fourth.line(), "OK ping", "the released claimant gives way");
    assert!(rest_until_closed(&mut keepers[0].0).is_empty());
    assert_eq!(
        rig.stub.released(),
        vec![released_id],
        "Drop cleanup remains"
    );
    let (keeper, id) = &mut keepers[1];
    assert_eq!(&keeper.request("who"), id, "live claim kept");
    assert_eq!(third.request("who"), third_id, "live claim kept");
}

/// A connection holding a claim (a supervisor's or a driver's, bound to the
/// connection it claimed on, and ended with it) is the byte budget's LAST
/// candidate: past the budget the parker closes a holder with no claim first,
/// though the claimant has gone longer without a byte. Last, not exempt: when
/// only claimants are left past the budget they close too (the next test's
/// holders all claimed). Without the claim's rank the stalest holder went
/// whatever it held, and a supervisor that had not begun to read a large
/// answer on its claim connection lost its claim with it.
#[test]
fn a_budget_close_takes_a_holder_without_a_claim_before_a_claimant() {
    let answer = BIG_ANSWER + "OK \n".len();
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 1,
        inline: 1,
        open: 16,
        subscriptions: 1,
    });
    rig.lanes.set_parked_bytes_max(usize::MAX);
    let mut idle = rig.idle().expect("an idle connection admitted");
    rig.settle("idle parked", |c, _| c.parked == 1 && c.rpc == 0);
    // Stalest first: the claimant, then a holder of the same answer with no
    // claim, then the newest; each parked before the next asks.
    let mut claimant = rig.idle().expect("a claim holder admitted");
    assert_eq!(claimant.request("claim"), "OK claim");
    let id: u64 = claimant
        .request("who")
        .trim_start_matches("OK ")
        .parse()
        .expect("a serial");
    let mut holders = Vec::new();
    for index in 0..3 {
        let holder = if index == 0 {
            &mut claimant
        } else {
            holders.push(rig.idle().expect("a holder admitted"));
            holders.last_mut().expect("just pushed")
        };
        holder.send(&format!("answer {index} big\n"));
        rig.settle("the holder parked owing its answer", |c, _| {
            c.rpc == 0 && c.parked == index + 2 && c.held >= (index + 1) * (BIG_ANSWER / 2)
        });
    }
    // Any two holders fit; all three (each owed the unsent rest of an answer,
    // at least three quarters of it) do not.
    rig.lanes.set_parked_bytes_max(2 * answer);
    // A pass: the idle connection's request wakes the parker.
    assert_eq!(idle.request("ping"), "OK ping");
    rig.settle("one holder closed for the budget", |c, _| {
        c.parked == 3 && c.open == 3 && c.rpc == 0
    });
    assert!(
        !rig.stub.released().contains(&id),
        "the budget closed the claimant, though a holder without a claim was there to close"
    );
    assert!(
        whole_or_cut(&mut claimant, answer),
        "the claimant's whole answer"
    );
    assert_eq!(
        claimant.request("who"),
        format!("OK {id}"),
        "the claimant served on"
    );
    let [unclaimed, newest] = &mut holders[..] else {
        panic!("two holders without a claim");
    };
    assert!(
        !whole_or_cut(unclaimed, answer),
        "the stalest holder without a claim is the one closed"
    );
    assert!(whole_or_cut(newest, answer), "the newest holder kept");
}

/// A close for the byte budget whose claim's release panics costs that
/// connection alone: four claim holders park, each owed a large answer, then
/// the budget shrinks, and in one pass the parker closes three of them,
/// every release panicking, and goes on — the fourth reads its whole answer,
/// and a parked connection is still served. Each such panic used to unwind
/// the whole pass, and three with no pass completed between them stop the
/// parker, closing every connection.
#[test]
fn a_close_for_the_budget_that_panics_costs_that_connection_alone() {
    const HOLDERS: usize = 4;
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 1,
        inline: 1,
        open: 16,
        subscriptions: 1,
    });
    rig.lanes.set_parked_bytes_max(usize::MAX);
    let mut idle = rig.idle().expect("an idle connection admitted");
    let mut holders = Vec::new();
    for index in 0..HOLDERS {
        let mut holder = rig.idle().expect("a claim holder admitted");
        assert_eq!(holder.request("claim"), "OK claim");
        let id: u64 = holder
            .request("who")
            .trim_start_matches("OK ")
            .parse()
            .expect("a serial");
        rig.stub
            .panic_release
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(id);
        holder.send(&format!("answer {index} big\n"));
        holders.push(holder);
        rig.settle("the holder parked owing its answer", |c, _| {
            c.rpc == 0 && c.parked == index + 2 && c.held >= (index + 1) * (BIG_ANSWER / 2)
        });
    }
    rig.lanes.set_parked_bytes_max(BIG_ANSWER / 2);
    // A pass: the idle connection's request wakes the parker.
    assert_eq!(idle.request("ping"), "OK ping");
    let census = rig.settle("three closed for the budget", |c, _| {
        c.parked == 2 && c.open == 2 && c.rpc == 0
    });
    assert!(census.parking, "the parker goes on: {census:?}");
    let whole = holders
        .iter_mut()
        .map(|holder| whole_or_cut(holder, BIG_ANSWER + "OK \n".len()))
        .filter(|&whole| whole)
        .count();
    assert_eq!(whole, 1, "one holder kept, its whole answer read");
    assert_eq!(
        idle.request("ping"),
        "OK ping",
        "a parked connection, served"
    );
    assert!(rig.lanes.census().parking, "the parker still runs");
}

/// WHAT THE PARKED HOLD (ATERM-KI-1015-13, until 2026-10-19): clients that ask for a large answer
/// and never read it each park owing it. Past the budget the parker closes the
/// stalest of them (each reads what it had been sent, then the hang-up) until
/// the rest fit, and the rest read their answers whole — never the last one
/// holding any, so an answer larger than the budget alone is still delivered.
/// Idle connections hold no byte. The negative control is no budget: every
/// answer stays held, one per connection (the cap times one answer was the
/// bound: about 450 MB at 1024).
#[test]
fn the_parked_hold_no_more_than_their_budget() {
    const IDLE: usize = 4;
    const READERS: usize = 5;
    let answer = BIG_ANSWER + "OK \n".len();
    for (budget, kept) in [
        (2 * BIG_ANSWER + BIG_ANSWER / 2, 2),
        (BIG_ANSWER / 2, 1),
        (usize::MAX, READERS),
    ] {
        let rig = Rig::new(LaneLimits {
            rpc: 2,
            wait: 1,
            inline: 1,
            open: 16,
            subscriptions: 1,
        });
        rig.lanes.set_parked_bytes_max(budget);
        // Each parked before the next dials: a lane is busy until its request
        // is answered AND its connection is placed.
        let _idle: Vec<Client> = (0..IDLE)
            .map(|index| {
                let idle = rig.idle().expect("an idle connection admitted");
                rig.settle("idle parked", |c, _| c.parked == index + 1 && c.rpc == 0);
                idle
            })
            .collect();
        let mut readers = Vec::new();
        for index in 0..READERS {
            let mut reader = rig.idle().expect("a reader admitted");
            reader.send(&format!("answer {index} big\n"));
            readers.push(reader);
            let left = (index + 1).min(kept);
            // The unsent rest of the answers left, and nothing else: the idle
            // hold nothing. (A lane delivers what the socket takes at once,
            // far less than a quarter of an answer, before it parks the rest.)
            rig.settle("the answers parked within the budget", |c, _| {
                c.parked == IDLE + left
                    && c.open == IDLE + left
                    && c.rpc == 0
                    && (left * (answer - BIG_ANSWER / 4)..=left * answer).contains(&c.held)
            });
        }
        let held = rig.lanes.census().held;
        eprintln!(
            "budget {budget}: {READERS} unread answers of {answer} bytes, {kept} kept, {held} bytes held"
        );
        // Which were the stalest is the parker's to judge (a peer's socket
        // that makes room stirs it): counted, not named.
        let whole = readers
            .iter_mut()
            .map(|reader| whole_or_cut(reader, answer))
            .filter(|&whole| whole)
            .count();
        assert_eq!(whole, kept, "the readers kept read their whole answers");
    }
}

/// What a reader owed a large answer (`answer` bytes, newline included) reads
/// of it: `true` for the whole answer (it was kept), `false` for what it had
/// been sent and then the hang-up (it was closed partway; a minute's read is
/// the hang detector for one never closed).
fn whole_or_cut(reader: &mut Client, answer: usize) -> bool {
    let mut got = Vec::new();
    match reader.reader.read_until(b'\n', &mut got) {
        Ok(_) if got.len() == answer && got.ends_with(b"\n") => return true,
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        Err(error) => panic!(
            "the connection was neither answered nor closed: {error} after {} bytes",
            got.len()
        ),
    }
    assert!(
        got.starts_with(b"OK x") && got.len() < answer && !got.ends_with(b"\n"),
        "a reader closed partway read {} bytes",
        got.len()
    );
    assert!(rest_until_closed(reader).is_empty(), "then the hang-up");
    false
}

/// A STEADY READER keeps its answer when a later one parks unread past the
/// byte budget: the budget closes the later holder, which has taken nothing,
/// never the reader taking its own a chunk at a time, however recently the
/// later one was served (b144d471a4's guarantee, ported from the
/// fix/r21-smallfixes branch). The reader is closed if the budget ranks the
/// two by their last stir alone: the later answer is stamped as it parks,
/// after the reader's last read.
#[test]
fn a_steady_reader_keeps_its_answer_when_a_later_one_parks() {
    let answer = BIG_ANSWER + "OK \n".len();
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 1,
        inline: 1,
        open: 8,
        subscriptions: 1,
    });
    // One answer fits; two (the reader's unread rest and the later one's) do
    // not, until the reader has taken more than half of its own.
    rig.lanes.set_parked_bytes_max(BIG_ANSWER + BIG_ANSWER / 2);
    let mut steady = rig.idle().expect("the reader admitted");
    rig.settle("idle parked", |c, _| c.parked == 1 && c.rpc == 0);
    steady.send("answer 0 big\n");
    rig.settle("the reader parked owing its answer", |c, _| {
        c.rpc == 0 && c.parked == 1 && c.held >= BIG_ANSWER / 2
    });
    let (started, has_started) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut started = Some(started);
        let mut got = 0usize;
        let mut chunk = vec![0u8; 16 * 1024];
        while got < answer {
            match steady.reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => got += n,
            }
            if let Some(started) = started.take() {
                let _ = started.send(());
            }
            // Its pace: a chunk at a time, as a client working through it.
            std::thread::sleep(Duration::from_millis(5));
        }
        got
    });
    has_started
        .recv_timeout(SETTLE)
        .expect("the steady reader took its first bytes");
    let mut later = rig.idle().expect("the later one admitted");
    later.send("answer 1 big\n");
    let got = reader.join().expect("the steady reader");
    assert_eq!(
        got, answer,
        "the steady reader was cut off at {got} of {answer} bytes"
    );
    assert!(
        !whole_or_cut(&mut later, answer),
        "the later holder, which took nothing, is the one closed"
    );
}

/// What a lane began to read of a large request before it parked leaves the
/// heap as it parks, as the parker's own reads would have gathered it: a line
/// not whole yet past `SPILL_FROM` moves to a mapping with room for the
/// longest line any request may have, every byte kept; a short begun request
/// stays on the heap; a frame's header buffered behind an answer moves with
/// room for its body, and is served from there. (Which share the lane reads
/// and which the parker is the sockets' timing; this holds the lane's share to
/// the parker's bound.)
#[test]
fn a_large_request_a_lane_began_leaves_the_heap_as_it_parks() {
    let lanes = Lanes::new(LaneLimits {
        rpc: 1,
        wait: 1,
        inline: 1,
        open: 4,
        subscriptions: 1,
    });
    lanes.publish(Arc::new(Stub::default()));
    let opened = Arc::new(crate::budget::Slots::new(4));
    let begun = |bytes: &[u8]| {
        let (server, peer) = CtlStream::pair().expect("private control pair");
        let open = opened.try_take().expect("an open slot");
        let mut conn = lanes.fresh_conn(server, open);
        conn.carry = super::Unread::holding(bytes.to_vec());
        conn.hold_only_what_is_owed();
        (conn, peer)
    };
    let line = vec![b'y'; 40 * 1024];
    let (large, _large_peer) = begun(&line);
    assert_eq!(large.carry.bytes.capacity(), 0, "the heap buffer let go");
    assert_eq!(
        large.held_bytes(),
        super::MAX_REQUEST_LINE + 1,
        "a mapping with room for the longest line"
    );
    assert!(large.carry.as_slice() == line.as_slice(), "every byte kept");
    let (short, _short_peer) = begun(b"vers");
    assert!(
        short.carry.mapped.is_none(),
        "a short begun request stays on the heap"
    );
    assert_eq!(short.carry.as_slice(), b"vers");

    // The next frame's header is actionable before its body is whole. It
    // can be buffered behind a reply and moved into mapped storage on park.
    let request = b"feed-bin 32768\n".to_vec();
    let (mut pipelined, mut peer) = begun(&request);
    pipelined.scope = Some(Scope::Owner);
    pipelined.reply.extend_from_slice(b"OK first\n");
    pipelined.hold_only_what_is_owed();
    assert_eq!(
        pipelined.carry.bytes.capacity(),
        0,
        "the heap buffer let go"
    );
    assert!(
        pipelined.carry.mapped.is_some(),
        "the header and its body's room mapped"
    );
    assert_eq!(pipelined.carry.as_slice(), request);
    assert!(matches!(
        super::held(&request, true),
        super::Held::Header(_)
    ));
    // A hang detector, never a latency budget.
    let hang_detector = Some(aterm_spec::wait::MUST_HAPPEN);
    pipelined.stream.set_write_timeout(hang_detector).unwrap();
    peer.set_read_timeout(hang_detector).unwrap();
    assert!(matches!(pipelined.deliver_parked(), super::Step::Serve));
    let mut reply = [0u8; 9];
    peer.read_exact(&mut reply).unwrap();
    assert_eq!(&reply, b"OK first\n");
    assert_eq!(
        pipelined.carry.as_slice(),
        request,
        "the header is still buffered"
    );
    pipelined.header_done = true;
    assert!(matches!(pipelined.deliver_parked(), super::Step::Stay));
}

/// A connection keeps its serial — the binding a connection-scoped claim is
/// made to — across every lane that carries it, and its claim is released
/// when it ends, however it ends: here, its peer hanging up while parked.
#[test]
fn claims_follow_the_connection_across_lanes_and_end_with_it() {
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 2,
        inline: 1,
        open: 8,
        subscriptions: 1,
    });
    let mut driver = rig.idle().expect("admitted");
    assert_eq!(driver.request("claim"), "OK claim");
    let id = driver.request("who");
    rig.settle("parked", |c, _| c.parked == 1 && c.rpc == 0);
    // Across a wait lane: the wait, then `who` served where it lands.
    driver.send("wait\n");
    rig.settle("on the wait lane", |_, f| f == (0, 1));
    rig.stub.let_finish(true);
    assert_eq!(driver.line(), "OK wait");
    assert_eq!(driver.request("who"), id, "one serial across every lane");
    assert!(rig.stub.released().is_empty(), "no release while it lives");
    drop(driver);
    let deadline = Instant::now() + SETTLE;
    while rig.stub.released().is_empty() {
        assert!(
            Instant::now() < deadline,
            "the claim outlived its connection"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        rig.stub.released(),
        vec![
            id.trim_start_matches("OK ")
                .parse::<u64>()
                .expect("a serial")
        ]
    );
    rig.settle("closed", |c, _| c.open == 0 && c.parked == 0);
}

/// Requests written together are answered in order, whichever lanes serve
/// them: a buffered next request is never left behind a move.
#[test]
fn pipelined_requests_answer_in_order() {
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 2,
        inline: 1,
        open: 8,
        subscriptions: 1,
    });
    let mut driver = rig.idle().expect("admitted");
    // Whichever lane serves each blocking request, it may finish.
    for _ in 0..3 {
        rig.stub.let_finish(true);
        rig.stub.let_finish(false);
    }
    driver.send("wait one\nping\nwho\nwork two\nlane\n");
    assert_eq!(driver.line(), "OK wait one");
    assert_eq!(driver.line(), "OK ping");
    assert!(driver.line().starts_with("OK "));
    assert_eq!(driver.line(), "OK work two");
    assert!(driver.line().starts_with("OK "));
    // Sent one at a time, the wait moves to a wait lane, and once the
    // connection has parked again its next request comes back to a request
    // lane. (A request already readable when a wait lane answers is served
    // there, where it is.)
    assert_eq!(driver.request("wait three"), "OK wait three");
    rig.settle("parked again", |c, f| {
        c.parked == 1 && c.rpc == 0 && c.waits == 0 && f == (0, 0)
    });
    assert_eq!(driver.request("lane"), "OK rpc");
}

/// Read one line within `within`, or say that none came. (macOS refuses a
/// read timeout on a socket its peer has already closed; a read there cannot
/// block anyway, so the refusal is ignored.) A hang-up reads `<EOF>` on every
/// platform: Linux answers a read on a socket whose peer closed it with this
/// side's request still unread there with `ECONNRESET`, where macOS answers
/// end-of-file (measured on the Linux lane).
fn line_within(client: &mut Client, within: Duration) -> Option<String> {
    let _ = client.stream.set_read_timeout(Some(within));
    let mut reply = String::new();
    let read = client.reader.read_line(&mut reply);
    let _ = client.stream.set_read_timeout(Some(SETTLE));
    match read {
        Ok(0) => Some("<EOF>".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {
            Some("<EOF>".to_string())
        }
        Ok(_) => Some(reply.trim_end().to_string()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            None
        }
        Err(error) => Some(format!("<{error}>")),
    }
}

/// WAITS PAST THE WAIT LANES (the review of 2026-09-27): with every wait lane
/// taken, a wait is served on a request lane only within the budget, and the
/// one past it is answered busy at once — so a parked driver's next request,
/// and a fresh client, are still served while every wait runs. The negative
/// control is the fallback as first shipped (a budget of every request lane):
/// the same waits take both lanes, and the parked driver's request waits
/// behind them — never refused, never answered — until a wait ends.
#[test]
fn waits_past_every_wait_lane_leave_a_request_lane_for_requests() {
    let model = control_lane_tenure_model();
    for (inline, shipping) in [(1, true), (2, false)] {
        let rig = Rig::new(LaneLimits {
            rpc: 2,
            wait: 1,
            inline,
            open: 8,
            subscriptions: 1,
        });
        let mut parked = rig.idle().expect("the parked driver admitted");
        rig.settle("the driver parked", |c, _| c.parked == 1 && c.rpc == 0);
        let mut waits = Vec::new();
        for index in 0..2 {
            let mut wait = rig.dial().expect("a wait admitted");
            wait.send("AUTH ok\nwait\n");
            waits.push(wait);
            // The first takes the wait lane, the second a request lane.
            rig.settle("placed", |c, f| {
                f == (index, 1) && c.inline == index && c.rpc == index
            });
        }
        // A third wait: no wait lane is free, and the one request lane a wait
        // may take is taken.
        let mut third = rig.dial().expect("the third wait admitted");
        third.send("AUTH ok\nwait\n");
        if shipping {
            assert_eq!(
                line_within(&mut third, SETTLE).as_deref(),
                Some(BUSY),
                "the wait past the budget is answered busy, with no wait ending"
            );
            assert_eq!(third.request("ping"), "OK ping", "and its connection kept");
            let census = rig.settle("the third parked", |c, f| {
                f == (1, 1) && c.rpc == 1 && c.parked == 2 && c.waits == f.1
            });
            let facts = Facts {
                arrivals: 4,
                busied: 1,
                ..Facts::default()
            };
            let state = project(census, rig.stub.in_flight(), facts);
            assert!(
                model.check_invariant("RequestLaneLeftForRequests", &state),
                "{state:?}"
            );
            // The parked driver's next request, and a fresh client, are served
            // at once on the lane the waits leave.
            parked.send("lane\n");
            assert_eq!(
                line_within(&mut parked, SETTLE).as_deref(),
                Some("OK rpc"),
                "the parked driver's request, beside every wait"
            );
            // (A lane is busy until its connection is placed again.)
            rig.settle("the driver parked again", |c, f| {
                f == (1, 1) && c.rpc == 1 && c.parked == 2
            });
            let mut fresh = rig
                .dial()
                .expect("a fresh client admitted beside every wait");
            fresh.send("AUTH ok\nping\n");
            assert_eq!(line_within(&mut fresh, SETTLE).as_deref(), Some("OK ping"));
        } else {
            let census = rig.settle("the third on the other request lane", |c, f| {
                f == (2, 1) && c.rpc == 2 && c.inline == 2 && c.waits == f.1
            });
            let facts = Facts {
                arrivals: 4,
                ..Facts::default()
            };
            let state = project(census, rig.stub.in_flight(), facts);
            assert!(
                !model.check_invariant("RequestLaneLeftForRequests", &state),
                "negative control: the model rejects waits on every request lane: {state:?}"
            );
            assert!(
                rig.dial().is_none(),
                "negative control: waits on every request lane refuse a fresh client"
            );
            parked.send("lane\n");
            // It is served only when a wait ends and frees its lane.
            rig.stub.let_finish(false);
            assert_eq!(parked.line(), "OK rpc");
            rig.stub.let_finish(false);
            assert_eq!(third.line(), "OK wait");
        }
        rig.stub.let_finish(true);
        if shipping {
            rig.stub.let_finish(false);
        }
        for wait in &mut waits {
            assert_eq!(wait.line(), "OK wait");
        }
    }
}

/// A WAIT WITH A REQUEST PIPELINED BEHIND IT (the review of the budget,
/// 2026-09-27) — a wait with `version` and `lane` behind it in one write,
/// one connection each — moves
/// to a wait lane like any other, carrying the requests behind it: more such
/// waits than the request lanes and their budget are all held, none is answered
/// busy, a fresh client is served beside them, and each connection's pipelined
/// requests are answered after its wait, in order, on the wait lane it moved
/// to. The negative control is the rule before the carry (a wait with anything
/// buffered behind it stays on its request lane): the first takes the budget's
/// one place and every later one is answered busy with seven wait lanes free —
/// the fifth connection turned away — which the model rejects (`WaitLaneTakenFirst`).
#[test]
fn pipelined_waits_move_with_the_requests_behind_them() {
    const WAITS: usize = 5;
    let model = control_lane_tenure_model();
    for shipping in [true, false] {
        let rig = Rig::new(LaneLimits {
            rpc: 2,
            wait: 8,
            inline: 1,
            open: 16,
            subscriptions: 1,
        });
        if !shipping {
            rig.lanes.keep_pipelined_waits();
        }
        let mut relays = Vec::new();
        for index in 0..WAITS {
            let mut relay = rig.dial().expect("a relay admitted");
            // One write, so the lane's first read takes every line: the wait is
            // read with the rest already buffered behind it.
            relay.send(&format!("AUTH ok\nwait relay {index}\nversion\nlane\n"));
            if shipping {
                rig.settle("on a wait lane", |c, f| {
                    f == (0, index + 1) && c.waits == index + 1 && c.rpc == 0 && c.inline == 0
                });
            } else if index == 0 {
                rig.settle("kept on its request lane", |c, f| {
                    f == (1, 0) && c.inline == 1 && c.waits == 0
                });
            } else {
                assert_eq!(
                    line_within(&mut relay, SETTLE).as_deref(),
                    Some(BUSY),
                    "negative control: relay {index} is turned away beside free wait lanes"
                );
                assert_eq!(relay.line(), "OK version");
                assert_eq!(relay.line(), "OK rpc");
                // (A lane is busy until its connection is placed again.)
                rig.settle("turned away, then parked", |c, _| {
                    c.parked == index && c.rpc == 1
                });
            }
            relays.push(relay);
        }
        let census = rig.settle("every relay placed", |c, f| {
            if shipping {
                f == (0, WAITS) && c.rpc == 0 && c.waits == WAITS
            } else {
                f == (1, 0) && c.rpc == 1 && c.inline == 1 && c.parked == WAITS - 1
            }
        });
        let facts = Facts {
            arrivals: count(WAITS),
            busied: i64::from(!shipping),
            bypassed: i64::from(!shipping),
            ..Facts::default()
        };
        let state = project(census, rig.stub.in_flight(), facts);
        if shipping {
            assert_eq!(
                (census.waits, census.inline),
                (WAITS, 0),
                "every relay held on a wait lane"
            );
            assert!(
                model.check_invariant("WaitLaneTakenFirst", &state),
                "{state:?}"
            );
            let mut fresh = rig.dial().expect("a fresh client beside every relay");
            fresh.send("AUTH ok\nlane\n");
            assert_eq!(line_within(&mut fresh, SETTLE).as_deref(), Some("OK rpc"));
            for _ in 0..WAITS {
                rig.stub.let_finish(true);
            }
            for (index, relay) in relays.iter_mut().enumerate() {
                assert_eq!(
                    relay.line(),
                    format!("OK wait relay {index}"),
                    "relay {index} held, never answered busy"
                );
                assert_eq!(relay.line(), "OK version", "then the request behind it");
                assert_eq!(
                    relay.line(),
                    "OK wait",
                    "served on the wait lane it moved to"
                );
            }
            rig.settle("every relay parked", |c, f| {
                f == (0, 0) && c.parked == WAITS + 1 && c.waits == 0
            });
        } else {
            assert_eq!(
                census.waits, 0,
                "negative control: no relay reached a wait lane"
            );
            assert!(
                !model.check_invariant("WaitLaneTakenFirst", &state),
                "negative control: the model rejects a wait kept beside a free wait lane: \
                 {state:?}"
            );
            rig.stub.let_finish(false);
            assert_eq!(relays[0].line(), "OK wait relay 0");
            assert_eq!(relays[0].line(), "OK version");
            assert_eq!(relays[0].line(), "OK rpc");
        }
    }
}

/// A wait lane's spawn, held in [`Lanes::before_wait_spawn`] until the test
/// rules on it; every spawn after the ruling follows it at once.
#[derive(Default)]
struct HeldSpawn {
    state: Mutex<HeldState>,
    moved: Condvar,
}

#[derive(Default)]
struct HeldState {
    entered: usize,
    /// `Some(true)`: the spawn goes on; `Some(false)`: it fails.
    ruling: Option<bool>,
}

impl HeldSpawn {
    fn hold(&self) -> std::io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.entered += 1;
        self.moved.notify_all();
        loop {
            match state.ruling {
                Some(true) => return Ok(()),
                Some(false) => {
                    return Err(std::io::Error::other(
                        "injected: the wait lane cannot start",
                    ));
                }
                None => {
                    state = self
                        .moved
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
    }

    /// Wait (the hang detector) until `count` spawns have entered the hold.
    fn entered(&self, count: usize) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (state, waited) = self
            .moved
            .wait_timeout_while(state, SETTLE, |state| state.entered < count)
            .unwrap_or_else(PoisonError::into_inner);
        assert!(
            !waited.timed_out(),
            "{count} wait-lane spawns never began: {} did",
            state.entered
        );
    }

    fn entries(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entered
    }

    fn rule(&self, start: bool) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ruling = Some(start);
        self.moved.notify_all();
    }
}

/// A SLOW SPAWN (a wait lane's thread can take a while to create) holds no
/// lock: while B's new lane is still being spawned, the lane A's wait frees
/// takes the next wait, C's, through the request lane that defers it, and
/// answers it. When that spawn then fails, B's request lane gets back B's own
/// wait (never C's, which an idle lane was counted on) and serves it where it
/// is. The negative control holds the wait lanes' lock for as long as the
/// spawn takes, as `defer` did before, and checks only that every wait is
/// answered once the spawn ends: an absence is never judged by a window.
#[test]
fn a_slow_spawn_does_not_block_completing_waits() {
    for shipping in [true, false] {
        let rig = Rig::new(LaneLimits {
            rpc: 3,
            wait: 2,
            inline: 1,
            open: 8,
            subscriptions: 1,
        });
        let mut a = rig.dial().expect("A admitted");
        a.send("AUTH ok\nwait a\n");
        rig.settle("A on the first wait lane", |c, f| {
            f == (0, 1) && c.waits == 1 && c.wait_threads == 1 && c.rpc == 0
        });
        let held = Arc::new(HeldSpawn::default());
        let hook = Arc::clone(&held);
        rig.lanes.before_wait_spawn(move || hook.hold());
        // No lane is idle: B's wait grows a second, whose spawn is held.
        let mut b = rig.dial().expect("B admitted");
        b.send("AUTH ok\nwait b\nlane\n");
        held.entered(1);
        let lock_held = (!shipping).then(|| {
            rig.lanes
                .waits
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
        });
        rig.stub.let_finish(true);
        assert_eq!(a.line(), "OK wait a", "A's wait ends on its lane");
        if shipping {
            rig.settle("A's lane idle beside the held spawn", |c, f| {
                f == (0, 0) && c.wait_idle == 1 && c.waits == 1 && c.wait_threads == 2 && c.rpc == 1
            });
        }
        // C's wait, released in advance: an idle lane takes it, or it waits.
        rig.stub.let_finish(true);
        let mut c = rig.dial().expect("C admitted");
        c.send("AUTH ok\nwait c\nlane\n");
        if shipping {
            assert_eq!(
                line_within(&mut c, SETTLE).as_deref(),
                Some("OK wait c"),
                "C's wait is served while B's lane is still being spawned"
            );
            assert_eq!(c.line(), "OK wait", "by the lane A's wait freed");
            rig.settle("C parked, B's spawn still held", |c, f| {
                f == (0, 0) && c.wait_idle == 1 && c.waits == 1 && c.rpc == 1
            });
            assert_eq!(held.entries(), 1, "C's wait needed no spawn");
            held.rule(false);
            rig.settle(
                "B's wait given back and served on its request lane",
                |c, f| {
                    f == (1, 0)
                        && c.inline == 1
                        && c.waits == 0
                        && c.wait_threads == 1
                        && c.wait_idle == 1
                },
            );
            rig.stub.let_finish(false);
            assert_eq!(b.line(), "OK wait b");
            assert_eq!(b.line(), "OK rpc", "B was never on a wait lane");
        } else {
            drop(lock_held);
            held.rule(true);
            // B serves on its new lane; C on the freed lane, or on its request
            // lane if it deferred before that lane was idle again.
            rig.stub.let_finish(true);
            rig.stub.let_finish(false);
            assert_eq!(c.line(), "OK wait c");
            let _lane = c.line();
            assert_eq!(b.line(), "OK wait b");
            assert_eq!(b.line(), "OK wait", "B's lane started");
        }
    }
}

/// A panic while ONE parked connection wakes — here the release of its claims,
/// run when its peer hangs up while parked — costs that connection alone. The
/// parker goes on, and every other parked connection is still served. (Before,
/// the panic ended the parker's thread: every parked connection was stranded,
/// its next request never read, and every connection parked after it too.)
/// Three such wakes come in a row here, and none counts toward the parker's
/// stop (`PARKER_PANICS_IN_A_ROW`): a parker that let them reach its restart
/// would stop on the third and hang up the connection kept beside them.
#[test]
fn a_panic_as_one_parked_connection_wakes_costs_that_connection_alone() {
    const DOOMED: usize = 3;
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 1,
        inline: 1,
        open: 8,
        subscriptions: 1,
    });
    let mut doomed = Vec::new();
    let mut doomed_ids = Vec::new();
    for index in 0..DOOMED {
        let mut conn = rig.idle().expect("admitted");
        assert_eq!(conn.request("claim"), "OK claim");
        let id: u64 = conn
            .request("who")
            .trim_start_matches("OK ")
            .parse()
            .expect("a serial");
        rig.settle("parked", |c, _| c.parked == index + 1 && c.rpc == 0);
        doomed.push(conn);
        doomed_ids.push(id);
    }
    let mut kept = rig.idle().expect("admitted");
    rig.settle("all parked", |c, _| c.parked == DOOMED + 1 && c.rpc == 0);
    *rig.stub
        .panic_release
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = doomed_ids;
    // Their peers hang up while parked: the parker closes each, and each
    // release panics there, one after another.
    drop(doomed);
    rig.settle("the doomed ones closed", |c, _| {
        c.open == 1 && c.parked == 1
    });
    assert!(
        rig.lanes.census().parking,
        "{DOOMED} wake panics in a row must not stop the parker"
    );
    kept.send("ping\n");
    assert_eq!(
        line_within(&mut kept, SETTLE).as_deref(),
        Some("OK ping"),
        "a connection parked across the panics is still served"
    );
    // (A lane is busy until its connection is placed again.)
    rig.settle("parked again, the parker running", |c, _| {
        c.parking && c.parked == 1 && c.rpc == 0
    });
    let mut later = rig.idle().expect("a connection parked after the panics");
    rig.settle("parked after the panics", |c, _| {
        c.parked == 2 && c.rpc == 0
    });
    assert_eq!(later.request("ping"), "OK ping");
}

/// A panic in the parker's loop itself (injected, one pass) restarts the loop
/// over the SAME parked set: the connection whose readiness it was handling
/// when it panicked, a connection parked beside it, and one parked after it
/// are all served. (Before, the thread died with every parked connection in
/// it.)
#[test]
fn a_parker_panic_restarts_it_with_every_parked_connection() {
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 1,
        inline: 1,
        open: 8,
        subscriptions: 1,
    });
    let mut first = rig.idle().expect("admitted");
    let mut beside = rig.idle().expect("admitted");
    rig.settle("both parked", |c, _| c.parked == 2 && c.rpc == 0);
    rig.lanes.inject_parker_panics(1);
    first.send("ping\n");
    assert_eq!(
        line_within(&mut first, SETTLE).as_deref(),
        Some("OK ping"),
        "the connection that woke the panicking pass is served after the restart"
    );
    assert_eq!(
        beside.request("ping"),
        "OK ping",
        "its neighbour kept its place"
    );
    // (A lane is busy until its connection is placed again.)
    rig.settle("both parked again, the parker running", |c, _| {
        c.parking && c.parked == 2 && c.rpc == 0
    });
    let mut later = rig.idle().expect("a connection parked after the restart");
    rig.settle("three parked", |c, _| c.parked == 3 && c.rpc == 0);
    assert_eq!(later.request("ping"), "OK ping");
    assert_eq!(first.request("again"), "OK again");
}

/// A parker that cannot go on — its loop panicking pass after pass — STOPS
/// safely: every connection it held is closed (a hang-up its client rides
/// out, never a request left unread for ever), their claims end with them,
/// and the lanes serve on as without a parker: a fresh client is admitted and
/// answered, and keeps its lane while idle.
#[test]
fn a_parker_that_cannot_go_on_closes_what_it_holds_and_the_lanes_serve_on() {
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 1,
        inline: 1,
        open: 8,
        subscriptions: 1,
    });
    let mut claimed = rig.idle().expect("admitted");
    assert_eq!(claimed.request("claim"), "OK claim");
    let claimed_id: u64 = claimed
        .request("who")
        .trim_start_matches("OK ")
        .parse()
        .expect("a serial");
    // A lane is busy until its request is answered AND its connection is
    // placed, and the claim's three requests may each have woken it onto a
    // lane: the next dial waits for them to be done, or it can find every
    // request lane still charged and be handed back busy.
    rig.settle("the claimed one parked", |c, _| c.parked == 1 && c.rpc == 0);
    let mut idle = rig.idle().expect("admitted");
    rig.settle("both parked", |c, _| c.parked == 2 && c.rpc == 0);
    rig.lanes.inject_parker_panics(usize::MAX);
    claimed.send("ping\n");
    assert_eq!(
        line_within(&mut claimed, SETTLE).as_deref(),
        Some("<EOF>"),
        "a stopped parker hangs its connections up rather than strand them"
    );
    assert_eq!(line_within(&mut idle, SETTLE).as_deref(), Some("<EOF>"));
    rig.settle("stopped: nothing parked, nothing open", |c, _| {
        !c.parking && c.parked == 0 && c.open == 0
    });
    assert_eq!(
        rig.stub.released(),
        vec![claimed_id],
        "its claim ended with it"
    );
    // The lanes run as without a parker: served, and never handed to the
    // parker that is gone.
    let mut fresh = rig.idle().expect("a fresh client after the stop");
    assert_eq!(fresh.request("again"), "OK again");
    assert_eq!(rig.lanes.census().parked, 0, "nothing parks after the stop");
}

/// A CLIENT THAT HALF-CLOSED AFTER ITS REQUEST (`printf … | nc -U`) reads its
/// answer and then END-OF-FILE when its lane closes the connection, though a
/// copy of the server end survives — the copy any thread's fork holds until it
/// execs (a `try_clone` here: one more descriptor on the same socket, which is
/// all the kernel counts). The claim's release is the fact that the lane has
/// closed it ([`super::Conn::close`] hangs up, then the tenure drops), so one
/// non-blocking read after it is the verdict.
///
/// NEGATIVE CONTROL (run by restoring the both-ways `shutdown`): Darwin refused
/// it for a socket whose read half the client's half-close had shut
/// (`ENOTCONN`) and shut nothing, so the client read `WouldBlock` after its
/// answer, not its end-of-file, for as long as the copy lived.
#[test]
fn a_half_closed_client_reads_eof_when_its_lane_closes_though_a_copy_survives() {
    use std::io::Read as _;

    let rig = Rig::new(model_limits());
    let (client, server) = CtlStream::pair().expect("real control socket pair");
    let forked_copy = server
        .try_clone()
        .expect("a surviving copy of the server end");
    assert!(rig.lanes.admit(server).is_ok(), "admitted");
    (&client)
        .write_all(b"AUTH ok\nclaim\n")
        .expect("the request");
    client
        .shutdown(std::net::Shutdown::Write)
        .expect("the client half-closes after its request");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the lane to close the half-closed connection",
        || (!rig.stub.released().is_empty()).then_some(()),
    );

    client.set_nonblocking(true).expect("non-blocking");
    let mut answer = Vec::new();
    let end = (&client).read_to_end(&mut answer).map_err(|e| e.kind());
    assert_eq!(String::from_utf8_lossy(&answer), "OK claim\n");
    assert!(
        end.is_ok(),
        "the client read no end-of-file from a lane that closed while a copy of its end \
         lived: {end:?}"
    );
    drop(forked_copy);
}

/// A REQUEST IS SERVED ON A BLOCKING SOCKET, whatever the request before it left
/// the descriptor in. The lanes switch it only when the mode they need differs
/// from the one they last set ([`super::SocketMode`]), and drop that record
/// whenever a handler has had the descriptor, since a handler may switch it
/// itself: the handshake's and the ACK's detached-peer drain makes it
/// non-blocking (`AUTH drain` and `drain` here). With a parker and without one;
/// two request lanes, so without a parker each connection keeps one.
///
/// NEGATIVE CONTROLS: the lane trusted the record and served the next request
/// on the non-blocking socket the drain had left, so `blocking` answered
/// `nonblocking`. Run by keeping the record across `authenticate`: the request
/// that came with the handshake, with a parker and without one (it is whole in
/// the buffer, so nothing is gathered and nothing switches before it is
/// served). Across `serve_line`: the request after `drain`, without a parker.
#[test]
fn a_request_is_served_blocking_whatever_the_one_before_left_its_socket_in() {
    for open in [8, 0] {
        let rig = Rig::new(LaneLimits {
            rpc: 2,
            wait: 1,
            inline: 0,
            open,
            subscriptions: 1,
        });
        let mut drained = rig.dial().expect("admitted");
        drained.send("AUTH drain\nblocking\n");
        assert_eq!(
            drained.line(),
            "OK blocking",
            "open={open}: served on the non-blocking socket the handshake left"
        );
        let mut client = rig.idle().expect("admitted");
        assert_eq!(client.request("blocking"), "OK blocking", "open={open}");
        assert_eq!(client.request("drain"), "OK drain", "open={open}");
        assert_eq!(
            client.request("blocking"),
            "OK blocking",
            "open={open}: served on the non-blocking socket the drain left"
        );
    }
}

/// A REQUEST ON A KEPT CONNECTION SWITCHES ITS DESCRIPTOR'S MODE AT MOST TWICE:
/// to blocking for its handler, and back for the lane's delivery, the gather
/// behind it and the park ([`super::SocketMode`]). It cost six before: the
/// pick-up, a switch each way around the delivery and around the gather, and
/// the park. One request lane, so every switch made for this connection is made
/// on the thread whose count `switches` reads.
///
/// NEGATIVE CONTROL (run by switching on every `set`): four a request.
#[test]
fn a_request_on_a_kept_connection_switches_its_socket_mode_at_most_twice() {
    const REQUESTS: u64 = 16;
    let rig = Rig::new(LaneLimits {
        rpc: 1,
        wait: 1,
        inline: 0,
        open: 8,
        subscriptions: 1,
    });
    let mut client = rig.idle().expect("admitted");
    let count = |client: &mut Client| -> u64 {
        let answer = client.request("switches");
        answer
            .strip_prefix("OK ")
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("a count, not {answer:?}"))
    };
    let before = count(&mut client);
    for n in 0..REQUESTS {
        assert_eq!(client.request(&format!("ping{n}")), format!("OK ping{n}"));
    }
    let switches = count(&mut client) - before;
    // Each `ping`'s two, the first `switches`' second and the last one's first.
    assert!(
        switches <= 2 * (REQUESTS + 1),
        "{switches} switches for {REQUESTS} requests"
    );
}

/// A CONNECTION'S UNREAD BYTES ARE ONE BUFFER for its life ([`super::Unread`]):
/// a request whose start came with the end of the one before keeps its place
/// as the served bytes are dropped, a small request's storage is read into
/// again rather than rebuilt, and storage a large request grew is given back
/// once it is served, so a lane between requests keeps one read's room at
/// most (a parked idle connection keeps none: `Conn::hold_only_what_is_owed`).
///
/// NEGATIVE CONTROL (run by freeing only storage past 8 × `READ_BUFFER`, the
/// Outbox's rule): the 32 KiB the body grew stayed with the emptied buffer.
#[test]
fn a_connections_unread_bytes_keep_their_order_and_their_storage() {
    use super::{Gathered, READ_BUFFER, Unread};

    let (client, server) = CtlStream::pair().expect("real control socket pair");
    server
        .set_nonblocking(true)
        .expect("non-blocking, as parked");
    let mut unread = Unread::default();
    let a_line = |bytes: &[u8]| bytes.contains(&b'\n');

    (&client).write_all(b"first\nsec").expect("send");
    assert_eq!(unread.gather(&server, a_line), Gathered::Enough);
    unread.consume(b"first\n".len());
    let storage = unread.bytes.as_ptr();
    (&client).write_all(b"ond\n").expect("send");
    assert_eq!(unread.gather(&server, a_line), Gathered::Enough);
    assert_eq!(unread.as_slice(), b"second\n", "in order across the reads");
    unread.consume(b"second\n".len());
    assert_eq!(unread.gather(&server, a_line), Gathered::Short);
    assert_eq!(
        unread.bytes.as_ptr(),
        storage,
        "the storage is read into again"
    );

    // A body larger than one read's room, gathered as the parker gathers one.
    let size = 4 * READ_BUFFER;
    let writer = std::thread::spawn(move || {
        (&client).write_all(&vec![b'x'; size]).expect("send");
        client
    });
    aterm_spec::wait::until_within(SETTLE, "the body to be gathered", || {
        (unread.gather(&server, |bytes| bytes.len() >= size) == Gathered::Enough).then_some(())
    });
    let _client = writer.join().expect("the writer");
    assert!(unread.bytes.len() > READ_BUFFER, "it grew for the body");
    unread.consume(size);
    assert_eq!(
        unread.bytes.capacity(),
        0,
        "a request's storage past one read's room is given back once it is served"
    );
}

/// An answer the exit waits for (`close`'s: its action can end the process,
/// `control::FinalReplies`) stays owed after its lane is done with it, while the
/// parker holds the part its peer has not taken yet, and is released once the peer
/// has it all. Counted once from before the dispatch, so a request served is owed
/// at least until its lane gives the connection back.
#[test]
fn an_answer_the_exit_waits_for_is_owed_until_the_parker_delivers_it() {
    static REPLIES: super::super::FinalReplies = super::super::FinalReplies::new();
    let rig = Rig::new(model_limits());
    rig.lanes.count_final_replies_on(&REPLIES);
    let mut client = rig.idle().expect("admitted");

    client.send("close big\n");
    // A hang detector, never a latency budget.
    let deadline = Instant::now() + SETTLE;
    while REPLIES.owed() == 0 {
        assert!(Instant::now() < deadline, "the lane took the request");
        std::thread::sleep(Duration::from_millis(1));
    }
    // The lane gave the connection back, its answer's tail with the parker.
    rig.settle("the answer's tail parked", |c, f| {
        c.parked == 1 && c.rpc == 0 && f == (0, 0)
    });
    assert_eq!(REPLIES.owed(), 1, "undelivered: still owed");
    assert!(
        !REPLIES.settle(Duration::from_millis(20)),
        "the exit's wait ends at its bound"
    );

    let answer = client.line();
    assert_eq!(answer.len(), "OK ".len() + BIG_ANSWER, "the whole answer");
    assert!(
        REPLIES.settle(SETTLE),
        "delivered: the exit no longer waits for it"
    );

    // An answer delivered at once is released with it; any other verb is never
    // owed.
    assert_eq!(client.request("close"), "OK close");
    assert_eq!(client.request("ping"), "OK ping");
    assert!(REPLIES.settle(SETTLE));
    assert_eq!(REPLIES.owed(), 0);
}
