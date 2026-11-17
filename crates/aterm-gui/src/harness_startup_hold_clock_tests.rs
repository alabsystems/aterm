// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::task_record::{TaskMark, TaskRecord, TaskState};

struct ClaimHoldWorld {
    store: Store,
    worker: crate::session_store::SessionHandle,
    start: Instant,
    committed: Arc<AtomicUsize>,
}

impl ClaimHoldWorld {
    fn new(agent: Program) -> Self {
        let store = crate::session_store::new_store();
        let worker = crate::session_store::test_handle(7640);
        let start = Instant::now() - 3 * STARTUP_DEADLINE;
        {
            let mut timeline = worker.ctx.timeline.lock().unwrap();
            timeline.mark_task(
                &TaskMark::new(TaskState::Working),
                &Default::default(),
                start,
                0,
            );
            timeline.set_task_worker(agent, None);
            timeline.note_foreground_group(78);
            timeline.set_program(78, Some(agent.name().to_string()));
        }
        worker
            .term
            .lock()
            .unwrap()
            .process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        store.write().unwrap().register(worker.clone());
        Self {
            store,
            worker,
            start,
            committed: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn hooks(&self) -> Hooks {
        let world = Arc::new(World::default());
        let mut hooks = hooks(&world, parking_body(&world));
        let reads = Arc::clone(&self.store);
        let marks = Arc::clone(&self.store);
        let facts = Arc::clone(&self.store);
        let committed = Arc::clone(&self.committed);
        hooks.acts = Acts {
            startups: Arc::new(move |judged| startups_of(&reads, judged)),
            startup_mark: Arc::new(move |worker, mark| {
                let marked = startup_mark_named(&marks, worker, mark, || None).is_some();
                if marked {
                    committed.fetch_add(1, Ordering::SeqCst);
                }
                marked
            }),
            startup_took_up: Arc::new(move |worker| startup_took_up_live(&facts, worker)),
            ..Acts::inert()
        };
        hooks
    }

    /// The session's record and attention as `before` stood, its start now noted on the
    /// record ([`TaskRecord::took_up`]): a fact, which moves no rev.
    fn noted_started(before: &(TaskRecord, Option<String>)) -> (TaskRecord, Option<String>) {
        (
            TaskRecord {
                took_up: true,
                ..before.0.clone()
            },
            before.1.clone(),
        )
    }

    fn read(&self) -> Startup {
        startups_of(&self.store, &HashSet::new())
            .into_iter()
            .next()
            .expect("the actual session's startup read")
    }

    fn state(&self) -> (TaskRecord, Option<String>) {
        let record = self
            .worker
            .ctx
            .timeline
            .lock()
            .unwrap()
            .task()
            .cloned()
            .unwrap();
        let attention = self
            .worker
            .ctx
            .meta
            .lock()
            .unwrap()
            .attention_owners
            .get(ATTENTION_OWNER)
            .map(str::to_string);
        (record, attention)
    }

    fn set_hold(&self) {
        assert!(crate::fabric::apply_hold_for_test(
            &self.worker.ctx,
            Some(crate::fabric::Hold {
                reason: "keep this session".to_string(),
                origin: "fleet".to_string(),
            }),
        ));
    }

    fn lift_hold(&self) -> Instant {
        assert!(crate::fabric::apply_hold_for_test(&self.worker.ctx, None));
        let hold = self.worker.ctx.fabric.hold_observation();
        assert!(!hold.is_held());
        hold.released_at().expect("the actual aggregate release")
    }

    fn coalesced_hold(&self) -> Instant {
        self.set_hold();
        self.lift_hold()
    }

    fn claim(&self) {
        crate::session_timeline::claim_supervisor_as(
            &self.worker.ctx,
            &format!("codex-alab@{}", std::process::id()),
            Some(1),
            None,
            Some(crate::self_claim::stand_in::running()),
            crate::metrics::now_us(),
            aterm_slog::Actor::Ctl,
        )
        .expect("the actual session's proven claim");
    }

    fn look(&self, books: &mut Startups, now: Instant, hooks: &Hooks) -> Option<Instant> {
        look_at_startups(books, now, true, &OffSessions::default(), hooks)
    }

    fn commits(&self) -> usize {
        self.committed.load(Ordering::SeqCst)
    }
}

#[test]
fn a_coalesced_hold_release_gives_an_unclaimed_codex_its_whole_startup_deadline_once() {
    let world = ClaimHoldWorld::new(Program::Codex);
    let hooks = world.hooks();
    let mut books = Startups::default();
    let before = world.state();
    let first = world.read();
    assert!(first.claim_hold.is_some_and(|hold| !hold.is_held()));
    assert!(matches!(first.seen, StartupSeen::Holds(_)));
    let old_due = world.start + STARTUP_DEADLINE;
    assert_eq!(world.look(&mut books, world.start, &hooks), Some(old_due));

    // No startup look occurs between these two real fabric mutations. The wakes may coalesce.
    let released = world.coalesced_hold();
    assert!(old_due < released, "the old deadline has already passed");
    let due = released + STARTUP_DEADLINE;
    for offset in [
        Duration::ZERO,
        STARTUP_DEADLINE / 3,
        2 * STARTUP_DEADLINE / 3,
    ] {
        assert_eq!(world.look(&mut books, released + offset, &hooks), Some(due));
        assert_eq!(
            world.state(),
            before,
            "reads must neither stall nor slide the deadline"
        );
        assert_eq!(world.commits(), 0);
    }
    assert_eq!(
        world.read().claim_hold.unwrap().released_at(),
        Some(released)
    );

    world.look(&mut books, due, &hooks);
    let stalled = world.state();
    assert_eq!(stalled.0.state, TaskState::Stalled);
    assert_eq!(stalled.0.why, Some("startup"));
    assert_eq!(stalled.0.rev, before.0.rev + 1);
    assert!(
        stalled
            .1
            .is_some_and(|attention| attention.starts_with(STARTUP_HEAD))
    );
    assert_eq!(world.commits(), 1);
    world.look(&mut books, due + STARTUP_DEADLINE, &hooks);
    assert_eq!(world.state().0, stalled.0);
    assert_eq!(world.commits(), 1, "the deadline commits only once");
}

#[test]
fn a_hold_set_or_set_and_lifted_during_a_stall_decision_changes_no_task_or_attention() {
    for lift_before_mark in [false, true] {
        let world = ClaimHoldWorld::new(Program::Codex);
        let hooks = world.hooks();
        let mut books = Startups::default();
        world.look(&mut books, world.start, &hooks);
        let before = world.state();
        let captured = world.read();
        let StartupSeen::Holds(hold) = &captured.seen else {
            panic!("an unclaimed Codex awaiting its start");
        };
        let marked = startup_mark_named(
            &world.store,
            &captured,
            &StartupMark::Stalled { hold },
            || {
                // The existing member-label seam runs after the read, before the final mark.
                world.set_hold();
                if lift_before_mark {
                    world.lift_hold();
                }
                None
            },
        );
        assert!(marked.is_none(), "the decision's hold history is obsolete");
        assert_eq!(
            world.state(),
            before,
            "both record and attention stay untouched"
        );

        let released = if lift_before_mark {
            world
                .worker
                .ctx
                .fabric
                .hold_observation()
                .released_at()
                .unwrap()
        } else {
            assert_eq!(world.read().seen, StartupSeen::CannotClaim);
            world.look(&mut books, Instant::now(), &hooks);
            assert_eq!(world.state(), before, "a still-held claim is not judged");
            world.lift_hold()
        };
        let refreshed = world.read();
        assert_ne!(refreshed.claim_hold, captured.claim_hold);
        let due = released + STARTUP_DEADLINE;
        assert_eq!(world.look(&mut books, released, &hooks), Some(due));
        assert_eq!(
            world.state(),
            before,
            "a fresh read starts a whole deadline"
        );
        world.look(&mut books, due, &hooks);
        let stalled = world.state().0;
        assert_eq!(
            (stalled.state, stalled.why),
            (TaskState::Stalled, Some("startup"))
        );
        assert_eq!(stalled.rev, before.0.rev + 1);
        assert_eq!(world.commits(), 1);
    }
}

#[test]
fn autopilot_off_during_a_stall_decision_changes_no_task_or_attention() {
    let world = ClaimHoldWorld::new(Program::Codex);
    let hooks = world.hooks();
    let mut books = Startups::default();
    world.look(&mut books, world.start, &hooks);
    let before = world.state();
    let captured = world.read();
    let observation = world.worker.ctx.fabric.hold_observation();
    assert_eq!(captured.claim_hold, Some(observation));
    let StartupSeen::Holds(hold) = &captured.seen else {
        panic!("an awaited Codex before Autopilot goes Off");
    };
    let marked = startup_mark_named(
        &world.store,
        &captured,
        &StartupMark::Stalled { hold },
        || {
            assert!(
                world
                    .worker
                    .ctx
                    .meta
                    .lock()
                    .unwrap()
                    .set_autopilot(Some("off".to_string()), None,)
            );
            None
        },
    );
    assert!(marked.is_none());
    assert_eq!(world.worker.ctx.fabric.hold_observation(), observation);
    assert_eq!(
        world.state(),
        before,
        "Off changes neither task revision nor attention"
    );
    let fresh = world.read();
    assert_eq!(fresh.record_rev, captured.record_rev);
    assert_eq!(fresh.seen, StartupSeen::CannotClaim);
    world.look(&mut books, Instant::now(), &hooks);
    assert_eq!(
        world.state(),
        before,
        "the expired startup deadline judges no Off claim"
    );
    assert_eq!(world.commits(), 0);
}

#[test]
fn a_proven_claim_during_a_stall_decision_changes_no_task_or_attention() {
    let world = ClaimHoldWorld::new(Program::Codex);
    let hooks = world.hooks();
    let mut books = Startups::default();
    world.look(&mut books, world.start, &hooks);
    let before = world.state();
    let captured = world.read();
    let observation = world.worker.ctx.fabric.hold_observation();
    assert_eq!(captured.claim_hold, Some(observation));
    let StartupSeen::Holds(hold) = &captured.seen else {
        panic!("an awaited Codex before its proven claim");
    };
    let marked = startup_mark_named(
        &world.store,
        &captured,
        &StartupMark::Stalled { hold },
        || {
            world.claim();
            None
        },
    );
    assert!(marked.is_none());
    assert_eq!(world.worker.ctx.fabric.hold_observation(), observation);
    assert_eq!(
        world.state(),
        before,
        "the claim changes neither task revision nor attention"
    );
    assert!(
        world
            .worker
            .ctx
            .meta
            .lock()
            .unwrap()
            .supervisor_proven(crate::metrics::now_us())
    );
    let fresh = world.read();
    assert_eq!(fresh.record_rev, captured.record_rev);
    assert_eq!(fresh.seen, StartupSeen::Started);
    assert!(fresh.claim_hold.is_none());
    let now = Instant::now();
    assert_eq!(world.look(&mut books, now, &hooks), None);
    assert_eq!(world.look(&mut books, now + STARTUP_DEADLINE, &hooks), None);
    assert_eq!(
        world.state(),
        ClaimHoldWorld::noted_started(&before),
        "the claim's start is noted on the record, and no mark is made"
    );
    assert_eq!(world.commits(), 0);
}

#[test]
fn a_stall_read_from_another_launch_cannot_use_the_same_hold_generation() {
    let world = ClaimHoldWorld::new(Program::Codex);
    let before = world.state();
    let captured = world.read();
    let mut foreign = captured.clone();
    let zero = aterm_session::LaunchNonce::from_bytes([0; 16]);
    foreign.nonce = if captured.nonce == zero {
        aterm_session::LaunchNonce::from_bytes([1; 16])
    } else {
        zero
    };
    assert_ne!(foreign.nonce, captured.nonce);
    let StartupSeen::Holds(hold) = &captured.seen else {
        panic!("an awaited Codex");
    };
    assert_eq!(foreign.record_rev, captured.record_rev);
    assert_eq!(foreign.claim_hold, captured.claim_hold);
    assert!(
        startup_mark_named(
            &world.store,
            &foreign,
            &StartupMark::Stalled { hold },
            || None,
        )
        .is_none()
    );
    assert_eq!(
        world.state(),
        before,
        "a matching hold history cannot cross launches"
    );
    assert!(
        startup_mark_named(
            &world.store,
            &captured,
            &StartupMark::Stalled { hold },
            || None,
        )
        .is_some(),
        "the actual launch is the positive control"
    );
}

#[test]
fn a_fabric_release_does_not_restart_shell_claude_or_launch_pool_deadlines() {
    for context in ["shell", "claude", "pool"] {
        let world = ClaimHoldWorld::new(if context == "claude" {
            Program::Claude
        } else {
            Program::Codex
        });
        {
            let mut timeline = world.worker.ctx.timeline.lock().unwrap();
            match context {
                "shell" => {
                    timeline.set_program(78, Some("zsh".to_string()));
                }
                "pool" => {
                    timeline.set_worker_pool_hold(Some(crate::session_timeline::WorkerPoolHold {
                        until: 4_102_444_800,
                        because: "the launch pool has no available account",
                    }))
                }
                "claude" => {}
                _ => unreachable!(),
            }
        }
        let hooks = world.hooks();
        let mut books = Startups::default();
        assert!(world.read().claim_hold.is_none(), "{context}");
        assert_eq!(
            world.look(&mut books, world.start, &hooks),
            Some(world.start + STARTUP_DEADLINE)
        );
        let released = world.coalesced_hold();
        assert!(world.read().claim_hold.is_none(), "{context}");
        world.look(&mut books, released, &hooks);
        let stalled = world.state().0;
        assert_eq!(
            (stalled.state, stalled.why),
            (TaskState::Stalled, Some("startup")),
            "{context}"
        );
        assert_eq!(world.commits(), 1, "{context} keeps its original deadline");
    }
}

#[test]
fn a_fabric_release_does_not_replace_an_installs_own_quiet_clock() {
    let world = ClaimHoldWorld::new(Program::Codex);
    {
        let mut timeline = world.worker.ctx.timeline.lock().unwrap();
        timeline.set_program(78, Some("zsh".to_string()));
        timeline.set_worker_installing(true);
    }
    let hooks = world.hooks();
    let mut books = Startups::default();
    let before = world.state();
    assert!(world.read().claim_hold.is_none());
    world.look(&mut books, world.start, &hooks);
    let quiet_due = world.start + INSTALL_QUIET_LIMIT;
    let released = world.coalesced_hold();
    assert!(released < quiet_due);
    assert!(world.read().claim_hold.is_none());
    assert_eq!(
        world.look(&mut books, released, &hooks),
        Some(quiet_due.min(released + HELD_RELOOK)),
    );
    assert_eq!(books.installs[world.worker.sid.as_str()].since, world.start);
    assert_eq!(world.state(), before);
    world.look(&mut books, quiet_due, &hooks);
    assert_eq!(world.state().0.state, TaskState::Stalled);
    assert_eq!(world.commits(), 1);
}

#[test]
fn a_carried_or_already_started_codex_has_no_claim_hold_deadline() {
    for carried in [false, true] {
        let world = ClaimHoldWorld::new(Program::Codex);
        if carried {
            world.claim();
        } else {
            world.worker.ctx.timeline.lock().unwrap().publish_agent(
                "busy",
                None,
                None,
                Some(Program::Codex),
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                    fp: 1,
                },
            );
        }
        let hooks = world.hooks();
        let mut books = Startups::default();
        if !carried {
            books.started.insert(world.worker.sid.as_str().to_string());
        }
        let before = world.state();
        let released = world.coalesced_hold();
        let read = startups_of(&world.store, &books.started)
            .into_iter()
            .next()
            .unwrap();
        assert!(read.claim_hold.is_none(), "carried={carried}");
        assert_eq!(world.look(&mut books, released, &hooks), None);
        assert_eq!(
            world.look(&mut books, released + STARTUP_DEADLINE, &hooks),
            None
        );
        assert_eq!(
            world.state(),
            ClaimHoldWorld::noted_started(&before),
            "carried={carried}: its start is noted on the record, and no mark is made"
        );
        assert_eq!(world.commits(), 0);
    }
}

/// Codex's at-rest report before any turn: it begins none.
const CODEX_IDLE: &[u8] = b"\x1b]7501;state=idle:app=codex\x1b\\";
/// Codex's report of a turn running.
const CODEX_WORKING: &[u8] = b"\x1b]7501;state=working:app=codex\x1b\\";
/// Codex's report of its turn ended with its work done.
const CODEX_DONE: &[u8] = b"\x1b]7501;state=done:app=codex\x1b\\";

/// As measured on m79: a codex-alab worker that held no claim was marked `stalled why=startup` at
/// its deadline and read so through a 20-hour turn. A codex whose own reports began a turn has
/// started, claim or none: the stall comes down and the record reads `working`, and a worker whose
/// turn began before its deadline is not stalled while its screen moves. A codex at rest that
/// begins no turn is still stalled at its start (the control), and a started one is the held
/// deadline's, which judges a worker no claim carries: once its screen stops it is `stalled
/// why=held`, not left `working`.
#[test]
fn an_unclaimed_codex_whose_own_reports_began_a_turn_reads_working() {
    let world = ClaimHoldWorld::new(Program::Codex);
    world.worker.term.lock().unwrap().process(CODEX_IDLE);
    let hooks = world.hooks();
    let mut books = Startups::default();
    let before = world.state();
    let due = world.start + STARTUP_DEADLINE;
    assert_eq!(world.look(&mut books, world.start, &hooks), Some(due));
    world.look(&mut books, due, &hooks);
    let (stalled, attention) = world.state();
    assert_eq!(
        (stalled.state, stalled.why, stalled.rev),
        (TaskState::Stalled, Some("startup"), before.0.rev + 1),
        "CONTROL: a codex at rest with no claim and no turn never started"
    );
    assert!(attention.is_some_and(|said| said.starts_with(STARTUP_HEAD)));

    world.worker.term.lock().unwrap().process(CODEX_WORKING);
    let read = world.read();
    assert_eq!(
        (read.state, read.seen),
        (StartupState::StalledAtStartup, StartupSeen::Started),
        "its own report began its task's turn"
    );
    world.look(&mut books, due + HELD_RELOOK, &hooks);
    let (working, attention) = world.state();
    assert_eq!(
        (working.state, working.why, attention),
        (TaskState::Working, None, None),
        "the stall it ran past comes down"
    );
    assert_eq!(world.commits(), 2);
    let held = startups_of(&world.store, &books.started)
        .into_iter()
        .next()
        .expect("the worker's read")
        .held
        .expect("a started worker's held read");
    assert!(!held.carried && held.may_be_held(), "{held:?}");
    assert!(
        !world.state().0.took_up,
        "PRECONDITION: the look that ended the stall read it stalled, and noted no start"
    );
    let (ran, _) = world.state();
    world.look(&mut books, due + 2 * HELD_RELOOK, &hooks);
    let (noted, _) = world.state();
    assert_eq!(
        noted,
        TaskRecord {
            took_up: true,
            ..ran
        },
        "the next look reads it working and notes its start, moving no rev"
    );
    assert_eq!(world.commits(), 2, "a note is no mark");

    let fresh = ClaimHoldWorld::new(Program::Codex);
    fresh.worker.term.lock().unwrap().process(CODEX_WORKING);
    let hooks = fresh.hooks();
    let mut books = Startups::default();
    let before = fresh.state();
    // Each status sweep reads its screen moved (a new fingerprint of the live zone).
    let moved = |n: u32| {
        fresh.worker.ctx.timeline.lock().unwrap().note_agent_stamp(
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen {
                    epoch: 1,
                    seq: u64::from(n),
                },
                fp: u64::from(n),
            },
        );
    };
    for n in 0..4 {
        moved(n);
        fresh.look(&mut books, fresh.start + n * STARTUP_DEADLINE, &hooks);
    }
    assert_eq!(
        fresh.state(),
        ClaimHoldWorld::noted_started(&before),
        "a worker whose turn began, its screen moving, never stalls: its start is only noted"
    );
    assert_eq!(fresh.commits(), 0);
    assert!(books.started.contains(fresh.worker.sid.as_str()));
    fresh.look(
        &mut books,
        fresh.start + 3 * STARTUP_DEADLINE + HELD_DEADLINE,
        &hooks,
    );
    let (stopped, _) = fresh.state();
    assert_eq!(
        (stopped.state, stopped.why),
        (TaskState::Stalled, Some("held")),
        "its screen stopped: the held deadline's stall"
    );
}

