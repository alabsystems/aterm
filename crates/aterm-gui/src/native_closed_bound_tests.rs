// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A closed-budget renewal worker whose successor never attaches ends once, after a bounded
//! number of continuations, and only once its last budget is closed and no person holds it;
//! then it hands the session back to the agent its Signal ended, once, unless someone took the
//! session since the end.

use super::*;
use std::sync::atomic::AtomicUsize;

/// How many continuations the negative control lets an unbounded worker drive before this
/// test stops it with an owner Stop, so a lost bound fails the test instead of hanging it.
const STORM: usize = 4;

/// How many passes past its last deadline a root must keep waiting while a successor its
/// continuation granted Execute still runs, or a person holds the session.
const WAITING: usize = 5;

/// How many hand-back steps another tab's sweep defers the hand-back: more than the 600 steps a
/// hand-back once counted before it left the session as it stood.
const DEFERRED: usize = 700;

/// The agent that takes the tab after the root ended, in [`Successor::ClaimedAtEnd`].
const ANOTHER: u32 = 5151;

/// What each continuation's launcher meets at the host.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Successor {
    /// Its Attach is refused, as the finding's host refused every one: no successor attaches.
    Refused,
    /// Its Attach is admitted, its Execute granted, and it is still running at its deadline,
    /// until it exits [`WAITING`] passes later without having registered.
    Running,
    /// Its Attach is refused, and a person places a hold on the session during that
    /// continuation, lifted [`WAITING`] passes past its deadline.
    Held,
    /// Its Attach is refused, and a person holds the session from the root's end on.
    HeldAtEnd,
    /// Its Attach is refused, and a person types in the bare shell once the root has ended.
    KeysAtEnd,
    /// Its Attach is refused, and once the root has ended another codex-alab's proven claim of
    /// the tab leaves its copy there, as one a person or a driver started does.
    ClaimedAtEnd,
    /// Its Attach is refused, and once the root has ended another process holds the
    /// conversation's writer lock, as one resumed in another tab does.
    ConversationTaken,
    /// Its Attach is refused, and another tab's relaunch holds the sweep for the first
    /// [`DEFERRED`] hand-back steps.
    SweepHeld,
}

/// A host that admits each continuation and meets its launcher as `successor` says. Its clock
/// is the world's: each drive spends a budget.
struct NeverAttached<'a> {
    world: &'a World,
    successor: Successor,
    admitted: Mutex<Vec<(String, Binding)>>,
    /// The phase each drive was handed to start from.
    latched: Mutex<Vec<ImagePhase>>,
    drives: AtomicUsize,
    passes: AtomicUsize,
    refused: AtomicUsize,
    /// Passes counted since a continuation's launcher started running or a hold was placed,
    /// while either still stands.
    waiting: Mutex<Option<usize>>,
    /// Whether the root ended while it should still have been waiting.
    ended_early: AtomicBool,
    stopped: Mutex<Vec<(String, u64)>>,
    /// The login each hand-back step relaunched the source's line on.
    handed: Mutex<Vec<Option<String>>>,
}

const LAUNCHER: u32 = 601;

impl<'a> NeverAttached<'a> {
    fn new(world: &'a World, successor: Successor) -> Self {
        Self {
            world,
            successor,
            admitted: Mutex::new(Vec::new()),
            latched: Mutex::new(Vec::new()),
            drives: AtomicUsize::new(0),
            passes: AtomicUsize::new(0),
            refused: AtomicUsize::new(0),
            waiting: Mutex::new(None),
            ended_early: AtomicBool::new(false),
            stopped: Mutex::new(Vec::new()),
            handed: Mutex::new(Vec::new()),
        }
    }

    fn stop(&self) {
        let _first = self
            .world
            .seal
            .cancel(recovery::CancellationKind::OwnerStop);
    }

