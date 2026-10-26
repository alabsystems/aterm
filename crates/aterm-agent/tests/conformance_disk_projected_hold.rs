// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests for `DiskProjectedHold` against the ask's lease (`disk::ask`, the
//! design's §8.3 and §8.7): two hosts step one real lease file under its lock (`ask::transact`)
//! from their own looks. P's plain floor is the lower, and its fill ring projects a crossing of it
//! (`ask::Ahead::projected`) or, its projection lapsed, none; Q's floor is the higher, and Q
//! projects nothing. After each real step the lease is projected onto the model (whether an
//! episode is open, the floor the lease holds it to, whether P's projection record stands), the
//! model action the step is fires, and the projected lease must be the model's state after it:
//! so the real step opens, holds and ends exactly as the model does, and the model's laws hold
//! after every step. Every order of up to five moves from a closed volume is walked, and every
//! move from every state the model reaches.
//!
//! Each host's one session is held at a usage-limit wall, so a round past its time waits for it
//! (`ask::Said::Waiting`) and no episode parks: a parked episode giving way to a new one a
//! cooldown later is `DiskFreeAsk`'s (`conformance_disk_free_ask.rs`).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;

use aterm_agent::harness::disk::Level;
use aterm_agent::harness::disk::ask::{self, Ahead, Done, Entry, HeldBy, HostId, Lease, Said};
use aterm_agent::harness::disk::fill::Projection;
use aterm_models_harness::disk_projected_hold_model;
use aterm_spec::derive::Model;

type S = BTreeMap<&'static str, i64>;

const GIB: u64 = 1 << 30;
const KEY: &str = "dev:conformance-hold";
const T0: i64 = 1_792_000_000_000;

/// The host whose floor is the lower and whose ring projects.
const P: usize = 0;
/// The host whose floor is the higher, which projects nothing.
const Q: usize = 1;

/// Each host's plain floor: P's, then Q's.
const FLOORS: [u64; 2] = [10 * GIB, 50 * GIB];

/// The margin past the floor an episode is held to that the model's places are cut at: the
/// design's 5 GiB, held here apart from the code's own constant
/// ([`the_models_places_are_the_leases`]).
const MARGIN: u64 = 5 * GIB;

/// What the volume reads at each of the model's `free` places: below P's floor; past it by the
/// margin and below Q's; past Q's by the margin.
const PLACES: [u64; 3] = [5 * GIB, 30 * GIB, 80 * GIB];

/// The volume's size.
const TOTAL: u64 = 500 * GIB;

/// P's fill rate: 40 MiB/s, about 141 GiB an hour (the design's night).
const RATE_BPS: u64 = 40 << 20;

/// The model's `free` place a move goes to, and the moves the walks take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Move {
    /// Free space falls one place.
    Fall,
    /// Free space rises one place.
    Rise,
    /// P's look, its ring projecting a crossing of its floor.
    Project,
    /// P's warm look whose ring projects nothing: its projection lapsed.
    Plain,
    /// Q's look.
    LookQ,
    /// A minute passes.
    Tick,
    /// [`ask::COLD_HOLD_MS`] passes: a record no look refreshed meanwhile stands no more.
    Lapse,
}

const MOVES: [Move; 7] = [
    Move::Fall,
    Move::Rise,
    Move::Project,
    Move::Plain,
    Move::LookQ,
    Move::Tick,
    Move::Lapse,
];

