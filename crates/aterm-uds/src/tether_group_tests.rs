// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`end_group`] and [`linger`] over a scripted kernel: the group a tethered leader leads ends
//! whole, whoever of it the watcher saw, and a group that empties by itself gets no signal.

use super::{GroupControl, GroupEnded, KILL_ROUNDS, UNWATCHED_RECHECK, Woke, end_group, linger};
use std::time::{Duration, Instant};

const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;
const LEADER: i32 = 4242;
/// The watcher's anchor: a zombie in the group, listed and never read.
const ANCHOR: i32 = 4250;

/// One scripted process: its group, and the signals that end it.
#[derive(Clone, Debug)]
struct Proc {
    pid: i32,
    pgid: i32,
    /// The signals that end it (`SIGKILL` ends all but one blocked in the kernel).
    ends_on: &'static [i32],
    /// The script time (naps, waits and signals so far) at which it appears.
    arrives: usize,
    /// The script time at which it exits by itself, if it does.
    leaves: Option<usize>,
    alive: bool,
}

const fn ours(pid: i32, ends_on: &'static [i32]) -> Proc {
    Proc {
        pid,
        pgid: LEADER,
        ends_on,
        arrives: 0,
        leaves: None,
        alive: true,
    }
}

const TERM: &[i32] = &[SIGTERM, SIGKILL];
const KILL: &[i32] = &[SIGKILL];
const NONE: &[i32] = &[];

/// A kernel of scripted processes whose clock moves only by naps and waits.
struct World {
    procs: Vec<Proc>,
    naps: usize,
    clock: Instant,
    unlisted: bool,
    signals: Vec<(i32, i32)>,
    /// The script time at which the parent exits, if it does.
    parent_leaves: Option<usize>,
    /// Each wait's members and patience.
    waits: Vec<(Vec<i32>, Option<Duration>)>,
    /// A member forked as the first `SIGKILL` lands, which XNU's fork lets join unsignalled.
    escapes: Option<i32>,
}

impl World {
    fn new(procs: Vec<Proc>) -> Self {
        Self {
            procs,
            naps: 0,
            clock: Instant::now(),
            unlisted: false,
            signals: Vec::new(),
            parent_leaves: None,
            waits: Vec::new(),
            escapes: None,
        }
    }
    /// Naps, waits and signals so far: the script's time.
    fn events(&self) -> usize {
        self.naps + self.signals.len()
    }
    fn here(&self) -> impl Iterator<Item = &Proc> {
        let events = self.events();
        self.procs.iter().filter(move |p| {
            p.alive && p.arrives <= events && p.leaves.is_none_or(|left| left > events)
        })
    }
    /// The live members of the leader's group left at the end.
    fn left(&self) -> Vec<i32> {
        self.here()
            .filter(|p| p.pgid == LEADER)
            .map(|p| p.pid)
            .collect()
    }
}

impl GroupControl for World {
    fn group_of(&mut self, pid: i32) -> Option<i32> {
        self.here().find(|p| p.pid == pid).map(|p| p.pgid)
    }
    fn listing(&mut self, pgid: i32) -> Option<(Vec<i32>, bool)> {
        (!self.unlisted).then(|| {
            let mut pids: Vec<i32> = self
                .here()
                .filter(|p| p.pgid == pgid)
                .map(|p| p.pid)
                .collect();
            if pgid == LEADER {
                pids.push(ANCHOR);
            }
            (pids, true)
        })
    }
    fn signal_group(&mut self, pgid: i32, sig: i32) -> std::io::Result<()> {
        let events = self.events();
        self.signals.push((pgid, sig));
        let mut any = false;
        for p in self.procs.iter_mut().filter(|p| {
            p.alive
                && p.arrives <= events
                && p.leaves.is_none_or(|left| left > events)
                && p.pgid == pgid
        }) {
            any = true;
            if p.ends_on.contains(&sig) {
                p.alive = false;
            }
        }
        if sig == SIGKILL
            && let Some(pid) = self.escapes.take()
        {
            self.procs.push(Proc {
                arrives: events,
                ..ours(pid, KILL)
            });
        }
        if any {
            Ok(())
        } else {
            // XNU's answer for a group that holds only its anchor.
            Err(std::io::Error::from_raw_os_error(1))
        }
    }
    fn wait(&mut self, members: &[i32], patience: Option<Duration>) -> Woke {
        self.waits.push((members.to_vec(), patience));
        self.naps += 1;
        self.clock += patience.unwrap_or(Duration::from_millis(1));
        if self.parent_leaves.is_some_and(|at| at <= self.events()) {
            Woke::Parent
        } else {
            Woke::Other
        }
    }
    fn now(&mut self) -> Instant {
        self.clock
    }
    fn nap(&mut self, pause: Duration) {
        self.naps += 1;
        self.clock += pause;
    }
}

const fn ended(termed: bool, killed: bool, emptied: bool) -> GroupEnded {
    GroupEnded {
        termed,
        killed,
        emptied,
    }
}

