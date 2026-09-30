// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 of the PTY keeper's two machines (P2 of
//! `docs/DESIGN-pty-keeper-2026-09-26.md` §6.4; the design names
//! `tests/conformance_custody.rs` — it is a unit-test module instead, as P1's
//! `crash_journal_conformance.rs` is, so the `#[refines]` anchors it checks
//! are compiled in: `cfg(test)` turns them on, an integration test's build of
//! the library does not): the REAL `KeeperCore` and
//! `RelaunchBrake` against `PtyKeeperCustody` and `KeeperRelaunchBrake`
//! (`aterm_spec::derive`), and the §5.4 classifier over real wait statuses.
//!
//! CUSTODY. A runner plays the processes around the keeper — the window `w`,
//! its update successor `s`, the window `r` the keeper relaunches — and
//! drives the real core with exactly the events the server would feed it:
//! accept, HELLO, REGISTER (with the descriptor's rdev), RELEASE, BYE,
//! PENDING, EOF, the kernel's exit status, and the clock. The world the core
//! asks about (the holder scan, a shell's and a pid's liveness) is answered
//! from the model state the runner is in. Every bounded sequence of those
//! events is explored; after each, the keeper's own state (`k_holds_*`,
//! `orphan_*`, `k_live`, the relaunch count) is projected and the step is
//! checked as a transition the committed model admits — a clock tick that
//! relaunched is checked as `Relaunch` followed by one `Offer` per master the
//! relaunched window's HELLO was offered. Every reached state satisfies every
//! invariant. Wherever the model FORBIDS a relaunch or an offer, the real
//! core made none; wherever it ALLOWS one after a tick, the core made it.
//!
//! NEGATIVE CONTROL: the same real steps against `Buggy = 1` are rejected at
//! the mutants' sites — a release the buggy keeper forgets, an update's exit
//! the buggy keeper orphans, an offer that moves the copy, a BYE read as a
//! crash.
//!
//! BRAKE. Every schedule of `KeeperRelaunchBrake` is replayed on the real
//! `RelaunchBrake`; the buggy model rejects the real decisions at its mutants.
//!
//! CLASSIFIER. Real children end with `exit(0)`, `exit(74)`, `exit(101)`,
//! `SIGKILL` and `SIGTERM`; the keeper's own exit watch reads each status and
//! the real core judges the window that ended so. (SIGABRT and SIGSEGV are
//! judged from their encodings only: AGENTS.md rule 6.)
//!
//! JUDGEMENT. Every schedule of `KeeperDeathJudgement` — the window's build
//! and end, the evidence degrading, its shell's own end — is driven on the
//! real core: HELLO naming the crash marker, REGISTER, BYE, the EOF, the
//! kernel's status (or none, past the grace), the classifier reading the
//! marker, the orphan's leader watched and its exit pruning the record. The
//! buggy model (the classifier without the marker, the marker trusted alone,
//! the orphan never pruned) rejects real steps at each mutant's site.

use std::collections::{BTreeMap, BTreeSet};

use crate::core::{
    BrakeDecision, ConnId, DeathEvidence, Holders, KeeperCore, KeeperEnv, MarkerEvidence, Out,
    Rdev, RecordState, RelaunchBrake, Verdict, classify_death,
};
use crate::wire::{Birth, CAP_LINK, CAP_SENDS_BYE, MarkerRef, MasterHeader, PeerClass};
use aterm_spec::derive::{
    Model, keeper_death_judgement_model, keeper_relaunch_brake_model, pty_keeper_custody_model,
};
use aterm_spec::interp::{self, State};

const A: Rdev = 0x0f00_0010;
const B: Rdev = 0x0f00_0011;
const SHELL_A: u32 = 9_100;
const SHELL_B: u32 = 9_101;
const TICK_MS: u64 = 61_000;

fn header(rdev: Rdev) -> MasterHeader {
    MasterHeader {
        rdev,
        shell_pid: if rdev == A { SHELL_A } else { SHELL_B },
        shell_birth: Birth::default(),
        local_id: rdev,
    }
}

/// The world, answered from a model state.
struct World<'a> {
    st: &'a State,
    pids: &'a Pids,
}

#[derive(Clone, Debug)]
struct Pids {
    w: u32,
    s: u32,
    r: u32,
}

impl KeeperEnv for World<'_> {
    fn holders(&mut self, rdev: Rdev) -> Holders {
        let x = if rdev == A { "a" } else { "b" };
        let mut held = Vec::new();
        for (who, pid) in [("w", self.pids.w), ("s", self.pids.s), ("r", self.pids.r)] {
            if self.st[format!("{who}_live").as_str()] == 1
                && self.st[format!("{who}_holds_{x}").as_str()] == 1
            {
                held.push(pid);
            }
        }
        if held.is_empty() {
            Holders::None
        } else {
            Holders::Held(held)
        }
    }
    fn shell_alive(&mut self, pid: u32, _birth: Birth) -> bool {
        let x = if pid == SHELL_A { "a" } else { "b" };
        self.st[format!("closed_{x}").as_str()] == 0
    }
    fn pid_alive(&mut self, pid: u32, _birth: Option<Birth>) -> bool {
        [("w", self.pids.w), ("s", self.pids.s), ("r", self.pids.r)]
            .iter()
            .any(|(who, p)| *p == pid && self.st[format!("{who}_live").as_str()] == 1)
    }
}

/// The runner: the real core (None while the keeper is down), the model state
/// it is in, and which connection is which process.
#[derive(Clone)]
struct Rig {
    core: Option<KeeperCore>,
    st: State,
    pids: Pids,
    now: u64,
    next_conn: ConnId,
    /// Connection of each live, connected role in the current core.
    conn: BTreeMap<&'static str, ConnId>,
    /// A quit whose BYE the live keeper read, whose EOF and exit are still to
    /// be delivered (`KeeperHonoursBye`).
    quit_pending: bool,
    next_pid: u32,
}

