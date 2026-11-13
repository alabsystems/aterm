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
            ..Acts::inert()
        };
        hooks
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
    assert_eq!(world.state(), before);
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
        assert_eq!(world.state(), before);
        assert_eq!(world.commits(), 0);
    }
}
