// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
use super::leader_gone;
use super::{
    FgCutter, FgEdge, FgOwners, FgRole, ProcSeen, TREE_CAP, group_orphaned, parse_children,
    proc_stat_is_stopped_in, restored_payload, splice_at, tree_any,
};
use crate::holder_image::EdgeKind;
use aterm_core::terminal::program_evidence::{
    ALT_SCREEN, CURSOR_HIDDEN, FOCUS, KITTY, MOUSE, SYNC, VT52,
};

const SHELL: i32 = 100;
const JOB: i32 = 200;

#[test]
fn foreground_handback_cutter_starts_from_an_invalid_or_a_valid_sample() {
    let mut c = FgCutter::new(-1);
    c.begin();
    assert!(!c.sample(JOB, 10));
    assert_eq!(c.finish(|| JOB, 20), None);
    c.begin();
    assert_eq!(c.finish(|| SHELL, 5), Some(FgEdge::group(0, JOB, SHELL)));

    let mut c = FgCutter::new(JOB);
    c.begin();
    assert_eq!(c.finish(|| SHELL, 7), Some(FgEdge::group(0, JOB, SHELL)));
}

#[test]
fn foreground_handback_cutter_moves_the_cut_and_cuts_at_the_last_same_sample() {
    let mut c = FgCutter::new(SHELL);
    c.begin();
    assert!(!c.sample(SHELL, 1024), "unchanged: keep gathering");
    assert!(!c.sample(SHELL, 4096), "unchanged: the cut moves forward");
    assert!(c.sample(JOB, 6000), "changed at a park: deliver now");
    assert_eq!(
        c.finish(
            || panic!("an edge already exists: no batch-end probe"),
            6000
        ),
        Some(FgEdge::group(4096, SHELL, JOB))
    );
}

#[test]
fn foreground_handback_cutter_cuts_at_batch_end_and_ignores_invalid_samples() {
    let mut c = FgCutter::new(JOB);
    c.begin();
    assert!(!c.sample(-1, 100), "tcgetpgrp failed: no information");
    assert!(!c.sample(0, 200), "no foreground group: no information");
    assert!(!c.sample(JOB, 300));
    assert_eq!(
        c.finish(|| SHELL, 900),
        Some(FgEdge::group(300, JOB, SHELL))
    );
    c.begin();
    assert!(!c.sample(SHELL, 50));
    assert_eq!(c.finish(|| SHELL, 80), None);
}

#[test]
fn foreground_handback_cutter_reports_at_most_one_edge_per_batch() {
    let mut c = FgCutter::new(SHELL);
    c.begin();
    assert!(!c.sample(SHELL, 10));
    assert!(c.sample(JOB, 20));
    assert!(c.sample(SHELL, 30), "still delivering: the edge is taken");
    assert_eq!(c.finish(|| SHELL, 30), Some(FgEdge::group(10, SHELL, JOB)));
    c.begin();
    assert_eq!(c.finish(|| SHELL, 5), Some(FgEdge::group(0, JOB, SHELL)));
}

#[test]
fn foreground_handback_cutter_begin_resets_the_cut() {
    let mut c = FgCutter::new(SHELL);
    c.begin();
    assert!(!c.sample(SHELL, 5000));
    assert_eq!(c.finish(|| SHELL, 6000), None);
    c.begin();
    assert!(c.sample(JOB, 100));
    assert_eq!(
        c.finish(|| JOB, 100).map(|e| e.at),
        Some(0),
        "the previous batch's offset must not leak into this one"
    );
}

#[test]
fn foreground_handback_cutter_reports_its_holder() {
    let mut c = FgCutter::new(-1);
    assert_eq!(c.holder(), 0, "no holder yet");
    c.begin();
    assert_eq!(c.finish(|| SHELL, 3), None);
    assert_eq!(c.holder(), SHELL);
    c.begin();
    assert!(c.finish(|| JOB, 3).is_some());
    assert_eq!(c.holder(), JOB, "after an edge the holder is edge.to");
    let mut c = FgCutter::new(JOB);
    c.begin();
    assert!(c.sample(SHELL, 0));
    assert_eq!(c.finish(|| SHELL, 40), Some(FgEdge::group(0, JOB, SHELL)));
}

/// An `exec` heard at a park.
fn exec_of(pid: i32, pgid: i32, fg: i32) -> ProcSeen {
    ProcSeen { pid, pgid, fg }
}

