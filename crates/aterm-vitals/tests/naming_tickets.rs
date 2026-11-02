// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit naming through the real sweep/plan/read/absorb engine.

#[path = "support/world.rs"]
mod world;

use aterm_vitals::jobs::{
    Class, Consumer, Fd, FdKind, GateWord, NamingError, NamingSelector, NamingTicket, ProcState,
    Standing, TcpState, TierB, Verdict, WaitName,
};
use world::{OutFile, Proc, World, hint};

const LEAF: u32 = 7700;
const FILE: (u64, u64) = (1, 99_000);

fn settle(w: &mut World) {
    for _ in 0..240 {
        let _ = w.tick(2_000);
    }
}

fn quiet(polling: bool) -> World {
    let mut w = World::new();
    let mut leaf = Proc::new(LEAF, 0, "cargo");
    leaf.out = Some(FILE);
    if polling {
        leaf.syscalls_per_s = 40;
        leaf.wakeups_per_s = 20;
    }
    w.files.insert(
        FILE,
        OutFile {
            size: 0,
            per_s: 0,
            lf: Some(true),
        },
    );
    w.run_fg(1, 77, "cargo build", leaf);
    settle(&mut w);
    w
}

fn ticket(w: &World) -> NamingTicket {
    w.jobs
        .naming_target(1, NamingSelector::Job("b77"))
        .expect("a quiet waiting job")
}

fn tcp_wait(background: bool) -> World {
    let mut w = World::new();
    let mut leaf = Proc::new(LEAF, 0, "cargo");
    leaf.fds.push(Fd {
        fd: 5,
        kind: FdKind::Tcp {
            state: TcpState::Established,
            remote: Some("127.0.0.1:47811".parse().unwrap()),
            local_port: 0,
            queued_read: Some(0),
        },
    });
    if background {
        w.run_bg(1, leaf);
    } else {
        w.run_fg(1, 77, "cargo build", leaf);
    }
    for _ in 0..600 {
        let _ = w.tick(2_000);
    }
    w
}

#[test]
fn a_generic_read_or_unknown_stack_cannot_clear_or_cap_a_known_tcp_wait() {
    for word in [TierB::Read, TierB::Unparsed] {
        for background in [false, true] {
            let mut w = tcp_wait(background);
            let id = if background { "j7700" } else { "b77" };
            let t = w.jobs.naming_target(1, NamingSelector::Job(id)).unwrap();
            let before = w.observe();
            assert_eq!(before.row(id).unwrap().standing, Some(Standing::Capable));
            assert_eq!(before.row(id).unwrap().verdict, Verdict::Stuck);
            assert_eq!(w.jobs.accept_name(&t, word), Ok(()));
            for _ in 0..3 {
                let after = w.tick(2_000);
                let row = after.row(id).unwrap();
                assert_eq!(row.class, Class::Waiting);
                assert_eq!(row.wait.as_ref().unwrap().name, WaitName::Tcp);
                assert_eq!(row.standing, Some(Standing::Capable));
                assert_eq!(row.verdict, Verdict::Stuck);
                assert!(row.tick.progress.is_none());
                assert_eq!(row.named.as_ref().unwrap().word, word);
                let mut unlabelled = row.clone();
                unlabelled.named = None;
                assert_eq!(row.said(), unlabelled.said());
                assert!(w.jobs.name_is_shown(&t, word));
            }
        }
        let mut w = quiet(false);
        let t = ticket(&w);
        let _ = w.observe();
        assert_eq!(w.jobs.accept_name(&t, word), Ok(()));
        for _ in 0..3 {
            let after = w.tick(2_000);
            let row = after.row("b77").unwrap();
            assert_eq!(row.wait.as_ref().unwrap().name, WaitName::Unnamed);
            assert_eq!(row.standing, Some(Standing::Capped));
            assert_eq!(row.verdict, Verdict::Quiet);
        }
    }
}

