// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LAND'S HOLD, STORED ON THE LOOP'S THREAD
//! (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.3.3 item 3):
//! the window's host hears the hold from its loop (`IdleHost::land_held`),
//! stores it on the session's timeline before the call returns
//! (`Hooks::hold`, `store_land_held`, `SessionTimeline::set_land_held`),
//! where `status` reads it, and wakes the window with a fact that carries
//! nothing (`HarnessFact::LandHeld`) only where it moved. A run's end takes
//! it down: nobody tells it any more.
//!
//! The host here is the real one (`HostHandle`, its `WorkerIdle`), its
//! `hold` hook the live `store_land_held` over a real headless App's store;
//! the worker's body stands in for the loop. Each test names its NEGATIVE
//! CONTROL: the mutant it was run against, and what failed.

use std::sync::mpsc;

use aterm_agent::harness::acts::{HoldLeg, LandHeld};

use super::*;
use crate::App;

/// What the worker's body does, one step at a time, at the test's word.
enum Step {
    /// The loop tells the hold (`IdleHost::land_held`). Answered with what
    /// the session's timeline held as the call returned.
    Tell(Option<LandHeld>),
    /// The loop's run ends (a failure: the host starts the next run).
    End,
}

/// The real host supervising a headless App's session 0 (a Claude Code
/// under Land), its `hold` hook the live [`store_land_held`] over the App's
/// store and its hold facts kept on a channel the test drains.
struct Desk {
    app: App,
    host: HostHandle,
    steps: mpsc::Sender<Step>,
    /// After each step, what the timeline held as it was taken.
    done: mpsc::Receiver<Option<LandHeld>>,
    /// At each run's start, what the timeline held.
    runs: mpsc::Receiver<Option<LandHeld>>,
    facts: mpsc::Receiver<crate::HarnessFact>,
}

impl Drop for Desk {
    fn drop(&mut self) {
        self.host.shutdown_and_join();
    }
}

impl Desk {
    fn new() -> Self {
        let app = App::headless_for_test();
        let ctx = app.pool.get(0).expect("session 0").ctx.clone();
        let sid = ctx.self_id.as_str().to_string();
        {
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(7);
            tl.set_program(7, Some("claude".into()));
        }
        let (steps, steps_rx) = mpsc::channel::<Step>();
        let (done_tx, done) = mpsc::channel();
        let (runs_tx, runs) = mpsc::channel();
        let (steps_rx, done_tx, runs_tx) = (
            Mutex::new(steps_rx),
            Mutex::new(done_tx),
            Mutex::new(runs_tx),
        );
        let body_ctx = Arc::clone(&ctx);
        let body: Arc<BodyFn> = Arc::new(move |job: &WorkerJob| {
            let idle = job.opts.idle_host.as_ref().expect("the worker's idle host");
            let _ = runs_tx
                .lock()
                .unwrap()
                .send(body_ctx.timeline.lock().unwrap().land_held());
            while !job.stop.load(Ordering::SeqCst) {
                let step = steps_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_millis(5));
                match step {
                    Ok(Step::Tell(held)) => {
                        idle.land_held(held);
                        let stored = body_ctx.timeline.lock().unwrap().land_held();
                        let _ = done_tx.lock().unwrap().send(stored);
                    }
                    Ok(Step::End) => return BodyEnd::Failed("the run ended".to_string()),
                    Err(_) => {}
                }
            }
            BodyEnd::Stopped
        });
        let world = Arc::new(World::default());
        world.set(&[(sid.as_str(), Program::Claude)]);
        world.word(&sid, Some(Autopilot::Land));
        let mut h = hooks(&world, body);
        let store = app.store.clone();
        h.hold = Arc::new(move |sid, held| store_land_held(&store, sid, held));
        let (fact_tx, facts) = mpsc::channel();
        let fact_tx = Mutex::new(fact_tx);
        let mine = sid;
        h.fact = Arc::new(move |sid, fact| {
            if sid == mine && fact == crate::HarnessFact::LandHeld {
                let _ = fact_tx.lock().unwrap().send(fact);
            }
        });
        let host = HostHandle::start(on(), false, false, h);
        Self {
            app,
            host,
            steps,
            done,
            runs,
            facts,
        }
    }

    /// The loop tells `held`; wait for it (a hang detector): what the
    /// timeline held as the call returned.
    fn tell(&self, held: Option<LandHeld>) -> Option<LandHeld> {
        self.steps.send(Step::Tell(held)).expect("the body listens");
        self.done
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("not within 60 s: the hold told")
    }

    /// The next run's start (a hang detector): what the timeline held.
    fn run_started(&self) -> Option<LandHeld> {
        self.runs
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("not within 60 s: a run started")
    }

    /// The hold facts the host has sent the window since the last call.
    fn woken(&self) -> usize {
        self.facts.try_iter().count()
    }

    /// The hold on session 0's timeline now.
    fn stored(&self) -> Option<LandHeld> {
        self.app
            .pool
            .get(0)
            .expect("session 0")
            .ctx
            .timeline
            .lock()
            .unwrap()
            .land_held()
    }
}