impl Rig {
    fn new(model: &Model) -> Self {
        let mut rig = Rig {
            core: Some(KeeperCore::new(true)),
            st: model.init_state(),
            pids: Pids {
                w: 5_000,
                s: 6_000,
                r: 7_000,
            },
            now: 0,
            next_conn: 1,
            conn: BTreeMap::new(),
            quit_pending: false,
            next_pid: 7_001,
        };
        let st = rig.st.clone();
        rig.connect("w", 0, &st);
        rig
    }

    fn connect(&mut self, role: &'static str, caps: u8, env_st: &State) -> Vec<Out> {
        let pid = self.pid_of(role);
        let id = self.next_conn;
        self.next_conn += 1;
        let Some(core) = self.core.as_mut() else {
            return Vec::new();
        };
        assert!(core.accept(id, pid, None));
        self.conn.insert(role, id);
        let mut env = World {
            st: env_st,
            pids: &self.pids,
        };
        core.hello(id, PeerClass::App, CAP_SENDS_BYE | caps, self.now, &mut env)
    }

    fn pid_of(&self, role: &str) -> u32 {
        match role {
            // The outgoing window after Commit is the same process.
            "w" | "o" => self.pids.w,
            "s" => self.pids.s,
            _ => self.pids.r,
        }
    }

    /// Deliver a peer's death: its EOF and the kernel's status.
    fn die(&mut self, role: &'static str, status: i32, env_st: &State) -> Vec<Out> {
        let pid = self.pid_of(role);
        let conn = self.conn.remove(role);
        let now = self.now;
        let Some(core) = self.core.as_mut() else {
            return Vec::new();
        };
        let mut env = World {
            st: env_st,
            pids: &self.pids,
        };
        let mut out = Vec::new();
        if let Some(c) = conn {
            out.extend(core.eof(c, now, &mut env));
        }
        out.extend(core.peer_exit(pid, status, now, &mut env));
        out
    }

    fn register(&mut self, role: &'static str, rdev: Rdev, env_st: &State) -> Vec<Out> {
        let Some(conn) = self.conn.get(role).copied() else {
            return Vec::new();
        };
        let Some(core) = self.core.as_mut() else {
            return Vec::new();
        };
        let _ = env_st;
        core.register(conn, header(rdev), b"tag".to_vec(), rdev)
    }

    /// The keeper's variables of the model, from the real core.
    fn project(&self, env: &State) -> State {
        let mut st = env.clone();
        match &self.core {
            None => {
                for v in ["k_holds_a", "k_holds_b", "orphan_a", "orphan_b"] {
                    st.insert(v, 0);
                }
                st.insert("k_live", 0);
            }
            Some(core) => {
                let view = core.custody_view(A, B);
                st.insert("k_live", 1);
                st.insert("k_holds_a", i64::from(view.holds_a));
                st.insert("k_holds_b", i64::from(view.holds_b));
                st.insert("orphan_a", i64::from(view.orphan_a));
                st.insert("orphan_b", i64::from(view.orphan_b));
            }
        }
        st
    }

    /// A shape of the real core for de-duplicating the exploration.
    fn shape(&self) -> String {
        let Some(core) = &self.core else {
            return "down".to_string();
        };
        let role_of = |c: &ConnId| {
            self.conn
                .iter()
                .find(|(_, v)| *v == c)
                .map_or("gone", |(k, _)| *k)
        };
        let recs: Vec<String> = core
            .records()
            .iter()
            .map(|(r, rec)| {
                let claim: Vec<&str> = rec.claimants.iter().map(role_of).collect();
                let state = match &rec.state {
                    RecordState::Claimed => "c".to_string(),
                    RecordState::Held { .. } => "h".to_string(),
                    RecordState::Orphaned => "o".to_string(),
                    RecordState::Offered { to } => format!("f{}", role_of(to)),
                };
                format!("{r:x}{state}{claim:?}")
            })
            .collect();
        format!(
            "{recs:?} {:?} {} {} {:?} {}",
            std::mem::discriminant(&core.relaunch_state()),
            core.brake().streak(),
            core.brake().held(),
            core.pending_successor().map(|p| p.pid),
            self.quit_pending
        )
    }
}