/// [`codex_took_a_turn`] reads a turn codex's own reports began, running or ended; never codex at
/// rest, nor another program's turn.
#[test]
fn a_codex_turn_is_read_from_codex_reports_alone() {
    let took = |reports: &[&[u8]]| {
        let mut term = aterm_core::terminal::Terminal::new(24, 80);
        for report in reports {
            term.process(report);
        }
        codex_took_a_turn(&term)
    };
    assert!(!took(&[]), "no report");
    assert!(!took(&[CODEX_IDLE]), "at rest before any turn");
    assert!(
        !took(&[b"\x1b]7501;state=working:app=claude-code\x1b\\"]),
        "another program's turn"
    );
    assert!(took(&[CODEX_IDLE, CODEX_WORKING]), "a turn running");
    assert!(
        took(&[CODEX_WORKING, CODEX_IDLE]),
        "a turn that has ended at rest"
    );
}

/// A seamless update's successor restores each terminal from its checkpoint, which carries the
/// report standing and no turn backlog: `terminal` after `reports`, as the successor holds it.
fn restored_after(reports: &[&[u8]]) -> aterm_core::terminal::Terminal {
    let mut term = aterm_core::terminal::Terminal::new(24, 80);
    for report in reports {
        term.process(report);
    }
    aterm_core::terminal::Terminal::from_checkpoint(&term.checkpoint())
}