/// An Exec edge (the running leader replaced its image) cuts at the last
/// clean offset, not at the poll that heard it, and lays the new run at
/// that poll: the bytes between are no holder's.
#[test]
fn foreground_handback_cutter_cuts_an_exec_at_the_last_clean_poll() {
    let mut c = FgCutter::new(JOB);
    c.begin();
    assert!(!c.sample(JOB, 100));
    assert!(!c.sample(JOB, 200));
    let mut asked = None;
    assert!(c.proc_seen(exec_of(JOB, JOB, JOB), 200, |kind, to| {
        asked = Some((kind, to));
        true
    }));
    assert_eq!(asked, Some((EdgeKind::Exec, JOB)));
    let edge = c.finish(|| JOB, 200).expect("the exec edge");
    assert_eq!(
        (edge.at, edge.lay, edge.from, edge.to, edge.kind),
        (100, 200, JOB, JOB, EdgeKind::Exec),
        "cut at the last clean poll, laid at the one that heard it"
    );
    assert_eq!(c.holder(), JOB);

    c.begin();
    assert!(!c.sample(JOB, 100));
    assert!(!c.proc_seen(exec_of(JOB, JOB, JOB), 100, |_, _| false));
    c.proc_drained(100);
    assert!(!c.sample(JOB, 300));
    assert!(c.proc_seen(exec_of(JOB, JOB, JOB), 300, |_, _| true));
    let edge = c.finish(|| JOB, 300).expect("the exec edge");
    assert_eq!((edge.at, edge.lay), (100, 300));

    c.begin();
    assert!(c.proc_seen(exec_of(JOB, JOB, JOB), 0, |_, _| true));
    let edge = c.finish(|| JOB, 0).expect("the exec edge");
    assert_eq!((edge.at, edge.lay), (0, 0));
}

/// A Group edge (the `exec`ed process leads the group now in front) cuts
/// where a sampled one would: the last sample that showed the old group.
#[test]
fn foreground_handback_cutter_cuts_a_kqueue_group_edge_at_the_last_same_sample() {
    let mut c = FgCutter::new(SHELL);
    c.begin();
    assert!(!c.sample(SHELL, 40));
    assert!(!c.sample(-1, 90));
    assert!(c.proc_seen(exec_of(JOB, JOB, JOB), 90, |kind, to| {
        assert_eq!((kind, to), (EdgeKind::Group, JOB));
        true
    }));
    assert!(c.carries_edge());
    let edge = c.finish(|| JOB, 90).expect("the group edge");
    assert_eq!(
        (edge.at, edge.lay, edge.from, edge.to, edge.kind),
        (40, 90, SHELL, JOB, EdgeKind::Group)
    );
    assert_eq!(c.holder(), JOB);
}

/// Only an `exec` that enters or leaves the terminal's holder, and that the
/// gather calls card-relevant, is an edge; the rest fold into the sampled
/// cut, and a batch keeps at most one edge.
#[test]
fn foreground_handback_cutter_folds_every_other_process_event() {
    const MEMBER: i32 = 201;
    const BG: i32 = 300;
    let mut c = FgCutter::new(JOB);
    c.begin();
    let never = |_: EdgeKind, _: i32| -> bool { panic!("no candidate") };
    assert!(!c.proc_seen(exec_of(MEMBER, JOB, JOB), 0, never));
    assert!(!c.proc_seen(exec_of(BG, BG, JOB), 0, never));
    assert!(!c.proc_seen(exec_of(JOB, 0, JOB), 0, never));
    assert!(!c.proc_seen(exec_of(JOB, JOB, -1), 0, never));
    assert!(!c.proc_seen(exec_of(JOB, JOB, JOB), 0, |_, _| false));
    assert!(!c.carries_edge());
    assert_eq!(c.finish(|| JOB, 10), None);
    let mut c = FgCutter::new(-1);
    c.begin();
    assert!(!c.proc_seen(exec_of(JOB, JOB, JOB), 0, never));

    let mut c = FgCutter::new(SHELL);
    c.begin();
    assert!(c.proc_seen(exec_of(JOB, JOB, JOB), 0, |_, _| true));
    assert!(c.proc_seen(exec_of(JOB, JOB, JOB), 5, |_, _| true));
    assert_eq!(c.finish(|| JOB, 5).map(|e| e.kind), Some(EdgeKind::Group));
}