type Step = (State, &'static str, State);

/// Environment moves the explorer may take (the keeper's own moves come only
/// from a clock tick or a HELLO).
const ENV_MOVES: &[&str] = &[
    "RegisterA",
    "RegisterB",
    "LinkDrops",
    "CloseTabA",
    "CloseTabB",
    "Quit",
    "KeeperHonoursBye",
    "WindowCrashes",
    "StartSuccessor",
    "Commit",
    "OutgoingReleasesA",
    "SuccessorClosesTabA",
    "OutgoingDrained",
    "Rollback",
    "SuccessorFailStops",
    "SuccessorCrashes",
    "AdoptA",
    "AdoptB",
    "RelaunchedLinkDrops",
    "RelaunchedWindowQuits",
    "RelaunchedWindowCrashes",
    "KeeperCrashes",
    "KeeperRestarts",
];

/// Apply one environment move to the rig; returns the model steps it made.
fn env_move(model: &Model, rig: &mut Rig, action: &'static str) -> Vec<Step> {
    let before = rig.st.clone();
    let mut next = before.clone();
    assert!(model.fire(action, &mut next), "{action} was enabled");
    let k_live = rig.core.is_some();
    match action {
        "RegisterA" | "RegisterB" => {
            let (rdev, x) = if action == "RegisterA" {
                (A, "a")
            } else {
                (B, "b")
            };
            let role = if before["w_live"] == 1 && before[format!("w_holds_{x}").as_str()] == 1 {
                "w"
            } else if before["s_live"] == 1
                && before["committed"] == 1
                && before[format!("s_holds_{x}").as_str()] == 1
            {
                "s"
            } else {
                "r"
            };
            let outs = rig.register(role, rdev, &next);
            assert_eq!(outs, vec![Out::Keep(rdev)], "{action} by {role}");
        }
        "CloseTabA" | "CloseTabB" => {
            let (rdev, x) = if action == "CloseTabA" {
                (A, "a")
            } else {
                (B, "b")
            };
            let role = if before["w_live"] == 1 && before[format!("w_reads_{x}").as_str()] == 1 {
                "w"
            } else {
                "r"
            };
            if let (Some(core), Some(conn)) = (rig.core.as_mut(), rig.conn.get(role).copied()) {
                let _ = core.release(conn, rdev);
            }
        }
        "LinkDrops" => {
            // The window's link reads EOF while the window lives, and its link
            // reconnects (CAP_LINK) and registers again what the keeper held.
            let now = rig.now;
            let pids = rig.pids.clone();
            if let (Some(core), Some(conn)) = (rig.core.as_mut(), rig.conn.remove("w")) {
                let mut env = World {
                    st: &next,
                    pids: &pids,
                };
                assert!(
                    core.eof(conn, now, &mut env).is_empty(),
                    "an EOF alone decides nothing"
                );
                let held: Vec<Rdev> = [A, B]
                    .into_iter()
                    .filter(|r| {
                        core.records()
                            .get(r)
                            .is_some_and(|rec| rec.claimants.contains(&conn))
                    })
                    .collect();
                let outs = rig.connect("w", CAP_LINK, &next);
                assert_eq!(
                    outs,
                    vec![Out::Welcome {
                        conn: rig.conn["w"],
                        offers: 0
                    }]
                );
                for rdev in held {
                    let outs = rig.register("w", rdev, &next);
                    assert_eq!(outs, vec![Out::DropDuplicate(rdev)], "the claim moves");
                }
            }
        }
        "Quit" => {
            if let (Some(core), Some(conn)) = (rig.core.as_mut(), rig.conn.get("w").copied()) {
                core.bye(conn);
                rig.quit_pending = true;
            }
        }
        "KeeperHonoursBye" => {
            assert!(rig.quit_pending, "only a quit the keeper read is honoured");
            rig.quit_pending = false;
            rig.die("w", 0, &next);
        }
        "WindowCrashes" => {
            rig.die("w", 9, &next);
        }
        "StartSuccessor" => {
            rig.pids.s = rig.next_pid;
            rig.next_pid += 1;
            let now = rig.now;
            let s = rig.pids.s;
            if let (Some(core), Some(conn)) = (rig.core.as_mut(), rig.conn.get("w").copied()) {
                assert_eq!(core.pending(conn, s, now, None), vec![Out::WatchPid(s)]);
            }
        }
        "Commit" => {
            // The successor says HELLO and registers what the keeper holds
            // (beside its Commit); the outgoing window `_exit`s, but its
            // stream — late frames, EOF, exit status — is read later
            // (`OutgoingDrained`), so until then it is a second claimant.
            if let Some(conn) = rig.conn.remove("w") {
                rig.conn.insert("o", conn);
            }
            if k_live {
                rig.connect("s", 0, &next);
                for (rdev, x) in [(A, "a"), (B, "b")] {
                    let held = rig
                        .core
                        .as_ref()
                        .is_some_and(|c| c.records().contains_key(&rdev));
                    if held && next[format!("s_holds_{x}").as_str()] == 1 {
                        let outs = rig.register("s", rdev, &next);
                        assert_eq!(outs, vec![Out::DropDuplicate(rdev)], "the claim is shared");
                    }
                }
            }
        }
        "OutgoingReleasesA" | "SuccessorClosesTabA" => {
            // A RELEASE from one of the two claimants: the outgoing window's
            // late `Session::drop`, or the successor's own tab close.
            let rdev = A;
            let role = if action.starts_with("Outgoing") {
                "o"
            } else {
                "s"
            };
            if let (Some(core), Some(conn)) = (rig.core.as_mut(), rig.conn.get(role).copied()) {
                // A master the keeper never registered (the tab closed with
                // the keeper down) is refused: nothing to release.
                let registered = core.records().contains_key(&rdev);
                let outs = core.release(conn, rdev);
                let closed = outs == vec![Out::Close(rdev)];
                assert!(
                    outs.is_empty() || closed || !registered,
                    "{action}: a release keeps or closes, nothing else: {outs:?}"
                );
            }
        }
        "OutgoingDrained" => {
            // The outgoing window's EOF and `_exit(0)` (no BYE) are read.
            rig.die("o", 0, &next);
        }
        "Rollback" => {
            // The window proves the successor dead: it was SIGKILLed.
            let now = rig.now;
            let s = rig.pids.s;
            let pids = rig.pids.clone();
            if let Some(core) = rig.core.as_mut() {
                let mut env = World {
                    st: &next,
                    pids: &pids,
                };
                core.peer_exit(s, 9, now, &mut env);
            }
        }
        "SuccessorFailStops" => {
            let now = rig.now;
            let s = rig.pids.s;
            let pids = rig.pids.clone();
            if let Some(core) = rig.core.as_mut() {
                let mut env = World {
                    st: &next,
                    pids: &pids,
                };
                core.peer_exit(s, 74 << 8, now, &mut env);
            }
        }
        "SuccessorCrashes" => {
            rig.die("s", 9, &next);
        }
        "AdoptA" | "AdoptB" => {
            // The relaunched window adopts and REGISTERs (the claim moves).
            let rdev = if action == "AdoptA" { A } else { B };
            // A record its dropped link's EOF returned to Orphaned is claimed
            // the same way: REGISTER proves possession, not the offer.
            let offered = rig.core.as_ref().is_some_and(|c| {
                c.records().get(&rdev).is_some_and(|r| {
                    matches!(
                        r.state,
                        RecordState::Offered { .. } | RecordState::Claimed | RecordState::Orphaned
                    )
                })
            });
            if offered {
                let outs = rig.register("r", rdev, &next);
                assert_eq!(outs, vec![Out::DropDuplicate(rdev)]);
            }
        }
        "RelaunchedLinkDrops" => {
            // The relaunched window's link reads EOF before it registered its
            // offers, and reconnects (CAP_LINK). Every record offered on the
            // dropped link is an orphan again, its leader watched again
            // (round seven's finding 65); the reconnected link is offered
            // nothing, since the window still holds each descriptor.
            let now = rig.now;
            let pids = rig.pids.clone();
            if let (Some(core), Some(conn)) = (rig.core.as_mut(), rig.conn.remove("r")) {
                let offered: Vec<Rdev> = [A, B]
                    .into_iter()
                    .filter(|r| {
                        core.records()
                            .get(r)
                            .is_some_and(|rec| rec.state == RecordState::Offered { to: conn })
                    })
                    .collect();
                assert!(!offered.is_empty(), "{action}: an offer was outstanding");
                let mut env = World {
                    st: &next,
                    pids: &pids,
                };
                let outs = core.eof(conn, now, &mut env);
                let watched: Vec<Out> = offered
                    .iter()
                    .map(|r| Out::WatchPid(header(*r).shell_pid))
                    .collect();
                assert_eq!(outs, watched, "{action}: the offers return, nothing else");
                // What it had registered, its reconnected link registers again
                // (as `LinkDrops`); what it was only offered stays an orphan
                // until it adopts (`AdoptA`).
                let held: Vec<Rdev> = [A, B]
                    .into_iter()
                    .filter(|r| {
                        core.records()
                            .get(r)
                            .is_some_and(|rec| rec.claimants.contains(&conn))
                    })
                    .collect();
                let outs = rig.connect("r", CAP_LINK, &next);
                assert_eq!(
                    outs,
                    vec![Out::Welcome {
                        conn: rig.conn["r"],
                        offers: 0
                    }]
                );
                for rdev in held {
                    let outs = rig.register("r", rdev, &next);
                    assert_eq!(outs, vec![Out::DropDuplicate(rdev)], "the claim moves");
                }
            }
        }
        "RelaunchedWindowQuits" => {
            // BYE on the relaunched window's current link, then its EOF and
            // `exit(0)`: the keeper judges every link of the process at that
            // exit, a dropped one included.
            if let (Some(core), Some(conn)) = (rig.core.as_mut(), rig.conn.get("r").copied()) {
                core.bye(conn);
            }
            rig.die("r", 0, &next);
        }
        "RelaunchedWindowCrashes" => {
            rig.die("r", 9, &next);
        }
        "KeeperCrashes" => {
            rig.core = None;
            rig.conn.clear();
            rig.quit_pending = false;
        }
        "KeeperRestarts" => {
            rig.core = Some(KeeperCore::new(true));
            // Every live window's link reconnects (not a launch); a window
            // with an update in flight names its successor again.
            for role in ["w", "s", "r"] {
                let live = next[format!("{role}_live").as_str()] == 1;
                let connected = match role {
                    "s" => next["committed"] == 1,
                    _ => true,
                };
                if live && connected {
                    let outs = rig.connect(role, CAP_LINK, &next);
                    assert_eq!(
                        outs,
                        vec![Out::Welcome {
                            conn: rig.conn[role],
                            offers: 0
                        }]
                    );
                }
            }
            if next["pending"] == 1 && next["w_live"] == 1 {
                let now = rig.now;
                let s = rig.pids.s;
                let conn = rig.conn["w"];
                if let Some(core) = rig.core.as_mut() {
                    core.pending(conn, s, now, None);
                }
            }
        }
        other => panic!("not an environment move: {other}"),
    }
    let after = rig.project(&next);
    rig.st = after.clone();
    vec![(before, action, after)]
}

/// The clock: relaunch if due, and the relaunched window's HELLO with its
/// offers. Returns the model steps it made (none: a stutter).
fn tick(model: &Model, rig: &mut Rig) -> Vec<Step> {
    rig.now += TICK_MS;
    let now = rig.now;
    let before = rig.st.clone();
    let pids = rig.pids.clone();
    let Some(core) = rig.core.as_mut() else {
        return Vec::new();
    };
    let mut env = World {
        st: &before,
        pids: &pids,
    };
    let outs = core.tick(now, &mut env);
    let relaunched = outs.contains(&Out::Relaunch);
    let offers_before = outs
        .iter()
        .filter(|o| matches!(o, Out::Offer { .. }))
        .count();
    assert_eq!(offers_before, 0, "a tick offers nothing by itself");
    if !relaunched {
        assert_eq!(
            rig.project(&before),
            before,
            "a tick that relaunched nothing changed the keeper's state"
        );
        assert!(
            !model.action_enabled("Relaunch", &before),
            "the model allows a relaunch the real keeper did not make: {before:?}\n{}",
            rig.shape()
        );
        return Vec::new();
    }
    assert!(
        model.action_enabled("Relaunch", &before),
        "the real keeper relaunched where the model forbids it: {before:?}"
    );
    let mut steps = Vec::new();
    let mut st = before.clone();
    assert!(model.fire("Relaunch", &mut st));
    rig.pids.r = rig.next_pid;
    rig.next_pid += 1;
    steps.push((before, "Relaunch", st.clone()));
    // The relaunched window says HELLO: WELCOME and its offers.
    let outs = rig.connect("r", 0, &st);
    for out in &outs {
        if let Out::Offer { rdev, .. } = out {
            let action = if *rdev == A { "OfferA" } else { "OfferB" };
            let prev = st.clone();
            assert!(
                model.fire(action, &mut st),
                "the real keeper offered where the model forbids it: {prev:?}"
            );
            steps.push((prev, action, st.clone()));
        }
    }
    for action in ["OfferA", "OfferB"] {
        assert!(
            !model.action_enabled(action, &st),
            "the model allows {action} the real keeper did not make: {st:?}"
        );
    }
    let after = rig.project(&st);
    assert_eq!(after, st, "the keeper's state after the relaunch's HELLO");
    rig.st = after;
    steps
}

struct Explored {
    steps: BTreeSet<Step>,
    reached: BTreeSet<State>,
    fired: BTreeSet<&'static str>,
    sequences: usize,
}

fn explore(
    model: &Model,
    rig: Rig,
    depth: usize,
    seen: &mut BTreeSet<(State, String, usize)>,
    out: &mut Explored,
) {
    if !seen.insert((rig.st.clone(), rig.shape(), depth)) {
        return;
    }
    out.sequences += 1;
    if depth == 0 {
        return;
    }
    let mut moves: Vec<&'static str> = ENV_MOVES
        .iter()
        .copied()
        .filter(|a| model.action_enabled(a, &rig.st))
        .filter(|a| *a != "KeeperHonoursBye" || rig.quit_pending)
        .collect();
    moves.push("Tick");
    for mv in moves {
        let mut next = rig.clone();
        let steps = if mv == "Tick" {
            tick(model, &mut next)
        } else {
            env_move(model, &mut next, mv)
        };
        for (before, action, after) in steps {
            assert!(
                model.successors(action, &before).contains(&after),
                "{action}: {before:?} -> {after:?} does not conform to PtyKeeperCustody"
            );
            for inv in &model.invariants {
                assert!(
                    model.check_invariant(inv.name, &after),
                    "after {action}: {} on {after:?}",
                    inv.name
                );
            }
            out.fired.insert(action);
            out.reached.insert(after.clone());
            out.steps.insert((before, action, after));
        }
        explore(model, next, depth - 1, seen, out);
    }
}

/// The real keeper refines `PtyKeeperCustody` on every bounded event
/// sequence, and between them the sequences fire every action of the
/// committed model. The negative control: the buggy model rejects real steps
/// at each mutant's site.
#[test]
fn the_real_keeper_conforms_to_the_custody_model() {
    let model = pty_keeper_custody_model();
    let mut out = Explored {
        steps: BTreeSet::new(),
        reached: BTreeSet::new(),
        fired: BTreeSet::new(),
        sequences: 0,
    };
    let mut seen = BTreeSet::new();
    explore(&model, Rig::new(&model), 18, &mut seen, &mut out);
    let every: Vec<&str> = model.actions.iter().map(|a| a.name).collect();
    for action in &every {
        assert!(
            out.fired.contains(action),
            "the exploration never fired {action}: {:?}",
            out.fired
        );
    }
    assert!(
        out.reached.len() > 1200,
        "the exploration reaches the states it exists for: {}",
        out.reached.len()
    );
    // What the exploration does not reach, it cannot reach: every model state
    // it misses is one inside a relaunch's HELLO — the relaunched window alive
    // with an orphan the keeper had not yet offered it, or a keeper that died
    // between the relaunch and the HELLO. The real keeper offers every orphan
    // in the HELLO's one answer, so those interleavings of the model's
    // separate `Relaunch`/`OfferA`/`OfferB` steps never exist in it.
    let mut all: BTreeSet<State> = BTreeSet::from([model.init_state()]);
    let mut queue = vec![model.init_state()];
    while let Some(st) = queue.pop() {
        for a in &model.actions {
            for n in model.successors(a.name, &st) {
                if all.insert(n.clone()) {
                    queue.push(n);
                }
            }
        }
    }
    let missing: Vec<&State> = all.iter().filter(|s| !out.reached.contains(*s)).collect();
    for m in &missing {
        // An orphan the relaunched window still HOLDS is not inside a HELLO:
        // it is the return of an offer whose link dropped, which the real
        // keeper must reach.
        let unoffered = |x: &str| {
            m[format!("orphan_{x}").as_str()] == 1 && m[format!("r_holds_{x}").as_str()] == 0
        };
        let inside_a_hello = m["r_live"] == 1
            && (unoffered("a") || unoffered("b") || m["k_faults"] == 1)
            && m["orphan_a"] * m["r_holds_a"] + m["orphan_b"] * m["r_holds_b"] == 0;
        assert!(
            inside_a_hello,
            "a reachable model state the real keeper never reached: {m:?}"
        );
    }
    eprintln!(
        "custody conformance: {} explored nodes, {} distinct real steps, {} model states",
        out.sequences,
        out.steps.len(),
        out.reached.len()
    );

    // NEGATIVE CONTROL: the buggy machine rejects real steps at each mutant.
    let buggy = interp::with_buggy(&model, 1);
    let mut rejected: BTreeMap<&str, usize> = BTreeMap::new();
    for (before, action, after) in &out.steps {
        if !buggy.successors(action, before).contains(after) {
            *rejected.entry(action).or_default() += 1;
        }
    }
    for site in [
        "CloseTabA",
        "CloseTabB",
        "OutgoingReleasesA",
        "Commit",
        "OfferA",
        "OfferB",
        "KeeperHonoursBye",
        "RelaunchedLinkDrops",
        "RelaunchedWindowQuits",
    ] {
        assert!(
            rejected.get(site).copied().unwrap_or(0) > 0,
            "the buggy model accepts every real {site} step: {rejected:?}"
        );
    }

    // Both tiers, on a sample of distinct real transitions (every action).
    let mut per_action: BTreeMap<&str, usize> = BTreeMap::new();
    for (before, action, after) in &out.steps {
        let n = per_action.entry(action).or_default();
        if *n >= 2 {
            continue;
        }
        *n += 1;
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            before,
            after,
            Some(action),
            "keeper custody conformance",
        );
        assert!(ok, "{action}: {why}");
    }
}