/// AFTER A SEAMLESS UPDATE, A CODEX AT REST AFTER ITS TURN HAS STARTED. The successor's
/// startup books are empty, so it judges every `working` record's start again, and the restore
/// keeps no turn, only the report standing: codex's `done` (or `error`), which it says only after
/// a turn and stands at until a key acknowledges it, is that start. An unclaimed codex worker
/// whose turn ended before the update reads started in the successor and is never marked
/// `stalled why=startup` ("never claimed its tab") for a task it ran. CONTROL: a codex restored
/// at rest before any turn (`idle`) has not started, and another program's `done` is not codex's.
#[test]
fn a_codex_at_rest_after_its_turn_has_started_across_a_seamless_update() {
    let cases: [(&[&[u8]], bool); 4] = [
        (&[CODEX_WORKING, CODEX_DONE], true),
        (
            &[CODEX_WORKING, b"\x1b]7501;state=error:app=codex\x1b\\"],
            true,
        ),
        (&[CODEX_IDLE], false),
        (
            &[
                b"\x1b]7501;state=working:app=claude-code\x1b\\",
                b"\x1b]7501;state=done:app=claude-code\x1b\\",
            ],
            false,
        ),
    ];
    for (reports, took) in cases {
        let term = restored_after(reports);
        assert_eq!(
            term.program_turns().iter().count(),
            0,
            "PRECONDITION: the restore keeps no turn at rest"
        );
        let said: Vec<_> = reports.iter().map(|r| String::from_utf8_lossy(r)).collect();
        assert_eq!(codex_took_a_turn(&term), took, "{said:?}");
    }

    // The successor of an unclaimed codex worker whose turn ended: fresh books, its screen
    // moving, no stall at its startup deadline.
    let world = ClaimHoldWorld::new(Program::Codex);
    *world.worker.term.lock().unwrap() = restored_after(&[CODEX_WORKING, CODEX_DONE]);
    let read = world.read();
    assert_eq!(
        (read.state, read.seen),
        (StartupState::Working, StartupSeen::Started),
        "its own standing report says its turn ran"
    );
    let hooks = world.hooks();
    let mut books = Startups::default();
    let before = world.state();
    for n in 0..2 {
        world.worker.ctx.timeline.lock().unwrap().note_agent_stamp(
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen {
                    epoch: 1,
                    seq: u64::from(n),
                },
                fp: u64::from(n),
            },
        );
        world.look(&mut books, world.start + n * STARTUP_DEADLINE, &hooks);
    }
    assert_eq!(
        world.state(),
        ClaimHoldWorld::noted_started(&before),
        "never stalled at its start, which is noted on its record"
    );
    assert_eq!(world.commits(), 0);
    assert!(books.started.contains(world.worker.sid.as_str()));
}

