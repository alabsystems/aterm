// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance and scheduling tests for `control_lane_tenure_model` driving [`Lanes`].

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

/// Test `LaneService` stub supporting gated `work`/`wait`.
#[derive(Default)]
struct Stub {
    gate: Mutex<Gate>,
    moved: Condvar,
    released: Mutex<Vec<u64>>,
    claims: Mutex<BTreeSet<u64>>,
    panic_release: Mutex<Vec<u64>>,
}

const BIG_ANSWER: usize = 4 << 20;

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

    fn let_finish(&self, wait: bool) {
        let mut gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        if wait {
            gate.wait_open += 1;
        } else {
            gate.rpc_open += 1;
        }
        self.moved.notify_all();
    }

    fn end_push(&self) {
        let mut gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        gate.push_open += 1;
        self.moved.notify_all();
    }

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

fn blocks(stream: &CtlStream) -> bool {
    use std::os::fd::AsRawFd;

    // SAFETY: `F_GETFL` reads the status flags of a descriptor `stream` holds open for the call.
    let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0, "F_GETFL: {}", std::io::Error::last_os_error());
    flags & libc::O_NONBLOCK == 0
}

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

    fn idle(&self) -> Option<Client> {
        let mut client = self.dial()?;
        client.send("AUTH ok\nping\n");
        assert_eq!(client.line(), "OK ping");
        Some(client)
    }

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

/// What the Tier-1 test knows that the lanes do not count.
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

/// Project `census`, `flight`, and `facts` onto a model [`State`] at a quiescent point.
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

/// Assert `model` reaches `after` from `before` along `path`.
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

    assert_eq!(a.request("ping"), "OK ping");
    let s3a = observe(&rig, facts, "A parked", &|c, _| c.parked == 3 && c.rpc == 0);
    assert_path(&model, &s3, &["Wake", "Pick", "Finish"], &s3a);
    assert_eq!(b.request("ping"), "OK ping");
    let s3b = observe(&rig, facts, "B parked", &|c, _| c.parked == 3 && c.rpc == 0);
    assert_path(&model, &s3a, &["Wake", "Pick", "Finish"], &s3b);

    assert!(model.successors("Admit", &s3b).is_empty());
    assert!(
        model.successors("Refuse", &s3b).is_empty(),
        "the model refuses no client a parked connection can make room for"
    );
    let mut third = rig.dial().expect("X admitted at the cap");
    facts.arrivals += 1;
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

    a.send("work\n");
    let s4 = observe(&rig, facts, "A working", &|c, f| {
        f == (1, 0) && c.parked == 2
    });
    assert_path(&model, &s3, &["Wake", "Pick"], &s4);
    b.send("wait\n");
    let s5 = observe(&rig, facts, "B waiting", &|c, f| {
        f == (1, 1) && c.parked == 1 && c.rpc == 1
    });
    assert_path(&model, &s4, &["Wake", "Pick", "Defer"], &s5);

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

    let mut d = rig.dial().expect("D admitted");
    facts.arrivals += 1;
    d.send("AUTH ok\nwait\nlane\n");
    let s10 = observe(&rig, facts, "D waiting", &|c, f| f == (0, 1) && c.rpc == 0);
    assert_path(&model, &s9, &["Admit", "Defer"], &s10);
    b.send("wait\n");
    let s11 = observe(&rig, facts, "B waiting inline", &|c, f| {
        f == (1, 1) && c.rpc == 1 && c.inline == 1 && c.parked == 1
    });
    assert_path(&model, &s10, &["Wake", "Pick", "InlineWait"], &s11);
    third.send("wait\n");
    assert_eq!(third.line(), BUSY, "a wait past every lane it may take");
    facts.busied = 1;
    let s12 = observe(&rig, facts, "C parked after its busy wait", &|c, f| {
        f == (1, 1) && c.rpc == 1 && c.inline == 1 && c.parked == 1
    });
    assert_path(&model, &s11, &["Wake", "Pick", "BusyWait"], &s12);
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
    rig.stub.let_finish(false);
    assert_eq!(b.line(), "OK wait");
    let s14 = observe(&rig, facts, "B parked", &|c, f| {
        f == (0, 0) && c.parked == 2 && c.inline == 0
    });
    assert_path(&model, &s13, &["InlineFinish"], &s14);
    assert_eq!(third.request("ping"), "OK ping");
    let s15 = observe(&rig, facts, "C parked again", &|c, f| {
        f == (0, 0) && c.parked == 2
    });
    assert_path(&model, &s14, &["Wake", "Pick", "Finish"], &s15);

    drop(b);
    let s16 = observe(&rig, facts, "B closed", &|c, _| {
        c.open == 1 && c.parked == 1
    });
    assert_path(&model, &s15, &["ParkedClose"], &s16);
    drop(third);
}

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