/// The anchors the conformance exercises are compiled in and were ENTERED —
/// the refinements name real functions that ran, not decoration.
#[test]
fn the_custody_anchors_are_live() {
    assert!(
        aterm_spec::xref::reset_entered_anchors(),
        "the evidence window opens"
    );
    let model = pty_keeper_custody_model();
    let mut rig = Rig::new(&model);
    for mv in ["RegisterA", "RegisterB", "WindowCrashes"] {
        env_move(&model, &mut rig, mv);
    }
    tick(&model, &mut rig);
    let _ = classify_death(DeathEvidence {
        successor_holds: false,
        bye: true,
        sends_bye: true,
        status: Some(0),
        marker: MarkerEvidence::Unknown,
    });
    let anchored: BTreeSet<(&str, &str)> = aterm_spec::xref::refinements()
        .filter(|r| r.machine == "PtyKeeperCustody" || r.machine == "KeeperRelaunchBrake")
        .map(|r| (r.machine, r.action))
        .collect();
    for action in [
        "RegisterA",
        "OfferA",
        "Relaunch",
        "WindowCrashes",
        "CloseTabA",
        "KeeperHonoursBye",
        "RelaunchedLinkDrops",
        "RelaunchedWindowQuits",
    ] {
        assert!(
            anchored.contains(&("PtyKeeperCustody", action)),
            "{action}: {anchored:?}"
        );
    }
    let entered = aterm_spec::xref::entered_anchor_ids();
    for id in [
        "PtyKeeperCustody::RegisterA @ register",
        "PtyKeeperCustody::Relaunch @ tick",
        "PtyKeeperCustody::WindowCrashes @ peer_died",
        "PtyKeeperCustody::OfferA @ offers_for",
        "PtyKeeperCustody::KeeperHonoursBye @ classify_death",
    ] {
        assert!(entered.contains(id), "{id} never entered: {entered:?}");
    }
    // KeeperDeathJudgement's two keeper actions: the classifier (entered
    // above) and the prune.
    let judged = keeper_death_judgement_model();
    let st = judged.init_state();
    let mut world = JudgeWorld {
        st: &st,
        alive: false,
    };
    let _ = KeeperCore::new(false).prune_dead_orphans(&mut world);
    let entered = aterm_spec::xref::entered_anchor_ids();
    for id in [
        "KeeperDeathJudgement::Judge @ classify_death",
        "KeeperDeathJudgement::KeeperSeesShellExit @ prune_dead_orphans",
    ] {
        assert!(entered.contains(id), "{id} never entered: {entered:?}");
    }
}