#[test]
fn the_models_places_are_the_leases() {
    assert_eq!(ask::END_MARGIN, MARGIN);
    let [p, q] = FLOORS;
    assert!(PLACES[0] < p, "place 0 is below P's floor");
    assert!(
        PLACES[1] >= p + MARGIN && PLACES[1] < q,
        "place 1 is past P's floor by the margin and below Q's"
    );
    assert!(
        PLACES[2] >= q + MARGIN,
        "place 2 is past Q's floor by the margin"
    );
    let projected = Projection::of(RATE_BPS, PLACES[2], p, Some(TOTAL));
    assert!(
        p + projected.need > q,
        "P's projected floor lies past Q's, as the first version's single slot took it: {projected:?}"
    );
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "aterm-conformance-diskhold-{name}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Where a walk stands, to come back to: the clock, the volume, the model and the lease file.
#[derive(Clone)]
struct Node {
    now: i64,
    free: usize,
    s: S,
    trail: Vec<String>,
    lease: Option<String>,
}

/// The two hosts' world: the lease's directory, the clock, the volume's place, and the model.
struct World {
    dir: Scratch,
    now: i64,
    /// The model's `free` place the volume reads at ([`PLACES`]).
    free: usize,
    hosts: [HostId; 2],
    /// After each step the whole projected lease must be the model's state (`true`), or only
    /// whether an episode is open, as the laws read it.
    strict: bool,
    m: Model,
    s: S,
    /// The real steps taken, for a failure's dump.
    trail: Vec<String>,
}

impl World {
    fn new(name: &str) -> World {
        let m = disk_projected_hold_model();
        let s = m.init_state();
        World {
            dir: Scratch::new(name),
            now: T0,
            free: 2,
            hosts: [
                HostId {
                    pid: 101,
                    start: "p".to_owned(),
                },
                HostId {
                    pid: 202,
                    start: "q".to_owned(),
                },
            ],
            strict: true,
            m,
            s,
            trail: Vec::new(),
        }
    }

    fn fail(&self, why: String) -> String {
        format!("{why}\nreal steps:\n  {}", self.trail.join("\n  "))
    }

    fn lease(&self) -> Lease {
        ask::read(&self.dir.0)
    }

    fn entry(&self) -> Option<Entry> {
        self.lease().entry(KEY).cloned()
    }

    /// Fire the model's `action`: it must be enabled, and every law must hold after it.
    fn fire(&mut self, action: &str) -> Result<(), String> {
        self.trail.push(format!("model {action}"));
        if !self.m.action_enabled(action, &self.s) {
            return Err(self.fail(format!("`{action}` is not enabled in {:?}", self.s)));
        }
        assert!(self.m.fire(action, &mut self.s));
        for inv in &self.m.invariants {
            if !self.m.check_invariant(inv.name, &self.s) {
                return Err(self.fail(format!(
                    "`{}` broken after `{action}`: {:?}",
                    inv.name, self.s
                )));
            }
        }
        Ok(())
    }

    /// The lease as the model reads it now: `(ep, bar, rec)`, `bar` `-1` where the lease holds
    /// the episode to a floor that is no host's plain floor.
    fn projected(&self) -> Result<(i64, i64, i64), String> {
        let lease = self.lease();
        let Some(e) = lease.entry(KEY) else {
            return Ok((0, 0, 0));
        };
        let ep = i64::from(e.episode_ms.is_some());
        let bar = match &e.floor {
            None => 0,
            Some(f) if f.host == self.hosts[P] && f.bytes == FLOORS[P] => 1,
            Some(f) if f.host == self.hosts[Q] && f.bytes == FLOORS[Q] => 2,
            Some(_) => -1,
        };
        if let Some(r) = e.projected.iter().find(|r| r.host != self.hosts[P]) {
            return Err(self.fail(format!(
                "the lease records a projection of a host that projects nothing: {r:?}"
            )));
        }
        let rec = i64::from(
            e.projected
                .iter()
                .any(|r| self.now.saturating_sub(r.at_ms) < ask::COLD_HOLD_MS),
        );
        Ok((ep, bar, rec))
    }

    /// The projected lease is the model's state.
    fn check(&self) -> Result<(), String> {
        let (ep, bar, rec) = self.projected()?;
        if self.strict && bar < 0 {
            return Err(self.fail(format!(
                "the lease holds the episode to a floor that is no host's plain floor: {:?}",
                self.entry().and_then(|e| e.floor)
            )));
        }
        let model = (self.s["ep"], self.s["bar"], self.s["rec"]);
        let same = if self.strict {
            (ep, bar, rec) == model
        } else {
            ep == model.0
        };
        if same {
            Ok(())
        } else {
            Err(self.fail(format!(
                "the lease reads (ep, bar, rec) = {:?}, the model {model:?}",
                (ep, bar, rec)
            )))
        }
    }

    fn snapshot(&self) -> Node {
        Node {
            now: self.now,
            free: self.free,
            s: self.s.clone(),
            trail: self.trail.clone(),
            lease: std::fs::read_to_string(self.dir.0.join(ask::LEASE_FILE)).ok(),
        }
    }

    fn restore(&mut self, n: &Node) {
        self.now = n.now;
        self.free = n.free;
        self.s.clone_from(&n.s);
        self.trail.clone_from(&n.trail);
        let file = self.dir.0.join(ask::LEASE_FILE);
        match &n.lease {
            Some(text) => std::fs::write(&file, text).expect("the lease is restored"),
            None => {
                let _ = std::fs::remove_file(&file);
            }
        }
    }

    /// Whether `mv` changes anything from where the world stands: free space moves only within
    /// the model's places.
    fn moves(&self, mv: Move) -> bool {
        match mv {
            Move::Fall => self.free > 0,
            Move::Rise => self.free < 2,
            _ => true,
        }
    }

    fn apply(&mut self, mv: Move) -> Result<(), String> {
        match mv {
            Move::Fall if self.free > 0 => {
                self.free -= 1;
                self.trail
                    .push(format!("free {} GiB", PLACES[self.free] / GIB));
                self.fire("Fall")?;
                self.check()
            }
            Move::Rise if self.free < 2 => {
                self.free += 1;
                self.trail
                    .push(format!("free {} GiB", PLACES[self.free] / GIB));
                self.fire("Rise")?;
                self.check()
            }
            Move::Fall | Move::Rise => Ok(()),
            Move::Project => self.look(P, true),
            Move::Plain => self.look(P, false),
            Move::LookQ => self.look(Q, false),
            Move::Tick => self.wait(60_000),
            Move::Lapse => self.wait(ask::COLD_HOLD_MS),
        }
    }

    fn run(&mut self, moves: &[Move]) -> Result<(), String> {
        for &mv in moves {
            self.apply(mv)?;
        }
        Ok(())
    }

    /// `ms` pass: a projection record no look refreshed for [`ask::COLD_HOLD_MS`] stands no more,
    /// the model's `Lapse`.
    fn wait(&mut self, ms: i64) -> Result<(), String> {
        self.now += ms;
        self.trail.push(format!("wait {} s", ms / 1000));
        if self.s["rec"] == 1 && self.projected()?.2 == 0 {
            self.fire("Lapse")?;
        }
        self.check()
    }

    /// Host `h`'s look, its ring projecting a crossing of its floor or not: the real step under
    /// the lease's lock, its action fired on the model, and the lease projected against the
    /// model's state after it. A look that is not pressed and finds no episode open takes no
    /// lock (`ask::rests`), as the window's does.
    fn look(&mut self, h: usize, projecting: bool) -> Result<(), String> {
        assert!(h == P || !projecting, "Q projects nothing");
        let free = PLACES[self.free];
        let short = free < FLOORS[h];
        let pressed = short || projecting;
        let name = ["P", "Q"][h];
        if !pressed && ask::rests(&self.lease(), KEY, None) {
            self.trail.push(format!("{name} look: rests"));
            return self.check();
        }
        let host = self.hosts[h].clone();
        let sid = ["s-p", "s-q"][h].to_owned();
        let live = vec![sid.clone()];
        let held = vec![(sid, HeldBy::Limit)];
        let alive = |_: &HostId| true;
        let look = ask::Look {
            now_ms: self.now,
            host: &host,
            key: KEY,
            level: if short { Level::Short } else { Level::Low },
            free,
            floor: FLOORS[h],
            files_free: None,
            files_floor: aterm_agent::harness::disk::EMERGENCY_FILES_FREE,
            done: Some(Done::Spent),
            seats: &[],
            supervised: &live,
            barred: &[],
            live: &live,
            held: &held,
            reclaimed: &[],
            passing: false,
            alive: &alive,
            ahead: Ahead {
                projected: projecting
                    .then(|| Projection::of(RATE_BPS, free, FLOORS[h], Some(TOTAL))),
                ..Ahead::default()
            },
        };
        let open = self.entry().is_some_and(|e| e.episode_ms.is_some());
        let said = ask::transact(&self.dir.0, &ask::LOCK_LADDER, |l| ask::step(l, &look))
            .expect("the lease is written")
            .expect("one host at a time");
        self.trail.push(format!(
            "{name} look{} at {} GiB: {said:?}",
            if projecting { " projecting" } else { "" },
            free / GIB
        ));
        let action = match &said {
            Said::Opened { projected, .. } => {
                if open {
                    return Err(self.fail(
                        "a parked episode gave way, though this walk parks none".to_owned(),
                    ));
                }
                if projected.is_some() != projecting {
                    return Err(self.fail(format!(
                        "the opening says {projected:?}, though the look projected {projecting}"
                    )));
                }
                Some(match (h, projecting) {
                    (P, true) => "OpenProjected",
                    (P, false) => "OpenShortP",
                    _ => "OpenShortQ",
                })
            }
            Said::Ended { .. } if pressed => {
                return Err(self.fail("a pressed look ended the episode".to_owned()));
            }
            Said::Ended { .. } => Some(if h == P { "EndP" } else { "EndQ" }),
            _ if pressed && !open => {
                return Err(self.fail(
                    "a pressed look, aterm's freeing done and outside the cooldown, opened nothing"
                        .to_owned(),
                ));
            }
            _ if !open => None,
            _ if projecting => Some("Project"),
            _ if h == P => Some("HoldP"),
            _ => Some("HoldQ"),
        };
        if let Some(action) = action {
            self.fire(action)?;
        }
        self.check()
    }
}

/// Every order of up to `depth` moves from where `w` stands, each from the lease file as the
/// order before it left it. Returns the steps taken and the model actions reached.
fn every_order(w: &mut World, depth: usize) -> Result<(usize, BTreeSet<String>), String> {
    fn down(
        w: &mut World,
        node: &Node,
        depth: usize,
        steps: &mut usize,
        reached: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        if depth == 0 {
            return Ok(());
        }
        for mv in MOVES {
            w.restore(node);
            if !w.moves(mv) {
                continue;
            }
            w.apply(mv)?;
            *steps += 1;
            reached.extend(
                w.trail[node.trail.len()..]
                    .iter()
                    .filter_map(|t| t.strip_prefix("model "))
                    .map(str::to_owned),
            );
            let child = w.snapshot();
            down(w, &child, depth - 1, steps, reached)?;
        }
        Ok(())
    }
    let root = w.snapshot();
    let (mut steps, mut reached) = (0, BTreeSet::new());
    down(w, &root, depth, &mut steps, &mut reached)?;
    Ok((steps, reached))
}

/// Every move from every state the model reaches, each state reached first by the shortest order
/// of moves that reaches it and kept with the real phase its lease is in. Returns the model states
/// reached.
fn every_state(w: &mut World) -> Result<BTreeSet<Vec<i64>>, String> {
    let key = |w: &World| {
        let phase = w.entry().map_or("none", |e| e.phase.word());
        (w.s.values().copied().collect::<Vec<i64>>(), w.free, phase)
    };
    let mut seen = BTreeSet::from([key(w)]);
    let mut queue = VecDeque::from([w.snapshot()]);
    while let Some(node) = queue.pop_front() {
        for mv in MOVES {
            w.restore(&node);
            if !w.moves(mv) {
                continue;
            }
            w.apply(mv)?;
            if seen.insert(key(w)) {
                queue.push_back(w.snapshot());
            }
        }
    }
    Ok(seen.into_iter().map(|(s, ..)| s).collect())
}

/// The design's case and its neighbours (§8.3): Q reads the volume short and opens the episode,
/// and P projects a crossing of its lower floor. Past Q's floor Q's look ends nothing while P's
/// record stands; back below it, P's lapsed look holds the episode to Q's floor, where the first
/// version's single slot ended it on P's own. P projects again and its record lapses by time,
/// and past Q's floor Q's look ends the episode. P's projection opens the next above both floors,
/// Q's look holds it to Q's floor, and P's lapsed look ends it; then P, below its own floor and
/// projecting nothing, opens one held to its own floor and ends it past that.
fn story(w: &mut World) -> Result<(), String> {
    use Move::{Fall, Lapse, LookQ, Plain, Project, Rise, Tick};
    w.run(&[Fall, LookQ, Project])?;
    w.run(&[Rise, LookQ])?;
    w.run(&[Fall, Plain, LookQ])?;
    w.run(&[Tick, Tick, Tick, LookQ])?;
    w.run(&[Project, Lapse, Rise, LookQ])?;
    w.run(&[Project, LookQ, Plain])?;
    w.run(&[Fall, Fall, Plain, Rise, Plain])?;
    Ok(())
}

#[test]
fn two_hosts_ends_and_holds_of_a_projected_episode_are_the_models() {
    let mut w = World::new("story");
    story(&mut w).unwrap_or_else(|e| panic!("{e}"));
    let fired: Vec<&str> = w
        .trail
        .iter()
        .filter_map(|t| t.strip_prefix("model "))
        .filter(|a| !matches!(*a, "Fall" | "Rise"))
        .collect();
    assert_eq!(
        fired,
        [
            "OpenShortQ",
            "Project",
            "HoldQ",
            "HoldP",
            "HoldQ",
            "HoldQ",
            "Project",
            "Lapse",
            "EndQ",
            "OpenProjected",
            "HoldQ",
            "EndP",
            "OpenShortP",
            "EndP",
        ],
        "{:#?}",
        w.trail
    );
    assert!(
        w.trail
            .iter()
            .any(|t| t.starts_with("Q look at 30 GiB: Waiting")),
        "the round past its time waits on the held session, so nothing parks: {:#?}",
        w.trail
    );
}

#[test]
fn every_order_of_five_moves_takes_only_the_models_steps() {
    let mut w = World::new("orders");
    let (steps, reached) = every_order(&mut w, 5).unwrap_or_else(|e| panic!("{e}"));
    assert!(steps > 5_000, "{steps} steps");
    for action in [
        "Fall",
        "Rise",
        "Lapse",
        "OpenProjected",
        "OpenShortP",
        "OpenShortQ",
        "Project",
        "HoldP",
        "EndP",
        "HoldQ",
        "EndQ",
    ] {
        assert!(
            reached.contains(action),
            "no order reached {action}: {reached:?}"
        );
    }
}

#[test]
fn every_move_from_every_state_the_model_reaches_takes_only_the_models_steps() {
    let mut w = World::new("states");
    let reached = every_state(&mut w).unwrap_or_else(|e| panic!("{e}"));
    let model = aterm_spec::interp::reachable_states(&disk_projected_hold_model());
    let names: Vec<&str> = disk_projected_hold_model()
        .vars
        .iter()
        .map(|v| v.name)
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    for state in &model {
        let values: Vec<i64> = sorted.iter().map(|n| state[n]).collect();
        assert!(
            reached.contains(&values),
            "the lease never reached the model's {state:?}: {reached:?}"
        );
    }
}

/// NEGATIVE CONTROL: a look that ends an episode while a projection record stands
/// (`Fault::EndWhileProjected`) lets Q's look past its floor end the episode P still reads a
/// crossing in, and the checker rejects that end, in the design's case and among every order.
#[test]
fn a_look_that_ends_an_episode_while_a_projection_stands_is_rejected() {
    let mut w = World::new("end-while-projected");
    let caught = ask::fault::with(ask::fault::Fault::EndWhileProjected, || story(&mut w))
        .expect_err("the checker must reject an end while a projection record stands");
    assert!(caught.contains("`EndQ` is not enabled"), "{caught}");
    let mut w = World::new("end-while-projected-orders");
    let caught = ask::fault::with(ask::fault::Fault::EndWhileProjected, || {
        every_order(&mut w, 3)
    })
    .expect_err("some order must catch it");
    assert!(caught.contains("`EndQ` is not enabled"), "{caught}");
}

/// NEGATIVE CONTROL: a lease that records the projected floor in the episode's floor and keeps no
/// projection record (`Fault::ProjectionInFloor`, the first version) is rejected at P's first
/// projecting look, whose floor is no host's plain floor; and, read only for whether an episode is
/// open, at P's lapsed look, which ends the episode on P's own plain floor while Q reads the
/// volume short.
#[test]
fn a_lease_that_records_the_projection_in_the_floor_is_rejected() {
    let mut w = World::new("projection-in-floor");
    let caught = ask::fault::with(ask::fault::Fault::ProjectionInFloor, || story(&mut w))
        .expect_err("the checker must reject the projected floor");
    assert!(
        caught.contains("no host's plain floor"),
        "rejected at the floor recorded: {caught}"
    );
    let mut w = World::new("projection-in-floor-ends");
    w.strict = false;
    let caught = ask::fault::with(ask::fault::Fault::ProjectionInFloor, || story(&mut w))
        .expect_err("the checker must reject the end it leads to");
    assert!(caught.contains("`EndP` is not enabled"), "{caught}");
    assert!(
        caught.contains("P look at 30 GiB: Ended"),
        "P's lapsed look ended it below Q's floor: {caught}"
    );
    let mut w = World::new("projection-in-floor-orders");
    ask::fault::with(ask::fault::Fault::ProjectionInFloor, || {
        every_order(&mut w, 3)
    })
    .expect_err("some order must catch it");
}