#[test]
fn a_sampled_read_does_not_replace_a_later_genuine_terminal_wait() {
    for word in [TierB::Read, TierB::Unparsed] {
        let mut sampled = tcp_wait(false);
        let mut control = tcp_wait(false);
        let t = ticket(&sampled);
        let _ = sampled.observe();
        let _ = control.observe();
        assert_eq!(sampled.jobs.accept_name(&t, word), Ok(()));
        for w in [&mut sampled, &mut control] {
            w.p(LEAF).fds.clear();
            w.tty_mode(1, false, true);
        }
        for _ in 0..3 {
            let actual = sampled.tick(2_000);
            let plain = control.tick(2_000);
            let actual = actual.row("b77").unwrap();
            let plain = plain.row("b77").unwrap();
            assert_eq!(actual.class, plain.class);
            assert_eq!(actual.standing, plain.standing);
            assert_eq!(actual.verdict, plain.verdict);
            assert_eq!(
                actual.wait.as_ref().unwrap().name,
                plain.wait.as_ref().unwrap().name
            );
            assert!(actual.tick.progress.is_none());
            assert_eq!(actual.named.as_ref().unwrap().word, word);
        }
        let final_row = sampled.observe();
        let final_row = final_row.row("b77").unwrap();
        assert_eq!(final_row.wait.as_ref().unwrap().name, WaitName::TtyRaw);
        assert_eq!(final_row.standing, Some(Standing::Legit));
        assert_eq!(final_row.verdict, Verdict::Fine);
    }
}

#[test]
fn naming_requires_a_fresh_probe_and_publishes_only_after_acceptance() {
    for polling in [false, true] {
        let mut w = quiet(polling);
        let t = ticket(&w);
        assert_eq!((t.tab(), t.job(), t.waiter().pid), (1, "b77", LEAF));
        assert_eq!(t.waiter().exec_version, Some(1));
        assert_eq!(w.jobs.accept_name(&t, TierB::Lock), Err(NamingError::Stale));
        let before = w.observe();
        assert_eq!(
            before.row("b77").unwrap().class,
            if polling {
                Class::Polling
            } else {
                Class::Waiting
            }
        );
        assert_eq!(w.jobs.accept_name(&t, TierB::Lock), Ok(()));
        assert!(!w.jobs.name_is_shown(&t, TierB::Lock));
        assert_eq!(
            w.jobs.accept_name(&t, TierB::Timer),
            Err(NamingError::AlreadyNamed)
        );
        let after = w.observe();
        let row = after.row("b77").unwrap();
        assert_eq!(row.class, Class::Waiting);
        assert!(row.tick.progress.is_none());
        let named = row.named.as_ref().unwrap();
        assert_eq!(named.word, TierB::Lock);
        assert!(named.holder.is_none());
        assert!(w.jobs.name_is_shown(&t, TierB::Lock));
        assert_eq!(
            w.jobs
                .naming_target(1, NamingSelector::Job("b77"))
                .unwrap_err(),
            NamingError::AlreadyNamed
        );
    }
}

#[test]
fn a_non_lock_name_is_published_without_weakening_the_polling_verdict() {
    for word in [
        TierB::Timer,
        TierB::Read,
        TierB::Event,
        TierB::Ipc,
        TierB::Mutex,
        TierB::Children,
        TierB::Unparsed,
    ] {
        let mut w = quiet(true);
        let t = ticket(&w);
        let before = w.observe();
        assert_eq!(w.jobs.accept_name(&t, word), Ok(()));
        let after = w.observe();
        let row = after.row("b77").unwrap();
        assert_eq!(row.class, Class::Polling);
        assert_eq!(row.standing, before.row("b77").unwrap().standing);
        assert_eq!(row.verdict, before.row("b77").unwrap().verdict);
        assert!(row.tick.progress.is_none());
        assert_eq!(row.named.as_ref().unwrap().word, word);
        assert!(row.named.as_ref().unwrap().holder.is_none());
        assert!(w.jobs.name_is_shown(&t, word));
        let mut unlabelled = row.clone();
        unlabelled.named = None;
        assert_eq!(row.said(), unlabelled.said());
    }
}

#[test]
fn a_name_ticket_does_not_survive_leaf_identity_loss_or_an_intermediate_exec() {
    for change in 0..4 {
        let mut w = quiet(false);
        let t = ticket(&w);
        match change {
            0 => w.p(LEAF).exec += 1,
            1 => w.p(LEAF).start += 1,
            2 => w.p(LEAF).exe = Some("another-program".into()),
            _ => {
                w.unread.insert(LEAF);
            }
        }
        let _ = w.observe();
        assert_eq!(w.jobs.accept_name(&t, TierB::Lock), Err(NamingError::Stale));
        // Returning to the old reading cannot revive a ticket whose intervening probe differed.
        match change {
            0 => w.p(LEAF).exec -= 1,
            1 => w.p(LEAF).start -= 1,
            2 => w.p(LEAF).exe = None,
            _ => {
                w.unread.remove(&LEAF);
            }
        }
        let _ = w.observe();
        assert_eq!(w.jobs.accept_name(&t, TierB::Lock), Err(NamingError::Stale));
    }
}