/// Every schedule of `KeeperRelaunchBrake` replayed on the real brake.
#[test]
fn the_real_brake_conforms_to_the_brake_model() {
    let model = keeper_relaunch_brake_model();
    // Every schedule to depth 14: enough for three failed relaunches, a hold,
    // a manual launch and a fresh streak.
    let mut steps: BTreeSet<Step> = BTreeSet::new();
    let mut reached: BTreeSet<State> = BTreeSet::from([model.init_state()]);
    let mut stack: Vec<(State, RelaunchBrake, usize)> =
        vec![(model.init_state(), RelaunchBrake::new(), 0)];
    let mut seen: BTreeSet<(State, usize)> = BTreeSet::new();
    while let Some((st, brake, depth)) = stack.pop() {
        if depth == 14 || !seen.insert((st.clone(), depth)) {
            continue;
        }
        for action in model.actions.iter().map(|a| a.name) {
            if !model.action_enabled(action, &st) {
                continue;
            }
            let mut real = brake.clone();
            let mut next = st.clone();
            match action {
                "PickRepeatedDeaths" => {
                    next.insert("kind", 1);
                }
                "PickBootTrial" => {
                    next.insert("kind", 2);
                }
                "Death" | "RelaunchFails" => {
                    next.insert("phase", 1);
                    next.insert("clean", 0);
                }
                "CleanQuit" => {
                    next.insert("phase", 1);
                    next.insert("clean", 1);
                }
                "Decide" => {
                    let clean = st["clean"] == 1;
                    let decision = real.decide(1_000, clean);
                    let (phase, granted) = match decision {
                        BrakeDecision::Nothing => (0, false),
                        BrakeDecision::RelaunchAt(_) => (2, true),
                        BrakeDecision::Hold => (3, false),
                    };
                    next.insert("phase", phase);
                    if granted {
                        next.insert("grants", st["grants"] + 1);
                        if clean {
                            next.insert("after_quit", 1);
                        }
                    }
                }
                "Healthy" => {
                    real.healthy();
                    next.insert("phase", 0);
                    next.insert("grants", 0);
                }
                "ManualLaunch" => {
                    real.manual_launch();
                    next.insert("phase", 0);
                    next.insert("grants", 0);
                }
                other => panic!("unmapped {other}"),
            }
            next.insert("streak", i64::from(real.streak()));
            next.insert("held", i64::from(real.held()));
            assert!(
                model.successors(action, &st).contains(&next),
                "{action}: {st:?} -> {next:?} does not conform to KeeperRelaunchBrake"
            );
            for inv in &model.invariants {
                assert!(
                    model.check_invariant(inv.name, &next),
                    "{}: {next:?}",
                    inv.name
                );
            }
            reached.insert(next.clone());
            steps.insert((st.clone(), action, next.clone()));
            stack.push((next, real, depth + 1));
        }
    }
    // Every state the model can reach, the real brake reached.
    let all = interp::bmc(&model).expect("the committed brake holds");
    assert_eq!(
        reached.len(),
        all,
        "the real brake reaches every model state"
    );
    // NEGATIVE CONTROL: the buggy brake's reader relaunches after a quit and
    // past the streak; the real steps are rejected there.
    let buggy = interp::with_buggy(&model, 1);
    let rejected = steps
        .iter()
        .filter(|(b, a, n)| *a == "Decide" && !buggy.successors(a, b).contains(n))
        .count();
    assert!(rejected > 0, "the buggy brake accepts every real decision");
}

