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
use super::{Census, LaneLimits, LaneReader, LaneService, Lanes};

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
}

/// The stub service: `AUTH ok` authenticates; `work…` blocks on a request
/// lane and `wait…` (a wait request) blocks wherever it is served, each until
/// the test releases one of its kind; `claim` binds a claim to the connection;
/// `who` answers the serving connection's serial; `quit` closes; anything else
/// is answered at once. Releasing the claims of a connection named in
/// `panic_release` panics instead (a defective release, for the parker tests).
#[derive(Default)]
struct Stub {
    gate: Mutex<Gate>,
    moved: Condvar,
    released: Mutex<Vec<u64>>,
    panic_release: Mutex<Vec<u64>>,
}

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
        _reader: &mut LaneReader<'_>,
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
        let defective = self
            .panic_release
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&id);
        if defective {
            panic!("injected: releasing connection {id}'s claims panics");
        }
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
/// admitted so far, whether a refusal came while a lane held an idle one,
/// whether a wait was answered busy, and whether a wait was kept on a request
/// lane beside a free wait lane.
#[derive(Clone, Copy, Default)]
struct Facts {
    arrivals: i64,
    refused_idle: i64,
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
    state.insert("queued", 0);
    state.insert("open", count(census.open));
    state.insert("arrivals", facts.arrivals);
    state.insert("refused_idle", facts.refused_idle);
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
    let mut third = rig.idle().expect("C admitted");
    facts.arrivals += 1;
    let s3 = observe(&rig, facts, "C parked", &|c, _| c.parked == 3 && c.rpc == 0);
    assert_path(&model, &s2, &["Admit", "Finish"], &s3);

    // Every request lane is free, but the open-connection bound is reached: a
    // fourth client is refused, and not for an idle driver.
    assert!(rig.dial().is_none(), "the open-connection bound refuses");
    assert!(model.successors("Admit", &s3).is_empty());
    assert_path(&model, &s3, &["Refuse"], &s3);

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

    // A binds a claim to its connection and quits: the close ends the tenure,
    // and the claim with it.
    assert_eq!(a.request("claim"), "OK claim");
    let who = a.request("who");
    let a_id: u64 = who.trim_start_matches("OK ").parse().expect("a serial");
    let s8 = observe(&rig, facts, "A parked after claim", &|c, _| {
        c.parked == 3 && c.rpc == 0
    });
    assert_path(&model, &s7, &["Wake", "Pick", "Finish"], &s8);
    a.send("quit\n");
    assert_eq!(a.line(), "<EOF>");
    let s9 = observe(&rig, facts, "A closed", &|c, _| {
        c.open == 2 && c.parked == 2
    });
    assert_path(&model, &s8, &["Wake", "Pick", "Close"], &s9);
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
    let census = rig.settle("D waiting", |c, f| f == (0, 1) && c.rpc == 0);
    let s1 = project(census, rig.stub.in_flight(), facts);
    // `wait` with `quit` pipelined behind it: the one wait lane is D's, so it
    // is served where it is, within the budget.
    let mut e = rig.dial().expect("E admitted");
    facts.arrivals += 1;
    e.send("AUTH ok\nwait\nquit\n");
    let census = rig.settle("E waiting inline", |c, f| {
        f == (1, 1) && c.rpc == 1 && c.inline == 1
    });
    let s2 = project(census, rig.stub.in_flight(), facts);
    assert_path(&model, &s1, &["Admit", "InlineWait"], &s2);
    rig.stub.let_finish(false);
    assert_eq!(e.line(), "OK wait");
    assert_eq!(e.line(), "<EOF>");
    let census = rig.settle("E closed", |c, f| {
        f == (0, 1) && c.rpc == 0 && c.inline == 0 && c.open == 1
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
        inline: 1,
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
        inline: 2,
        open: 0,
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

/// How long the negative controls watch a request that must NOT be answered
/// (starved behind the waits) before calling it starved. Only an ABSENCE is
/// judged by it — a request that must be answered is read within [`SETTLE`],
/// since nothing here frees a lane before the test lets a wait finish.
const STARVED: Duration = Duration::from_millis(500);

/// Read one line within `within`, or say that none came. (macOS refuses a
/// read timeout on a socket its peer has already closed; a read there cannot
/// block anyway, so the refusal is ignored.)
fn line_within(client: &mut Client, within: Duration) -> Option<String> {
    let _ = client.stream.set_read_timeout(Some(within));
    let mut reply = String::new();
    let read = client.reader.read_line(&mut reply);
    let _ = client.stream.set_read_timeout(Some(SETTLE));
    match read {
        Ok(0) => Some("<EOF>".to_string()),
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
                f == (1, 1) && c.rpc == 1 && c.parked == 2
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
                f == (2, 1) && c.rpc == 2 && c.inline == 2
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
            assert_eq!(
                line_within(&mut parked, STARVED),
                None,
                "negative control: the parked driver's request is starved behind the waits"
            );
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
/// 2026-09-27) — `RelayClient::dial_via_local`'s shape, `dial <name>` then
/// `version` written back to back, one `aterm drive --dial` relay each — moves
/// to a wait lane like any other, carrying the requests behind it: more such
/// waits than the request lanes and their budget are all held, none is answered
/// busy, a fresh client is served beside them, and each connection's pipelined
/// requests are answered after its wait, in order, on the wait lane it moved
/// to. The negative control is the rule before the carry (a wait with anything
/// buffered behind it stays on its request lane): the first takes the budget's
/// one place and every later one is answered busy with seven wait lanes free —
/// the fifth relay turned away — which the model rejects (`WaitLaneTakenFirst`).
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
    });
    let mut claimed = rig.idle().expect("admitted");
    assert_eq!(claimed.request("claim"), "OK claim");
    let claimed_id: u64 = claimed
        .request("who")
        .trim_start_matches("OK ")
        .parse()
        .expect("a serial");
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
    // The lanes run as without a parker: served, and kept on its lane when
    // idle (never handed to the parker that is gone). A lane places a
    // connection in microseconds; for a quarter of a second it must stay there.
    let mut fresh = rig.idle().expect("a fresh client after the stop");
    let kept_until = Instant::now() + Duration::from_millis(250);
    while Instant::now() < kept_until {
        let census = rig.lanes.census();
        assert!(
            census.rpc == 1 && census.parked == 0,
            "an idle connection after the stop must keep its lane: {census:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(fresh.request("again"), "OK again");
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