/// AFTER A SEAMLESS UPDATE, A CODEX AT REST AFTER A TURN A KEY ACKNOWLEDGED HAS STARTED. Its
/// report stands at `idle` again, which the restore installs with no turn, so nothing on the
/// successor's screen says it started and its startup books are empty. The predecessor's deadline
/// judged the start when codex's report began the turn and noted it on the task record
/// (`took_up`), which the carry row takes across: the successor never marks the worker `stalled
/// why=startup` while its screen moves, and its held deadline still judges it, so once its screen
/// stops it is `stalled why=held`. CONTROL: the same successor whose row does not carry that
/// judgment (an older predecessor's) is marked `stalled why=startup` at its deadline, for a task
/// it ran.
#[test]
fn a_codex_at_rest_after_an_acknowledged_turn_keeps_its_start_across_a_seamless_update() {
    // The predecessor: an unclaimed codex worker whose own report began its turn, looked at.
    let predecessor = ClaimHoldWorld::new(Program::Codex);
    predecessor
        .worker
        .term
        .lock()
        .unwrap()
        .process(CODEX_WORKING);
    let hooks = predecessor.hooks();
    let mut books = Startups::default();
    let before = predecessor.state();
    predecessor.look(&mut books, predecessor.start, &hooks);
    assert!(books.started.contains(predecessor.worker.sid.as_str()));
    assert_eq!(
        predecessor.state(),
        ClaimHoldWorld::noted_started(&before),
        "the look notes the start it judged on the record, a fact that moves no rev"
    );
    assert_eq!(predecessor.commits(), 0);
    let row = predecessor
        .worker
        .ctx
        .timeline
        .lock()
        .unwrap()
        .task_carry_row()
        .expect("the record's carry row");
    assert!(
        row.split_whitespace().any(|word| word == "took_up=1"),
        "{row}"
    );
    let older = row.replace(" took_up=1", "");

    for (carried, row) in [(true, row.as_str()), (false, older.as_str())] {
        // The successor: the carried record, and the terminal restored at rest after the turn.
        let successor = ClaimHoldWorld::new(Program::Codex);
        crate::task_record::seed_carried(&successor.worker.ctx, Some(row));
        *successor.worker.term.lock().unwrap() =
            restored_after(&[CODEX_WORKING, CODEX_DONE, CODEX_IDLE]);
        assert!(
            !codex_took_a_turn(&successor.worker.term.lock().unwrap()),
            "PRECONDITION: nothing on the restored screen says it started"
        );
        let before = successor.state();
        assert_eq!(before.0.took_up, carried, "{row}");
        let read = successor.read();
        assert_eq!((read.state, read.took_up), (StartupState::Working, carried));
        let hooks = successor.hooks();
        let mut books = Startups::default();
        let t0 = successor.start.max(read.since);
        let moved = |n: u32| {
            successor
                .worker
                .ctx
                .timeline
                .lock()
                .unwrap()
                .note_agent_stamp(crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen {
                        epoch: 1,
                        seq: u64::from(n),
                    },
                    fp: u64::from(n),
                });
        };
        if carried {
            assert_eq!(
                read.seen,
                StartupSeen::Unread,
                "not read for its start again"
            );
            for n in 0..4 {
                moved(n);
                successor.look(&mut books, t0 + n * STARTUP_DEADLINE, &hooks);
            }
            assert_eq!(
                successor.state(),
                before,
                "its screen moving, never stalled at a start its predecessor judged"
            );
            assert_eq!(successor.commits(), 0);
            assert!(books.started.contains(successor.worker.sid.as_str()));
            successor.look(
                &mut books,
                t0 + 3 * STARTUP_DEADLINE + HELD_DEADLINE,
                &hooks,
            );
            let (stopped, _) = successor.state();
            assert_eq!(
                (stopped.state, stopped.why),
                (TaskState::Stalled, Some("held")),
                "its screen stopped: the held deadline judges it, it is not exempt"
            );
        } else {
            assert!(
                matches!(read.seen, StartupSeen::Holds(_)),
                "CONTROL: read for its start, which nothing on its screen shows: {:?}",
                read.seen
            );
            moved(0);
            successor.look(&mut books, t0, &hooks);
            assert_eq!(successor.state(), before);
            moved(1);
            successor.look(&mut books, t0 + STARTUP_DEADLINE, &hooks);
            let (stalled, attention) = successor.state();
            assert_eq!(
                (stalled.state, stalled.why, stalled.rev),
                (TaskState::Stalled, Some("startup"), before.0.rev + 1),
                "CONTROL: without the carried judgment, a task it ran reads never started"
            );
            assert!(attention.is_some_and(|said| said.starts_with(STARTUP_HEAD)));
        }
    }
}