#[test]
fn foreground_handback_owners_attribute_each_bit_to_its_setter() {
    const GDB: i32 = 300;
    const INFERIOR: i32 = 400;
    let dead = |pids: &'static [i32]| move |p: i32| pids.contains(&p);

    let mut o = FgOwners::default();
    o.observe(SHELL, 0, 0);
    o.observe(GDB, ALT_SCREEN | MOUSE, 0);
    o.observe(INFERIOR, ALT_SCREEN | MOUSE, 0);
    assert_eq!(
        o.orphaned_by(GDB, dead(&[INFERIOR])),
        None,
        "the alt screen and mouse are gdb's, and gdb lives"
    );
    o.observe(INFERIOR, ALT_SCREEN | MOUSE | FOCUS, 0);
    assert_eq!(o.orphaned_by(GDB, dead(&[INFERIOR])), Some(INFERIOR));
    o.observe(INFERIOR, ALT_SCREEN | MOUSE, 0);
    assert_eq!(o.orphaned_by(GDB, dead(&[INFERIOR])), None);
    assert_eq!(o.orphaned_by(SHELL, dead(&[GDB, INFERIOR])), Some(GDB));

    let mut o = FgOwners::default();
    o.observe(JOB, MOUSE, 0);
    assert_eq!(o.orphaned_by(JOB, |_| true), None);
    assert_eq!(o.orphaned_by(SHELL, |_| true), Some(JOB));
    o.observe(JOB, 0, 0);
    assert_eq!(o.orphaned_by(SHELL, |_| true), None);

    let mut o = FgOwners::default();
    o.observe(JOB, ALT_SCREEN | MOUSE, 0);
    assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), Some(JOB));
    let mut o = FgOwners::default();
    o.observe(0, MOUSE, 0);
    assert_eq!(o.orphaned_by(SHELL, |_| true), None);
    o.observe(-1, MOUSE | ALT_SCREEN, 0);
    assert_eq!(o.orphaned_by(SHELL, |_| true), None);
}

/// A bit re-armed over the shell's (a one-shot's `?1000h` parsed as the shell's, then a
/// job's `?1003h`) moves to the re-armer, so the job's death hands it back.
#[test]
fn foreground_handback_owners_give_a_re_armed_bit_to_its_re_armer() {
    let dead = |pids: &'static [i32]| move |p: i32| pids.contains(&p);
    let mut o = FgOwners::default();
    o.observe(SHELL, MOUSE, MOUSE);
    assert_eq!(o.orphaned_by(SHELL, |_| true), None, "the shell's own");
    o.observe(JOB, MOUSE, MOUSE);
    assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), Some(JOB));
    let mut o = FgOwners::default();
    o.observe(SHELL, KITTY, KITTY);
    o.observe(JOB, KITTY | ALT_SCREEN, KITTY | ALT_SCREEN);
    assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), Some(JOB));
    let mut o = FgOwners::default();
    o.observe(JOB, 0, MOUSE);
    assert_eq!(o.orphaned_by(SHELL, |_| true), None);
    let mut o = FgOwners::default();
    o.observe(JOB, KITTY, KITTY);
    o.observe(SHELL, KITTY, KITTY);
    assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), None);

    let mut o = FgOwners::default();
    o.observe(SHELL, MOUSE, MOUSE);
    o.observe(JOB, MOUSE | ALT_SCREEN, ALT_SCREEN);
    assert_eq!(o.orphaned_by(SHELL, dead(&[JOB])), None);
}

/// A gone group owning only display bits (`tput civis`, `tput smcup`, `tput smcup; cmd`)
/// orphans nothing; one owning an input bit orphans all of its bits.
#[test]
fn foreground_handback_a_display_only_one_shot_orphans_nothing() {
    const TPUT: i32 = 500;
    const CMD: i32 = 600;
    for display in [
        ALT_SCREEN,
        CURSOR_HIDDEN,
        SYNC,
        VT52,
        ALT_SCREEN | CURSOR_HIDDEN,
    ] {
        let mut o = FgOwners::default();
        o.observe(SHELL, 0, 0);
        o.observe(TPUT, display, 0);
        assert_eq!(
            o.orphaned_by(SHELL, |_| true),
            None,
            "a one-shot's display bits {display:#x} are kept"
        );
    }
    let mut o = FgOwners::default();
    o.observe(SHELL, 0, 0);
    o.observe(TPUT, ALT_SCREEN, 0);
    assert_eq!(o.orphaned_by(CMD, |p| p == TPUT), None);
    o.observe(CMD, ALT_SCREEN | MOUSE, 0);
    assert_eq!(o.orphaned_by(SHELL, |p| p == TPUT || p == CMD), Some(CMD));
    let mut o = FgOwners::default();
    o.observe(TPUT, MOUSE, 0);
    assert_eq!(o.orphaned_by(SHELL, |_| true), Some(TPUT));
    let mut o = FgOwners::default();
    o.observe(JOB, ALT_SCREEN | CURSOR_HIDDEN | SYNC | KITTY | MOUSE, 0);
    assert_eq!(o.orphaned_by(SHELL, |_| true), Some(JOB));
}