/// The classifier over real wait statuses, read by the keeper's own exit
/// watch, and the real core's judgement of a window that ended so.
#[test]
fn the_classifier_judges_real_ends() {
    use std::process::{Command, Stdio};
    /// No holder, a live shell, a dead window, and — once `marker` is set —
    /// that window's marker: held while it names it, then what `marker` says.
    struct NoWorld {
        marker: Option<MarkerEvidence>,
        named: bool,
    }
    impl KeeperEnv for NoWorld {
        fn holders(&mut self, _: Rdev) -> Holders {
            Holders::None
        }
        fn shell_alive(&mut self, _: u32, _: Birth) -> bool {
            true
        }
        fn pid_alive(&mut self, _: u32, _: Option<Birth>) -> bool {
            false
        }
        fn marker(&mut self, _: u32, _: &MarkerRef) -> MarkerEvidence {
            if self.named {
                self.marker.unwrap_or(MarkerEvidence::Unknown)
            } else {
                self.named = true;
                MarkerEvidence::Held
            }
        }
    }
    let real_status = |how: &str| -> i32 {
        let mut child = Command::new("/bin/sh")
            .args(["-c", &format!("read _; {how}")])
            .stdin(Stdio::piped())
            .spawn()
            .expect("spawn");
        let watch = aterm_uds::exitwatch::ExitWatch::watch(child.id()).expect("watch");
        drop(child.stdin.take());
        let _ = child.wait();
        let started = std::time::Instant::now();
        loop {
            if let Some(s) = watch.exit_status() {
                return s;
            }
            assert!(started.elapsed().as_secs() < 60, "no status for {how}");
            std::thread::yield_now();
        }
    };
    let cases: Vec<(&str, i32)> = vec![
        ("exit 0", real_status("exit 0")),
        ("exit 74", real_status("exit 74")),
        ("exit 101", real_status("exit 101")),
        ("SIGKILL", real_status("kill -KILL $$")),
        ("SIGTERM", real_status("kill -TERM $$")),
        // Encodings only, never a real core-dump signal (AGENTS.md rule 6).
        ("SIGABRT (encoding)", 6),
        ("SIGSEGV (encoding)", 11),
    ];
    assert_eq!(cases[0].1, 0);
    assert_eq!(cases[3].1, 9);
    use MarkerEvidence::{Dead, Released, Unknown};
    for (name, status) in cases {
        for (sends_bye, bye, marker) in [
            (true, false, Unknown),
            (false, false, Unknown),
            (true, true, Unknown),
            (true, false, Released),
            (true, false, Dead),
            (false, false, Dead),
        ] {
            let verdict = classify_death(DeathEvidence {
                successor_holds: false,
                bye,
                sends_bye,
                status: Some(status),
                marker,
            });
            let expected = if bye {
                Verdict::Quit
            } else if status == 0 && marker == Released {
                Verdict::CleanExit
            } else if !sends_bye && status == 0 && marker != Dead {
                Verdict::CleanPreKeeper
            } else {
                Verdict::Crash
            };
            assert_eq!(
                verdict, expected,
                "{name} sends_bye={sends_bye} bye={bye} marker={marker:?}"
            );
            // The real core judges a window that ended so.
            let mut core = KeeperCore::new(true);
            let mut world = NoWorld {
                marker: None,
                named: false,
            };
            assert!(core.accept(1, 4242, None));
            let caps = if sends_bye { CAP_SENDS_BYE } else { 0 };
            core.hello(1, PeerClass::App, caps, 0, &mut world);
            core.register(1, header(A), vec![], A);
            if bye {
                core.bye(1);
            }
            if marker != Unknown {
                world.marker = Some(marker);
                let named = core.name_marker(
                    1,
                    MarkerRef {
                        nanos: 1,
                        dir: b"/logs".to_vec(),
                    },
                    &mut world,
                );
                assert!(named, "a held marker is kept at HELLO");
            }
            core.eof(1, 1, &mut world);
            core.peer_exit(4242, status, 1, &mut world);
            let orphaned = core.custody_view(A, B).orphan_a;
            let held = core.custody_view(A, B).holds_a;
            match expected {
                Verdict::Crash => assert!(orphaned, "{name}: a crash orphans"),
                _ => assert!(!held, "{name}: a quit or a clean pre-keeper exit closes"),
            }
        }
    }
}