    /// Type the continuation's latched phase, as the host types a Resume it admits.
    fn resume(&self, binding: &Binding, phase: ImagePhase) {
        let world = self.world;
        let ctx = &world.handle.ctx;
        let reply = serve_by(
            ctx,
            -1,
            Request::Resume(binding.clone(), phase, "1.1".into(), ctx.human_input.seq()),
            world.actor.clone(),
            &world.facts,
            &mut |_| {
                let mark = InputMark::at_epoch(
                    ctx,
                    world.handle.local_id,
                    aterm_slog::Actor::Ctl,
                    crate::session_log::InputVerb::Turn,
                    None,
                    true,
                );
                let producer = mark.native_producer();
                let completion = mark.completion();
                assert!(completion.start_supervised());
                let receipt = ctx.sink.write_frame_with_prefix(b"R").unwrap();
                assert!(
                    ctx.sink
                        .wait_prefix_to_kernel(receipt.kernel_prefix().unwrap())
                );
                completion.finish(EgressReceipt::from_reported_delivery(
                    Delivery::Full,
                    receipt.order(),
                ));
                drop(producer);
                "OK typed=1\n".into()
            },
        );
        assert_eq!(
            reply,
            format!("OK native-act state=resumed phase={}\n", phase.word())
        );
    }

    /// End what the root waits for: the running successor exits, or the hold lifts.
    fn release(&self) {
        let facts = &self.world.facts;
        match self.successor {
            Successor::Running => {
                facts.processes.lock().unwrap().remove(&LAUNCHER);
                facts.foreground.store(400, Ordering::SeqCst);
            }
            Successor::Held => {
                assert!(crate::fabric::apply_hold_for_test(
                    &self.world.handle.ctx,
                    None
                ));
            }
            Successor::Refused
            | Successor::HeldAtEnd
            | Successor::KeysAtEnd
            | Successor::ClaimedAtEnd
            | Successor::ConversationTaken
            | Successor::SweepHeld => {}
        }
    }
}