/// A hold after a yield: two unpushed commits, a nudge.
fn held() -> LandHeld {
    LandHeld {
        since_ms: Some(1_792_159_320_000),
        nudges: 1,
        read: true,
        legs: HoldLeg::Repo.bit(),
        unpushed: 2,
        ..LandHeld::default()
    }
}

/// THE HOLD IS ON THE TIMELINE AS THE LOOP'S CALL RETURNS, AND WAKES ONLY
/// WHERE IT MOVED (§12.3.3 item 3): a hold told is stored by the call
/// itself, on the loop's thread, and the window woken once; told again as
/// it stands, nothing is woken; a nudge moves it and wakes; `None` takes it
/// down and wakes, and `None` again wakes nothing. NEGATIVE CONTROL (as
/// run): `WorkerIdle::land_held` waking whatever the hook said wakes the
/// window for the hold told again.
#[test]
fn the_host_stores_the_hold_and_wakes_only_where_it_moved() {
    let desk = Desk::new();
    assert_eq!(desk.run_started(), None);
    assert_eq!(desk.tell(Some(held())), Some(held()), "stored by the call");
    assert_eq!(desk.woken(), 1);
    assert_eq!(desk.tell(Some(held())), Some(held()));
    assert_eq!(desk.woken(), 0, "the same hold told again wakes nothing");
    let nudged = LandHeld {
        nudges: 2,
        ..held()
    };
    assert_eq!(desk.tell(Some(nudged)), Some(nudged));
    assert_eq!(desk.woken(), 1, "a nudge moved it");
    assert_eq!(desk.tell(None), None);
    assert_eq!(desk.woken(), 1, "taken down");
    assert_eq!(desk.tell(None), None);
    assert_eq!(desk.woken(), 0);
    assert_eq!(desk.stored(), None);
}

/// A RUN'S END TAKES ITS HOLD DOWN (§12.3.3 item 3): a loop whose run ended
/// without telling `None` (a failure, a panic) leaves no hold standing —
/// the next run starts with none on the timeline, and the window was woken
/// for it; the host's shutdown, which ends the last run the same way, takes
/// down one told since. NEGATIVE CONTROL (as run): `worker_run` without its
/// hold clear starts the next run with the old hold standing.
#[test]
fn a_runs_end_takes_its_hold_down() {
    let desk = Desk::new();
    assert_eq!(desk.run_started(), None);
    assert_eq!(desk.tell(Some(held())), Some(held()));
    assert_eq!(desk.woken(), 1);
    desk.steps.send(Step::End).expect("the body listens");
    assert_eq!(desk.run_started(), None, "the old run's hold went with it");
    assert_eq!(desk.woken(), 1, "and the window was told");
    assert_eq!(desk.tell(Some(held())), Some(held()));
    // `Drop` joins again: the join is taken once.
    desk.host.shutdown_and_join();
    assert_eq!(desk.stored(), None, "the shutdown took it down");
}
