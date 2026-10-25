// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests for `DiskFreeAsk` against the ask's lease (`disk::ask`): two hosts
//! step one real lease file under its lock (`ask::transact`) from their own looks, type at their
//! sessions' turn breaks only on a claim the lease grants, and every step is projected onto the
//! model, whose laws must hold after it.

use std::path::PathBuf;

use aterm_agent::harness::disk::Level;
use aterm_agent::harness::disk::ask::{self, Claim, Done, HostId, Lease, Phase, Said, Seat};
use aterm_models_harness::disk_free_ask_model;
use aterm_spec::derive::Model;

type S = std::collections::BTreeMap<&'static str, i64>;

const GIB: u64 = 1 << 30;
const KEY: &str = "dev:conformance";
const T0: i64 = 1_792_000_000_000;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "aterm-conformance-diskask-{name}-{}-{n}",
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

fn step(m: &Model, s: &mut S, action: &str) -> Result<(), String> {
    if !m.action_enabled(action, s) {
        return Err(format!("`{action}` is not enabled in {s:?}"));
    }
    assert!(m.fire(action, s));
    for inv in &m.invariants {
        if !m.check_invariant(inv.name, s) {
            return Err(format!("`{}` broken after `{action}`: {s:?}", inv.name));
        }
    }
    Ok(())
}

/// The model's name of session `sid`.
fn letter(sid: &str) -> &'static str {
    match sid {
        "s-a" => "A",
        "s-b" => "B",
        "s-c" => "C",
        other => panic!("no such session {other}"),
    }
}

/// The world two hosts share: the lease's directory, the clock, the volume, and the model.
struct World {
    dir: Scratch,
    now: i64,
    below: bool,
    free: u64,
    done: bool,
    /// When the last line went (the cooldown's clock).
    last_line: Option<i64>,
    /// The free figure when the last line went: a look that reads [`ask::FREED_MIN`] more is the
    /// model's `Frees`, whatever the lease then says.
    free_at: u64,
    hosts: [HostId; 2],
    /// Each host's sessions: A and B on the first, C on the second.
    seats: [Vec<Seat>; 2],
    /// The line each host's last look left owed one of its sessions (its board).
    owed: [Option<String>; 2],
    m: Model,
    s: S,
    /// The real steps taken, for a failure's dump.
    trail: Vec<String>,
}

impl World {
    fn new(name: &str) -> World {
        let seat = |sid: &str, held: u64| Seat {
            sid: sid.to_owned(),
            held: held * GIB,
            at_break: true,
        };
        let m = disk_free_ask_model();
        let s = m.init_state();
        World {
            dir: Scratch::new(name),
            now: T0,
            below: false,
            free: 30 * GIB,
            done: false,
            last_line: None,
            free_at: 0,
            hosts: [
                HostId {
                    pid: 101,
                    start: "first".to_owned(),
                },
                HostId {
                    pid: 202,
                    start: "second".to_owned(),
                },
            ],
            seats: [vec![seat("s-a", 30), seat("s-b", 5)], vec![seat("s-c", 12)]],
            owed: [None, None],
            m,
            s,
            trail: Vec::new(),
        }
    }

    fn fail(&self, why: String) -> String {
        format!("{why}\nreal steps:\n  {}", self.trail.join("\n  "))
    }

    fn fire(&mut self, action: &str) -> Result<(), String> {
        self.trail.push(format!("model {action}"));
        step(&self.m, &mut self.s, action).map_err(|e| self.fail(e))
    }

    fn lease(&self) -> Lease {
        ask::read(&self.dir.0)
    }

    /// The volume falls below its floor, or rises back above it.
    fn level(&mut self, below: bool) -> Result<(), String> {
        if below != self.below {
            self.below = below;
            self.free = if below { 20 * GIB } else { 40 * GIB };
            self.fire(if below { "Fall" } else { "Rise" })?;
        }
        Ok(())
    }

    /// aterm's own passes run out, or may free again.
    fn spend(&mut self, done: bool) -> Result<(), String> {
        if done != self.done {
            self.done = done;
            self.fire(if done { "Spend" } else { "Unspend" })?;
        }
        Ok(())
    }

    fn wait(&mut self, ms: i64) -> Result<(), String> {
        self.now += ms;
        if self.s["cool"] == 1
            && self
                .last_line
                .is_some_and(|at| self.now.saturating_sub(at) >= ask::COOLDOWN_MS)
        {
            self.fire("Cools")?;
        }
        Ok(())
    }