impl renewal::FixtureClosedDriver for NeverAttached<'_> {
    fn judged(&self, _ctx: &crate::SessionCtx, pass: &mut dyn FnMut(&dyn Kernel)) {
        pass(&self.world.facts);
    }

    fn reconcile(
        &self,
        _opts: &aterm_agent::harness::upgrade_drive::Opts,
        _sid: &str,
        prepare: &Prepare,
    ) {
        let binding = self
            .admitted
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| *id == prepare.id)
            .map(|(_, binding)| binding.clone())
            .unwrap_or_else(|| self.world.old.clone());
        let _reply = run(
            &self.world.handle.ctx,
            &self.world.facts,
            Request::Reconcile(binding),
            self.world.actor.clone(),
        );
    }

    fn drive(
        &self,
        _opts: &aterm_agent::harness::upgrade_drive::Opts,
        _sid: &str,
        prepare: &Prepare,
        latched: ImagePhase,
    ) -> Option<recovery::RestartCompletion> {
        if self.drives.fetch_add(1, Ordering::AcqRel) >= STORM {
            self.stop();
            return None;
        }
        self.latched.lock().unwrap().push(latched);
        let ctx = &self.world.handle.ctx;
        let facts = &self.world.facts;
        let binding = admitted_binding(&run(
            ctx,
            facts,
            Request::prepare(prepare.clone()),
            self.world.actor.clone(),
        ));
        self.admitted
            .lock()
            .unwrap()
            .push((prepare.id.clone(), binding.clone()));
        let launcher = SupervisorInputCaller {
            principal: Principal::Instance,
            connection: Some(81),
            peer: Some(LAUNCHER),
            proof: None,
        };
        if self.successor == Successor::Running {
            self.resume(&binding, latched);
            facts
                .processes
                .lock()
                .unwrap()
                .insert(LAUNCHER, process(LAUNCHER, 400, LAUNCHER, 400, 20));
            facts.foreground.store(LAUNCHER, Ordering::SeqCst);
        }
        let attached = run(
            ctx,
            facts,
            Request::Attach(binding.clone(), latched),
            launcher.clone(),
        );
        if attached == DENIED {
            let _refused = self.refused.fetch_add(1, Ordering::AcqRel);
        } else {
            assert!(
                matches!(Reply::parse(&attached), Some(Reply::Launch { .. })),
                "{attached}"
            );
            let executed = run(ctx, facts, Request::Execute(binding, latched), launcher);
            assert!(
                matches!(Reply::parse(&executed), Some(Reply::Execute { .. })),
                "{executed}"
            );
        }
        if self.successor == Successor::Held {
            assert!(crate::fabric::apply_hold_for_test(
                ctx,
                Some(crate::fabric::parse_hold("local mine"))
            ));
        }
        if matches!(
            self.successor,
            Successor::Running | Successor::Held | Successor::HeldAtEnd
        ) {
            *self.waiting.lock().unwrap() = Some(0);
        }
        facts
            .clock
            .store(prepare.deadline_us + 1, Ordering::Release);
        None
    }

    fn pause(&self) {
        if self.passes.fetch_add(1, Ordering::AcqRel) == 200 {
            self.stop();
        }
        let mut waiting = self.waiting.lock().unwrap();
        if let Some(passes) = waiting.as_mut() {
            if !self.world.seal.intent_active() {
                self.ended_early.store(true, Ordering::Release);
            }
            if self.successor == Successor::Running {
                assert!(
                    crash_relaunch_busy(self.world),
                    "a running launcher's operation holds the session"
                );
            }
            *passes += 1;
            if *passes == WAITING {
                *waiting = None;
                self.release();
            }
        }
    }

    fn stopped(&self, ctx: &crate::SessionCtx, id: &str, deadline_us: u64) {
        self.stopped
            .lock()
            .unwrap()
            .push((id.to_string(), deadline_us));
        if self.successor == Successor::HeldAtEnd {
            assert!(crate::fabric::apply_hold_for_test(
                ctx,
                Some(crate::fabric::parse_hold("local mine"))
            ));
        }
        if self.successor == Successor::KeysAtEnd {
            let _burst = ctx
                .human_input
                .note(crate::metrics::now_us(), crate::human_input::Gesture::Typed);
        }
        if self.successor == Successor::ClaimedAtEnd {
            let thread = self.world.root.intent.thread.clone();
            crate::self_claim::set_copy_for_test(
                ctx.self_id.as_str(),
                Some(aterm_agent::harness::self_claimed::Copy {
                    lifetime: None,
                    pid: ANOTHER,
                    start: "another-start".into(),
                    home: self.world.env.state.join("another-home"),
                    seen: aterm_agent::harness::self_claimed::Seen::Never,
                    resume: Ok(aterm_agent::harness::self_claimed::Resume {
                        argv: vec![
                            "/fixture/codex-alab".into(),
                            "resume".into(),
                            thread.clone(),
                        ],
                        session: thread,
                        codex_home: None,
                        codex_auth_path: None,
                        cwd: None,
                    }),
                }),
            );
        }
    }

    /// The first step finds the shell not yet holding the tab (after [`DEFERRED`] steps another
    /// relaunch's sweep held, for [`Successor::SweepHeld`]); the next types the line, records
    /// the source handled, and finds no answer yet, as a line that may have run is waited on.
    fn hand_back(
        &self,
        opts: &aterm_agent::harness::upgrade_drive::Opts,
        source: &aterm_agent::harness::self_claimed::Copy,
    ) -> String {
        let mut handed = self.handed.lock().unwrap();
        handed.push(
            source
                .resume
                .as_ref()
                .ok()
                .and_then(|resume| resume.codex_auth_path.clone()),
        );
        let busy = if self.successor == Successor::SweepHeld {
            DEFERRED
        } else {
            0
        };
        if handed.len() <= busy {
            "busy:another-sweep".into()
        } else if handed.len() == busy + 1 {
            "wait:shell-prompt".into()
        } else {
            aterm_agent::harness::self_claimed::Witnesses::under(&opts.state)
                .record(source.pid, &source.start)
                .unwrap();
            "wait:unanswered".into()
        }
    }

    fn conversation_taken(
        &self,
        _source: &aterm_agent::harness::self_claimed::Copy,
        _resume: &aterm_agent::harness::self_claimed::Resume,
    ) -> bool {
        self.successor == Successor::ConversationTaken
    }
}

