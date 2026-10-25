// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests for `DiskFreeAsk` against the ask's lease (`disk::ask`): two hosts
//! whose floors on the volume differ step one real lease file under its lock (`ask::transact`)
//! from their own looks, each judging the volume short against its own floor, type at their
//! sessions' turn breaks only on a claim the lease grants (`ask::at_break`), and every step is
//! projected onto the model, whose laws must hold after it. A host's process can end and a
//! successor adopt its sessions under their sids (a seamless update), while the other host's
//! looks leave the ended host's pick to it for `ask::ADOPT_MS`; and a line's record can be lost to
//! a write that fails and be recorded at its host's next look, after its pick was handed off and
//! another session elected, or after another host ended its episode: a look that reads the volume
//! clear leaves the lease alone only when no episode is open and its host has no line the lease
//! has yet to record (`ask::rests`). A successor adopts an owed pick of the host it replaced
//! whether it still supervises the pick or bars it, so a pick whose agent exited across the
//! update is released at no election's cost.

use std::path::PathBuf;

use aterm_agent::harness::disk::Level;
use aterm_agent::harness::disk::ask::{
    self, Break, Done, Entry, HostId, Lease, Miss, Phase, Pick, Said, Seat,
};
use aterm_models_harness::disk_free_ask_model;
use aterm_spec::derive::Model;

type S = std::collections::BTreeMap<&'static str, i64>;

const GIB: u64 = 1 << 30;
const KEY: &str = "dev:conformance";
const T0: i64 = 1_792_000_000_000;

/// Each host's floor: the first's is the model's H, the second's its L.
const FLOORS: [u64; 2] = [48 * GIB, 32 * GIB];

/// The margin past the floor an episode is held to that the model's places are cut at: the
/// design's 5 GiB, held here apart from the code's own constant, so a lease that ends inside it
/// is a step the model refuses, not a place the projection moved
/// ([`the_models_margin_is_the_leases`]).
const MARGIN: u64 = 5 * GIB;

/// What the volume reads at each of the model's `free` places: below both floors, within the
/// margin above L's, past it and below H's, within the margin above H's, past it.
const PLACES: [u64; 5] = [20 * GIB, 34 * GIB, 40 * GIB, 50 * GIB, 60 * GIB];

/// The model's place of `free` bytes free.
fn place(free: u64) -> i64 {
    let [high, low] = FLOORS;
    if free < low {
        0
    } else if free < low + MARGIN {
        1
    } else if free < high {
        2
    } else if free < high + MARGIN {
        3
    } else {
        4
    }
}

#[test]
fn the_models_margin_is_the_leases() {
    assert_eq!(ask::END_MARGIN, MARGIN);
    let [high, low] = FLOORS;
    assert!(
        high > low + MARGIN,
        "H's floor lies past L's by more than the margin, as the model has it"
    );
}

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
    free: u64,
    done: bool,
    /// A's task is done: the seat rule drops its seat and bars it on its host.
    done_a: bool,
    /// A's agent exited with its task: it leaves its host's supervised roster as well.
    exited_a: bool,
    /// When the episode parked (settled or out), as the lease's `at_ms` says it.
    parked_at: Option<i64>,
    /// When the last line went (the cooldown's clock).
    last_line: Option<i64>,
    /// The free figure when the last line went: a look that reads [`ask::FREED_MIN`] more is the
    /// model's `Frees`, whatever the lease then says.
    free_at: u64,
    hosts: [HostId; 2],
    /// The hosts whose processes ended ([`World::succeed`]), each with when.
    dead: Vec<(HostId, i64)>,
    /// A line a host typed whose record the lease could not write ([`World::turn_break_lost`]):
    /// its next look records it (`ask::typed_unrecorded`), as the window's look does.
    unrecorded: [Option<Lost>; 2],
    /// Each host's sessions: A and B on the first, C on the second.
    seats: [Vec<Seat>; 2],
    /// The line each host's last look left owed one of its sessions (its board).
    owed: [Option<String>; 2],
    m: Model,
    s: S,
    /// The real steps taken, for a failure's dump.
    trail: Vec<String>,
}