#[test]
fn idle_drivers_do_not_starve_a_fresh_client() {
    let rig = Rig::new(LaneLimits {
        rpc: 2,
        wait: 1,
        inline: 1,
        open: 64,
        subscriptions: 1,
    });
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
        drop(fresh);
        rig.settle("the one-shot client gone", |c, _| {
            c.open == 12 && c.rpc == 0
        });
    }
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
        let before = rig.lanes.census().held;
        half.send("pin");
        rig.settle("the half-sent bytes held", |c, _| c.held > before);
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
        let mut last = rig.dial().expect("one more fresh client admitted");
        last.send("AUTH ok\nping\n");
        assert_eq!(last.line(), "OK ping");
        assert!(
            rest_until_closed(&mut fresh[0]).is_empty(),
            "the first fresh client, now the stalest"
        );
        parked(CAP, "the last fresh client parked");
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
    rig.settle("the claim holders parked again", |c, _| {
        c.parked == 3 && c.rpc == 0
    });
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
    rig.lanes.set_parked_bytes_max(2 * answer);
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
        let whole = readers
            .iter_mut()
            .map(|reader| whole_or_cut(reader, answer))
            .filter(|&whole| whole)
            .count();
        assert_eq!(whole, kept, "the readers kept read their whole answers");
    }
}

/// Whether `reader` reads a full `answer`-byte line (`true`) or is cut short and closed.
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

const TAKEN_BEFORE_THE_LATER_ONE: usize = 256 << 10;

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
            if got >= TAKEN_BEFORE_THE_LATER_ONE
                && let Some(started) = started.take()
            {
                let _ = started.send(());
            }
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
    assert_eq!(driver.request("wait three"), "OK wait three");
    rig.settle("parked again", |c, f| {
        c.parked == 1 && c.rpc == 0 && c.waits == 0 && f == (0, 0)
    });
    assert_eq!(driver.request("lane"), "OK rpc");
}

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
            rig.settle("placed", |c, f| {
                f == (index, 1) && c.inline == index && c.rpc == index
            });
        }
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
            parked.send("lane\n");
            assert_eq!(
                line_within(&mut parked, SETTLE).as_deref(),
                Some("OK rpc"),
                "the parked driver's request, beside every wait"
            );
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

/// Wait-lane spawn gate for [`Lanes::before_wait_spawn`].
#[derive(Default)]
struct HeldSpawn {
    state: Mutex<HeldState>,
    moved: Condvar,
}

#[derive(Default)]
struct HeldState {
    entered: usize,
    /// `Some(true)` to allow the spawn, `Some(false)` to fail it.
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
            rig.stub.let_finish(true);
            rig.stub.let_finish(false);
            assert_eq!(c.line(), "OK wait c");
            let _lane = c.line();
            assert_eq!(b.line(), "OK wait b");
            assert_eq!(b.line(), "OK wait", "B's lane started");
        }
    }
}

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
    rig.settle("parked again, the parker running", |c, _| {
        c.parking && c.parked == 1 && c.rpc == 0
    });
    let mut later = rig.idle().expect("a connection parked after the panics");
    rig.settle("parked after the panics", |c, _| {
        c.parked == 2 && c.rpc == 0
    });
    assert_eq!(later.request("ping"), "OK ping");
}

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
    rig.settle("both parked again, the parker running", |c, _| {
        c.parking && c.parked == 2 && c.rpc == 0
    });
    let mut later = rig.idle().expect("a connection parked after the restart");
    rig.settle("three parked", |c, _| c.parked == 3 && c.rpc == 0);
    assert_eq!(later.request("ping"), "OK ping");
    assert_eq!(first.request("again"), "OK again");
}

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
    let mut fresh = rig.idle().expect("a fresh client after the stop");
    assert_eq!(fresh.request("again"), "OK again");
    assert_eq!(rig.lanes.census().parked, 0, "nothing parks after the stop");
}

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
    assert!(
        switches <= 2 * (REQUESTS + 1),
        "{switches} switches for {REQUESTS} requests"
    );
}

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

#[test]
fn an_answer_the_exit_waits_for_is_owed_until_the_parker_delivers_it() {
    static REPLIES: super::super::FinalReplies = super::super::FinalReplies::new();
    let rig = Rig::new(model_limits());
    rig.lanes.count_final_replies_on(&REPLIES);
    let mut client = rig.idle().expect("admitted");

    client.send("close big\n");
    let deadline = Instant::now() + SETTLE;
    while REPLIES.owed() == 0 {
        assert!(Instant::now() < deadline, "the lane took the request");
        std::thread::sleep(Duration::from_millis(1));
    }
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

    assert_eq!(client.request("close"), "OK close");
    assert_eq!(client.request("ping"), "OK ping");
    assert!(REPLIES.settle(SETTLE));
    assert_eq!(REPLIES.owed(), 0);
}