/// Install the root's service, as the commit does, and keep it past its end.
fn serving(world: &World) -> Arc<renewal::Service> {
    let service = Arc::new(renewal::Service::new(world.seal.clone()));
    world
        .handle
        .ctx
        .fenced_writes
        .native
        .lock()
        .unwrap()
        .renewal_services
        .insert(world.root.id.clone(), Arc::clone(&service));
    service
}

fn closed_opts(world: &World) -> aterm_agent::harness::upgrade_drive::Opts {
    let home = world.env.home.clone().unwrap();
    let mut opts = crate::harness_host::window_drive_opts(
        home,
        world.env.state.clone(),
        Some(
            world
                .env
                .state
                .join("absent.sock")
                .to_string_lossy()
                .into_owned(),
        ),
    );
    opts.only_sid = Some(world.root.sid.clone());
    opts
}

fn close(world: &World, driver: &NeverAttached<'_>) {
    renewal::fixture_close_worker(
        &Arc::downgrade(&world.handle.ctx),
        &closed_opts(world),
        &world.env,
        &world.root.id,
        world.original.deadline_us,
        driver,
    );
}

/// Whether the session's crash relaunch would be refused its native reservation because an
/// operation is unresolved, as the harness's exit lane is while a renewal holds the session.
fn crash_relaunch_busy(world: &World) -> bool {
    let slot = PrepareSlot::reserve(
        &world.handle.ctx,
        &"fe".repeat(16),
        world.facts.clock.load(Ordering::SeqCst),
        true,
        AccountSlot::Plain,
    );
    matches!(&slot, Err(refusal) if refusal == "ERR busy unresolved native operation\n")
}

/// The host's closure of the ended root's last budget, as the pool cleanup of a rotation reads it
/// ([`renewal::fixture_unattached_completion`]): the one the end waited on, kept by its service.
/// It binds that exact budget: the root's own first budget, which a continuation followed, never
/// passes it.
fn last_budget_closed(world: &World, service: &renewal::Service) -> bool {
    use aterm_agent::harness::codex_renewal::NativeUnattachedProof as _;
    let ended = recovery::read_receipt(&world.env, &world.root.id).unwrap();
    let last = recovery::active_prepare(&world.env, &ended).unwrap();
    let proof = renewal::fixture_unattached_completion(service, &world.facts)
        .expect("the closure the end waited on");
    assert!(!proof.ended_without_successor(&world.original));
    proof.ended_without_successor(&last)
}

/// The root ended once as `successor_never_attached` after its one continuation: one durable
/// record, a terminal receipt, its service and seals released, every operation settled so the
/// session's crash relaunch is admitted, its last budget proven closed for the pool's cleanup,
/// one coordinator exit at the last deadline, and the session handed back
/// ([`handed_back_once`]) unless someone took it since the end.
fn ended_once(
    world: &World,
    driver: &NeverAttached<'_>,
    service: &renewal::Service,
) -> recovery::Cancellation {
    let continuations = usize::try_from(renewal::MAX_CONTINUATIONS).unwrap();
    assert_eq!(
        driver.drives.load(Ordering::Acquire),
        continuations,
        "each continuation is driven once and no further child is appended"
    );
    let ended = recovery::read_receipt(&world.env, &world.root.id).unwrap();
    assert_eq!(ended.phase, recovery::Phase::Retired);
    assert!(ended.revoked);
    assert_eq!(
        ended
            .current_attempt
            .as_ref()
            .map(|attempt| attempt.generation),
        Some(renewal::MAX_CONTINUATIONS)
    );
    let record = recovery::read_cancellation(&world.env, &ended)
        .unwrap()
        .unwrap();
    assert_eq!(
        record.reason,
        recovery::CancellationKind::SuccessorNeverAttached
    );
    let table = world.handle.ctx.fenced_writes.native.lock().unwrap();
    assert!(!table.renewal_services.contains_key(&world.root.id));
    assert!(table.renewal_seals.is_empty());
    drop(table);
    assert!(
        !crash_relaunch_busy(world),
        "the ended move leaves no operation that refuses the session's crash relaunch"
    );
    assert!(
        last_budget_closed(world, service),
        "the host proves the last budget closed with no successor"
    );
    let last = recovery::active_prepare(&world.env, &ended).unwrap();
    assert_eq!(
        *driver.stopped.lock().unwrap(),
        [(world.root.id.clone(), last.deadline_us)]
    );
    assert!(
        successor_held_off(world),
        "the login that never attached is held off before the session is handed back"
    );
    if let Some(whose) = match driver.successor {
        Successor::HeldAtEnd => Some("a person holds the session"),
        Successor::KeysAtEnd => Some("a person used the session since the end"),
        Successor::ClaimedAtEnd => Some("another agent took the tab since the end"),
        Successor::ConversationTaken => Some("another process runs the conversation"),
        Successor::Refused | Successor::Running | Successor::Held | Successor::SweepHeld => None,
    } {
        assert_eq!(
            *driver.handed.lock().unwrap(),
            [],
            "{whose}: nothing is typed into the session"
        );
    } else {
        handed_back_once(world, driver);
    }
    record
}