    /// Host `h`'s disk look: the real step under the lease's lock, projected. A look that reads
    /// the asked session's rise is the model's `Frees` first, so the lease's step is checked
    /// against a model that knows space was freed.
    fn look(&mut self, h: usize) -> Result<(), String> {
        if self.below
            && self.s["phase"] == 3
            && self.s["rose"] == 0
            && self.free >= self.free_at + ask::FREED_MIN
        {
            self.fire("Frees")?;
        }
        let before = self.lease().entry(KEY).map(|e| e.phase.clone());
        let host = self.hosts[h].clone();
        let seats = self.seats[h].clone();
        let live: Vec<String> = seats.iter().map(|s| s.sid.clone()).collect();
        let alive = |_: &HostId| true;
        let look = ask::Look {
            now_ms: self.now,
            host: &host,
            key: KEY,
            level: if self.below { Level::Short } else { Level::Ok },
            free: self.free,
            done: self.done.then_some(Done::Spent),
            seats: &seats,
            supervised: &live,
            live: &live,
            alive: &alive,
        };
        let (said, after) = ask::transact(&self.dir.0, &ask::LOCK_LADDER, |l| {
            (ask::step(l, &look), l.entry(KEY).map(|e| e.phase.clone()))
        })
        .expect("the lease is written")
        .expect("one host at a time");
        self.trail.push(format!("host {h} look: {said:?}"));
        self.owed[h] = match &after {
            Some(Phase::Owed { pick, .. }) if pick.host == host => Some(pick.sid.clone()),
            _ => None,
        };
        match said {
            Said::Opened { .. } => self.fire("Open"),
            Said::Elected { pick, .. } => self.fire(&format!("Elect{}", letter(&pick.sid))),
            Said::Out { .. } => match before {
                Some(Phase::Nominating { .. }) => self.fire("ElectNone"),
                Some(Phase::Owed { .. }) => {
                    self.fire("HandOffOwed")?;
                    self.fire("ElectNone")
                }
                Some(Phase::Asked { .. }) => self.fire("OutAsked"),
                other => Err(self.fail(format!("ran out from {other:?}"))),
            },
            Said::Handoff { .. } => match before {
                Some(Phase::Owed { .. }) => self.fire("HandOffOwed"),
                Some(Phase::Asked { .. }) => self.fire("HandOffAsked"),
                other => Err(self.fail(format!("handed off from {other:?}"))),
            },
            Said::Freed { .. } => self.fire("Freed"),
            Said::Ended { .. } => self.fire("End"),
            Said::Cooldown { .. } if self.m.action_enabled("Open", &self.s) => {
                Err(self.fail("the lease waits out a cooldown the model is past".to_owned()))
            }
            Said::Freeing if self.s["done"] == 1 => {
                Err(self.fail("the lease waits on a pass the model says is spent".to_owned()))
            }
            Said::Quiet
            | Said::Freeing
            | Said::Cooldown { .. }
            | Said::Nominating { .. }
            | Said::Owed { .. }
            | Said::Asked { .. }
            | Said::Settled
            | Said::Spent => Ok(()),
        }
    }

    /// Session `sid`'s turn break on host `h`: the line its host's board holds is typed only on a
    /// claim the lease grants, as the window's act types it.
    fn turn_break(&mut self, h: usize) -> Result<(), String> {
        let Some(sid) = self.owed[h].take() else {
            return Ok(());
        };
        let host = self.hosts[h].clone();
        let now = self.now;
        let free = self.free;
        let typed = ask::transact(&self.dir.0, &ask::LOCK_LADDER, |l| {
            (ask::claim(l, KEY, &host, &sid, now) == Claim::Yours)
                .then(|| ask::typed(l, KEY, &host, &sid, now, free))
        })
        .expect("the lease is written")
        .expect("one host at a time");
        self.trail
            .push(format!("host {h} turn break @{sid}: typed={typed:?}"));
        if typed.is_some() {
            self.last_line = Some(now);
            self.free_at = free;
            self.fire(&format!("Type{}", letter(&sid)))?;
        }
        Ok(())
    }

    /// Session `sid`'s host finds it may not be asked.
    fn release(&mut self, h: usize) -> Result<(), String> {
        let Some(sid) = self.owed[h].take() else {
            return Ok(());
        };
        let host = self.hosts[h].clone();
        let now = self.now;
        let said = ask::transact(&self.dir.0, &ask::LOCK_LADDER, |l| {
            ask::release(l, KEY, &host, &sid, now)
        })
        .expect("the lease is written")
        .expect("one host at a time");
        self.trail
            .push(format!("host {h} release @{sid}: {said:?}"));
        match said {
            Some(Said::Handoff { .. }) => self.fire("HandOffOwed"),
            Some(Said::Out { .. }) => {
                self.fire("HandOffOwed")?;
                self.fire("ElectNone")
            }
            Some(other) => Err(self.fail(format!("a release said {other:?}"))),
            None => Ok(()),
        }
    }

    /// Free space rises by `gib` GiB (the asked agent freed it, or not enough).
    fn frees(&mut self, gib: u64) {
        self.free += gib * GIB;
        self.trail.push(format!("free +{gib} GiB"));
    }
}

