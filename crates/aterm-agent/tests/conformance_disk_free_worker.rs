// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance tests for `DiskFreeWorker` against the ask's lease (`disk::ask`): two hosts
//! step one real lease file under its lock (`ask::transact`), each look its take-over, its
//! `step` and its `worker_step` in one transaction, as the window does, and the first host then
//! reads `owner_needed` for its alert. The test plays each host's main thread and the worker's
//! session: on `WorkerSaid::Claimed` the claimant's stub spawn starts a session, fails, or says
//! nothing, and that host's next look reads what it did (`Ahead::spawned`); the session's task
//! record says it works, ended or stalled (`Ahead::task`); a host can end, with a successor that
//! runs its sessions under a new `HostId` or with none. Every look is projected onto the model
//! and its moves fired: a move the model does not enable, a law it breaks after one, or a lease
//! whose episode, worker record, due reading or round the model's state does not match fails the
//! walk. The projection reads the lease by the design's rules (§8.4, §8.5), not by the code's own
//! predicates, so a planted defect there is a step the model refuses.

use std::path::PathBuf;

use aterm_agent::harness::disk::ask::{
    self, Break, Done, Entry, HeldBy, HostId, Lease, Miss, Phase, Said, Seat, Spawned, TaskSeen,
    WorkerSaid, WorkerState,
};
use aterm_agent::harness::disk::fill::Projection;
use aterm_agent::harness::disk::{EMERGENCY_FILES_FREE, Level};
use aterm_models_harness::disk_free_worker_model;
use aterm_spec::derive::Model;

type S = std::collections::BTreeMap<&'static str, i64>;

/// A walk of the world, failing where the model refuses a step.
type Walk = fn(&mut World) -> Result<(), String>;

const GIB: u64 = 1 << 30;
const KEY: &str = "dev:conformance-worker";
const T0: i64 = 1_792_000_000_000;

/// Both hosts' floor.
const FLOOR: u64 = 47 * GIB;
const TOTAL: u64 = 1000 * GIB;
/// The free bytes below which writes fail, as `owner_needed` is told it.
const EMERGENCY: u64 = 2 * GIB;
/// Short of the floor.
const SHORT: u64 = 20 * GIB;
/// Clear of the floor past the margin an episode ends at.
const CLEAR: u64 = 60 * GIB;
/// A fill whose crossing of the floor from [`CLEAR`] is about 22 minutes away and whose emergency
/// line is about 99, past every step's window.
const SLOW: u64 = 10 << 20;
/// A fill that reaches the emergency line within a worker's window.
const FAST: u64 = 60 << 20;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "aterm-conformance-diskworker-{name}-{}-{n}",
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