/// The agent handed back on the login it left asks for the same successor at the same wall, as
/// a new process with an index key of its own: the root a Prepare would publish onto that
/// successor is refused, so the session is not moved onto a login that never attached again.
fn successor_held_off(world: &World) -> bool {
    let root = &world.root;
    let fresh = recovery::Receipt {
        id: "ef".repeat(16),
        old_pid: 4343,
        phase: recovery::Phase::Ready,
        launch_attempted: false,
        current_attempt: None,
        candidate: None,
        revoked: false,
        ..root.clone()
    };
    let published = recovery::with_successor_writer(
        &world.env,
        &root.old_auth,
        &root.intent.identity,
        |next, writer| {
            assert!(
                root.matches_successor(next),
                "the same successor is selected"
            );
            let key = recovery::IndexKey::login(
                fresh.old_pid,
                "fixture-start",
                &root.intent.wall,
                next.generation,
            );
            writer.publish_indexed(&key, None, &fresh)
        },
    );
    published == Err("successor-never-attached")
}

/// The Signal ended the session's agent and no successor took the session, so nothing else
/// relaunches it: the worker hands it back on the source's own line, the login it ran on and
/// never the destination's, taking a step that waits again and stopping once the line was typed,
/// though that step still waits for the new agent: it is the harness's now. Steps another tab's
/// sweep refused come first, as many as it held. The tab records the line as the crash rule's.
fn handed_back_once(world: &World, driver: &NeverAttached<'_>) {
    let source = world.root.old_auth.to_string_lossy().into_owned();
    assert_ne!(world.root.new_auth, world.root.old_auth);
    let busy = if driver.successor == Successor::SweepHeld {
        DEFERRED
    } else {
        0
    };
    let handed = driver.handed.lock().unwrap();
    assert_eq!(
        handed.len(),
        busy + 2,
        "the steps until the relaunch, and none after"
    );
    assert!(
        handed
            .iter()
            .all(|login| login.as_deref() == Some(source.as_str()))
    );
    assert_eq!(
        crate::self_claim::typed_for(world.handle.ctx.self_id.as_str())
            .and_then(|resume| resume.codex_auth_path),
        Some(source),
        "the tab's typed line, which an agent that ends before its record is written inherits"
    );
}