#[test]
fn foreground_handback_owners_ask_about_each_owner_once() {
    let mut o = FgOwners::default();
    o.observe(JOB, ALT_SCREEN | MOUSE | CURSOR_HIDDEN | KITTY, 0);
    let mut asked = Vec::new();
    assert_eq!(
        o.orphaned_by(SHELL, |p| {
            asked.push(p);
            false
        }),
        None
    );
    assert_eq!(asked, vec![JOB]);
}

/// A death no edge was cut for (a resumed job that wrote nothing, its hold never sampled)
/// is the edge from the gone owner into the shell at the batch's first byte, only while the
/// shell holds, only for an owner of an input bit still in force, and only once it is gone.
#[test]
fn foreground_handback_owners_cut_the_death_edge_the_gather_missed() {
    let mut o = FgOwners::default();
    o.observe(SHELL, 0, 0);
    o.observe(JOB, ALT_SCREEN | MOUSE, ALT_SCREEN | MOUSE);
    let mut asked = Vec::new();
    assert_eq!(
        o.missed_reclaim(SHELL, SHELL, |p| {
            asked.push(p);
            false
        }),
        None,
        "a stopped or bg'd job keeps its modes"
    );
    assert_eq!(asked, vec![JOB], "only the owner is asked");
    assert_eq!(
        o.missed_reclaim(SHELL, SHELL, |p| p == JOB),
        Some(FgEdge::group(0, JOB, SHELL))
    );
    assert_eq!(
        o.missed_reclaim(JOB, SHELL, |_| panic!("not asked")),
        None,
        "a batch the job holds is its own"
    );
    assert_eq!(
        o.missed_reclaim(SHELL, 0, |_| panic!("not asked")),
        None,
        "no shell known"
    );
    o.observe(SHELL, 0, 0);
    assert_eq!(
        o.missed_reclaim(SHELL, SHELL, |_| panic!("not asked")),
        None,
        "handed back already"
    );

    let mut o = FgOwners::default();
    o.observe(JOB, ALT_SCREEN | CURSOR_HIDDEN, ALT_SCREEN | CURSOR_HIDDEN);
    assert_eq!(
        o.missed_reclaim(SHELL, SHELL, |_| panic!("not asked")),
        None,
        "display bits alone orphan nothing"
    );
    o.observe(SHELL, ALT_SCREEN | CURSOR_HIDDEN | MOUSE, MOUSE);
    assert_eq!(
        o.missed_reclaim(SHELL, SHELL, |_| panic!("not asked")),
        None,
        "the shell's own mouse"
    );
}

/// The pre-lock probe asks each group the locked handback can ask about once (`from`
/// included even when it also owns bits), never the new holder or a display-only owner;
/// a group not probed reads as alive.
#[test]
fn foreground_handback_probe_edge_asks_each_group_once_before_the_lock() {
    const TPUT: i32 = 500;
    let mut o = FgOwners::default();
    o.observe(TPUT, ALT_SCREEN, 0);
    o.observe(JOB, ALT_SCREEN | MOUSE | KITTY, 0);
    let mut asked = Vec::new();
    let v = o.probe_edge(JOB, FgRole::Holder, SHELL, |p, role| {
        asked.push((p, role));
        true
    });
    assert_eq!(
        asked,
        vec![(JOB, FgRole::Holder)],
        "from once, as the holder; tput owns display bits only"
    );
    assert_eq!(v.probed(), vec![JOB]);
    assert!(v.gone(JOB));
    assert!(!v.gone(TPUT) && !v.gone(SHELL), "not probed: alive");
    assert_eq!(o.orphaned_by(SHELL, |p| v.gone(p)), Some(JOB));
    // A missed edge's group is judged by the bystander rule it was found gone by: a leader
    // pid that answers (reused outside the group) does not keep its modes.
    let mut asked = Vec::new();
    let v = o.probe_edge(JOB, FgRole::Bystander, SHELL, |p, role| {
        asked.push((p, role));
        role == FgRole::Bystander
    });
    assert_eq!(
        asked,
        vec![(JOB, FgRole::Bystander)],
        "from by its edge's rule"
    );
    assert_eq!(o.orphaned_by(SHELL, |p| v.gone(p)), Some(JOB));

    let mut o = FgOwners::default();
    o.observe(JOB, MOUSE, 0);
    o.observe(TPUT, MOUSE | FOCUS, 0);
    let mut asked = Vec::new();
    let v = o.probe_edge(SHELL, FgRole::Holder, TPUT, |p, _| {
        asked.push(p);
        p == JOB
    });
    assert_eq!(asked, vec![SHELL, JOB], "`to` (TPUT) is never probed");
    assert!(!v.gone(SHELL) && v.gone(JOB));
    assert_eq!(o.orphaned_by(TPUT, |p| v.gone(p)), Some(JOB));
    let v = FgOwners::default().probe_edge(0, FgRole::Holder, SHELL, |_, _| panic!("no group"));
    assert!(v.probed().is_empty());
}