// ------------------------------------------------ the death judgement (P3's limits)

/// The window whose end `KeeperDeathJudgement` judges, and when.
const JUDGED_WINDOW: u32 = 4_343;
const JUDGE_AT: u64 = 1 + crate::core::DEATH_GRACE_MS;

/// The world a judgement asks about, answered from a `KeeperDeathJudgement`
/// state: master `A`'s shell lives while `shell_live`; the window is dead once
/// it ended; its marker says what `marker` says (and HELD while the window
/// lives, `alive`).
struct JudgeWorld<'a> {
    st: &'a State,
    alive: bool,
}

impl KeeperEnv for JudgeWorld<'_> {
    fn holders(&mut self, _: Rdev) -> Holders {
        Holders::None
    }
    fn shell_alive(&mut self, pid: u32, _: Birth) -> bool {
        pid == SHELL_A && self.st["shell_live"] == 1
    }
    fn pid_alive(&mut self, pid: u32, _: Option<Birth>) -> bool {
        self.alive && pid == JUDGED_WINDOW
    }
    fn marker(&mut self, pid: u32, _: &MarkerRef) -> MarkerEvidence {
        assert_eq!(
            pid, JUDGED_WINDOW,
            "the keeper reads only that window's marker"
        );
        if self.alive {
            return MarkerEvidence::Held;
        }
        match self.st["marker"] {
            1 => MarkerEvidence::Released,
            2 => MarkerEvidence::Dead,
            3 => MarkerEvidence::Held,
            _ => MarkerEvidence::Unknown,
        }
    }
}

/// The model's wait status as the kernel encodes it.
fn wait_status(model_status: i64) -> Option<i32> {
    match model_status {
        1 => Some(0),
        2 => Some(101 << 8),
        3 => Some(9),
        _ => None,
    }
}

/// One real step of the judgement runner: the core after it, and the state it
/// projects to. `watching`: the core asked to watch master `A`'s shell.
#[derive(Clone)]
struct JudgeRig {
    core: KeeperCore,
    watching: bool,
}