/// The finding's unbounded storm: each expired continuation appended another with a fresh
/// budget, forever. The worker now appends at most [`renewal::MAX_CONTINUATIONS`], then ends
/// the root once; a later worker for the same root finds it terminal and does nothing.
#[test]
fn a_successor_that_never_attaches_ends_the_root_once_after_its_continuation_budget() {
    let world = world(8020);
    let service = serving(&world);
    let driver = NeverAttached::new(&world, Successor::Refused);
    close(&world, &driver);
    let continuations = usize::try_from(renewal::MAX_CONTINUATIONS).unwrap();
    assert_eq!(driver.refused.load(Ordering::Acquire), continuations);
    let record = ended_once(&world, &driver, &service);
    close(&world, &driver);
    assert_eq!(driver.drives.load(Ordering::Acquire), continuations);
    handed_back_once(&world, &driver);
    let ended = recovery::read_receipt(&world.env, &world.root.id).unwrap();
    assert_eq!(
        recovery::read_cancellation(&world.env, &ended)
            .unwrap()
            .unwrap(),
        record
    );
}

/// A continuation's successor that the host granted Execute and that still runs at its deadline
/// may yet register: the host cannot close that budget, and the root waits for it as it would
/// for a next continuation. Only once it exits without registering does the root end, once.
#[test]
fn an_executed_successor_still_running_at_the_last_deadline_keeps_the_root_until_it_exits() {
    let world = world(8021);
    let service = serving(&world);
    let driver = NeverAttached::new(&world, Successor::Running);
    close(&world, &driver);
    assert_eq!(
        driver.refused.load(Ordering::Acquire),
        0,
        "the launcher attached"
    );
    assert!(
        !driver.ended_early.load(Ordering::Acquire),
        "the root ended while its successor still ran"
    );
    assert!(driver.passes.load(Ordering::Acquire) >= WAITING);
    ended_once(&world, &driver, &service);
    // The budget ran a launcher, so the proof a cancellation with no launch needs refuses it;
    // the end's own proof, which [`last_budget_closed`] passed, admits a launcher that exited.
    let ended = recovery::read_receipt(&world.env, &world.root.id).unwrap();
    let last = recovery::active_prepare(&world.env, &ended).unwrap();
    let closed = service.ended_closure().unwrap();
    assert!(!closed.cancelled_without_successor(&last, &world.facts));
    assert!(closed.ended_without_successor(&last, &world.facts));
}

/// A person's hold over the session during the last continuation keeps the root: it ends only
/// once the hold lifts.
#[test]
fn a_hold_during_the_last_continuation_keeps_the_root_until_it_lifts() {
    let world = world(8022);
    let service = serving(&world);
    let driver = NeverAttached::new(&world, Successor::Held);
    close(&world, &driver);
    assert!(
        !driver.ended_early.load(Ordering::Acquire),
        "the root ended while a person held the session"
    );
    assert!(
        driver.world.handle.ctx.fabric.hold().is_none(),
        "the root ended before the hold lifted"
    );
    ended_once(&world, &driver, &service);
}

/// A root whose own budget closed in Rollback hands its continuation's drive that phase, the
/// one the host latched it in, so the drive never asks a Rollback-closed host for Forward.
#[test]
fn a_rollback_closed_root_hands_its_continuation_the_rollback_phase() {
    let world = world_with_retirement(8023, false);
    lookup(&world.handle.ctx, &world.old)
        .unwrap()
        .lock()
        .unwrap()
        .resume_phase = Some(ImagePhase::Rollback);
    assert_eq!(
        run(
            &world.handle.ctx,
            &world.facts,
            Request::Reconcile(world.old.clone()),
            world.actor.clone(),
        ),
        NOEXEC
    );
    install(&world.handle.ctx, 71, &[Scope::Upgrade, Scope::Relaunch]);
    world
        .facts
        .clock
        .store(world.original.deadline_us + 1, Ordering::SeqCst);
    let service = serving(&world);
    let driver = NeverAttached::new(&world, Successor::Refused);
    close(&world, &driver);
    assert_eq!(*driver.latched.lock().unwrap(), [ImagePhase::Rollback]);
    ended_once(&world, &driver, &service);
}

/// A person who types in the bare shell once the root has ended has taken the session, as a
/// person's keys make a crashed agent's exit theirs: the root ends as ever, and the worker types
/// nothing into it, however long the person then leaves it idle.
#[test]
fn a_persons_keys_after_the_end_leave_the_session_to_them() {
    let world = world(8025);
    let service = serving(&world);
    let driver = NeverAttached::new(&world, Successor::KeysAtEnd);
    close(&world, &driver);
    ended_once(&world, &driver, &service);
}

