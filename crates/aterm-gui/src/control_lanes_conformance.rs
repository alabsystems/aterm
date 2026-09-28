// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for the control socket's lane tenure
//! (`aterm_spec::derive::control_lane_tenure_model`), and the lanes' scheduling
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

use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use aterm_spec::derive::{Model, control_lane_tenure_model};
use aterm_spec::interp::{State, admits};
use aterm_uds::CtlStream;

use super::super::{Scope, ServeDisposition};
use super::{Census, LaneLimits, LaneService, Lanes};

/// How long any settle may take before the test calls the lanes wedged.
const SETTLE: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Gate {
    /// Requests served on request lanes / wait lanes, blocked in the stub.
    rpc: usize,
    wait: usize,
    /// Releases handed out, per lane kind.
    rpc_open: usize,
    wait_open: usize,
}

/// The stub service: `AUTH ok` authenticates; `work…` blocks on a request
/// lane and `wait…` (a wait request) blocks wherever it is served, each until
/// the test releases one of its kind; `claim` binds a claim to the connection;
/// `who` answers the serving connection's serial; `quit` closes; anything else
/// is answered at once.
#[derive(Default)]
struct Stub {
    gate: Mutex<Gate>,
    moved: Condvar,
    released: Mutex<Vec<u64>>,
}

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
        reader: &mut BufReader<&CtlStream>,
    ) -> Option<(Scope, Option<String>)> {
        let first = super::super::read_request_line(reader)?;
        if first == "AUTH ok" {
            return Some((Scope::Owner, None));
        }
        let mut writer = stream;
        let _ = writer.write_all(b"ERR auth\n");
        None
    }

    fn serve_line(
        &self,
        line: String,
        _scope: Scope,
        stream: &CtlStream,
        _reader: &mut BufReader<&CtlStream>,
    ) -> Option<ServeDisposition> {
        let reply = match line.as_str() {
            "quit" => return Some(ServeDisposition::Close),
            "claim" => {
                super::super::note_connection_claim();
                "OK claim".to_string()
            }
            "who" => format!("OK {}", super::super::serving_connection().unwrap_or(0)),
            "lane" => format!("OK {}", if on_wait_lane() { "wait" } else { "rpc" }),
            other if other.starts_with("work") || other.starts_with("wait") => {
                self.block();
                format!("OK {other}")
            }
            other => format!("OK {other}"),
        };
        let mut writer = stream;
        writer
            .write_all(format!("{reply}\n").as_bytes())
            .is_err()
            .then_some(ServeDisposition::Close)
    }

    fn is_wait(&self, line: &str, _scope: Scope) -> bool {
        line.starts_with("wait")
    }

    fn subscribe(&self, _line: String, _scope: Scope, stream: CtlStream) {
        let _ = (&stream).write_all(b"ERR no push here\n");
    }

    fn release(&self, id: u64) {
        self.released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(id);
    }
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
        assert_eq!(lanes.start(), limits.rpc, "every request lane started");
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
/// admitted so far, and whether a refusal came while a lane held an idle one.
#[derive(Clone, Copy, Default)]
struct Facts {
    arrivals: i64,
    refused_idle: i64,
}

fn count(n: usize) -> i64 {
    i64::try_from(n).expect("small counts fit i64")
}