/// A START NOTE THE RECORD REFUSED IS WRITTEN AGAIN. The look that judged a codex worker started
/// notes it on the record ([`Acts::startup_took_up`]); where the note does not land (the record
/// moved past the rev the look read), the look arms a relook no later than [`HELD_RELOOK`] on,
/// and that look notes it. CONTROL: a note that lands is written by the first look, and the
/// next look asks for none again.
#[test]
fn a_refused_start_note_arms_a_relook_and_lands_at_the_next_look() {
    let control = ClaimHoldWorld::new(Program::Codex);
    control.worker.term.lock().unwrap().process(CODEX_WORKING);
    let before = control.state();
    let mut hooks = control.hooks();
    let live = Arc::clone(&hooks.acts.startup_took_up);
    let notes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = Arc::clone(&notes);
    hooks.acts.startup_took_up = Arc::new(move |worker| {
        count.fetch_add(1, Ordering::SeqCst);
        live(worker)
    });
    let mut books = Startups::default();
    control.look(&mut books, control.start, &hooks);
    assert_eq!(control.state(), ClaimHoldWorld::noted_started(&before));
    control.look(&mut books, control.start + HELD_RELOOK, &hooks);
    assert_eq!(
        notes.load(Ordering::SeqCst),
        1,
        "CONTROL: a record that carries its start is not noted again"
    );

    let world = ClaimHoldWorld::new(Program::Codex);
    world.worker.term.lock().unwrap().process(CODEX_WORKING);
    let mut hooks = world.hooks();
    let live = Arc::clone(&hooks.acts.startup_took_up);
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = Arc::clone(&asked);
    hooks.acts.startup_took_up =
        Arc::new(move |worker| count.fetch_add(1, Ordering::SeqCst) > 0 && live(worker));
    let mut books = Startups::default();
    let before = world.state();
    let next = world.look(&mut books, world.start, &hooks);
    assert_eq!(
        asked.load(Ordering::SeqCst),
        1,
        "the look noted its start once"
    );
    assert_eq!(
        world.state(),
        before,
        "the refused note left the record as it was"
    );
    assert!(
        next.is_some_and(|at| at <= world.start + HELD_RELOOK),
        "a relook for the note: {:?}",
        next.map(|at| at - world.start)
    );
    world.look(&mut books, world.start + HELD_RELOOK, &hooks);
    assert_eq!(asked.load(Ordering::SeqCst), 2);
    assert_eq!(
        world.state(),
        ClaimHoldWorld::noted_started(&before),
        "the next look notes it"
    );
    assert_eq!(world.commits(), 0, "a note is no mark");
}