/// Another codex-alab that claims the tab once the root has ended, started by a person or a
/// driver, has the session: the worker types nothing into it, even once that agent has gone.
#[test]
fn another_agent_on_the_tab_after_the_end_keeps_the_session_from_being_handed_back() {
    let world = world(8027);
    let service = serving(&world);
    let driver = NeverAttached::new(&world, Successor::ClaimedAtEnd);
    close(&world, &driver);
    crate::self_claim::set_copy_for_test(world.handle.ctx.self_id.as_str(), None);
    ended_once(&world, &driver, &service);
}

/// Another process that runs the conversation once the root has ended, from any tab, has it:
/// the worker does not resume it again in this one.
#[test]
fn another_process_on_the_conversation_keeps_the_session_from_being_handed_back() {
    let world = world(8028);
    let service = serving(&world);
    let driver = NeverAttached::new(&world, Successor::ConversationTaken);
    close(&world, &driver);
    ended_once(&world, &driver, &service);
}

/// Another tab's relaunch holding the machine's sweep defers the hand-back for as long as it
/// holds it, however many steps that takes; then the session is handed back once.
#[test]
fn another_relaunchs_sweep_defers_the_hand_back_until_it_is_free() {
    let world = world(8026);
    let service = serving(&world);
    let driver = NeverAttached::new(&world, Successor::SweepHeld);
    close(&world, &driver);
    ended_once(&world, &driver, &service);
}

/// A person who holds the session once its root has ended keeps it: the root ends as ever, and
/// the worker types nothing into the session.
#[test]
fn a_hold_at_the_end_keeps_the_session_from_being_handed_back() {
    let world = world(8024);
    let service = serving(&world);
    let driver = NeverAttached::new(&world, Successor::HeldAtEnd);
    close(&world, &driver);
    assert!(world.handle.ctx.fabric.hold().is_some());
    ended_once(&world, &driver, &service);
}

/// A login renewal whose successor attached shows that login attaches: its terminal cleanup
/// lifts the hold-off an earlier root's end without a successor left on that successor, so the
/// session's next renewal onto it is not refused for that old failure.
#[test]
fn a_login_renewal_that_attached_lifts_its_successors_hold_off() {
    let world = world(8029);
    let earlier = recovery::Receipt {
        id: "cd".repeat(16),
        ..world.root.clone()
    };
    recovery::with_receipt_writer(&world.env, |writer| {
        recovery::write_receipt(&world.env, &earlier)?;
        writer.cancel(
            &earlier,
            recovery::CancellationKind::SuccessorNeverAttached,
            0,
            0,
        )?;
        let ended = recovery::read_receipt(&world.env, &earlier.id).ok_or("ended")?;
        writer.note_unattached_login(&ended)
    })
    .unwrap()
    .unwrap();
    assert!(successor_held_off(&world), "the earlier end holds it off");
    let attached = recovery::Receipt {
        phase: recovery::Phase::Committed,
        successor_pid: Some(LAUNCHER),
        successor_birth: Some(20),
        ..world.root.clone()
    };
    recovery::with_receipt_writer(&world.env, |_| {
        recovery::write_receipt(&world.env, &attached)
    })
    .unwrap()
    .unwrap();
    let service = renewal::Service::new(world.seal.clone());
    assert!(renewal::fixture_cleanup_terminal_pool(
        &world.handle.ctx,
        &service,
        &world.env,
        &attached,
        &world.facts,
    ));
    assert_eq!(
        recovery::with_receipt_writer(&world.env, |writer| {
            writer.unattached_login_until(
                &world.root.intent.identity,
                &world.root.new_auth,
                world.root.generation,
            )
        }),
        Ok(Some(None)),
        "the attach lifted the hold-off"
    );
    assert!(!successor_held_off(&world));
}