/// A line whose record was lost: its session, when it went, and the free figure then.
#[derive(Clone)]
struct Lost {
    sid: String,
    at: i64,
    free: u64,
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
            free: PLACES[4],
            done: false,
            done_a: false,
            exited_a: false,
            parked_at: None,
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
            dead: Vec::new(),
            unrecorded: [None, None],
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
        step(&self.m, &mut self.s, action).map_err(|e| self.fail(e))?;
        self.parked_at = (self.s["phase"] > 3).then(|| self.parked_at.unwrap_or(self.now));
        Ok(())
    }

    fn lease(&self) -> Lease {
        ask::read(&self.dir.0)
    }

    /// The model's moves from where it holds free space to where the volume reads now.
    fn follow(&mut self) -> Result<(), String> {
        let to = place(self.free);
        while self.s["free"] > to {
            self.fire("Drop")?;
        }
        while self.s["free"] < to {
            self.fire("Climb")?;
        }
        Ok(())
    }

    /// The volume reads at the model's place `at`.
    fn to(&mut self, at: usize) -> Result<(), String> {
        self.free = PLACES[at];
        self.trail.push(format!("free {} GiB", self.free / GIB));
        self.follow()
    }

    /// A's task turns done (it landed or exited), or it takes a new one.
    fn finish(&mut self, done: bool) -> Result<(), String> {
        if done != self.done_a {
            self.done_a = done;
            self.exited_a = false;
            self.fire(if done { "Finish" } else { "Unfinish" })?;
        }
        Ok(())
    }

    /// A's agent exits with no relaunch: its task record says exited, and it leaves its host's
    /// supervised roster while its tab lives on.
    fn exit_a(&mut self) -> Result<(), String> {
        self.finish(true)?;
        self.exited_a = true;
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
        if self.s["aged"] == 0
            && self
                .parked_at
                .is_some_and(|at| self.now.saturating_sub(at) >= ask::COOLDOWN_MS)
        {
            self.fire("Ages")?;
        }
        Ok(())
    }

    /// Host `h`'s process ends, and a successor adopts its sessions under their sids with a new
    /// [`HostId`] (a seamless update): the successor's board holds no line until its own look.
    /// A script that never looks with the successor again plays a host that ended with none.
    fn succeed(&mut self, h: usize) {
        let gone = std::mem::replace(
            &mut self.hosts[h],
            HostId {
                pid: 303 + u32::try_from(self.dead.len()).unwrap_or(0),
                start: format!("successor-{}", self.dead.len()),
            },
        );
        self.trail
            .push(format!("host {h} succeeded: pid {} ended", gone.pid));
        self.dead.push((gone, self.now));
        self.owed[h] = None;
    }

    /// Whether host `host` ended less than [`ask::ADOPT_MS`] before now: its successor may not
    /// have looked yet.
    fn ended_lately(&self, host: &HostId) -> bool {
        self.dead
            .iter()
            .any(|(h, at)| h == host && self.now.saturating_sub(*at) < ask::ADOPT_MS)
    }

    /// Host `h`'s disk look: the real step under the lease's lock, projected. Each host reads the
    /// volume short against its own floor; A's host holds A unseated and barred once its task is
    /// done, and off its supervised roster once its agent exited. A look that reads the asked
    /// session's rise is the model's `Frees` first, so the lease's step is checked against a
    /// model that knows space was freed. A look that reads the volume clear of its host's floor
    /// steps nothing where the window's look takes no lock (`ask::rests`: no episode open, no
    /// line of its host's to record). A host holding a line whose record was lost records it
    /// first, in the same step ([`World::record`]). A look that leaves standing what the model
    /// would move (a cooldown it is past, a pass it says is spent, a parked episode it gives way)
    /// is refused, as is a hand-off that spends an election on a pick its own host bars, on one
    /// whose host ended while this host supervises its session or bars it, or, before the pick's
    /// deadline, on one whose host ended less than [`ask::ADOPT_MS`] ago (its successor may yet
    /// look).
    fn look(&mut self, h: usize) -> Result<(), String> {
        if self.s["phase"] == 3 && self.s["rose"] == 0 && self.free >= self.free_at + ask::FREED_MIN
        {
            self.fire("Frees")?;
        }
        let short = self.free < FLOORS[h];
        let lost = self.unrecorded[h].take();
        if !short && ask::rests(&self.lease(), KEY, lost.as_ref().map(|t| t.at)) {
            self.trail.push(format!("host {h} look: rests"));
            self.unrecorded[h] = lost;
            return Ok(());
        }
        let mut before = self.lease().entry(KEY).cloned();
        let host = self.hosts[h].clone();
        let barred: Vec<String> = if h == 0 && self.done_a {
            vec!["s-a".to_owned()]
        } else {
            Vec::new()
        };
        let seats: Vec<Seat> = self.seats[h]
            .iter()
            .filter(|s| !barred.contains(&s.sid))
            .cloned()
            .collect();
        let live: Vec<String> = self.seats[h].iter().map(|s| s.sid.clone()).collect();
        let supervised: Vec<String> = live
            .iter()
            .filter(|sid| !(h == 0 && self.exited_a && *sid == "s-a"))
            .cloned()
            .collect();
        let dead: Vec<HostId> = self.dead.iter().map(|(h, _)| h.clone()).collect();
        let alive = move |x: &HostId| !dead.contains(x);
        let look = ask::Look {
            now_ms: self.now,
            host: &host,
            key: KEY,
            level: if short { Level::Short } else { Level::Ok },
            free: self.free,
            floor: FLOORS[h],
            files_free: None,
            files_floor: aterm_agent::harness::disk::EMERGENCY_FILES_FREE,
            done: self.done.then_some(Done::Spent),
            seats: &seats,
            supervised: &supervised,
            barred: &barred,
            live: &live,
            alive: &alive,
        };
        let (recorded, said, after) = ask::transact(&self.dir.0, &ask::LOCK_LADDER, |l| {
            let recorded = lost.as_ref().map(|t| {
                ask::typed_unrecorded(l, KEY, &host, &t.sid, t.at, t.free);
                l.entry(KEY).cloned()
            });
            (recorded, ask::step(l, &look), l.entry(KEY).cloned())
        })
        .expect("the lease is written")
        .expect("one host at a time");
        if let Some(recorded) = recorded {
            self.trail.push(format!("host {h} records its lost line"));
            self.record(before.as_ref(), recorded.as_ref())?;
            before = recorded;
        }
        self.trail.push(format!("host {h} look: {said:?}"));
        self.owed[h] = match after.as_ref().map(|e| &e.phase) {
            Some(Phase::Owed { pick, .. }) if pick.host == host => Some(pick.sid.clone()),
            _ => None,
        };
        let held_by = |e: Option<&ask::Entry>| {
            e.and_then(|e| e.floor.as_ref())
                .and_then(|f| self.hosts.iter().position(|h| *h == f.host))
        };
        if !matches!(said, Said::Opened { .. })
            && held_by(before.as_ref()) == Some(1)
            && held_by(after.as_ref()) == Some(0)
        {
            // The model's hosts never end, so it holds H's floor across H's successor: the lease
            // gave it to L's looks only while H's host had ended, and the successor takes it back.
            if self.s["bar"] == 1 {
                self.fire("RaiseH")?;
            } else if self.dead.is_empty() {
                return Err(
                    self.fail("the lease raised a floor the model holds already".to_owned())
                );
            }
        }
        let open = if h == 0 { "OpenH" } else { "OpenL" };
        let released = |e: Option<&ask::Entry>| e.map_or(0, |e| e.released);
        let freed_a_pick = released(after.as_ref()) > released(before.as_ref());
        let before = before.map(|e| e.phase);
        let barred_here = |pick: &Pick| pick.host == host && barred.contains(&pick.sid);
        let ended: Vec<HostId> = self.dead.iter().map(|(h, _)| h.clone()).collect();
        let adoptable = |pick: &Pick| {
            pick.host != host
                && (supervised.contains(&pick.sid) || barred.contains(&pick.sid))
                && ended.contains(&pick.host)
        };
        // An owed pick whose host ended lately, handed off before its deadline: its successor may
        // not have looked yet.
        let early = |pick: &Pick, until_ms: i64| {
            pick.host != host && self.ended_lately(&pick.host) && self.now < until_ms
        };
        match said {
            Said::Opened { .. } => {
                if matches!(before, Some(Phase::Settled { .. } | Phase::Out { .. })) {
                    self.fire("Expire")?;
                }
                self.fire(open)
            }
            Said::Elected { pick, .. } => self.fire(&format!("Elect{}", letter(&pick.sid))),
            Said::Owed { pick, .. } if h == 0 && pick.sid == "s-a" => self.fire("KeepA"),
            Said::Out { last } => match before {
                Some(Phase::Nominating { .. }) => self.fire("ElectNone"),
                Some(Phase::Owed { pick, until_ms, .. }) => {
                    let early = early(&pick, until_ms);
                    self.owed_off(
                        last,
                        freed_a_pick,
                        barred_here(&pick),
                        adoptable(&pick) || early,
                    )?;
                    self.fire("ElectNone")
                }
                Some(Phase::Asked { .. }) => self.fire("OutAsked"),
                other => Err(self.fail(format!("ran out from {other:?}"))),
            },
            Said::Handoff { why, from } => match before {
                Some(Phase::Owed { until_ms, .. }) => {
                    let early = early(&from, until_ms);
                    self.owed_off(
                        Some(why),
                        freed_a_pick,
                        barred_here(&from),
                        adoptable(&from) || early,
                    )
                }
                Some(Phase::Asked { .. }) => self.fire("HandOffAsked"),
                other => Err(self.fail(format!("handed off from {other:?}"))),
            },
            Said::Freed { .. } => self.fire("Freed"),
            Said::Ended { .. } => self.fire("End"),
            Said::Cooldown { .. } if self.m.action_enabled(open, &self.s) => {
                Err(self.fail("the lease waits out a cooldown the model is past".to_owned()))
            }
            Said::Freeing if self.s["done"] == 1 => {
                Err(self.fail("the lease waits on a pass the model says is spent".to_owned()))
            }
            Said::Settled | Said::Spent if short && self.m.action_enabled("Expire", &self.s) => {
                Err(self.fail(
                    "the lease keeps parked an episode the model gives way to a new one".to_owned(),
                ))
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

    /// The model's step for a look whose host recorded its lost line before stepping
    /// (`ask::typed_unrecorded`), from the lease `before` it to the lease `after` the record: a
    /// line the lease counted (its `asks` rose) while it still owed it (`RecordA`); after its
    /// hand-off, so the episode waits on its answer again (`RecordBackA`); or while it owes
    /// another session elected since, which keeps its line (`RecordOverA`) or is withdrawn untyped
    /// so the episode waits on A's answer (`RecordWithdrawA`), as the model's count says. A
    /// record that leaves it uncounted is the model's `RecordLostA`, which only a planted defect
    /// takes. A line of an episode that ended before its record counts for the cooldown alone, as
    /// the model's end forgets it.
    fn record(&mut self, before: Option<&Entry>, after: Option<&Entry>) -> Result<(), String> {
        if self.s["uA"] == 0 {
            return Ok(());
        }
        let asks = |e: Option<&Entry>| e.map_or(0, |e| e.asks);
        if asks(after) <= asks(before) {
            return self.fire("RecordLostA");
        }
        let phase = |e: Option<&Entry>| e.map(|e| e.phase.clone());
        let owed_to = |p: &Option<Phase>| match p {
            Some(Phase::Owed { pick, .. }) => Some(pick.sid.clone()),
            _ => None,
        };
        let (before, after) = (phase(before), phase(after));
        let asked_a = matches!(&after, Some(Phase::Asked { pick, .. }) if pick.sid == "s-a");
        match owed_to(&before) {
            Some(sid) if sid == "s-a" && asked_a => self.fire("RecordA"),
            Some(sid) if sid != "s-a" && asked_a => self.fire("RecordWithdrawA"),
            Some(sid) if owed_to(&after).as_deref() == Some(sid.as_str()) => {
                self.fire("RecordOverA")
            }
            None if asked_a => self.fire("RecordBackA"),
            _ => Err(self.fail(format!(
                "the record of A's line moved the lease from {before:?} to {after:?}"
            ))),
        }
    }

    /// A's turn break on the first host whose line goes but whose record the lease cannot write
    /// (a full disk): the claim and the record are made on a copy no write lands, and the host
    /// remembers the line, so its next look records it, as the window's act and look do. The
    /// model's `TypeALost`.
    fn turn_break_lost(&mut self) -> Result<(), String> {
        let Some(sid) = self.owed[0].take() else {
            return Ok(());
        };
        let host = self.hosts[0].clone();
        let mut copy = self.lease();
        match ask::at_break(&mut copy, KEY, &host, &sid, self.now, self.done_a) {
            Break::Yours if sid == "s-a" => {
                ask::typed(&mut copy, KEY, &host, &sid, self.now, self.free);
                self.trail
                    .push(format!("host 0 turn break @{sid}: typed, its record lost"));
                self.unrecorded[0] = Some(Lost {
                    sid,
                    at: self.now,
                    free: self.free,
                });
                self.last_line = Some(self.now);
                self.free_at = self.free;
                self.fire("TypeALost")
            }
            other => Err(self.fail(format!("a lost record at @{sid}'s turn break: {other:?}"))),
        }
    }

    /// Session `sid`'s turn break on host `h`: the line its host's board holds is released when
    /// the seat rule bars it (A, its task done) and typed only on a claim the lease grants, as
    /// the window's act does (`ask::at_break`).
    fn turn_break(&mut self, h: usize) -> Result<(), String> {
        let Some(sid) = self.owed[h].take() else {
            return Ok(());
        };
        let host = self.hosts[h].clone();
        let now = self.now;
        let free = self.free;
        let barred = h == 0 && sid == "s-a" && self.done_a;
        let stepped = ask::transact(&self.dir.0, &ask::LOCK_LADDER, |l| {
            match ask::at_break(l, KEY, &host, &sid, now, barred) {
                Break::Yours => Ok(Some(ask::typed(l, KEY, &host, &sid, now, free))),
                Break::Gone => Ok(None),
                Break::Released(said) => Err(said),
            }
        })
        .expect("the lease is written")
        .expect("one host at a time");
        match stepped {
            Ok(typed) => {
                self.trail
                    .push(format!("host {h} turn break @{sid}: typed={typed:?}"));
                if typed.is_some() {
                    self.last_line = Some(now);
                    self.free_at = free;
                    self.fire(&format!("Type{}", letter(&sid)))?;
                }
                Ok(())
            }
            Err(said) => {
                self.trail
                    .push(format!("host {h} turn break @{sid}: released {said:?}"));
                self.released(&said)
            }
        }
    }

    /// The model's steps for what releasing an owed line said: its own host releases it, so it
    /// is neither another host's successor's pick nor an ended host's.
    fn released(&mut self, said: &Said) -> Result<(), String> {
        match said {
            Said::Handoff { why, .. } => self.owed_off(Some(*why), true, true, false),
            Said::Out { last } => {
                self.owed_off(*last, true, true, false)?;
                self.fire("ElectNone")
            }
            other => Err(self.fail(format!("a release said {other:?}"))),
        }
    }

    /// The model's step for an owed pick handed off for `why`: A released as its task is done
    /// (`ReleaseA`), or any other hand-off. A release must count in the lease's `released`
    /// (`freed_a_pick`), so it spends no election, and nothing else may; a pick its own host
    /// bars (`barred_here`) is never handed off as undelivered, which would spend one; nor is one
    /// whose host ended while the looking host supervises its session or bars it, or ended less
    /// than [`ask::ADOPT_MS`] before its deadline (`adoptable`): the first host is its successor,
    /// and adopts it (releasing one it bars), and any other leaves it to the successor's first
    /// look.
    fn owed_off(
        &mut self,
        why: Option<Miss>,
        freed_a_pick: bool,
        barred_here: bool,
        adoptable: bool,
    ) -> Result<(), String> {
        let release = why == Some(Miss::Released);
        if release != freed_a_pick {
            return Err(self.fail(format!(
                "a hand-off for {why:?} {} the lease's released picks",
                if freed_a_pick { "raised" } else { "left" }
            )));
        }
        if barred_here && why == Some(Miss::Undelivered) {
            return Err(
                self.fail("the lease spent an election on a pick its own host bars".to_owned())
            );
        }
        if adoptable && why == Some(Miss::Undelivered) {
            return Err(self.fail(
                "the lease handed off as undelivered a pick its host's successor supervises, \
                 bars or may yet adopt"
                    .to_owned(),
            ));
        }
        self.fire(if release { "ReleaseA" } else { "HandOffOwed" })
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
            // The worker's own finding (its transcript, its Autopilot), not the task record the
            // model's `dA` is: the model's plain hand-off from an owed line.
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
    fn frees(&mut self, gib: u64) -> Result<(), String> {
        self.free += gib * GIB;
        self.trail.push(format!("free +{gib} GiB"));
        self.follow()
    }
}

/// The scripted episode, at a free figure only the first host reads short: the first host
/// elects A, A never comes to a turn break and the second host hands it off while the first's
/// board still holds A's line; then C takes its line, frees nothing, B takes the last, and the
/// owner is told. The second host's looks step the episode throughout and end none of it. Past
/// the first host's floor by the margin the episode ends, the cooldown keeps a new one shut, and
/// once it is over A frees what it was asked to.
fn scripted(w: &mut World) -> Result<(), String> {
    w.to(2)?;
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
    w.frees(0)?;
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
    w.to(3)?;
    w.look(1)?;
    w.look(0)?;
    w.to(4)?;
    w.look(1)?;
    w.to(2)?;
    w.look(0)?;
    w.wait(ask::COOLDOWN_MS)?;
    w.look(1)?;
    w.look(0)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(1)?;
    w.turn_break(0)?;
    w.look(0)?;
    w.turn_break(0)?;
    w.frees(3)?;
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
    assert_eq!(
        w.trail.iter().filter(|t| *t == "model End").count(),
        1,
        "the second host, whose floor the volume clears, ended nothing below the first's: {:#?}",
        w.trail
    );
    for action in [
        "OpenH",
        "ElectA",
        "ElectB",
        "ElectC",
        "KeepA",
        "TypeA",
        "TypeB",
        "TypeC",
        "HandOffOwed",
        "HandOffAsked",
        "OutAsked",
        "Climb",
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

/// The second host, whose floor is the lower, opens an episode; the first reads it short too and
/// raises the floor it is held to; the volume then clears the second's floor by its margin, and
/// the second's looks step the episode on without ending it, until it clears the first's.
fn raised(w: &mut World) -> Result<(), String> {
    w.spend(true)?;
    w.to(0)?;
    w.look(1)?;
    w.look(0)?;
    w.to(2)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(1)?;
    w.to(3)?;
    w.look(1)?;
    w.look(0)?;
    w.to(4)?;
    w.look(0)?;
    Ok(())
}

#[test]
fn an_episode_the_lower_floor_opened_is_held_to_the_higher_one_that_reads_it_short() {
    let mut w = World::new("raised");
    raised(&mut w).unwrap_or_else(|e| panic!("{e}"));
    for action in ["OpenL", "RaiseH", "ElectA", "End"] {
        assert!(
            w.trail.iter().any(|t| t == &format!("model {action}")),
            "the script reaches {action}: {:#?}",
            w.trail
        );
    }
    assert_eq!(w.s["phase"], 0, "{:#?}", w.trail);
}

/// The scripted episode settles, and the volume stays short of the first host's floor: a
/// cooldown after it settled (and after its last line) the first host's short look gives the
/// parked episode way to a new one, which elects again.
fn parked(w: &mut World) -> Result<(), String> {
    scripted(w)?;
    w.wait(ask::COOLDOWN_MS)?;
    w.look(1)?;
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(0)?;
    Ok(())
}

#[test]
fn a_parked_episode_gives_way_to_a_new_one_a_cooldown_after_it_parked() {
    let mut w = World::new("parked");
    parked(&mut w).unwrap_or_else(|e| panic!("{e}"));
    for action in ["Freed", "Ages", "Expire", "OpenH", "ElectA"] {
        assert!(
            w.trail.iter().any(|t| t == &format!("model {action}")),
            "the script reaches {action}: {:#?}",
            w.trail
        );
    }
    assert_eq!(w.s["phase"], 2, "{:#?}", w.trail);
}

/// A is elected, then its task turns done: its host's next look (`at_break` false) or the turn
/// break its board still holds A's line for (`at_break` true) releases it, at no election's
/// cost, and the next round elects C. With `exited`, A's agent exited, so A has also left its
/// host's supervised roster when that look comes.
fn done_pick(w: &mut World, at_break: bool, exited: bool) -> Result<(), String> {
    w.to(2)?;
    w.spend(true)?;
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(0)?;
    if exited {
        w.exit_a()?;
    } else {
        w.finish(true)?;
    }
    if at_break {
        w.turn_break(0)?;
    } else {
        w.look(0)?;
    }
    w.look(1)?;
    w.look(0)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(1)?;
    Ok(())
}

#[test]
fn a_pick_whose_task_is_done_is_released_at_its_hosts_next_look_or_turn_break() {
    for (at_break, exited) in [(false, false), (true, false), (false, true)] {
        let mut w = World::new("done");
        done_pick(&mut w, at_break, exited)
            .unwrap_or_else(|e| panic!("at_break={at_break} exited={exited}: {e}"));
        let e = w.lease().entry(KEY).cloned().expect("entry");
        assert_eq!(
            (e.picks.len(), e.released, e.asks),
            (2, 1, 0),
            "released, not handed off undelivered: {:#?}",
            w.trail
        );
        assert!(
            matches!(&e.phase, Phase::Owed { pick, .. } if pick.sid == "s-c"),
            "{:#?}",
            w.trail
        );
        for action in ["Finish", "ReleaseA", "ElectC"] {
            assert!(
                w.trail.iter().any(|t| t == &format!("model {action}")),
                "at_break={at_break} reaches {action}: {:#?}",
                w.trail
            );
        }
    }
}

/// A seamless update: the first host elects A, then its process ends and a successor adopts A
/// and B under their sids. The successor's first look moves A's pick to it, so its later looks
/// keep A owed past the adoption window and A's line is typed there, and A frees what it was
/// asked to.
fn succession(w: &mut World) -> Result<(), String> {
    w.to(2)?;
    w.spend(true)?;
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(0)?;
    w.succeed(0);
    w.look(0)?;
    w.wait(ask::ADOPT_MS)?;
    w.look(0)?;
    w.turn_break(0)?;
    w.frees(3)?;
    w.look(1)?;
    Ok(())
}

#[test]
fn a_successor_adopts_the_pick_of_the_host_it_replaced() {
    let mut w = World::new("succession");
    succession(&mut w).unwrap_or_else(|e| panic!("{e}"));
    let e = w.lease().entry(KEY).cloned().expect("entry");
    assert_eq!(
        (e.picks.len(), e.asks, e.asked.as_slice()),
        (1, 1, ["s-a".to_owned()].as_slice()),
        "A was elected once and asked, not handed off: {:#?}",
        w.trail
    );
    assert_eq!(e.picks[0].host, w.hosts[0], "{:#?}", w.trail);
    for action in ["ElectA", "KeepA", "TypeA", "Frees", "Freed"] {
        assert!(
            w.trail.iter().any(|t| t == &format!("model {action}")),
            "the script reaches {action}: {:#?}",
            w.trail
        );
    }
    assert_eq!(w.s["phase"], 4, "settled: {:#?}", w.trail);
}

/// A's line goes near the end of its delivery window, but its record is lost to a write that
/// fails, so the lease never sees the claim: the second host hands A off as undelivered at the
/// deadline. The first host's next look records the line: it is counted, and as nobody was
/// elected since, the episode waits on A's answer again. A frees nothing; C is asked, frees
/// nothing, and the episode is out after two lines.
fn lost_record(w: &mut World) -> Result<(), String> {
    w.to(2)?;
    w.spend(true)?;
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(0)?;
    w.wait(ask::DELIVER_MS - 60_000)?;
    w.turn_break_lost()?;
    w.wait(60_000)?;
    w.look(1)?;
    w.wait(30_000)?;
    w.look(0)?;
    w.wait(ask::ANSWER_MS)?;
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(1)?;
    w.turn_break(1)?;
    w.wait(ask::ANSWER_MS)?;
    w.look(1)?;
    Ok(())
}

#[test]
fn a_line_whose_record_was_lost_counts_once_its_host_records_it() {
    let mut w = World::new("lost-record");
    lost_record(&mut w).unwrap_or_else(|e| panic!("{e}"));
    let e = w.lease().entry(KEY).cloned().expect("entry");
    assert_eq!(
        (e.asks, e.asked.as_slice()),
        (2, ["s-a".to_owned(), "s-c".to_owned()].as_slice()),
        "two lines went and both are counted: {:#?}",
        w.trail
    );
    assert!(matches!(e.phase, Phase::Out { .. }), "{:#?}", w.trail);
    for action in [
        "TypeALost",
        "HandOffOwed",
        "RecordBackA",
        "HandOffAsked",
        "ElectC",
        "TypeC",
        "OutAsked",
    ] {
        assert!(
            w.trail.iter().any(|t| t == &format!("model {action}")),
            "the script reaches {action}: {:#?}",
            w.trail
        );
    }
}

/// A seamless update with the other host's looks between the first host's end and its
/// successor's first look: they leave the ended host's pick owed, as its successor may yet adopt
/// it, and the successor's look adopts it, so A's line is typed there. Free space is below both
/// floors, so the other host's looks, which hold the episode to their own floor once the first
/// host ended, step it on rather than end it.
fn succession_after_another(w: &mut World) -> Result<(), String> {
    w.to(0)?;
    w.spend(true)?;
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(0)?;
    w.succeed(0);
    w.wait(60_000)?;
    w.look(1)?;
    w.wait(60_000)?;
    w.look(1)?;
    w.wait(30_000)?;
    w.look(0)?;
    w.turn_break(0)?;
    w.frees(3)?;
    w.look(1)?;
    Ok(())
}

#[test]
fn another_hosts_look_leaves_an_ended_hosts_pick_to_its_successor() {
    let mut w = World::new("succession-after-another");
    succession_after_another(&mut w).unwrap_or_else(|e| panic!("{e}"));
    let e = w.lease().entry(KEY).cloned().expect("entry");
    assert_eq!(
        (e.picks.len(), e.asks, e.asked.as_slice()),
        (1, 1, ["s-a".to_owned()].as_slice()),
        "A was elected once and asked on its successor, not handed off: {:#?}",
        w.trail
    );
    assert_eq!(e.picks[0].host, w.hosts[0], "{:#?}", w.trail);
    assert_eq!(w.s["phase"], 4, "settled: {:#?}", w.trail);
}

/// A host ends with no successor: the other host's looks leave its pick owed for
/// [`ask::ADOPT_MS`] from the first that read it ended, then hand it on as undelivered, and the
/// next round elects C.
fn orphaned(w: &mut World) -> Result<(), String> {
    w.to(0)?;
    w.spend(true)?;
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(0)?;
    w.succeed(0);
    w.wait(60_000)?;
    w.look(1)?;
    w.wait(ask::ADOPT_MS - 1)?;
    w.look(1)?;
    w.wait(1)?;
    w.look(1)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(1)?;
    Ok(())
}

#[test]
fn an_ended_hosts_pick_nobody_adopts_is_handed_on_once_the_adoption_window_is_over() {
    let mut w = World::new("orphaned");
    orphaned(&mut w).unwrap_or_else(|e| panic!("{e}"));
    let looks: Vec<&str> = w
        .trail
        .iter()
        .filter_map(|t| t.strip_prefix("host 1 look: "))
        .map(|t| t.split_whitespace().next().unwrap_or(""))
        .collect();
    assert_eq!(
        looks,
        [
            "Nominating",
            "Owed",
            "Owed",
            "Handoff",
            "Nominating",
            "Elected"
        ],
        "owed through the adoption window, handed on at its end: {:#?}",
        w.trail
    );
    let e = w.lease().entry(KEY).cloned().expect("entry");
    assert_eq!((e.picks.len(), e.released, e.asks), (2, 0, 0));
    assert!(
        matches!(&e.phase, Phase::Owed { pick, .. } if pick.sid == "s-c"),
        "{:#?}",
        w.trail
    );
    for action in ["ElectA", "HandOffOwed", "ElectC"] {
        assert!(
            w.trail.iter().any(|t| t == &format!("model {action}")),
            "the script reaches {action}: {:#?}",
            w.trail
        );
    }
}

/// B is asked first (A's task is done through the first round, so the first host nominates B,
/// and the second host does not look) and frees nothing. A is elected next; its line goes near
/// its deadline but its record is lost, and the second host hands A off at the deadline, then
/// nominates C and elects it before the first host looks again. That look records A's line: the
/// episode's second, so C, elected after A's hand-off, is withdrawn untyped and the episode waits
/// on A's answer; C's turn break types nothing, A frees nothing, and the episode is out after two
/// lines.
fn lost_record_over_a_later_pick(w: &mut World) -> Result<(), String> {
    w.to(2)?;
    w.spend(true)?;
    w.finish(true)?;
    w.look(0)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(0)?;
    w.turn_break(0)?;
    w.finish(false)?;
    w.wait(ask::ANSWER_MS)?;
    w.look(0)?;
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(0)?;
    w.wait(ask::DELIVER_MS - 60_000)?;
    w.turn_break_lost()?;
    w.wait(60_000)?;
    w.look(1)?;
    w.wait(60_000)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(1)?;
    w.look(0)?;
    w.turn_break(1)?;
    w.wait(ask::ANSWER_MS)?;
    w.look(1)?;
    Ok(())
}

#[test]
fn a_late_record_that_spends_the_episodes_lines_withdraws_a_pick_elected_since() {
    let mut w = World::new("lost-record-over");
    lost_record_over_a_later_pick(&mut w).unwrap_or_else(|e| panic!("{e}"));
    let e = w.lease().entry(KEY).cloned().expect("entry");
    assert_eq!(
        (e.asks, e.asked.as_slice()),
        (2, ["s-b".to_owned(), "s-a".to_owned()].as_slice()),
        "two lines went, both counted, and C took none: {:#?}",
        w.trail
    );
    assert!(matches!(e.phase, Phase::Out { .. }), "{:#?}", w.trail);
    assert!(
        w.trail
            .iter()
            .any(|t| t.contains("turn break @s-c: typed=None")),
        "C's board still held its line, and the lease refused it: {:#?}",
        w.trail
    );
    for action in [
        "TypeB",
        "ElectA",
        "TypeALost",
        "HandOffOwed",
        "ElectC",
        "RecordWithdrawA",
        "OutAsked",
    ] {
        assert!(
            w.trail.iter().any(|t| t == &format!("model {action}")),
            "the script reaches {action}: {:#?}",
            w.trail
        );
    }
}

/// A's line went, its record was lost and its pick handed off, and C was elected since; A's
/// record is the episode's first line, so C keeps its line, which is the second.
fn lost_record_under_a_later_pick(w: &mut World) -> Result<(), String> {
    w.to(2)?;
    w.spend(true)?;
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(0)?;
    w.wait(ask::DELIVER_MS - 60_000)?;
    w.turn_break_lost()?;
    w.wait(60_000)?;
    w.look(1)?;
    w.wait(60_000)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(1)?;
    w.look(0)?;
    w.turn_break(1)?;
    Ok(())
}

#[test]
fn a_late_record_that_leaves_a_line_keeps_a_pick_elected_since() {
    let mut w = World::new("lost-record-under");
    lost_record_under_a_later_pick(&mut w).unwrap_or_else(|e| panic!("{e}"));
    let e = w.lease().entry(KEY).cloned().expect("entry");
    assert_eq!(
        (e.asks, e.asked.as_slice()),
        (2, ["s-a".to_owned(), "s-c".to_owned()].as_slice()),
        "{:#?}",
        w.trail
    );
    for action in ["TypeALost", "ElectC", "RecordOverA", "TypeC"] {
        assert!(
            w.trail.iter().any(|t| t == &format!("model {action}")),
            "the script reaches {action}: {:#?}",
            w.trail
        );
    }
}

/// A's line goes with its record lost, and A frees enough that the second host's look ends the
/// episode before the first host looks again. The first host's next look reads the volume clear
/// of its floor with no episode open, yet records A's line before it rests, so the cooldown A's
/// line started holds a new episode shut when the volume falls short again, until it is over.
fn lost_record_past_the_end(w: &mut World) -> Result<(), String> {
    w.to(2)?;
    w.spend(true)?;
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(0)?;
    w.turn_break_lost()?;
    w.to(4)?;
    w.look(1)?;
    w.look(0)?;
    w.look(0)?;
    w.to(0)?;
    w.wait(60_000)?;
    w.look(1)?;
    w.look(0)?;
    w.wait(ask::COOLDOWN_MS)?;
    w.look(1)?;
    Ok(())
}

#[test]
fn a_lost_record_is_kept_by_a_look_that_reads_the_volume_clear_after_its_episode_ended() {
    let mut w = World::new("lost-record-past-end");
    lost_record_past_the_end(&mut w).unwrap_or_else(|e| panic!("{e}"));
    let looks: Vec<&str> = w
        .trail
        .iter()
        .filter_map(|t| t.strip_prefix("host 0 look: "))
        .map(|t| t.split_whitespace().next().unwrap_or(""))
        .collect();
    assert_eq!(
        looks,
        ["Opened", "Elected", "Quiet", "rests", "Cooldown"],
        "the clear look after the end recorded the line, and the next one rested: {:#?}",
        w.trail
    );
    assert!(
        w.trail
            .iter()
            .any(|t| t.starts_with("host 1 look: Cooldown")),
        "the second host's short look waits out A's cooldown: {:#?}",
        w.trail
    );
    for action in ["TypeALost", "End", "Cools", "OpenL"] {
        assert!(
            w.trail.iter().any(|t| t == &format!("model {action}")),
            "the script reaches {action}: {:#?}",
            w.trail
        );
    }
}

/// A is elected; its agent exits with its task (its record says exited, and it leaves the
/// supervised roster) and its host is replaced by a seamless update before that host looks
/// again. The successor's seat rule bars A, so its first look adopts A's pick and releases it at
/// no election's cost, and the next round elects C.
fn exited_across_a_succession(w: &mut World) -> Result<(), String> {
    w.to(2)?;
    w.spend(true)?;
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS)?;
    w.look(0)?;
    w.exit_a()?;
    w.succeed(0);
    w.wait(2 * 60_000)?;
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::ADOPT_MS)?;
    w.look(0)?;
    w.look(1)?;
    Ok(())
}

#[test]
fn a_successor_releases_an_exited_pick_of_the_host_it_replaced_at_no_elections_cost() {
    let mut w = World::new("exited-succession");
    exited_across_a_succession(&mut w).unwrap_or_else(|e| panic!("{e}"));
    let e = w.lease().entry(KEY).cloned().expect("entry");
    assert_eq!(
        (e.picks.len(), e.released, e.asks),
        (2, 1, 0),
        "released, not handed off undelivered: {:#?}",
        w.trail
    );
    assert!(
        matches!(&e.phase, Phase::Owed { pick, .. } if pick.sid == "s-c"),
        "{:#?}",
        w.trail
    );
    for action in ["ElectA", "Finish", "ReleaseA", "ElectC"] {
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
        let at = usize::try_from(place(w.free)).unwrap_or(0);
        match next() % 24 {
            0 => w.to(usize::try_from(next() % 5).unwrap_or(0))?,
            1 => w.to(at.saturating_sub(1))?,
            2 => w.to((at + 1).min(4))?,
            3 => w.spend(true)?,
            4 => w.spend(false)?,
            5 | 6 => w.wait(60_000)?,
            7 => w.wait(ask::NOMINATE_MS)?,
            8 => w.wait(ask::DELIVER_MS.max(ask::ANSWER_MS))?,
            9 => w.wait(ask::COOLDOWN_MS)?,
            10 => w.frees([0, 0, 0, 2][usize::try_from(next() % 4).unwrap_or(0)])?,
            11 => match next() % 3 {
                0 => w.finish(true)?,
                1 => w.finish(false)?,
                _ => w.exit_a()?,
            },
            12..=17 => w.look(h)?,
            18..=21 => w.turn_break(h)?,
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
        "OpenH",
        "OpenL",
        "RaiseH",
        "ElectNone",
        "KeepA",
        "ReleaseA",
        "TypeC",
        "HandOffOwed",
        "HandOffAsked",
        "OutAsked",
        "Frees",
        "Freed",
        "Ages",
        "Expire",
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

/// NEGATIVE CONTROL: a look that ends an episode once it reads the volume clear of its own floor
/// (`Fault::EndAtOwnFloor`, the rule before floors were recorded) lets the second host end what
/// the first opened at a figure the first still reads short, and the checker rejects that end;
/// so it does the end a lower floor's look makes of an episode a higher one raised.
#[test]
fn a_look_that_ends_an_episode_short_of_its_floors_margin_is_rejected() {
    for (name, script) in [
        (
            "own-floor",
            scripted as fn(&mut World) -> Result<(), String>,
        ),
        ("raised", raised),
    ] {
        let mut w = World::new(name);
        let caught = ask::fault::with(ask::fault::Fault::EndAtOwnFloor, || script(&mut w))
            .expect_err("the checker must reject an end short of the floor's margin");
        assert!(caught.contains("`End` is not enabled"), "{name}: {caught}");
    }
}

/// NEGATIVE CONTROL: a look that ends an episode at the floor it is held to, with no margin past
/// it (`Fault::EndAtHeldFloor`), ends the scripted episode at the first look past the first
/// host's floor, inside its margin, and the checker rejects that end: the hysteresis is the
/// model's, not the projection's ([`the_models_margin_is_the_leases`]).
#[test]
fn a_look_that_ends_an_episode_inside_its_floors_margin_is_rejected() {
    let mut w = World::new("no-margin");
    let caught = ask::fault::with(ask::fault::Fault::EndAtHeldFloor, || scripted(&mut w))
        .expect_err("the checker must reject an end inside the floor's margin");
    assert!(caught.contains("`End` is not enabled"), "{caught}");
    assert_eq!(
        caught
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| l.starts_with("free ")),
        Some("free 50 GiB"),
        "rejected at the first look past the floor: {caught}"
    );
}

/// NEGATIVE CONTROL: a look that keeps a parked episode parked however long ago it parked
/// (`Fault::ParkForever`) leaves a volume short of its floor unasked, and the checker rejects
/// that look.
#[test]
fn a_look_that_keeps_an_episode_parked_past_the_cooldown_is_rejected() {
    let mut w = World::new("park-forever");
    let caught = ask::fault::with(ask::fault::Fault::ParkForever, || parked(&mut w))
        .expect_err("the checker must reject a parked episode kept past its cooldown");
    assert!(
        caught.contains("keeps parked an episode the model gives way"),
        "{caught}"
    );
}

/// NEGATIVE CONTROL: a look that keeps a pick its seat rule bars owed (`Fault::KeepBarred`, the
/// look before releases were taken there) is rejected: the model keeps only a pick it may ask.
#[test]
fn a_look_that_keeps_a_done_pick_owed_is_rejected() {
    let mut w = World::new("kept");
    let caught = ask::fault::with(ask::fault::Fault::KeepBarred, || {
        done_pick(&mut w, false, false)
    })
    .expect_err("the checker must reject a done pick kept owed");
    assert!(caught.contains("`KeepA` is not enabled"), "{caught}");
}

/// NEGATIVE CONTROL: a look that hands off an exited pick as undelivered because it left the
/// supervised roster (`Fault::GoneFirst`, the order before the task record was read first)
/// spends an election on a pick its own host bars, and the checker rejects that hand-off.
#[test]
fn a_look_that_spends_an_election_on_an_exited_pick_is_rejected() {
    let mut w = World::new("gone-first");
    let caught = ask::fault::with(ask::fault::Fault::GoneFirst, || {
        done_pick(&mut w, false, true)
    })
    .expect_err("the checker must reject an exited pick handed off undelivered");
    assert!(
        caught.contains("spent an election on a pick its own host bars"),
        "{caught}"
    );
}

/// NEGATIVE CONTROL: a turn break that claims a line the seat rule bars (`Fault::TypeBarred`,
/// the turn break that read only the transcript) types it, and the checker rejects that line.
#[test]
fn a_turn_break_that_types_a_barred_session_is_rejected() {
    let mut w = World::new("typed-barred");
    let caught = ask::fault::with(ask::fault::Fault::TypeBarred, || {
        done_pick(&mut w, true, false)
    })
    .expect_err("the checker must reject a line typed to a session the seat rule bars");
    assert!(caught.contains("`TypeA` is not enabled"), "{caught}");
}

/// NEGATIVE CONTROL: a look that hands off a pick whose host ended though this look's host
/// supervises its session (`Fault::NoAdopt`, the look before successors adopted picks) spends an
/// election on the session a seamless update's successor runs, and the checker rejects it.
#[test]
fn a_look_that_hands_off_a_pick_its_successor_supervises_is_rejected() {
    let mut w = World::new("no-adopt");
    let caught = ask::fault::with(ask::fault::Fault::NoAdopt, || succession(&mut w))
        .expect_err("the checker must reject a successor's pick handed off undelivered");
    assert!(
        caught.contains("a pick its host's successor supervises"),
        "{caught}"
    );
}

/// NEGATIVE CONTROL: a look that hands off an ended host's pick at once though it does not
/// supervise its session (`Fault::OrphanAtOnce`, the look before such picks waited
/// `ask::ADOPT_MS`) gives away the session a seamless update's successor was about to adopt, and
/// the checker rejects that hand-off.
#[test]
fn a_look_that_hands_off_an_ended_hosts_pick_before_its_successor_looks_is_rejected() {
    let mut w = World::new("orphan-at-once");
    let caught = ask::fault::with(ask::fault::Fault::OrphanAtOnce, || {
        succession_after_another(&mut w)
    })
    .expect_err("the checker must reject a pick handed off before its successor could look");
    assert!(caught.contains("may yet adopt"), "{caught}");
}

/// NEGATIVE CONTROL: a late record that reaches the episode's two lines but leaves a pick elected
/// since owed (`Fault::OwedPastTwo`, the record before such picks were withdrawn) would let that
/// pick take a third line, and the checker rejects the record.
#[test]
fn a_late_record_that_leaves_a_line_past_the_bound_owed_is_rejected() {
    let mut w = World::new("owed-past-two");
    let caught = ask::fault::with(ask::fault::Fault::OwedPastTwo, || {
        lost_record_over_a_later_pick(&mut w)
    })
    .expect_err("the checker must reject a record that keeps a line past the bound owed");
    assert!(caught.contains("`RecordOverA` is not enabled"), "{caught}");
}

/// NEGATIVE CONTROL: a record that counts a line only while the lease still owes it
/// (`Fault::CountOwedOnly`, the record before late lines were counted) leaves the lost line out
/// of the episode's lines, and the checker rejects that record.
#[test]
fn a_record_that_leaves_a_lost_line_uncounted_is_rejected() {
    let mut w = World::new("count-owed-only");
    let caught = ask::fault::with(ask::fault::Fault::CountOwedOnly, || lost_record(&mut w))
        .expect_err("the checker must reject a line the lease never counts");
    assert!(caught.contains("`RecordLostA` is not enabled"), "{caught}");
}

/// NEGATIVE CONTROL: a look that reads the volume clear and rests with its host's line
/// unrecorded (`Fault::RestUnrecorded`, the window's look before it recorded such a line first)
/// never starts the cooldown of a line whose episode another host ended, so the second host opens
/// a new episode inside it, and the checker rejects that opening.
#[test]
fn a_look_that_rests_with_a_lost_line_unrecorded_is_rejected() {
    let mut w = World::new("rest-unrecorded");
    let caught = ask::fault::with(ask::fault::Fault::RestUnrecorded, || {
        lost_record_past_the_end(&mut w)
    })
    .expect_err("the checker must reject an episode opened inside a lost line's cooldown");
    assert!(caught.contains("`OpenL` is not enabled"), "{caught}");
}

/// NEGATIVE CONTROL: a look that adopts an ended host's pick only where it supervises its session
/// (`Fault::AdoptSupervisedOnly`, the look before it adopted one its seat rule bars) leaves an
/// exited pick owed at its successor's look, which bars it, and the checker rejects that look.
#[test]
fn a_successor_that_keeps_an_exited_pick_of_the_host_it_replaced_owed_is_rejected() {
    let mut w = World::new("adopt-supervised-only");
    let caught = ask::fault::with(ask::fault::Fault::AdoptSupervisedOnly, || {
        exited_across_a_succession(&mut w)
    })
    .expect_err("the checker must reject an exited pick its successor keeps owed");
    assert!(caught.contains("`KeepA` is not enabled"), "{caught}");
}