/// What the claimant's main thread does with a claim.
#[derive(Clone, Copy)]
enum Plan {
    /// Starts the worker's session, and says so at its host's next look.
    Start,
    /// Starts none, for this reason.
    Fail(&'static str),
    /// Never gets to it: its host's looks say nothing of it.
    Silent,
}

/// The worker's session.
#[derive(Clone)]
struct Session {
    sid: String,
    /// The index of the host that runs it.
    host: usize,
    /// Its Autopilot drives it: the model's `ws` is 1.
    works: bool,
    /// What its host's task record says of it.
    task: TaskSeen,
    /// It still runs on its host.
    live: bool,
}

/// The world two hosts share: the lease's directory, the clock, the volume, the hosts, their
/// sessions, the worker's, and the model.
struct World {
    dir: Scratch,
    now: i64,
    free: u64,
    done: bool,
    /// The fill rate each host's ring projects a crossing at (bytes a second); `None`: none.
    rate: Option<u64>,
    hosts: [HostId; 2],
    /// Each host can start a worker (`Ahead::worker_ok`).
    able: [bool; 2],
    /// The hosts that ended.
    dead: Vec<HostId>,
    /// Each host's ask sessions, with what the pass's holders hold under their checkouts (GiB).
    seats: [Vec<(&'static str, u64)>; 2],
    /// The sessions held at a usage-limit wall.
    held: Vec<&'static str>,
    plan: Plan,
    /// What each host's main thread's last spawn did, until its look records it.
    spawned: [Option<Spawned>; 2],
    session: Option<Session>,
    /// Workers spawned, for their sids.
    spawns: u32,
    /// When the last claim was made.
    claimed_at: Option<i64>,
    /// The line each host's last look left owed one of its sessions.
    owed: [Option<String>; 2],
    m: Model,
    s: S,
    trail: Vec<String>,
}

impl World {
    fn new(name: &str) -> World {
        let m = disk_free_worker_model();
        let s = m.init_state();
        World {
            dir: Scratch::new(name),
            now: T0,
            free: CLEAR,
            done: false,
            rate: None,
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
            able: [true, false],
            dead: Vec::new(),
            seats: [Vec::new(), Vec::new()],
            held: Vec::new(),
            plan: Plan::Start,
            spawned: [None, None],
            session: None,
            spawns: 0,
            claimed_at: None,
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

    fn wait(&mut self, ms: i64) {
        self.now += ms;
        self.trail.push(format!("wait {} s", ms / 1000));
    }

    fn wait_until(&mut self, at: i64) {
        assert!(at >= self.now, "{at} is past");
        self.wait(at - self.now);
    }

    /// The volume reads `free` bytes free.
    fn to(&mut self, free: u64) {
        self.free = free;
        self.trail.push(format!("free {} GiB", free / GIB));
    }

    /// The asked session frees `gib` GiB.
    fn frees(&mut self, gib: u64) {
        self.free += gib * GIB;
        self.trail.push(format!("free +{gib} GiB"));
    }

    /// The hosts' rings project a crossing at `rate` (bytes a second), or none.
    fn fills(&mut self, rate: Option<u64>) {
        self.rate = rate;
        self.trail.push(format!("fill {rate:?}"));
    }

    /// aterm's own passes run out, or may free again.
    fn spend(&mut self, done: bool) {
        self.done = done;
        self.trail.push(format!("aterm done {done}"));
    }

    /// The worker's task record says `task`: it ended or stalled, so its session stops working.
    fn worker_ends(&mut self, task: TaskSeen) -> Result<(), String> {
        let Some(s) = self.session.as_mut() else {
            return Err(self.fail("no worker session to end".to_owned()));
        };
        let worked = s.works;
        s.task = task.clone();
        s.works = false;
        self.trail.push(format!("worker task {task:?}"));
        if worked {
            self.fire("Stops")?;
        }
        Ok(())
    }

    /// Host `h`'s process ends: with a successor that runs its sessions under the same sids as a
    /// new `HostId` (a seamless update), or with none, its sessions ending with it.
    fn end_host(&mut self, h: usize, successor: bool) -> Result<(), String> {
        let gone = self.hosts[h].clone();
        self.dead.push(gone.clone());
        self.spawned[h] = None;
        self.owed[h] = None;
        self.trail.push(format!(
            "host {h} (pid {}) ended, {}",
            gone.pid,
            if successor {
                "a successor runs its sessions"
            } else {
                "with no successor"
            }
        ));
        if successor {
            self.hosts[h] = HostId {
                pid: 303 + u32::try_from(self.dead.len()).unwrap_or(0),
                start: format!("successor-{}", self.dead.len()),
            };
            return Ok(());
        }
        self.seats[h].clear();
        if let Some(s) = self.session.as_mut().filter(|s| s.host == h) {
            let worked = s.works;
            s.live = false;
            s.works = false;
            if worked {
                self.fire("Stops")?;
            }
        }
        Ok(())
    }

    /// The model's moves from where it holds the world to what this look reads: the pressure,
    /// aterm's freeing, and the two cooldowns as the lease's clock says them.
    fn world_moves(&mut self) -> Result<(), String> {
        let to = if self.rate.is_some() {
            2
        } else if self.free < FLOOR {
            1
        } else {
            0
        };
        while self.s["pres"] != to {
            let mv = match (self.s["pres"], to) {
                (_, 0) => "Relax",
                (0, 1) => "Press",
                (2, 1) => "Uncross",
                _ => "Cross",
            };
            self.fire(mv)?;
        }
        if self.done != (self.s["done"] == 1) {
            self.fire(if self.done { "Spend" } else { "Unspend" })?;
        }
        let e = self.lease().entry(KEY).cloned();
        let within =
            |at: Option<i64>, ms: i64| at.is_some_and(|at| (0..ms).contains(&(self.now - at)));
        let lcool = within(e.as_ref().and_then(|e| e.last_ask_ms), ask::COOLDOWN_MS);
        let wcool = within(
            e.as_ref().and_then(|e| e.last_worker_ms),
            ask::WORKER_COOLDOWN_MS,
        );
        for (var, lease_says, cools) in [
            ("lcool", lcool, "LineCools"),
            ("wcool", wcool, "WorkerCools"),
        ] {
            match (self.s[var], lease_says) {
                (1, false) => self.fire(cools)?,
                (0, true) => {
                    return Err(self.fail(format!(
                        "the lease's {var} stands, though the model's does not"
                    )));
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Host `h`'s look: its take-over, its step and its worker step under one lock; then, on the
    /// first host, its read of `owner_needed`.
    fn look(&mut self, h: usize) -> Result<(), String> {
        self.world_moves()?;
        let host = self.hosts[h].clone();
        let seats: Vec<Seat> = self.seats[h]
            .iter()
            .filter(|(sid, _)| !self.held.contains(sid))
            .map(|(sid, held)| Seat {
                sid: (*sid).to_owned(),
                held: held * GIB,
                at_break: true,
            })
            .collect();
        let held: Vec<(String, HeldBy)> = self.seats[h]
            .iter()
            .filter(|(sid, _)| self.held.contains(sid))
            .map(|(sid, _)| ((*sid).to_owned(), HeldBy::Limit))
            .collect();
        let supervised: Vec<String> = self.seats[h].iter().map(|(s, _)| (*s).to_owned()).collect();
        let session = self.session.clone().filter(|s| s.host == h && s.live);
        let live: Vec<String> = supervised
            .iter()
            .cloned()
            .chain(session.iter().map(|s| s.sid.clone()))
            .collect();
        let task = move |sid: &str| {
            session
                .as_ref()
                .filter(|s| s.sid == sid)
                .map(|s| s.task.clone())
        };
        let dead = self.dead.clone();
        let alive = move |x: &HostId| !dead.contains(x);
        let spawned = self.spawned[h].clone();
        let look = ask::Look {
            now_ms: self.now,
            host: &host,
            key: KEY,
            level: if self.free < FLOOR {
                Level::Short
            } else {
                Level::Ok
            },
            free: self.free,
            floor: FLOOR,
            files_free: None,
            files_floor: EMERGENCY_FILES_FREE,
            done: self.done.then_some(Done::Spent),
            seats: &seats,
            supervised: &supervised,
            barred: &[],
            live: &live,
            held: &held,
            reclaimed: &[],
            passing: false,
            alive: &alive,
            ahead: ask::Ahead {
                projected: self
                    .rate
                    .map(|r| Projection::of(r, self.free, FLOOR, Some(TOTAL))),
                cold: false,
                worker_ok: self.able[h],
                task: Some(&task),
                spawned: spawned.as_ref(),
            },
        };
        let roster = ask::Roster {
            host: &host,
            key: KEY,
            supervised: &supervised,
            barred: &[],
            live: &live,
            held: &held,
            alive: &alive,
        };
        let (said, worker, after) = ask::transact(&self.dir.0, &ask::LOCK_LADDER, |l| {
            let _took = ask::take_over(l, &roster);
            let said = ask::step(l, &look);
            let worker = ask::worker_step(l, &look);
            (said, worker, l.entry(KEY).cloned())
        })
        .expect("the lease is written")
        .expect("one host at a time");
        let needed = (h == 0).then(|| ask::owner_needed(after.as_ref(), &look, EMERGENCY));
        self.trail
            .push(format!("host {h} look: {said:?}; worker {worker:?}"));
        let claimed_here = after
            .as_ref()
            .and_then(|e| e.worker.as_ref())
            .is_some_and(|w| w.host == host && w.state == WorkerState::Claimed);
        if !claimed_here {
            self.spawned[h] = None;
        }
        self.owed[h] = match after.as_ref().map(|e| &e.phase) {
            Some(Phase::Owed { pick, .. }) if pick.host == host => Some(pick.sid.clone()),
            _ => None,
        };
        self.episode_moves(&said)?;
        self.round_moves(after.as_ref())?;
        self.worker_moves(h, worker.as_ref(), after.as_ref())?;
        self.matches(after.as_ref())?;
        if let Some(needed) = needed {
            self.alert_moves(needed, after.as_ref())?;
        }
        Ok(())
    }

    /// The model's move for what the step said of the episode.
    fn episode_moves(&mut self, said: &Said) -> Result<(), String> {
        let mv = match said {
            Said::Opened { .. } if self.s["ep"] == 0 => "Open",
            Said::Opened { .. } => "GiveWay",
            Said::Elected { .. } => "Elect",
            Said::Freed { .. } => "Settle",
            Said::Handoff {
                why: Miss::NothingFreed | Miss::Undelivered,
                ..
            } => "HandOffMissed",
            Said::Handoff { .. } | Said::Returned { .. } => "HandOff",
            Said::Out { .. } => "RunOut",
            Said::Ended { .. } => "End",
            Said::Quiet
            | Said::Freeing
            | Said::Cooldown { .. }
            | Said::WorkerRuns { .. }
            | Said::Nominating { .. }
            | Said::Owed { .. }
            | Said::Asked { .. }
            | Said::Waiting { .. }
            | Said::Settled
            | Said::Spent => return Ok(()),
        };
        self.fire(mv)
    }

    /// The model's moves for what a round reads: whether it waits only on held sessions, and
    /// whether a worker is due there.
    fn round_moves(&mut self, e: Option<&Entry>) -> Result<(), String> {
        if self.s["ep"] != 1 {
            return Ok(());
        }
        let (waits, need) = (
            waits(e, self.now),
            need_of(e, self.now, self.rate.is_some()),
        );
        match (self.s["wt"], waits) {
            (0, true) => self.fire("Wait")?,
            (1, false) => self.fire("Unwait")?,
            _ => {}
        }
        if self.s["need"] == 1 && need == 0 {
            self.fire("Undue")?;
        }
        Ok(())
    }

    /// The model's moves for what the worker step said, and the claimant's main thread's spawn.
    fn worker_moves(
        &mut self,
        h: usize,
        said: Option<&WorkerSaid>,
        e: Option<&Entry>,
    ) -> Result<(), String> {
        let can = self.able[h]
            || e.and_then(|e| e.due_ms)
                .is_none_or(|due| self.now - due < ask::SPAWN_MS);
        match said {
            None => {}
            Some(WorkerSaid::Claimed) => {
                self.can_to(true)?;
                self.fire("Claim")?;
                self.claimed_at = Some(self.now);
                self.spawn(h)?;
            }
            Some(WorkerSaid::Started { sid }) => {
                if self.session.as_ref().is_none_or(|s| s.sid != *sid) {
                    return Err(self.fail(format!("@{sid} started, but no stub spawned it")));
                }
                self.fire("Start")?;
            }
            Some(WorkerSaid::Over { why, here, .. }) => {
                let works = self.session.as_ref().is_some_and(|s| s.works);
                if why.starts_with("spawn:") || why == "lost" {
                    self.fire("Unstarted")?;
                } else if why == "expired" && works {
                    if !here {
                        return Err(self.fail("another host expired the worker".to_owned()));
                    }
                    // Its host turns its Autopilot Off: nothing more is typed into it.
                    if let Some(s) = self.session.as_mut() {
                        s.works = false;
                    }
                    self.fire("Expire")?;
                } else {
                    self.fire("Over")?;
                }
            }
        }
        self.can_to(can)
    }

    fn can_to(&mut self, can: bool) -> Result<(), String> {
        match (self.s["can"], can) {
            (0, true) => self.fire("Able"),
            (1, false) => self.fire("Unable"),
            _ => Ok(()),
        }
    }

    /// The claimant's main thread acts on its claim as [`World::plan`] says.
    fn spawn(&mut self, h: usize) -> Result<(), String> {
        let outcome = match self.plan {
            Plan::Start => {
                self.spawns += 1;
                let sid = format!("w-{}", self.spawns);
                self.session = Some(Session {
                    sid: sid.clone(),
                    host: h,
                    works: true,
                    task: TaskSeen::Working,
                    live: true,
                });
                self.fire("Spawn")?;
                Ok(sid)
            }
            Plan::Fail(why) => Err(why.to_owned()),
            Plan::Silent => {
                self.trail.push(format!("host {h} spawns nothing"));
                return Ok(());
            }
        };
        self.trail.push(format!("host {h} spawn: {outcome:?}"));
        self.spawned[h] = Some(Spawned {
            at_ms: self.now,
            outcome,
        });
        Ok(())
    }

    /// The lease and the model agree on the episode, the worker's record, the due reading, the
    /// round's wait, and whether the worker's session works.
    fn matches(&mut self, e: Option<&Entry>) -> Result<(), String> {
        let lease = [
            ("ep", ep_of(e)),
            ("wk", wk_of(e)),
            ("need", need_of(e, self.now, self.rate.is_some())),
            (
                "ws",
                i64::from(self.session.as_ref().is_some_and(|s| s.works)),
            ),
        ];
        for (var, says) in lease {
            let model = if var == "ws" {
                i64::from(self.s["ws"] == 1)
            } else {
                self.s[var]
            };
            if model != says {
                return Err(self.fail(format!(
                    "the lease reads {var} = {says}, the model {model}: {e:?}"
                )));
            }
        }
        if self.s["ep"] == 1 && (self.s["wt"] == 1) != waits(e, self.now) {
            return Err(self.fail("the round's wait is not the model's".to_owned()));
        }
        Ok(())
    }

    /// The first host's alert, as its look read `owner_needed`: raised, kept or withdrawn.
    fn alert_moves(&mut self, needed: bool, e: Option<&Entry>) -> Result<(), String> {
        if let Some(end) = e.and_then(|e| in_hand_end(e, self.now)) {
            let late = self.rate.is_some_and(|r| {
                let left_ms = i64::try_from(self.free.saturating_sub(EMERGENCY) / r * 1000)
                    .unwrap_or(i64::MAX);
                self.now + left_ms < end
            });
            match (self.s["late"], late) {
                (0, true) => self.fire("Late")?,
                (1, false) => self.fire("InTime")?,
                _ => {}
            }
        }
        match (needed, self.s["alert"]) {
            (true, 0) => self.fire("Raise"),
            (true, _) => self.fire("Keep"),
            (false, 1) => self.fire("Withdraw"),
            (false, _) => Ok(()),
        }
    }

    /// Session `sid`'s turn break on host `h`: its host types the line its board holds only on a
    /// claim the lease grants (`ask::at_break`); the line's cooldown starts.
    fn turn_break(&mut self, h: usize) -> Result<(), String> {
        let Some(sid) = self.owed[h].take() else {
            return Ok(());
        };
        let host = self.hosts[h].clone();
        let (now, free) = (self.now, self.free);
        let typed = ask::transact(&self.dir.0, &ask::LOCK_LADDER, |l| {
            match ask::at_break(l, KEY, &host, &sid, now, false) {
                Break::Yours => Some(ask::typed(l, KEY, &host, &sid, now, free)),
                Break::Gone | Break::Released(_) => None,
            }
        })
        .expect("the lease is written")
        .expect("one host at a time");
        self.trail
            .push(format!("host {h} turn break @{sid}: typed={typed:?}"));
        if typed.is_some() && self.s["lcool"] == 0 {
            self.fire("Type")?;
        }
        Ok(())
    }

    fn fired(&self, action: &str) -> usize {
        let line = format!("model {action}");
        self.trail.iter().filter(|t| **t == line).count()
    }
}

/// The model's `ep` of the lease's entry.
fn ep_of(e: Option<&Entry>) -> i64 {
    match e.map(|e| &e.phase) {
        None | Some(Phase::Idle) => 0,
        Some(Phase::Nominating { .. }) => 1,
        Some(Phase::Owed { .. } | Phase::Asked { .. }) => 2,
        Some(Phase::Out { .. }) => 3,
        Some(Phase::Settled { .. }) => 4,
    }
}

/// The model's `wk` of the lease's worker record.
fn wk_of(e: Option<&Entry>) -> i64 {
    match e.and_then(|e| e.worker.as_ref()).map(|w| &w.state) {
        None => 0,
        Some(WorkerState::Claimed) => 1,
        Some(WorkerState::Running { .. }) => 2,
        Some(WorkerState::Over { .. }) => 3,
    }
}

/// Whether the lease's round waits at `now` only on held sessions (design §2): its time is over,
/// no nominee is left unpicked, and a held session not picked is recorded.
fn waits(e: Option<&Entry>, now: i64) -> bool {
    let Some(e) = e else {
        return false;
    };
    let picked = |sid: &str| e.picks.iter().any(|p| p.sid == sid);
    match &e.phase {
        Phase::Nominating {
            until_ms, nominees, ..
        } => {
            now >= *until_ms
                && nominees.iter().all(|n| picked(&n.sid))
                && e.held.iter().any(|h| !picked(&h.sid))
        }
        _ => false,
    }
}

/// The model's `need` of the lease (design §8.4): the episode is out; its round waits only on
/// held sessions or follows a pick that freed nothing or never took its line; it settled while
/// a crossing is projected; or none is open and the line cooldown stands.
fn need_of(e: Option<&Entry>, now: i64, crossing: bool) -> i64 {
    let line_cool = e
        .and_then(|e| e.last_ask_ms)
        .is_some_and(|at| (0..ask::COOLDOWN_MS).contains(&(now - at)));
    let due = match e.map(|e| &e.phase) {
        None | Some(Phase::Idle) => line_cool,
        Some(Phase::Nominating { after, .. }) => {
            waits(e, now) || matches!(after, Some(Miss::NothingFreed | Miss::Undelivered))
        }
        Some(Phase::Owed { .. } | Phase::Asked { .. }) => false,
        Some(Phase::Out { .. }) => true,
        Some(Phase::Settled { .. }) => crossing,
    };
    i64::from(due)
}

/// The window a step of `e`'s episode has at `now` (design §8.3): a third of the least time a
/// recorded crossing leaves, at least five minutes and at most `cap`.
fn window(e: &Entry, now: i64, cap: i64) -> i64 {
    e.projected
        .iter()
        .filter(|p| now - p.at_ms < ask::COLD_HOLD_MS)
        .map(|p| (p.eta_ms - (now - p.at_ms)).max(0))
        .min()
        .map_or(cap, |left| {
            (left / 3).clamp(ask::FLOOR_WINDOW_MS.min(cap), cap)
        })
}

/// When the step in hand ends (design §8.5): a line owed (its delivery, then its answer) or out
/// (its answer), a worker claimed (its spawn, then its run) or running (its run).
fn in_hand_end(e: &Entry, now: i64) -> Option<i64> {
    let line = match &e.phase {
        Phase::Owed { until_ms, .. } => Some(until_ms + window(e, now, ask::ANSWER_MS)),
        Phase::Asked { until_ms, .. } => Some(*until_ms),
        _ => None,
    };
    let worker = e.worker.as_ref().and_then(|w| match &w.state {
        WorkerState::Claimed => Some(w.at_ms + ask::SPAWN_MS + ask::WORKER_MS),
        WorkerState::Running { started_ms, .. } => Some(started_ms + ask::WORKER_MS),
        WorkerState::Over { .. } => None,
    });
    line.max(worker)
}

/// An episode opened by the first host with no session on either host to ask, run out at the
/// second host's look past the round's time (it cannot start a worker, so the worker is due
/// there and claimed at the first host's next look).
fn opened_out(w: &mut World) -> Result<(), String> {
    w.to(SHORT);
    w.spend(true);
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS);
    w.look(1)?;
    w.wait(60_000);
    w.look(0)
}

/// Out → claim → start → finished; the alert while it works and after; then, in the next
/// episode, no second claim within the worker cooldown, and one past it.
fn out_then_finished(w: &mut World) -> Result<(), String> {
    opened_out(w)?;
    w.wait(60_000);
    w.look(1)?;
    w.look(0)?;
    w.wait(10 * 60_000);
    w.look(0)?;
    w.worker_ends(TaskSeen::Ended)?;
    w.look(1)?;
    w.look(0)?;
    w.wait(60_000);
    w.look(0)?;
    w.to(CLEAR);
    w.look(0)?;
    w.to(SHORT);
    w.look(0)?;
    w.wait(ask::NOMINATE_MS);
    w.look(0)?;
    w.look(1)?;
    let cooled = w.claimed_at.expect("claimed") + ask::WORKER_COOLDOWN_MS + 60_000;
    w.wait_until(cooled);
    w.look(0)
}

#[test]
fn an_out_episode_claims_one_worker_that_starts_and_finishes() {
    let mut w = World::new("out");
    out_then_finished(&mut w).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(w.fired("Claim"), 2, "one an episode: {:#?}", w.trail);
    for action in [
        "Open",
        "RunOut",
        "Spawn",
        "Start",
        "Stops",
        "Over",
        "Raise",
        "Keep",
        "Withdraw",
        "End",
        "WorkerCools",
    ] {
        assert!(w.fired(action) > 0, "{action}: {:#?}", w.trail);
    }
    assert!(
        w.trail
            .iter()
            .any(|t| t.contains("Over") && t.contains("finished")),
        "{:#?}",
        w.trail
    );
}

/// A round waiting only on a held session claims the worker; past its wall the session is
/// nominated, but the round elects nobody while the worker works, and elects it once the worker
/// stalled and is over.
fn held_round(w: &mut World) -> Result<(), String> {
    w.seats[0] = vec![("s-a", 30)];
    w.held = vec!["s-a"];
    w.to(SHORT);
    w.spend(true);
    w.look(0)?;
    w.wait(ask::NOMINATE_MS);
    w.look(0)?;
    w.held.clear();
    w.wait(60_000);
    w.look(0)?;
    w.wait(60_000);
    w.look(0)?;
    w.worker_ends(TaskSeen::Stalled("sign-in".to_owned()))?;
    w.look(0)?;
    w.wait(60_000);
    w.look(0)?;
    w.turn_break(0)?;
    w.frees(2);
    w.look(0)
}

#[test]
fn a_round_waiting_on_held_sessions_claims_the_worker_and_waits_on_it() {
    let mut w = World::new("held");
    held_round(&mut w).unwrap_or_else(|e| panic!("{e}"));
    for action in [
        "Wait", "Unwait", "Undue", "Claim", "Over", "Elect", "Type", "Settle",
    ] {
        assert!(w.fired(action) > 0, "{action}: {:#?}", w.trail);
    }
    assert!(
        w.trail.iter().any(|t| t.contains("WorkerRuns")),
        "the round waited on the worker: {:#?}",
        w.trail
    );
}

/// The asked session frees nothing: the round after its hand-off claims the worker, which works
/// past its time and expires; the round then asks the second host's session, which frees nothing
/// either, and the episode is out. Past the worker cooldown, in the same episode, no second
/// worker is claimed.
fn nothing_freed(w: &mut World) -> Result<(), String> {
    w.seats = [vec![("s-a", 30)], vec![("s-c", 12)]];
    w.to(SHORT);
    w.spend(true);
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS);
    w.look(0)?;
    w.turn_break(0)?;
    w.wait(ask::ANSWER_MS);
    w.look(1)?;
    w.look(0)?;
    w.wait(60_000);
    w.look(0)?;
    w.wait(ask::NOMINATE_MS);
    w.look(1)?;
    w.wait(ask::WORKER_MS);
    w.look(0)?;
    w.look(1)?;
    w.turn_break(1)?;
    w.wait(ask::ANSWER_MS);
    w.look(0)?;
    let cooled = w.claimed_at.expect("claimed") + ask::WORKER_COOLDOWN_MS + 60_000;
    w.wait_until(cooled);
    w.look(0)?;
    w.look(1)
}

#[test]
fn a_hand_off_after_nothing_freed_claims_the_worker_and_its_time_ends_it() {
    let mut w = World::new("nothing-freed");
    nothing_freed(&mut w).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(w.fired("Claim"), 1, "{:#?}", w.trail);
    for action in [
        "HandOffMissed",
        "Expire",
        "Elect",
        "RunOut",
        "Raise",
        "Keep",
    ] {
        assert!(w.fired(action) > 0, "{action}: {:#?}", w.trail);
    }
    assert_eq!(w.s["ep"], 3, "the episode stays out: {:?}", w.s);
}

/// A projected episode: the asked session frees 2 GiB, which settles nothing while the crossing
/// stands and its answer window runs; at its end the episode settles and the worker is due.
/// A fill that speeds up past the worker's window needs the owner while it works; once it slows
/// the alert is withdrawn; once the worker is over and nothing can act, raised again; and once
/// the fill stops, the episode ends.
fn settled_crossing(w: &mut World) -> Result<(), String> {
    w.seats[0] = vec![("s-a", 30)];
    w.to(CLEAR);
    w.fills(Some(SLOW));
    w.spend(true);
    w.look(0)?;
    w.look(1)?;
    w.wait(ask::NOMINATE_MS);
    w.look(0)?;
    w.turn_break(0)?;
    w.frees(2);
    w.wait(60_000);
    w.look(0)?;
    let until = match w.lease().entry(KEY).map(|e| e.phase.clone()) {
        Some(Phase::Asked { until_ms, .. }) => until_ms,
        other => return Err(w.fail(format!("the rise settled before its window: {other:?}"))),
    };
    w.wait_until(until);
    w.look(0)?;
    w.wait(60_000);
    w.look(0)?;
    w.fills(Some(FAST));
    w.look(0)?;
    w.fills(Some(SLOW));
    w.wait(60_000);
    w.look(0)?;
    w.worker_ends(TaskSeen::Ended)?;
    w.look(0)?;
    w.fills(None);
    w.wait(60_000);
    w.look(0)?;
    // The second host's projection record holds the episode open until its own look drops it.
    w.look(1)?;
    w.look(0)
}

#[test]
fn an_episode_settled_while_the_crossing_stands_claims_the_worker() {
    let mut w = World::new("settled");
    settled_crossing(&mut w).unwrap_or_else(|e| panic!("{e}"));
    for action in [
        "Cross", "Settle", "Claim", "Late", "InTime", "Raise", "Withdraw", "End",
    ] {
        assert!(w.fired(action) > 0, "{action}: {:#?}", w.trail);
    }
}

/// An episode settles and ends; the volume is short again within the line cooldown, so no
/// episode can open: once aterm's freeing is done the worker is claimed with none open. It
/// finishes; past the line cooldown an episode opens and runs out, which its own worker answers
/// once the worker cooldown is over.
fn line_cooldown(w: &mut World) -> Result<(), String> {
    w.seats[0] = vec![("s-a", 30)];
    w.to(SHORT);
    w.spend(true);
    w.look(0)?;
    w.wait(ask::NOMINATE_MS);
    w.look(0)?;
    w.turn_break(0)?;
    let line = w.now;
    w.frees(2);
    w.look(0)?;
    w.to(CLEAR);
    w.look(0)?;
    w.wait(3_600_000);
    w.spend(false);
    w.to(SHORT);
    w.look(0)?;
    w.spend(true);
    w.wait(60_000);
    w.look(0)?;
    w.wait(60_000);
    w.look(0)?;
    w.worker_ends(TaskSeen::Ended)?;
    w.wait(60_000);
    w.look(0)?;
    w.seats[0].clear();
    w.wait_until(line + ask::COOLDOWN_MS);
    w.look(0)?;
    w.wait(ask::NOMINATE_MS);
    w.look(0)?;
    let cooled = w.claimed_at.expect("claimed") + ask::WORKER_COOLDOWN_MS + 60_000;
    w.wait_until(cooled);
    w.look(0)
}

#[test]
fn the_line_cooldown_with_no_episode_open_claims_the_worker() {
    let mut w = World::new("line-cooldown");
    line_cooldown(&mut w).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(w.fired("Claim"), 2, "{:#?}", w.trail);
    assert!(
        w.trail
            .iter()
            .any(|t| t.contains("Cooldown") && t.contains("Claimed")),
        "{:#?}",
        w.trail
    );
    for action in [
        "Unspend",
        "LineCools",
        "Open",
        "RunOut",
        "Raise",
        "Withdraw",
    ] {
        assert!(w.fired(action) > 0, "{action}: {:#?}", w.trail);
    }
}

/// A claim whose spawn says nothing is lost `SPAWN_MS` after it; one whose spawn failed is over
/// at its host's next look; neither is claimed again that episode.
fn unstarted(w: &mut World, plan: Plan) -> Result<(), String> {
    w.plan = plan;
    opened_out(w)?;
    w.wait(60_000);
    w.look(0)?;
    w.wait(ask::SPAWN_MS);
    w.look(0)?;
    w.look(1)
}

#[test]
fn a_claim_whose_spawn_says_nothing_is_lost_and_one_that_failed_is_over() {
    for (plan, why) in [
        (Plan::Silent, "\"lost\""),
        (Plan::Fail("no-window"), "\"spawn:no-window\""),
    ] {
        let mut w = World::new("unstarted");
        unstarted(&mut w, plan).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            (w.fired("Claim"), w.fired("Unstarted"), w.fired("Spawn")),
            (1, 1, 0),
            "{:#?}",
            w.trail
        );
        assert!(
            w.trail.iter().any(|t| t.contains(why)),
            "{why}: {:#?}",
            w.trail
        );
        assert_eq!(w.s["alert"], 1, "nothing aterm runs can act: {:?}", w.s);
    }
}

/// The claimant ends before its spawn said anything, with no successor: another host's look
/// reads the claim lost `ADOPT_MS` after it first read that host ended.
fn claimant_ended(w: &mut World) -> Result<(), String> {
    w.plan = Plan::Silent;
    opened_out(w)?;
    w.end_host(0, false)?;
    w.wait(60_000);
    w.look(1)?;
    w.wait(ask::ADOPT_MS);
    w.look(1)
}

/// The worker's host ends while it runs: a successor that runs its session adopts it, and the
/// other host's looks leave it running; with no successor its session ends with it, and the other
/// host reads it gone `ADOPT_MS` later.
fn host_ended_running(w: &mut World, successor: bool) -> Result<(), String> {
    opened_out(w)?;
    w.wait(60_000);
    w.look(0)?;
    w.end_host(0, successor)?;
    w.look(1)?;
    w.wait(60_000);
    if successor {
        w.look(0)?;
    }
    w.wait(ask::ADOPT_MS);
    w.look(1)?;
    if successor {
        w.worker_ends(TaskSeen::Ended)?;
        w.look(0)?;
    }
    Ok(())
}

#[test]
fn a_claimant_that_ended_leaves_its_claim_lost_and_its_run_to_a_successor_or_gone() {
    let mut w = World::new("claimant-ended");
    claimant_ended(&mut w).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(w.fired("Unstarted"), 1, "{:#?}", w.trail);
    assert!(
        w.trail
            .iter()
            .any(|t| t.contains("\"lost\"") && t.contains("here: false")),
        "{:#?}",
        w.trail
    );

    let mut w = World::new("successor");
    host_ended_running(&mut w, true).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        w.trail
            .iter()
            .any(|t| t.contains("\"finished\"") && t.contains("here: true")),
        "the successor ran it to its end: {:#?}",
        w.trail
    );
    assert_eq!(
        w.lease()
            .entry(KEY)
            .and_then(|e| e.worker.clone())
            .map(|w| w.host),
        Some(w.hosts[0].clone()),
        "the record moved to the successor"
    );

    let mut w = World::new("no-successor");
    host_ended_running(&mut w, false).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        w.trail
            .iter()
            .any(|t| t.contains("\"gone\"") && t.contains("here: false")),
        "{:#?}",
        w.trail
    );
}

/// No host can start a worker: once the lease has read one due for `SPAWN_MS`, the owner is
/// needed; a host that can start one then claims it, and the alert is withdrawn.
fn nobody_can_start(w: &mut World) -> Result<(), String> {
    w.able = [false, false];
    w.to(SHORT);
    w.spend(true);
    w.look(0)?;
    w.wait(ask::NOMINATE_MS);
    w.look(0)?;
    w.wait(ask::SPAWN_MS);
    w.look(0)?;
    w.able[1] = true;
    w.wait(60_000);
    w.look(1)?;
    w.look(0)
}

#[test]
fn with_no_host_that_can_start_one_the_owner_is_needed_spawn_ms_after_it_is_due() {
    let mut w = World::new("nobody");
    nobody_can_start(&mut w).unwrap_or_else(|e| panic!("{e}"));
    for action in ["Unable", "Raise", "Able", "Claim", "Withdraw"] {
        assert!(w.fired(action) > 0, "{action}: {:#?}", w.trail);
    }
}

/// A settled episode with no crossing needs the owner; hours later a crossing makes the worker
/// due, and it is claimed and runs; a cooldown after the episode parked it gives way to a new
/// one, which keeps the worker, and whose round waits on it.
fn across_episodes(w: &mut World) -> Result<(), String> {
    w.seats[0] = vec![("s-a", 30)];
    w.to(SHORT);
    w.spend(true);
    w.look(0)?;
    w.wait(ask::NOMINATE_MS);
    w.look(0)?;
    w.turn_break(0)?;
    w.frees(2);
    w.look(0)?;
    let parked = w.now;
    w.wait(ask::COOLDOWN_MS - 10 * 60_000);
    w.fills(Some(SLOW));
    w.look(0)?;
    w.wait(60_000);
    w.look(0)?;
    w.wait_until(parked + ask::COOLDOWN_MS);
    w.look(0)?;
    w.wait(ask::NOMINATE_MS);
    w.look(0)?;
    w.worker_ends(TaskSeen::Ended)?;
    w.look(0)?;
    w.wait(60_000);
    w.look(0)
}

#[test]
fn a_working_worker_crosses_into_the_episode_its_own_gives_way_to() {
    let mut w = World::new("across");
    across_episodes(&mut w).unwrap_or_else(|e| panic!("{e}"));
    for action in ["Settle", "Cross", "Claim", "GiveWay", "Over", "Elect"] {
        assert!(w.fired(action) > 0, "{action}: {:#?}", w.trail);
    }
    assert!(
        w.trail.iter().any(|t| t.contains("WorkerRuns")),
        "the new round waited on the worker: {:#?}",
        w.trail
    );
}

/// Every walk together reaches every committed move of the model.
#[test]
fn the_walks_reach_every_committed_move() {
    let mut reached = std::collections::BTreeSet::new();
    let walks: [(&str, Walk); 10] = [
        ("out", out_then_finished),
        ("held", held_round),
        ("nothing-freed", nothing_freed),
        ("settled", settled_crossing),
        ("line-cooldown", line_cooldown),
        ("lost", |w| unstarted(w, Plan::Silent)),
        ("claimant-ended", claimant_ended),
        ("successor", |w| host_ended_running(w, true)),
        ("nobody", nobody_can_start),
        ("across", across_episodes),
    ];
    for (name, walk) in walks {
        let mut w = World::new(name);
        walk(&mut w).unwrap_or_else(|e| panic!("{name}: {e}"));
        reached.extend(
            w.trail
                .iter()
                .filter_map(|t| t.strip_prefix("model "))
                .map(str::to_owned),
        );
    }
    let mutants = [
        "ClaimTwice",
        "ClaimUndue",
        "ClaimBeforeDone",
        "ClaimInCooldown",
        "LineWhileWorker",
        "LineAfterOver",
        "AlertWhileWorker",
        "AlertStays",
    ];
    // Moves of the model's own world no walk needs: the volume never steps down from a crossing
    // to a plain short here, and a round after a released pick is the ask's conformance's.
    let unwalked = ["Uncross", "HandOff"];
    for action in disk_free_worker_model().actions.iter().map(|a| a.name) {
        if mutants.contains(&action) || unwalked.contains(&action) {
            continue;
        }
        assert!(
            reached.contains(action),
            "no walk reached {action}: {reached:?}"
        );
    }
}

/// NEGATIVE CONTROL: a worker step that claims after one is over this episode
/// (`Fault::WorkerTwice`) claims a second once the worker cooldown is over, in the same out
/// episode, and the checker rejects it.
#[test]
fn a_second_worker_in_one_episode_is_rejected() {
    let mut w = World::new("twice");
    let caught = ask::fault::with(ask::fault::Fault::WorkerTwice, || nothing_freed(&mut w))
        .expect_err("the checker must reject a second worker in one episode");
    assert!(caught.contains("`Claim` is not enabled"), "{caught}");
}

/// NEGATIVE CONTROL: a worker step that claims at any pressed, done look (`Fault::WorkerUndue`)
/// claims while a round still gathers nominees, and the checker rejects it.
#[test]
fn a_worker_claimed_when_none_is_due_is_rejected() {
    let mut w = World::new("undue");
    let caught = ask::fault::with(ask::fault::Fault::WorkerUndue, || nothing_freed(&mut w))
        .expect_err("the checker must reject a worker that is not due");
    assert!(caught.contains("`Claim` is not enabled"), "{caught}");
}

/// NEGATIVE CONTROL: a worker step that claims while aterm may still free space
/// (`Fault::WorkerBeforeDone`) claims at the line cooldown's look before the pass ran out, and
/// the checker rejects it.
#[test]
fn a_worker_claimed_before_aterm_is_done_is_rejected() {
    let mut w = World::new("before-done");
    let caught = ask::fault::with(ask::fault::Fault::WorkerBeforeDone, || {
        line_cooldown(&mut w)
    })
    .expect_err("the checker must reject a worker before aterm's freeing is done");
    assert!(caught.contains("`Claim` is not enabled"), "{caught}");
}

/// NEGATIVE CONTROL: a worker step that claims within the worker cooldown
/// (`Fault::WorkerInCooldown`) claims in the episode after the first worker's, and the checker
/// rejects it.
#[test]
fn a_worker_claimed_within_the_cooldown_is_rejected() {
    let mut w = World::new("in-cooldown");
    let caught = ask::fault::with(ask::fault::Fault::WorkerInCooldown, || {
        out_then_finished(&mut w)
    })
    .expect_err("the checker must reject a worker within the worker cooldown");
    assert!(caught.contains("`Claim` is not enabled"), "{caught}");
}

/// NEGATIVE CONTROL: a step that elects while the worker works (`Fault::LineWhileWorker`)
/// elects the session past its wall while the worker is claimed, and the checker rejects it.
#[test]
fn a_line_elected_while_the_worker_works_is_rejected() {
    let mut w = World::new("line-while");
    let caught = ask::fault::with(ask::fault::Fault::LineWhileWorker, || held_round(&mut w))
        .expect_err("the checker must reject a line while the worker works");
    assert!(caught.contains("`Elect` is not enabled"), "{caught}");
}

/// NEGATIVE CONTROL: a close that drops a worker that works (`Fault::ForgetWorkerAtClose`)
/// forgets the running worker as the parked episode gives way to a new one, and the checker
/// rejects the lease it leaves.
#[test]
fn a_close_that_forgets_a_working_worker_is_rejected() {
    let mut w = World::new("forget");
    let caught = ask::fault::with(ask::fault::Fault::ForgetWorkerAtClose, || {
        across_episodes(&mut w)
    })
    .expect_err("the checker must reject a worker forgotten while it works");
    assert!(
        caught.contains("the lease reads wk = 0, the model 2"),
        "{caught}"
    );
}

/// NEGATIVE CONTROL: an owner check that reads no worker in hand (`Fault::AlertWhileWorker`)
/// alerts the owner as the worker is claimed for an out episode, and the checker rejects it.
#[test]
fn an_alert_while_the_worker_works_is_rejected() {
    let mut w = World::new("alert-while");
    let caught = ask::fault::with(ask::fault::Fault::AlertWhileWorker, || {
        out_then_finished(&mut w)
    })
    .expect_err("the checker must reject an alert while the worker works in time");
    assert!(caught.contains("`Raise` is not enabled"), "{caught}");
}