/// Drive the REAL keeper through `action` from `st`, and project.
fn judge_step(model: &Model, rig: &JudgeRig, st: &State, action: &str) -> (JudgeRig, State) {
    let mut real = rig.clone();
    // The environment's own moves change what the world is; the model says
    // how (each is deterministic).
    let env_next = || {
        let mut succ = model.successors(action, st);
        assert_eq!(succ.len(), 1, "{action} is deterministic");
        succ.remove(0)
    };
    let mut next = st.clone();
    match action {
        "PickP3Window" | "PickPreByeWindow" | "StatusLost" | "MarkerSwept" | "MarkerInherited" => {
            next = env_next()
        }
        "QuitWithBye" | "QuitByeLost" | "Killed" | "PanicExit" => {
            // The window's life, on the real core: connect, HELLO (naming its
            // marker, which the keeper finds held), REGISTER; then its end —
            // the BYE if one arrives, and the connection's EOF.
            next = env_next();
            let core = &mut real.core;
            let mut alive = JudgeWorld { st, alive: true };
            assert!(core.accept(1, JUDGED_WINDOW, None));
            let caps = if st["sends_bye"] == 1 {
                CAP_SENDS_BYE
            } else {
                0
            };
            if st["names_marker"] == 1 {
                let marker = MarkerRef {
                    nanos: 1,
                    dir: b"/logs".to_vec(),
                };
                assert!(
                    core.name_marker(1, marker, &mut alive),
                    "a held marker is kept"
                );
            }
            core.hello(1, PeerClass::App, caps, 0, &mut alive);
            core.register(1, header(A), vec![], A);
            if next["bye"] == 1 {
                core.bye(1);
            }
            let mut dead = JudgeWorld {
                st: &next,
                alive: false,
            };
            let outs = core.eof(1, 1, &mut dead);
            assert!(outs.is_empty(), "an EOF alone judges nothing: {outs:?}");
        }
        "Judge" => {
            let mut world = JudgeWorld { st, alive: false };
            let outs = match wait_status(st["status"]) {
                Some(status) => real
                    .core
                    .peer_exit(JUDGED_WINDOW, status, JUDGE_AT, &mut world),
                // No status: the EOF and a dead pid, after the grace.
                None => real.core.tick(JUDGE_AT, &mut world),
            };
            let verdict = real.core.last_verdict().expect("the end was judged");
            let orphaned = real.core.custody_view(A, B).orphan_a;
            let quit = matches!(
                verdict,
                Verdict::Quit | Verdict::CleanExit | Verdict::CleanPreKeeper
            );
            real.watching = outs.contains(&Out::WatchPid(SHELL_A));
            assert_eq!(
                real.watching, orphaned,
                "an orphan's leader is watched, and only an orphan's: {outs:?}"
            );
            next.insert("judged", 1);
            next.insert("judged_quit", i64::from(quit));
            next.insert("orphaned", i64::from(orphaned));
            next.insert("ever_orphaned", i64::from(orphaned));
        }
        "ShellDies" => {
            next = env_next();
            // The exit is on its way to the keeper only if the keeper watches it.
            next.insert(
                "watch_pending",
                i64::from(real.watching && real.core.custody_view(A, B).orphan_a),
            );
        }
        "KeeperSeesShellExit" => {
            let mut world = JudgeWorld { st, alive: false };
            real.core.peer_exit(SHELL_A, 1, JUDGE_AT + 1, &mut world);
            real.watching = false;
            next.insert("watch_pending", 0);
            next.insert("orphaned", i64::from(real.core.custody_view(A, B).orphan_a));
        }
        other => panic!("unmapped {other}"),
    }
    (real, next)
}

/// Tier-1 of `KeeperDeathJudgement`: every schedule of the machine, driven on
/// the REAL `KeeperCore` — HELLO naming the marker, REGISTER, BYE, the EOF, the
/// kernel's status (or none, past the grace), the classifier reading the
/// marker, the orphan's leader watched and its exit pruning the record — each
/// real step checked as a transition the committed model admits, and every
/// model state reached. NEGATIVE CONTROL: the buggy model (the classifier
/// without the marker, the marker trusted alone, the orphan never pruned)
/// rejects real steps at each mutant's site.
#[test]
fn the_real_keeper_conforms_to_the_death_judgement_model() {
    let model = keeper_death_judgement_model();
    let start = JudgeRig {
        core: KeeperCore::new(false),
        watching: false,
    };
    let mut steps: BTreeSet<Step> = BTreeSet::new();
    let mut reached: BTreeSet<State> = BTreeSet::from([model.init_state()]);
    let mut stack = vec![(model.init_state(), start)];
    while let Some((st, rig)) = stack.pop() {
        for action in model.actions.iter().map(|a| a.name) {
            if !model.action_enabled(action, &st) {
                continue;
            }
            let (real, next) = judge_step(&model, &rig, &st, action);
            assert!(
                model.successors(action, &st).contains(&next),
                "{action}: {st:?} -> {next:?} does not conform to KeeperDeathJudgement"
            );
            for inv in &model.invariants {
                assert!(
                    model.check_invariant(inv.name, &next),
                    "{}: {next:?}",
                    inv.name
                );
            }
            steps.insert((st.clone(), action, next.clone()));
            if reached.insert(next.clone()) {
                stack.push((next, real));
            }
        }
    }
    let all = interp::bmc(&model).expect("the committed judgement holds");
    assert_eq!(
        reached.len(),
        all,
        "the real keeper reaches every model state"
    );
    // NEGATIVE CONTROL, per mutant site.
    let buggy = interp::with_buggy(&model, 1);
    let rejected = |pick: &dyn Fn(&State, &str) -> bool| {
        steps
            .iter()
            .filter(|(b, a, n)| pick(b, a) && !buggy.successors(a, b).contains(n))
            .count()
    };
    assert!(
        rejected(&|b, a| a == "Judge" && b["quit"] == 1) > 0,
        "the classifier without the marker accepts every real judgement of a quit"
    );
    assert!(
        rejected(&|b, a| a == "Judge" && b["quit"] == 0) > 0,
        "the marker trusted alone accepts every real judgement of a crash"
    );
    assert!(
        rejected(&|_, a| a == "KeeperSeesShellExit") > 0,
        "the keeper that never prunes accepts every real prune"
    );
    // Both tiers, on a sample of the real transitions (every action).
    let mut per_action: BTreeMap<&str, usize> = BTreeMap::new();
    for (before, action, after) in &steps {
        let n = per_action.entry(action).or_default();
        if *n >= 2 {
            continue;
        }
        *n += 1;
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            before,
            after,
            Some(action),
            "keeper death judgement conformance",
        );
        assert!(ok, "{action}: {why}");
    }
    eprintln!(
        "death judgement conformance: {} real steps over {} model states",
        steps.len(),
        reached.len()
    );
}
