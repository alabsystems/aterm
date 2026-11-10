// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`end_group`] and [`linger`] over a scripted kernel: the group a tethered leader leads ends
//! whole, whoever of it the watcher saw, and a group that empties by itself gets no signal.

use super::super::{ChildExitNotice, Proof, Watched, end_group_once_ready};
use super::{
    GroupControl, GroupEnded, KILL_ROUNDS, Live, Readiness, UNWATCHED_RECHECK, Woke, end_group,
    end_group_guarded, linger, live,
};
use crate::process::{Birth, EndControl, Identity, ProcessPresence};
use std::collections::VecDeque;
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
    /// Identity and presence queries are unreadable despite a whole listing.
    unread: bool,
    /// This live member's group read is unavailable; its presence still reads.
    unread_group: Option<i32>,
    /// After returning one listing, this parent forks then exits before reads.
    fork_before_reads: Option<(i32, i32)>,
    /// A second membership query does not return a whole listing.
    unlisted_after_first: bool,
    listings: usize,
    /// The earlier pid exits after listing, then is reused by the later member.
    reuse_before_reads: Option<(i32, i32)>,
    reuse_on_read: Option<(i32, i32)>,
    /// Non-owned zombies whose listed pids have no retained wait custody here.
    other_zombies: Vec<i32>,
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
            unread: false,
            unread_group: None,
            fork_before_reads: None,
            unlisted_after_first: false,
            listings: 0,
            reuse_before_reads: None,
            reuse_on_read: None,
            other_zombies: Vec::new(),
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
    fn fork_reused_before_read(&mut self, pid: i32) {
        if let Some((later, reused)) = self.reuse_on_read
            && pid == later
        {
            self.reuse_on_read = None;
            self.procs
                .iter_mut()
                .find(|p| p.pid == later)
                .unwrap()
                .alive = false;
            self.procs.push(ours(reused, KILL));
        }
    }
}