#[test]
fn a_different_waiting_member_or_job_cannot_take_an_old_ticket() {
    let mut w = quiet(false);
    let t = ticket(&w);
    let mut child = Proc::new(LEAF + 1, LEAF, "cargo");
    child.pgid = LEAF;
    child.sid = 200;
    child.tty = true;
    w.add(child);
    let _ = w.observe();
    w.exit(LEAF + 1, false);
    let _ = w.observe();
    assert_eq!(w.jobs.accept_name(&t, TierB::Lock), Err(NamingError::Stale));

    w.close_block(1, 0);
    let _ = w.observe();
    w.run_fg(1, 77, "cargo build", Proc::new(LEAF, 0, "cargo"));
    settle(&mut w);
    assert_eq!(w.jobs.accept_name(&t, TierB::Lock), Err(NamingError::Stale));

    let mut another_engine = quiet(false);
    let _ = another_engine.observe();
    assert_eq!(
        another_engine.jobs.accept_name(&t, TierB::Lock),
        Err(NamingError::Stale)
    );
}

#[test]
fn progress_rejects_pending_names_and_exec_expires_an_accepted_name_without_output() {
    let mut w = quiet(false);
    let t = ticket(&w);
    let _ = w.observe();
    assert_eq!(w.jobs.accept_name(&t, TierB::Lock), Ok(()));
    let _ = w.observe();
    assert!(w.jobs.name_is_shown(&t, TierB::Lock));
    // The program identity moves while its output and every clock stay unchanged.
    w.p(LEAF).exec += 1;
    let changed = w.observe();
    assert!(changed.row("b77").unwrap().tick.progress.is_none());
    assert!(changed.row("b77").unwrap().named.is_none());
    assert!(!w.jobs.name_is_shown(&t, TierB::Lock));
    let new_program = ticket(&w);
    assert_ne!(new_program.waiter().exec_version, t.waiter().exec_version);
    let _ = w.observe();
    assert_eq!(w.jobs.accept_name(&t, TierB::Lock), Err(NamingError::Stale));
    assert_eq!(w.jobs.accept_name(&new_program, TierB::Lock), Ok(()));
    let _ = w.observe();
    assert!(w.jobs.name_is_shown(&new_program, TierB::Lock));

    let mut w = quiet(false);
    let t = ticket(&w);
    let at = w.at;
    for _ in 0..2 {
        w.files.get_mut(&FILE).unwrap().size += 10;
        let changed = w.observe();
        assert!(changed.row("b77").unwrap().tick.progress.is_some());
        assert_eq!(w.at, at);
        assert_eq!(w.jobs.accept_name(&t, TierB::Lock), Err(NamingError::Stale));
    }
    settle(&mut w);
    assert_eq!(w.jobs.accept_name(&t, TierB::Lock), Err(NamingError::Stale));
    let fresh = ticket(&w);
    let _ = w.observe();
    assert_eq!(w.jobs.accept_name(&fresh, TierB::Lock), Ok(()));
}

#[test]
fn naming_excludes_running_stopped_input_and_exited_jobs() {
    for state in 0..4 {
        let mut w = quiet(false);
        let t = ticket(&w);
        match state {
            0 => {
                w.advance(2_000);
                w.line(1);
            }
            1 => w.p(LEAF).state = ProcState::Stopped,
            2 => w.gate(1, GateWord::Stalled, 1, 10_000),
            _ => w.close_block(1, 0),
        }
        let _ = w.observe();
        assert_eq!(w.jobs.accept_name(&t, TierB::Lock), Err(NamingError::Stale));
        assert!(w.jobs.naming_target(1, NamingSelector::Job("b77")).is_err());
    }
}

#[test]
fn task_naming_is_tab_scoped_and_rejects_ambiguous_live_calls() {
    let mut w = World::new();
    w.call(
        1,
        300,
        4000,
        Proc::new(4001, 0, "cargo"),
        Some(hint(4000, Consumer::AgentFg, Some("task-one"), None)),
    );
    settle(&mut w);
    let t = w
        .jobs
        .naming_target(1, NamingSelector::Task("task-one"))
        .unwrap();
    assert_eq!((t.job(), t.waiter().pid), ("a4000", 4001));
    assert_eq!(
        w.jobs
            .naming_target(2, NamingSelector::Task("task-one"))
            .unwrap_err(),
        NamingError::Gone
    );
    w.call(
        1,
        300,
        5000,
        Proc::new(5001, 0, "cargo"),
        Some(hint(5000, Consumer::AgentFg, Some("task-one"), None)),
    );
    settle(&mut w);
    assert_eq!(
        w.jobs
            .naming_target(1, NamingSelector::Task("task-one"))
            .unwrap_err(),
        NamingError::Ambiguous
    );
}