/// The scripted episode: the first host elects A, A never comes to a turn break and the second
/// host hands it off while the first's board still holds A's line; then C takes its line, frees
/// nothing, B takes the last, and the owner is told. Back above the floor the episode ends, the
/// cooldown keeps a new one shut, and once it is over A frees what it was asked to.
fn scripted(w: &mut World) -> Result<(), String> {
    w.level(true)?;
    w.look(0)?;
    w.spend(true)?;
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(0)?;
    w.wait(ask::DELIVER_MS)?;
    w.look(1)?;
    w.turn_break(0)?;
    w.look(1)?;
    w.look(0)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(1)?;
    w.look(0)?;
    w.turn_break(1)?;
    w.turn_break(0)?;
    w.frees(0);
    w.wait(ask::ANSWER_MS)?;
    w.look(0)?;
    w.look(1)?;
    w.look(0)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(0)?;
    w.turn_break(0)?;
    w.wait(ask::ANSWER_MS)?;
    w.look(1)?;
    w.look(0)?;
    w.level(false)?;
    w.look(1)?;
    w.level(true)?;
    w.look(0)?;
    w.wait(ask::COOLDOWN_MS)?;
    w.look(1)?;
    w.look(0)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(1)?;
    w.turn_break(0)?;
    w.look(0)?;
    w.turn_break(0)?;
    w.frees(3);
    w.look(1)?;
    w.look(0)?;
    Ok(())
}

#[test]
fn two_hosts_stepping_the_real_lease_take_only_the_models_steps() {
    let mut w = World::new("scripted");
    scripted(&mut w).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(w.s["phase"], 4, "the episode settled: {:?}", w.s);
    assert!(
        w.trail
            .iter()
            .any(|t| t.contains("turn break @s-a: typed=None")),
        "the stale line A's host held was not typed: {:#?}",
        w.trail
    );
    for action in [
        "Open",
        "ElectA",
        "ElectB",
        "ElectC",
        "TypeA",
        "TypeB",
        "TypeC",
        "HandOffOwed",
        "HandOffAsked",
        "OutAsked",
        "End",
        "Cools",
        "Frees",
        "Freed",
    ] {
        assert!(
            w.trail.iter().any(|t| t == &format!("model {action}")),
            "the script reaches {action}: {:#?}",
            w.trail
        );
    }
}

/// A fixed-seed walk over every host's looks, turn breaks and releases and the world's moves.
fn walk(w: &mut World, seed: u64, steps: usize) -> Result<(), String> {
    let mut x = seed;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for _ in 0..steps {
        let h = usize::from(next() % 2 == 0);
        match next() % 20 {
            0 => w.level(true)?,
            1 => w.level(false)?,
            2 => w.spend(true)?,
            3 => w.spend(false)?,
            4 | 5 => w.wait(60_000)?,
            6 => w.wait(ask::NOMINATE_MS)?,
            7 => w.wait(ask::DELIVER_MS.max(ask::ANSWER_MS))?,
            8 => w.wait(ask::COOLDOWN_MS)?,
            9 => w.frees([0, 0, 0, 2][usize::try_from(next() % 4).unwrap_or(0)]),
            10..=14 => w.look(h)?,
            15..=18 => w.turn_break(h)?,
            _ => w.release(h)?,
        }
    }
    Ok(())
}

#[test]
fn fixed_seed_walks_of_the_real_lease_take_only_the_models_steps() {
    let mut reached = std::collections::BTreeSet::new();
    for seed in 1..=24u64 {
        let mut w = World::new("walk");
        walk(&mut w, seed.wrapping_mul(0x9e37_79b9_7f4a_7c15), 400)
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        reached.extend(
            w.trail
                .iter()
                .filter_map(|t| t.strip_prefix("model "))
                .map(str::to_owned),
        );
    }
    for action in [
        "Open",
        "ElectNone",
        "TypeC",
        "HandOffOwed",
        "HandOffAsked",
        "OutAsked",
        "Frees",
        "Freed",
        "End",
    ] {
        assert!(
            reached.contains(action),
            "no walk reached {action}: {reached:?}"
        );
    }
}

/// NEGATIVE CONTROL: a claim that answers `Yours` whatever the lease says (`Fault::ClaimBlind`)
/// lets the first host type the line its stale board still holds after the second host handed
/// it on, and the checker rejects that line.
#[test]
fn a_host_that_types_without_the_leases_claim_is_rejected() {
    let mut w = World::new("blind");
    let caught = ask::fault::with(ask::fault::Fault::ClaimBlind, || scripted(&mut w))
        .expect_err("the checker must reject a line the lease no longer names");
    assert!(caught.contains("`TypeA` is not enabled"), "{caught}");
}
