// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for storing [`LandReceipt`] on the session timeline from the worker loop (§14.9,
//! slice 9b): kept beside the landed face, gone when the face moves to open or theirs, and gone
//! with the run that told it.

use std::sync::mpsc;

use aterm_agent::harness::acts::{LandReceipt, ReceiptLeg, TaskFace};

use super::*;
use crate::App;

enum Step {
    /// The loop tells the receipt (`IdleHost::land_receipt`). Answered with what the session's
    /// timeline held as the call returned.
    Tell(Option<LandReceipt>),
    /// The loop's run ends (a failure: the host starts the next run).
    End,
}

/// A real host over a headless window whose worker body tells receipts through its idle host
/// (`WorkerIdle`), stored by the hook [`start_default`] gives its host ([`store_hooks`]).
struct Desk {
    app: App,
    host: HostHandle,
    steps: mpsc::Sender<Step>,
    /// After each step, what the timeline held as it was taken.
    done: mpsc::Receiver<Option<LandReceipt>>,
    /// At each run's start, what the timeline held.
    runs: mpsc::Receiver<Option<LandReceipt>>,
}

impl Drop for Desk {
    fn drop(&mut self) {
        self.host.shutdown_and_join();
    }
}

fn stored_on(app: &App) -> Option<LandReceipt> {
    app.pool
        .get(0)
        .expect("session 0")
        .ctx
        .timeline
        .lock()
        .unwrap()
        .land_receipt()
        .map(|r| (**r).clone())
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
        let stored = {
            let ctx = Arc::clone(&ctx);
            move || {
                ctx.timeline
                    .lock()
                    .unwrap()
                    .land_receipt()
                    .map(|r| (**r).clone())
            }
        };
        let body: Arc<BodyFn> = Arc::new(move |job: &WorkerJob| {
            let idle = job.opts.idle_host.as_ref().expect("the worker's idle host");
            let _ = runs_tx.lock().unwrap().send(stored());
            while !job.stop.load(Ordering::SeqCst) {
                let step = steps_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_millis(5));
                match step {
                    Ok(Step::Tell(receipt)) => {
                        idle.land_receipt(receipt.as_ref());
                        let _ = done_tx.lock().unwrap().send(stored());
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
        h.receipt = store_hooks(&app.store).receipt;
        let host = HostHandle::start(on(), false, false, h);
        Self {
            app,
            host,
            steps,
            done,
            runs,
        }
    }

    fn tell(&self, receipt: Option<LandReceipt>) -> Option<LandReceipt> {
        self.steps
            .send(Step::Tell(receipt))
            .expect("the body listens");
        self.done
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("not within 60 s: the receipt told")
    }

    fn run_started(&self) -> Option<LandReceipt> {
        self.runs
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("not within 60 s: a run started")
    }
}

fn a_receipt() -> LandReceipt {
    LandReceipt {
        head: "3f9a1c2e4b7d".into(),
        main_ref: "origin/main".into(),
        repo: "/Users//a/src/parser".into(),
        branch: "main".into(),
        leg: ReceiptLeg::OnMain,
        check: true,
        said: "Nothing left: pushed.".into(),
        at_ms: 1_771_000_000_000,
        statement: aterm_agent::harness::land_receipt::Statement::Unread,
    }
}

/// A RECEIPT NEVER OUTLIVES ITS LANDING (§14.9, slice 9b): the loop's receipt is stored on the
/// tab's timeline and stays while the face is landed, and while the face goes unknown and comes
/// back landed; the face's move to open or theirs drops it, as does the loop's `None`. The stored
/// words pass the presentation sanitizer once more. NEGATIVE CONTROL (run by hand): the clearing
/// taken out of `set_task_face` keeps a receipt under the open face.
#[test]
fn a_receipt_never_outlives_its_landing() {
    let app = App::headless_for_test();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let sid = ctx.self_id.as_str().to_string();
    let stored = || {
        ctx.timeline
            .lock()
            .unwrap()
            .land_receipt()
            .map(|r| (**r).clone())
    };
    let face =
        |face: Option<TaskFace>, at: i64| ctx.timeline.lock().unwrap().set_task_face(face, at);
    let r = a_receipt();
    for (n, off) in [TaskFace::Open, TaskFace::Theirs].into_iter().enumerate() {
        let at = i64::try_from(n).unwrap() * 10;
        assert!(store_land_receipt(&app.store, &sid, Some(&r)), "{off:?}");
        assert!(face(Some(TaskFace::Landed), at + 1), "{off:?}");
        assert_eq!(stored(), Some(r.clone()), "{off:?}");
        assert!(
            !store_land_receipt(&app.store, &sid, Some(&r)),
            "the same receipt again does not move: {off:?}"
        );
        assert!(face(None, at + 2), "{off:?}");
        assert_eq!(
            stored(),
            Some(r.clone()),
            "a face gone unknown holds it: {off:?}"
        );
        assert!(face(Some(TaskFace::Landed), at + 3), "{off:?}");
        assert_eq!(stored(), Some(r.clone()), "{off:?}");
        assert!(face(Some(off), at + 4), "{off:?}");
        assert_eq!(stored(), None, "gone with the landing: {off:?}");
    }
    assert!(store_land_receipt(&app.store, &sid, Some(&r)));
    assert!(store_land_receipt(&app.store, &sid, None));
    assert_eq!(stored(), None, "the loop's None drops it");
    assert!(
        !store_land_receipt(&app.store, "s-nobody", Some(&r)),
        "no tab"
    );

    let hostile = LandReceipt {
        said: "  Nothing left:\u{202e} pushed.\u{7}\u{feff} ".into(),
        ..a_receipt()
    };
    assert!(store_land_receipt(&app.store, &sid, Some(&hostile)));
    assert_eq!(
        stored().map(|r| r.said),
        Some("Nothing left: pushed.".to_string())
    );
}

/// THE LOOP'S RECEIPT REACHES THE TIMELINE THROUGH ITS HOST, AND GOES WITH ITS RUN (§14.9.3): a
/// worker's tell (`WorkerIdle::land_receipt`) is stored on the tab's timeline by the time the
/// call returns, its words sanitized once more, and its `None` takes it down. A run's end takes
/// it down as it takes the hold (the next run's loop tells it again with the landed face, from
/// its journal), and so does the host's shutdown. NEGATIVE CONTROLS (run by hand):
/// `WorkerIdle::land_receipt` made a no-op fails the first tell, and so does the live receipt
/// hook ([`store_hooks`]) answering `false` without storing; the run's end not clearing it fails
/// the next run's start.
#[test]
fn the_loops_receipt_reaches_the_timeline_and_goes_with_its_run() {
    let desk = Desk::new();
    assert_eq!(desk.run_started(), None);
    let r = a_receipt();
    assert_eq!(
        desk.tell(Some(r.clone())),
        Some(r.clone()),
        "stored by the call"
    );
    let hostile = LandReceipt {
        said: "  Nothing left:\u{202e} pushed.\u{7}\u{feff} ".into(),
        ..a_receipt()
    };
    assert_eq!(
        desk.tell(Some(hostile)),
        Some(r.clone()),
        "its words sanitized"
    );
    assert_eq!(desk.tell(None), None, "the loop's None");
    assert_eq!(desk.tell(Some(r.clone())), Some(r.clone()));
    desk.steps.send(Step::End).expect("the body listens");
    assert_eq!(
        desk.run_started(),
        None,
        "the old run's receipt went with it"
    );
    assert_eq!(desk.tell(Some(r.clone())), Some(r));
    desk.host.shutdown_and_join();
    assert_eq!(stored_on(&desk.app), None, "the shutdown took it down");
}

/// A LANDING'S RECEIPT REACHES `status` BESIDE THE LANDED FACE (§14.9, slice 9c): stored beside the
/// landed face, `status` names its head, its ref, its leg and its repository
/// (`landed_head=3f9a1c2e4b7d landed_on=origin/main landed_leg=on-main landed_repo=…`);
/// before the face lands, and once it moves off, `status` says `-` for each. A folder, a branch or a ref holding hidden
/// characters (a bidi override, a zero-width space) reaches no surface with them: the store
/// passes each through the presentation sanitizer, as it does the agent's words. NEGATIVE
/// CONTROLS (run by hand): `status` reading the receipt without the face's filter names it
/// before the landing; the store sanitizing `said` alone keeps the hidden characters.
#[test]
fn a_landings_receipt_reaches_status_beside_the_landed_face() {
    let app = App::headless_for_test();
    let session = app.pool.get(0).expect("session 0");
    let ctx = session.ctx.clone();
    let sid = ctx.self_id.as_str().to_string();
    let r = a_receipt();
    let fields = |app: &App| {
        let line = app.session_status_record(0).expect("live session");
        ["landed_head", "landed_on", "landed_leg", "landed_repo"].map(|key| {
            line.split_whitespace()
                .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
                .unwrap_or_else(|| panic!("{key}= in {line}"))
                .to_owned()
        })
    };
    let none = ["-", "-", "-", "-"].map(str::to_owned);
    assert_eq!(fields(&app), none, "no landing");
    assert!(store_land_receipt(&app.store, &sid, Some(&r)));
    assert_eq!(fields(&app), none, "no landed face yet");
    assert!(
        ctx.timeline
            .lock()
            .unwrap()
            .set_task_face(Some(TaskFace::Landed), 1)
    );
    assert_eq!(
        fields(&app),
        [
            "3f9a1c2e4b7d",
            "origin/main",
            "on-main",
            "/Users//a/src/parser"
        ]
        .map(str::to_owned)
    );
    assert!(
        ctx.timeline
            .lock()
            .unwrap()
            .set_task_face(Some(TaskFace::Open), 2)
    );
    assert_eq!(fields(&app), none, "the face moved off");

    let hidden = LandReceipt {
        repo: "/Users//a/src/par\u{202e}ser\u{200b}".into(),
        branch: "main\u{202e}x".into(),
        main_ref: "origin/ma\u{200b}in".into(),
        ..a_receipt()
    };
    assert!(store_land_receipt(&app.store, &sid, Some(&hidden)));
    assert!(
        ctx.timeline
            .lock()
            .unwrap()
            .set_task_face(Some(TaskFace::Landed), 3)
    );
    let shown = ctx
        .timeline
        .lock()
        .unwrap()
        .land_receipt()
        .cloned()
        .expect("the stored receipt");
    assert_eq!(
        (
            shown.repo.as_str(),
            shown.branch.as_str(),
            shown.main_ref.as_str()
        ),
        ("/Users//a/src/parser", "mainx", "origin/main")
    );
    use aterm_agent::supervise::mode_words;
    for text in [
        mode_words::landed_words(Some(&shown)),
        app.session_status_record(0).expect("live session"),
    ] {
        assert!(
            !text
                .chars()
                .any(crate::session_timeline::is_forbidden_metadata_char),
            "{text:?}"
        );
    }
}

/// THE LANDED SENTENCE IS THE RECEIPT'S (§14.17 item 5, slice 13a, worded by 9c's receipt): the
/// stored receipt keeps its one fact, whether the land check was answered
/// (`LandReceipt::check`), and every surface that words what the agent said reads it there
/// (`mode_words::said_words`): beside the landed face, the `Autopilot
/// landed` record says the agent answered the land check where the receipt says so, and only what
/// it said where none was answered (a landing a build before the check noted); with no receipt,
/// no check is claimed. NEGATIVE CONTROL (run by hand): the store writing `check: false` over the
/// receipt's says the unchecked sentence over the checked receipt.
#[test]
fn the_landed_sentence_is_the_receipts() {
    use aterm_agent::supervise::mode_words::ANSWERED_CHECK;
    let app = App::headless_for_test();
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    let sid = ctx.self_id.as_str().to_string();
    assert!(
        ctx.timeline
            .lock()
            .unwrap()
            .set_task_face(Some(TaskFace::Landed), 1)
    );
    let said = || {
        let receipt = ctx.timeline.lock().unwrap().land_receipt().cloned();
        let record = crate::harness_task::task_record(
            "s-1",
            "in tab 3",
            "in tab 3",
            TaskFace::Landed,
            receipt.as_deref(),
        )
        .expect("a landing's record");
        record.detail.join("\n").to_lowercase()
    };
    let record = said();
    assert!(
        !record.contains(ANSWERED_CHECK),
        "no receipt claims no check: {record}"
    );
    for check in [true, false] {
        let r = LandReceipt {
            check,
            ..a_receipt()
        };
        assert!(store_land_receipt(&app.store, &sid, Some(&r)), "{check}");
        let record = said();
        assert_eq!(record.contains(ANSWERED_CHECK), check, "{check}: {record}");
    }
}