/// The Linux member walk: breadth first from the session leader, at most `TREE_CAP`
/// processes, `None` (fall back) only when the leader's own children cannot be listed.
#[test]
fn foreground_handback_session_tree_walk_is_bounded() {
    assert_eq!(
        parse_children("12 34 0 x -5 56\n").collect::<Vec<_>>(),
        vec![12, 34, 56]
    );
    assert_eq!(parse_children("").count(), 0);
    let tree = |pid: i32| match pid {
        10 => Some(vec![11, 12]),
        12 => Some(vec![13]),
        11 | 13 => Some(vec![]),
        _ => None,
    };
    assert_eq!(tree_any(10, tree, |p| p == 13, TREE_CAP), Some(true));
    assert_eq!(tree_any(10, tree, |_| false, TREE_CAP), Some(false));
    assert_eq!(
        tree_any(10, tree, |p| p == 10, TREE_CAP),
        Some(true),
        "the root too"
    );
    assert_eq!(
        tree_any(99, tree, |_| true, TREE_CAP),
        None,
        "no listing: fall back"
    );
    let gappy = |pid: i32| (pid == 10).then(|| vec![11, 12]);
    assert_eq!(tree_any(10, gappy, |p| p == 12, TREE_CAP), Some(true));
    let mut visits = 0;
    let endless = |pid: i32| Some(vec![pid + 1, pid + 2]);
    assert_eq!(
        tree_any(
            1,
            endless,
            |_| {
                visits += 1;
                false
            },
            50
        ),
        Some(false)
    );
    assert_eq!(visits, 50);
}

#[cfg(unix)]
#[test]
fn foreground_handback_leader_gone_is_esrch_only() {
    assert!(
        !leader_gone(std::process::id() as i32),
        "this process lives"
    );
    assert!(!leader_gone(0) && !leader_gone(-1), "no pid, no verdict");
    assert!(!leader_gone(1));
    let mut child = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("spawn sh");
    let pid = child.id() as i32;
    child.wait().expect("reap sh");
    assert!(leader_gone(pid), "a reaped leader is gone");
}

/// The orphan rule: a gone leader with a stopped member is a stopped job, with only running
/// members a killed one; a live leader never asks for the member listing.
#[test]
fn foreground_handback_a_gone_leader_with_a_stopped_member_is_not_orphaned() {
    assert!(!group_orphaned(true, || true), "Ctrl-Z on `producer | tui`");
    assert!(
        group_orphaned(true, || false),
        "the incident: MCP children run"
    );
    assert!(
        !group_orphaned(false, || panic!("a live leader needs no listing")),
        "a live leader"
    );
}

#[test]
fn foreground_handback_proc_stat_reads_state_and_group() {
    assert!(proc_stat_is_stopped_in(
        "4321 (sh) T 4000 4300 4000 34816 4000 0",
        4300
    ));
    assert!(
        proc_stat_is_stopped_in("4321 (sh) t 4000 4300 4000", 4300),
        "a trace stop is a stop"
    );
    assert!(!proc_stat_is_stopped_in("4321 (sh) S 4000 4300 4000", 4300));
    assert!(
        !proc_stat_is_stopped_in("4321 (sh) T 4000 4301 4000", 4300),
        "another group's"
    );
    assert!(proc_stat_is_stopped_in(
        "4321 (a) T (b) T 4000 4300 4000",
        4300
    ));
    assert!(!proc_stat_is_stopped_in("garbage", 4300));
    assert!(!proc_stat_is_stopped_in("4321 (sh) T", 4300));
}