/// NEGATIVE CONTROL: the previous rule, a signal only after a listing in which a member the
/// watcher already knew still read, fails "the leader gone before the first look" (nothing sent)
/// and "a member forked unsignalled whose parent died on the round" (the fork left running); a
/// single `SIGKILL` round fails the latter too.
#[test]
fn a_tethered_leaders_group_ends_whole() {
    struct Case {
        name: &'static str,
        procs: Vec<Proc>,
        escapes: Option<i32>,
        want: GroupEnded,
        signals: Vec<i32>,
    }
    let cases = [
        Case {
            name: "every member ends on the SIGTERM",
            procs: vec![ours(LEADER, TERM), ours(5001, TERM), ours(5002, TERM)],
            escapes: None,
            want: ended(true, false, true),
            signals: vec![SIGTERM],
        },
        Case {
            name: "a member that outlives the leader gets the SIGKILL",
            procs: vec![ours(LEADER, TERM), ours(5001, KILL)],
            escapes: None,
            want: ended(true, true, true),
            signals: vec![SIGTERM, SIGKILL],
        },
        Case {
            name: "the leader gone before the first look, its descendants left",
            procs: vec![ours(5001, TERM), ours(5002, KILL)],
            escapes: None,
            want: ended(true, true, true),
            signals: vec![SIGTERM, SIGKILL],
        },
        Case {
            name: "a member forked unsignalled whose parent died on the round",
            procs: vec![ours(LEADER, KILL), ours(5001, KILL)],
            escapes: Some(5003),
            want: ended(true, true, true),
            signals: vec![SIGTERM, SIGKILL, SIGKILL],
        },
        Case {
            name: "a group already empty but for the anchor gets no signal",
            procs: vec![],
            escapes: None,
            want: ended(false, false, true),
            signals: vec![],
        },
    ];
    for Case {
        name,
        procs,
        escapes,
        want,
        signals,
    } in cases
    {
        let mut world = World::new(procs);
        world.escapes = escapes;
        let out = end_group(LEADER, Duration::from_millis(100), &mut world);
        let sent: Vec<i32> = world.signals.iter().map(|&(_, sig)| sig).collect();
        assert_eq!(out, want, "{name}: sent {sent:?}");
        assert!(world.left().is_empty(), "{name}: left {:?}", world.left());
        assert!(
            world.signals.iter().all(|&(pgid, _)| pgid == LEADER),
            "{name}: every signal went to the leader's group: {:?}",
            world.signals
        );
        assert_eq!(sent, signals, "{name}");
    }
}

/// A member no `SIGKILL` ends is left to the kernel after [`KILL_ROUNDS`]: the watcher ends.
#[test]
fn a_member_no_sigkill_ends_is_left_after_the_rounds() {
    let mut world = World::new(vec![ours(LEADER, NONE)]);
    let start = world.clock;
    let out = end_group(LEADER, Duration::from_millis(100), &mut world);
    assert_eq!(out, ended(true, true, false));
    assert_eq!(world.signals.first(), Some(&(LEADER, SIGTERM)));
    assert!(world.signals[1..].iter().all(|&s| s == (LEADER, SIGKILL)));
    assert!(
        world.signals.len() > 2,
        "XNU's fork escape needs more than one SIGKILL round"
    );
    let elapsed = world.clock.saturating_duration_since(start);
    assert!(
        elapsed >= KILL_ROUNDS && elapsed <= KILL_ROUNDS + Duration::from_secs(1),
        "the rounds end at their bound, on the script's clock: {elapsed:?}"
    );
}

/// Once the leader exited first: the watch ends the group when the parent goes before the
/// group's members, waits on each listed member's exit, and leaves a group that empties by itself
/// with no signal. NEGATIVE CONTROL: a watcher that exits with its leader (the previous rule)
/// leaves "the parent goes while descendants run" running.
#[test]
fn a_leader_that_exits_first_leaves_its_group_watched() {
    // The parent goes while the descendants run: the caller ends the group.
    let mut world = World::new(vec![ours(5001, KILL), ours(5002, TERM)]);
    world.parent_leaves = Some(3);
    assert!(linger(LEADER, &mut world), "the parent went first");
    assert_eq!(
        world.waits.first(),
        Some(&(vec![5001, 5002], None)),
        "the wait is on the live members' exits, the anchor not among them"
    );
    assert!(world.signals.is_empty(), "the watch itself signals nothing");
    let out = end_group(LEADER, Duration::from_millis(100), &mut world);
    assert_eq!(out, ended(true, true, true));
    assert!(world.left().is_empty(), "left {:?}", world.left());

    // The descendants exit by themselves, one forking another first: nothing is signalled.
    let mut world = World::new(vec![
        Proc {
            leaves: Some(2),
            ..ours(5001, TERM)
        },
        Proc {
            arrives: 1,
            leaves: Some(4),
            ..ours(5003, TERM)
        },
    ]);
    world.parent_leaves = Some(100);
    assert!(!linger(LEADER, &mut world), "the group emptied first");
    assert!(world.signals.is_empty());
    assert!(
        world.waits.iter().any(|(members, _)| members == &[5003]),
        "the fork joined a later wait: {:?}",
        world.waits
    );

    // An unread listing is read again after the patience, and a group already empty ends it.
    let mut world = World::new(vec![ours(5001, TERM)]);
    world.unlisted = true;
    world.parent_leaves = Some(1);
    assert!(linger(LEADER, &mut world));
    assert_eq!(world.waits, vec![(vec![], Some(UNWATCHED_RECHECK))]);
    let mut world = World::new(vec![]);
    assert!(!linger(LEADER, &mut world));
    assert!(world.waits.is_empty());
}