impl GroupControl for World {
    fn anchor(&self) -> i32 {
        ANCHOR
    }
    fn group_of(&mut self, pid: i32) -> Option<i32> {
        self.fork_reused_before_read(pid);
        if self.unread || self.unread_group == Some(pid) {
            return None;
        }
        self.here().find(|p| p.pid == pid).map(|p| p.pgid)
    }
    fn presence(&mut self, pid: i32) -> ProcessPresence {
        self.fork_reused_before_read(pid);
        if self.unread {
            ProcessPresence::Unknown
        } else if pid == ANCHOR || self.other_zombies.contains(&pid) {
            ProcessPresence::Exited
        } else if self.here().any(|p| p.pid == pid) {
            ProcessPresence::Present
        } else {
            ProcessPresence::Absent
        }
    }
    fn listing(&mut self, pgid: i32) -> Option<(Vec<i32>, bool)> {
        self.listings += 1;
        if self.unlisted_after_first && self.listings > 1 {
            return None;
        }
        (!self.unlisted).then(|| {
            let mut pids: Vec<i32> = self
                .here()
                .filter(|p| p.pgid == pgid)
                .map(|p| p.pid)
                .collect();
            if pgid == LEADER {
                pids.push(ANCHOR);
                pids.extend(&self.other_zombies);
            }
            if let Some((parent, child)) = self.fork_before_reads.take() {
                self.procs
                    .iter_mut()
                    .find(|p| p.pid == parent)
                    .unwrap()
                    .alive = false;
                self.procs.push(ours(child, KILL));
            }
            if let Some((earlier, later)) = self.reuse_before_reads.take() {
                self.procs
                    .iter_mut()
                    .find(|p| p.pid == earlier)
                    .unwrap()
                    .alive = false;
                self.reuse_on_read = Some((later, earlier));
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

/// A whole census is not positive exit evidence when its members cannot be read.
/// NEGATIVE CONTROL: filtering unread group_of(None) values from the census
/// classifies this still-live group Empty and lets linger return without a wait.
#[test]
fn unread_members_never_prove_an_empty_group() {
    let mut world = World::new(vec![ours(LEADER, KILL)]);
    world.unread = true;
    world.parent_leaves = Some(1);
    assert_eq!(live(&mut world, LEADER), Live::Unknown);
    assert!(
        linger(LEADER, &mut world),
        "the parent's end still needs cleanup"
    );
    assert_eq!(world.left(), vec![LEADER]);
    assert_eq!(world.waits, vec![(vec![], Some(UNWATCHED_RECHECK))]);
    let out = end_group(LEADER, Duration::ZERO, &mut world);
    assert!(out.termed && out.killed);
    assert!(!out.emptied, "unread observations never certify emptiness");
}

/// A member may fork after enumeration and exit before its presence is read.
/// NEGATIVE CONTROL: the incoming one-listing classifier returns Empty here,
/// leaves the real new member running and skips the owned group's cleanup.
#[test]
fn a_fork_after_listing_requires_fresh_whole_membership() {
    let mut world = World::new(vec![ours(LEADER, KILL)]);
    world.fork_before_reads = Some((LEADER, 5003));
    assert_eq!(live(&mut world, LEADER), Live::Unknown);
    assert_eq!(world.left(), vec![5003]);
    assert_eq!(live(&mut world, LEADER), Live::Members(vec![5003]));
    let out = end_group(LEADER, Duration::ZERO, &mut world);
    assert_eq!(out, ended(true, true, true));
    assert!(world.left().is_empty());
    assert!(world.signals.iter().all(|&(pgid, _)| pgid == LEADER));
}

#[test]
fn empty_requires_a_second_whole_membership_read() {
    let mut world = World::new(vec![]);
    world.unlisted_after_first = true;
    assert_eq!(live(&mut world, LEADER), Live::Unknown);
    let mut whole = World::new(vec![]);
    assert_eq!(live(&mut whole, LEADER), Live::Empty);
}

/// An earlier absent pid may be reused by a later member's fork before that
/// member exits. A fresh numeric-pid subset is not lifetime continuity.
#[test]
fn a_reused_earlier_pid_cannot_prove_empty_by_subset() {
    let mut world = World::new(vec![ours(5001, KILL), ours(5002, KILL)]);
    world.reuse_before_reads = Some((5001, 5002));
    assert_eq!(live(&mut world, LEADER), Live::Unknown);
    assert_eq!(world.left(), vec![5001]);
    assert_eq!(live(&mut world, LEADER), Live::Members(vec![5001]));
    let out = end_group(LEADER, Duration::ZERO, &mut world);
    assert_eq!(out, ended(true, true, true));
    assert!(world.left().is_empty());
}

#[test]
fn another_exited_pid_never_stands_in_for_owned_anchor_custody() {
    let mut world = World::new(vec![]);
    world.other_zombies.push(5001);
    assert_eq!(live(&mut world, LEADER), Live::Unknown);
    world.other_zombies.clear();
    assert_eq!(live(&mut world, LEADER), Live::Empty);
}

struct ExecutionRead(Option<Identity>);

impl EndControl for ExecutionRead {
    fn identity(&mut self, _pid: i32) -> Option<Identity> {
        self.0
    }
    fn signal(&mut self, _token: &Identity, _sig: i32) -> std::io::Result<()> {
        panic!("the group gate must not substitute an individual-child signal");
    }
    fn nap(&mut self, _pause: Duration) {
        panic!("the group gate waits through the original child's typed exit reader");
    }
}

fn watched() -> Watched {
    Watched {
        pid: LEADER,
        birth: Birth(42),
        uniqueid: 7,
    }
}

fn proof(ran: bool) -> Option<Proof> {
    Some(Proof {
        uniqueid: watched().uniqueid,
        ran,
    })
}

/// NEGATIVE CONTROL: the incoming parent path discarded held_until_exit(false)
/// and unconditionally called end_group, sending TERM in this exact None-proof
/// case despite an otherwise matching original token and a still-live root.
#[test]
fn an_unread_or_moved_execution_proof_never_starts_group_cleanup() {
    let moved = Some(Proof {
        uniqueid: 8,
        ran: true,
    });
    for (observed, notice) in [
        (None, ChildExitNotice::Pending),
        (moved, ChildExitNotice::Pending),
        (proof(false), ChildExitNotice::Unavailable),
    ] {
        let mut control = ExecutionRead(Some(watched().token()));
        let mut world = World::new(vec![ours(LEADER, TERM)]);
        let result = end_group_once_ready(
            watched(),
            false,
            &mut control,
            |_| observed,
            |_| notice,
            &mut world,
        );
        assert_eq!(result, None);
        assert!(world.signals.is_empty());
        assert_eq!(world.left(), vec![LEADER]);
    }
    let mut control = ExecutionRead(Some(Identity {
        birth: Birth(43),
        ..watched().token()
    }));
    let mut world = World::new(vec![ours(LEADER, TERM)]);
    assert_eq!(
        end_group_once_ready(
            watched(),
            false,
            &mut control,
            |_| proof(true),
            |_| ChildExitNotice::Pending,
            &mut world,
        ),
        None
    );
    assert!(world.signals.is_empty());
}

#[test]
fn positive_start_or_actual_original_exit_allows_group_cleanup() {
    let mut control = ExecutionRead(Some(watched().token()));
    let mut world = World::new(vec![ours(LEADER, TERM)]);
    assert_eq!(
        end_group_once_ready(
            watched(),
            false,
            &mut control,
            |_| proof(true),
            |_| panic!("positive start needs no exit substitute"),
            &mut world,
        ),
        Some(ended(true, false, true))
    );
    assert_eq!(world.signals, vec![(LEADER, SIGTERM)]);

    // A NOTE_EXIT in the same batch as the parent, or a later positive exit
    // query, allows cleanup of descendants even though the root no longer reads.
    for known_exit in [true, false] {
        let mut control = ExecutionRead(None);
        let mut world = World::new(vec![ours(5001, TERM)]);
        assert_eq!(
            end_group_once_ready(
                watched(),
                known_exit,
                &mut control,
                |_| {
                    assert!(!known_exit, "known original exit needs no live proof");
                    None
                },
                |patience| {
                    assert!(!known_exit, "same-batch original exit is already positive");
                    assert_eq!(patience, Duration::ZERO);
                    ChildExitNotice::Exited
                },
                &mut world,
            ),
            Some(ended(true, false, true))
        );
        assert!(world.left().is_empty());
        assert_eq!(world.signals, vec![(LEADER, SIGTERM)]);
    }
}

#[test]
fn an_initial_held_root_keeps_the_existing_wait_before_group_cleanup() {
    let mut control = ExecutionRead(Some(watched().token()));
    let mut world = World::new(vec![ours(LEADER, TERM)]);
    let mut observations = VecDeque::from([proof(false), proof(true)]);
    let mut waits = Vec::new();
    assert_eq!(
        end_group_once_ready(
            watched(),
            false,
            &mut control,
            |_| observations.pop_front().unwrap_or_else(|| proof(true)),
            |patience| {
                waits.push(patience);
                ChildExitNotice::Pending
            },
            &mut world,
        ),
        Some(ended(true, false, true))
    );
    assert_eq!(waits, vec![Duration::ZERO, super::super::HELD_RECHECK]);
    assert_eq!(world.signals, vec![(LEADER, SIGTERM)]);
}

/// Each individual signal needs renewed point evidence. A positive TERM does
/// not authorize a later KILL after the original root became held or unread.
#[test]
fn losing_execution_readiness_between_term_and_kill_refuses_the_kill() {
    for lost in [Readiness::Held, Readiness::Unavailable] {
        let mut world = World::new(vec![ours(LEADER, KILL)]);
        let start = world.clock;
        let mut states = VecDeque::from([Readiness::Started, Readiness::Started]);
        let out = end_group_guarded(LEADER, Duration::ZERO, &mut world, || {
            states.pop_front().unwrap_or(lost)
        });
        assert_eq!(out, Some(ended(true, false, false)));
        assert_eq!(world.signals, vec![(LEADER, SIGTERM)]);
        assert_eq!(world.left(), vec![LEADER]);
        assert!(
            world.clock.saturating_duration_since(start) <= KILL_ROUNDS + Duration::from_secs(1)
        );
    }
}

#[test]
fn readiness_is_renewed_even_before_the_first_term() {
    let mut world = World::new(vec![ours(LEADER, TERM)]);
    let mut first = true;
    assert_eq!(
        end_group_guarded(LEADER, Duration::ZERO, &mut world, || {
            if std::mem::take(&mut first) {
                Readiness::Started
            } else {
                Readiness::Unavailable
            }
        }),
        Some(ended(false, false, false))
    );
    assert!(world.signals.is_empty());
    assert_eq!(world.left(), vec![LEADER]);
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

    // A whole PID listing cannot turn an unread live member into an empty group.
    // Its positive presence and the readable owned anchor remain separate facts.
    let mut world = World::new(vec![ours(5001, KILL)]);
    world.unread_group = Some(5001);
    world.parent_leaves = Some(1);
    assert_eq!(world.presence(5001), ProcessPresence::Present);
    assert_eq!(world.presence(ANCHOR), ProcessPresence::Exited);
    assert_eq!(world.listing(LEADER), Some((vec![5001, ANCHOR], true)));
    assert!(
        linger(LEADER, &mut world),
        "the unread member keeps its group watched until the parent goes"
    );
    assert_eq!(world.waits, vec![(vec![], Some(UNWATCHED_RECHECK))]);
    let out = end_group(LEADER, Duration::from_millis(100), &mut world);
    assert_eq!(out, ended(true, true, true));
    assert!(world.left().is_empty(), "left {:?}", world.left());
    assert_eq!(
        world.signals,
        [(LEADER, SIGTERM), (LEADER, SIGKILL)],
        "the anchored group still ends despite its unread individual member"
    );
}
