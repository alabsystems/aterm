// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for storing and clearing [`LandHeld`] on the session timeline from the worker loop.

use std::sync::mpsc;

use aterm_agent::harness::acts::{HoldLeg, LandHeld};

use super::*;
use crate::App;

enum Step {
    /// The loop tells the hold (`IdleHost::land_held`). Answered with what
    /// the session's timeline held as the call returned.
    Tell(Option<LandHeld>),
    /// The loop's run ends (a failure: the host starts the next run).
    End,
}

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

    fn tell(&self, held: Option<LandHeld>) -> Option<LandHeld> {
        self.steps.send(Step::Tell(held)).expect("the body listens");
        self.done
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("not within 60 s: the hold told")
    }

    fn run_started(&self) -> Option<LandHeld> {
        self.runs
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("not within 60 s: a run started")
    }

    fn woken(&self) -> usize {
        self.facts.try_iter().count()
    }

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

/// The host stores the hold synchronously on the timeline and wakes the window only on change.
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

/// Ending a worker run clears any standing hold from the timeline.
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
    desk.host.shutdown_and_join();
    assert_eq!(desk.stored(), None, "the shutdown took it down");
}