/// Project the genuine lanes onto the model at a QUIESCENT point (no resume in
/// the queue, no request between lanes). `held` is request-lane work no
/// request in flight explains: an idle connection kept on its lane.
fn project(census: Census, flight: (usize, usize), facts: Facts) -> State {
    assert_eq!(
        census.waits, flight.1,
        "a wait lane holds a connection with no wait in flight: {census:?}"
    );
    let model = control_lane_tenure_model();
    let mut state = model.init_state();
    state.insert("working", count(flight.0));
    state.insert("held", count(census.rpc) - count(flight.0));
    state.insert("waiting", count(flight.1));
    state.insert("parked", count(census.parked));
    state.insert("queued", 0);
    state.insert("open", count(census.open));
    state.insert("arrivals", facts.arrivals);
    state.insert("refused_idle", facts.refused_idle);
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
        open: 2,
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
    let s0 = observe(&rig, facts, "empty", &|c, _| c == Census::default());
    assert_eq!(s0, model.init_state());

    // Two drivers, served once and left idle: each parks.
    let mut a = rig.idle().expect("A admitted");
    facts.arrivals += 1;
    let s1 = observe(&rig, facts, "A parked", &|c, _| c.parked == 1 && c.rpc == 0);
    assert_path(&model, &s0, &["Admit", "Finish"], &s1);
    let mut b = rig.idle().expect("B admitted");
    facts.arrivals += 1;
    let s2 = observe(&rig, facts, "B parked", &|c, _| c.parked == 2 && c.rpc == 0);
    assert_path(&model, &s1, &["Admit", "Finish"], &s2);

    // Every request lane is free, but the open-connection bound is reached: a
    // third client is refused, and not for an idle driver.
    assert!(rig.dial().is_none(), "the open-connection bound refuses");
    assert!(model.successors("Admit", &s2).is_empty());
    assert_path(&model, &s2, &["Refuse"], &s2);

    // A asks for work: woken, taken back by a request lane.
    a.send("work\n");
    let s3 = observe(&rig, facts, "A working", &|c, f| {
        f == (1, 0) && c.parked == 1
    });
    assert_path(&model, &s2, &["Wake", "Pick"], &s3);
    // B asks for a wait: woken, taken back, moved to the wait lane.
    b.send("wait\n");
    let s4 = observe(&rig, facts, "B waiting", &|c, f| {
        f == (1, 1) && c.parked == 0 && c.rpc == 1
    });
    assert_path(&model, &s3, &["Wake", "Pick", "Defer"], &s4);

    // Each finishes and parks: the request lane's and the wait lane's finish.
    rig.stub.let_finish(false);
    assert_eq!(a.line(), "OK work");
    let s5 = observe(&rig, facts, "A parked again", &|c, f| {
        f == (0, 1) && c.parked == 1 && c.rpc == 0
    });
    assert_path(&model, &s4, &["Finish"], &s5);
    rig.stub.let_finish(true);
    assert_eq!(b.line(), "OK wait");
    let s6 = observe(&rig, facts, "B parked again", &|c, f| {
        f == (0, 0) && c.parked == 2
    });
    assert_path(&model, &s5, &["WaitFinish"], &s6);

    // A binds a claim to its connection and quits: the close ends the tenure,
    // and the claim with it.
    assert_eq!(a.request("claim"), "OK claim");
    let who = a.request("who");
    let a_id: u64 = who.trim_start_matches("OK ").parse().expect("a serial");
    let s7 = observe(&rig, facts, "A parked after claim", &|c, _| {
        c.parked == 2 && c.rpc == 0
    });
    assert_path(&model, &s6, &["Wake", "Pick", "Finish"], &s7);
    a.send("quit\n");
    assert_eq!(a.line(), "<EOF>");
    let s8 = observe(&rig, facts, "A closed", &|c, _| {
        c.open == 1 && c.parked == 1
    });
    assert_path(&model, &s7, &["Wake", "Pick", "Close"], &s8);
    assert_eq!(rig.stub.released(), vec![a_id], "A's claim ended with it");

    // B's peer hangs up while parked: the parker closes it.
    drop(b);
    let s9 = observe(&rig, facts, "B closed", &|c, _| {
        c.open == 0 && c.parked == 0
    });
    assert_path(&model, &s8, &["ParkedClose"], &s9);

    // Two more: D's wait takes the one wait lane; E's wait finds it taken and
    // is served on its request lane.
    let mut d = rig.dial().expect("D admitted");
    facts.arrivals += 1;
    d.send("AUTH ok\nwait\n");
    let s10 = observe(&rig, facts, "D waiting", &|c, f| f == (0, 1) && c.rpc == 0);
    assert_path(&model, &s9, &["Admit", "Defer"], &s10);
    let mut e = rig.dial().expect("E admitted");
    facts.arrivals += 1;
    e.send("AUTH ok\nwait\n");
    let s11 = observe(&rig, facts, "E waiting inline", &|c, f| {
        f == (1, 1) && c.rpc == 1
    });
    assert_path(&model, &s10, &["Admit"], &s11);
    // D's client says `quit` while its wait runs; the wait lane serves it next.
    d.send("quit\n");
    rig.stub.let_finish(true);
    assert_eq!(d.line(), "OK wait");
    assert_eq!(d.line(), "<EOF>");
    let s12 = observe(&rig, facts, "D closed", &|c, f| f == (1, 0) && c.open == 1);
    assert_path(&model, &s11, &["WaitClose"], &s12);
    rig.stub.let_finish(false);
    assert_eq!(e.line(), "OK wait");
    let s13 = observe(&rig, facts, "E parked", &|c, f| {
        f == (0, 0) && c.parked == 1
    });
    assert_path(&model, &s12, &["Finish"], &s13);
    drop(e);
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
        open: 0,
    });
    let s0 = project(rig.lanes.census(), rig.stub.in_flight(), Facts::default());
    let mut facts = Facts::default();
    let _a = rig.idle().expect("A admitted");
    let _b = rig.idle().expect("B admitted");
    facts.arrivals = 2;
    let census = rig.settle("both idle on their lanes", |c, f| c.rpc == 2 && f == (0, 0));
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
        open: 64,
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
        open: 0,
    });
    let _held: Vec<Client> = (0..2).map(|_| old.idle().expect("two fit")).collect();
    old.settle("two held", |c, f| c.rpc == 2 && f == (0, 0));
    assert!(
        old.dial().is_none(),
        "negative control: without parking two idle drivers refuse the third"
    );
}

/// Drivers parked in waits move off the request lanes; with no wait lanes (the
/// old design) the same waits take the request lanes and refuse a fresh
/// client.
#[test]
fn waiting_drivers_do_not_starve_a_fresh_client() {
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 8,
        open: 64,
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
        open: 64,
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

/// The busy reply is kept for a socket truly saturated with work, and it is
/// prompt: two requests working on two lanes refuse a third client at once,
/// and the lane a finished request frees admits again.
#[test]
fn a_pool_saturated_with_work_still_refuses_promptly() {
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 1,
        open: 64,
    });
    let mut workers = Vec::new();
    for index in 0..2 {
        let mut driver = rig.dial().expect("fits a lane");
        driver.send("AUTH ok\nwork\n");
        workers.push(driver);
        rig.settle("working", |c, f| f == (index + 1, 0) && c.rpc == index + 1);
    }
    let started = Instant::now();
    assert!(rig.dial().is_none(), "saturated with work: refused");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "and at once"
    );
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

/// A connection keeps its serial — the binding a connection-scoped claim is
/// made to — across every lane that carries it, and its claim is released
/// when it ends, however it ends: here, its peer hanging up while parked.
#[test]
fn claims_follow_the_connection_across_lanes_and_end_with_it() {
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 2,
        open: 8,
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
        open: 8,
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