/// The real process table, a group whose leader exited while a member lives: as the
/// holder it is gone while the member runs, not while it is stopped, and gone again once
/// resumed; as a bystander only once its last member is.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn foreground_handback_a_stopped_member_of_a_leaderless_group_is_seen() {
    use super::FgRole::{Bystander, Holder};
    use super::{group_empty, group_gone, group_has_stopped_member};
    use std::time::Duration;

    /// Kills and reaps what it holds, however the test ends.
    struct Reaped(Vec<aterm_uds::spawnfd::Child>);
    impl Drop for Reaped {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let seen = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(5),
            || done().then_some(()),
        );
        assert!(seen.is_some(), "timed out waiting for {what}");
    }

    let leader = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .expect("spawn the leader");
    let pgid = i32::try_from(leader.id()).expect("a pid");
    let mut held = Reaped(vec![leader]);
    let member = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("60")
        .process_group(pgid)
        .spawn()
        .expect("spawn the member into the leader's group");
    let member_pid = i32::try_from(member.id()).expect("a pid");
    held.0.push(member);

    assert!(!group_has_stopped_member(-1, pgid), "both run");
    assert!(!group_gone(-1, pgid, Holder), "the leader lives");
    assert!(!group_gone(-1, pgid, Bystander) && !group_empty(pgid));

    let mut leader = held.0.remove(0);
    leader.kill().expect("kill the leader");
    leader.wait().expect("reap the leader");
    assert!(leader_gone(pgid));
    assert!(
        group_gone(-1, pgid, Holder),
        "a running member: the incident's shape"
    );
    assert!(
        !group_gone(-1, pgid, Bystander),
        "a leaderless job running in the background is not gone"
    );

    // SAFETY: signals this test's own child by its pid.
    assert_eq!(unsafe { libc::kill(member_pid, libc::SIGSTOP) }, 0);
    until("the member reads as stopped", || {
        group_has_stopped_member(-1, pgid)
    });
    assert!(
        !group_gone(-1, pgid, Holder),
        "a stopped job keeps its modes"
    );
    assert!(!group_gone(-1, pgid, Bystander));

    // SAFETY: signals this test's own child by its pid.
    assert_eq!(unsafe { libc::kill(member_pid, libc::SIGCONT) }, 0);
    until("the member reads as running", || {
        !group_has_stopped_member(-1, pgid)
    });
    assert!(group_gone(-1, pgid, Holder));
    assert!(!group_gone(-1, pgid, Bystander));

    let mut member = held.0.remove(0);
    member.kill().expect("kill the member");
    member.wait().expect("reap the member");
    assert!(group_empty(pgid));
    assert!(group_gone(-1, pgid, Holder) && group_gone(-1, pgid, Bystander));
    assert!(!group_has_stopped_member(-1, 0) && !group_has_stopped_member(-1, -1));
    assert!(
        !group_empty(0) && !group_empty(-1) && !group_empty(1),
        "no group, or the broadcast pid: no verdict"
    );
}

/// A member owned by another user reads back (`PROC_PIDTBSDINFO` would refuse it with
/// `EPERM`).
#[cfg(target_os = "macos")]
#[test]
fn foreground_handback_another_users_member_status_reads_back() {
    let (status, pgid) =
        super::darwin_member_status(1).expect("launchd's status reads across users");
    assert_eq!(pgid, 1, "launchd leads its own group");
    assert!((1..=5).contains(&status), "a p_stat value: {status}");
    let own = i32::try_from(std::process::id()).expect("a pid");
    // SAFETY: getpgrp takes no arguments and cannot fail.
    let own_pgid = unsafe { libc::getpgrp() };
    let (status, pgid) = super::darwin_member_status(own).expect("this process");
    assert_eq!(pgid, own_pgid);
    assert!(matches!(status, 2 | 3), "this process runs: {status}");
}

#[test]
fn foreground_handback_splice_and_payload() {
    assert_eq!(splice_at(b"abcd", 2, b"XY"), b"abXYcd");
    assert_eq!(splice_at(b"abcd", 4, b"XY"), b"abcdXY");
    assert_eq!(splice_at(b"abcd", 0, b"XY"), b"XYabcd");
    let edge = FgEdge::group(3, JOB, SHELL);
    assert_eq!(
        restored_payload(edge, Some("claude"), &["alt", "kitty"], 42),
        "from=200 to=100 program=claude reverted=alt,kitty bytes=42"
    );
    assert_eq!(
        restored_payload(edge, None, &["parser"], 1),
        "from=200 to=100 program=- reverted=parser bytes=1"
    );
}
