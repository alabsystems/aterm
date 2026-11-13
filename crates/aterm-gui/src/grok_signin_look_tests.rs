// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Grok session's sign-in wall publishes its need without reading a file on the event loop.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use super::GrokLooks;
use crate::needs::{GROK_SIGNIN_READING, GROK_SIGNIN_WAIT, GROK_SIGNIN_WHAT, GrokSignin};
use crate::silent_file::SilentFile;
use aterm_agent::harness::grok_signin::RUNNING_FILE;
use aterm_spec::wait::{MUST_HAPPEN, within};

/// A headless App whose one stub session's agent reads Grok Build on its sign-in wall.
fn walled_grok() -> (crate::App, u64) {
    grok_on_wall(Some(crate::presence::SIGN_IN_SCREEN))
}

/// A headless App whose one stub session's agent reads Grok Build on a login wall whose subject is
/// `subject` (the sign-in screen's, [`crate::presence::SIGN_IN_SCREEN`], or none for a 401's
/// failure row).
fn grok_on_wall(subject: Option<&str>) -> (crate::App, u64) {
    let mut app = crate::App::headless_for_test();
    let sid = app.next_session_id;
    app.push_stub_tab(crate::WindowId(0), crate::stub_session(sid));
    let ctx = app.pool.get(sid).expect("stub pooled").ctx.clone();
    ctx.timeline.lock().unwrap().publish_agent(
        "wall:auth",
        None,
        subject.map(str::to_string),
        Some(aterm_phase::Program::Grok),
        crate::session_timeline::AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
            fp: 1,
        },
    );
    (app, sid)
}

/// The words session `sid`'s published verdict shows: its need's, else its wait entry's.
fn shown(app: &crate::App, sid: u64) -> Option<String> {
    let ctx = &app.pool.get(sid)?.ctx;
    let timeline = ctx.timeline.lock().unwrap();
    let verdict = timeline.need();
    verdict
        .need
        .as_ref()
        .map(|need| need.what.clone())
        .or_else(|| {
            verdict
                .waiting
                .as_ref()
                .and_then(|waiting| waiting.entry.as_ref())
                .map(|entry| entry.text.clone())
        })
}

/// The sign-in leader's record ([`RUNNING_FILE`]) a live sign-in writes, naming this process:
/// its kernel start record, or `-` where the kernel keeps none (Linux), as the leader writes it.
fn running_record(state: &std::path::Path) {
    let pid = std::process::id();
    let birth = aterm_uds::process::read_process_birth(i32::try_from(pid).unwrap()).map_or_else(
        || "-".to_owned(),
        |b| format!("{}.{:06}", b.seconds, b.microseconds),
    );
    let now = aterm_time::unix_secs().expect("the wall clock");
    std::fs::write(
        state.join(RUNNING_FILE),
        format!("pid={pid} birth={birth} t={now}\n"),
    )
    .unwrap();
}

/// Whether session `sid`'s published verdict raises a need.
fn needs(app: &crate::App, sid: u64) -> bool {
    app.pool
        .get(sid)
        .is_some_and(|session| session.ctx.timeline.lock().unwrap().need().need.is_some())
}

/// The harness state's sign-in record never answers a read (a home whose server stopped
/// answering): the wall's need publication returns at once, waiting on the look with no need
/// raised, and the look parked on the record is the look's own thread, never the event loop's.
#[test]
fn a_walled_groks_need_publication_reads_no_file_on_the_event_loop() {
    let state = aterm_tempfile::tempdir().expect("scratch harness state");
    let record = SilentFile::at(&state.path().join(RUNNING_FILE));
    let dir = state.path().to_path_buf();
    let (published, publication) = mpsc::channel();
    let (finished, done) = mpsc::channel();
    std::thread::spawn(move || {
        let (mut app, sid) = walled_grok();
        crate::harness_host::with_harness_state(&dir, || app.publish_need(sid));
        let _ = published.send((shown(&app, sid), needs(&app, sid)));
        settled(&app.grok_looks, 1);
        let _ = finished.send(());
    });
    let words = publication.recv_timeout(MUST_HAPPEN);
    if words.is_err() {
        record.release();
    }
    assert_eq!(
        words.expect("the publication returned within the hang detector's minute"),
        (Some(GROK_SIGNIN_READING.to_string()), false),
        "a wait on the look, no need, until the first look answers"
    );
    done.recv_timeout(MUST_HAPPEN)
        .expect("a FIFO at RUNNING_FILE never wedges the look thread");

    // Even when the look itself parks on a hung volume, need publication returns at once on the
    // event loop while the look waits on its own thread.
    let (hold, door) = mpsc::channel();
    let (script, looks) = Script::looks(vec![(Some(door), GrokSignin::Idle)], GrokSignin::Idle);
    let (mut app, sid) = walled_grok();
    app.grok_looks = looks;
    app.publish_need(sid);
    assert_eq!(
        (shown(&app, sid).as_deref(), needs(&app, sid)),
        (Some(GROK_SIGNIN_READING), false)
    );
    assert!(
        within(MUST_HAPPEN, Duration::from_millis(5), || {
            (!script.started.lock().unwrap().is_empty()).then_some(())
        })
        .is_some(),
        "the parked look started on its own thread"
    );
    assert_eq!(app.grok_looks.settled(), 0, "the look is still parked");
    drop(hold);
    settled(&app.grok_looks, 1);
}

/// The negative control of the wait before the first answer: a machine the look reads signed
/// out raises the wall's need once that look answers.
#[test]
fn a_signed_out_machine_the_look_reads_raises_the_walls_need() {
    let (mut app, sid) = walled_grok();
    app.grok_looks = GrokLooks::with_look(|_, _, _| GrokSignin::Idle);
    app.publish_need(sid);
    assert_eq!(
        (shown(&app, sid).as_deref(), needs(&app, sid)),
        (Some(GROK_SIGNIN_READING), false)
    );
    settled(&app.grok_looks, 1);
    app.publish_need(sid);
    assert_eq!(
        (shown(&app, sid).as_deref(), needs(&app, sid)),
        (Some(GROK_SIGNIN_WHAT), true)
    );
}

/// A GROK LOGIN WALL OFF ITS SIGN-IN SCREEN IS NO MACHINE SIGN-IN: a 401's failure row (or a
/// login-ended text) publishes `wall:auth` with no sign-in subject, so the need it raises is the
/// plain login's, never the machine sign-in's "approve the sign-in on its screen" (no sign-in runs
/// on that screen), and no look runs, so none can mark the machine's login refused for a grok
/// that drew no sign-in screen over it. CONTROL: the sign-in screen's wall asks the look.
#[test]
fn a_grok_login_wall_off_its_sign_in_screen_is_no_machine_sign_in() {
    let (script, looks) = Script::looks(Vec::new(), GrokSignin::Idle);
    let (mut app, sid) = grok_on_wall(None);
    app.grok_looks = looks;
    app.publish_need(sid);
    assert_eq!(
        (shown(&app, sid).as_deref(), needs(&app, sid)),
        (Some("logged out: log in there"), true)
    );
    app.publish_need(sid);
    assert_eq!(app.grok_looks.settled(), 0, "no look ran");
    assert!(script.started.lock().unwrap().is_empty(), "no look started");

    let (script, looks) = Script::looks(Vec::new(), GrokSignin::Idle);
    let (mut app, sid) = walled_grok();
    app.grok_looks = looks;
    app.publish_need(sid);
    assert_eq!(script.started(1).len(), 1, "CONTROL: the look started");
    settled(&app.grok_looks, 1);
    app.publish_need(sid);
    assert_eq!(
        shown(&app, sid).as_deref(),
        Some(GROK_SIGNIN_WHAT),
        "CONTROL: the machine's sign-in"
    );
}

/// The negative control: a sign-in that runs (its leader's record names this live process) is
/// what the wall shows once its look answers, through a need publication like any other.
#[test]
fn a_running_sign_in_the_look_reads_is_what_the_wall_shows() {
    let state = aterm_tempfile::tempdir().expect("scratch harness state");
    running_record(state.path());
    let (mut app, sid) = walled_grok();
    let landed = crate::harness_host::with_harness_state(state.path(), || {
        aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            std::time::Duration::from_millis(10),
            || {
                app.publish_need(sid);
                (shown(&app, sid).as_deref() == Some(GROK_SIGNIN_WAIT)).then_some(())
            },
        )
    });
    assert!(
        landed.is_some(),
        "the sign-in that runs is shown: {:?}",
        shown(&app, sid)
    );
}

/// Waits, within the hang detector's minute, until `n` looks of `looks` have settled.
fn settled(looks: &GrokLooks, n: u64) {
    assert!(
        within(MUST_HAPPEN, Duration::from_millis(5), || (looks.settled()
            >= n)
            .then_some(()))
        .is_some(),
        "{n} looks settled within the hang detector's minute ({} did)",
        looks.settled()
    );
}

/// Looks answering from a script, one step each in the order they start, a step with a door held
/// until it opens (or its opener goes); past the script, `rest`.
struct Script {
    steps: Mutex<VecDeque<(Option<mpsc::Receiver<()>>, GrokSignin)>>,
    started: Mutex<Vec<i32>>,
    rest: GrokSignin,
}

impl Script {
    fn looks(
        steps: Vec<(Option<mpsc::Receiver<()>>, GrokSignin)>,
        rest: GrokSignin,
    ) -> (Arc<Self>, GrokLooks) {
        let script = Arc::new(Self {
            steps: Mutex::new(steps.into()),
            started: Mutex::new(Vec::new()),
            rest,
        });
        let reads = Arc::clone(&script);
        let looks = GrokLooks::with_look(move |_, _, pgid| {
            // Claim this look's scripted step before advertising `started`, so a caller waiting on
            // `started(n)` never spawns a successor look that steals the earlier step.
            let step = reads.steps.lock().unwrap().pop_front();
            reads.started.lock().unwrap().push(pgid);
            match step {
                Some((door, answer)) => {
                    if let Some(door) = door {
                        let _ = door.recv();
                    }
                    answer
                }
                None => reads.rest,
            }
        });
        (script, looks)
    }

    /// The walled programs the looks were started for, once `n` have started (within the hang
    /// detector's minute).
    fn started(&self, n: usize) -> Vec<i32> {
        within(MUST_HAPPEN, Duration::from_millis(5), || {
            let started = self.started.lock().unwrap();
            (started.len() >= n).then(|| started.clone())
        })
        .unwrap_or_else(|| panic!("{n} looks started within the hang detector's minute"))
    }
}

/// A waker counting its wakes on `woke`.
fn waker(woke: &mpsc::Sender<()>) -> impl Fn() + Send + 'static {
    let woke = woke.clone();
    move || {
        let _ = woke.send(());
    }
}

/// The look's first answer moves what the wall shows (from the wait on the look), so it wakes the
/// session, whose next publication shows it; a look that answers the same again wakes nothing.
#[test]
fn a_look_that_moves_the_wall_wakes_its_session_and_one_that_does_not_stays_quiet() {
    let looks = GrokLooks::with_look(|_, _, _| GrokSignin::Running);
    let (woke, wakes) = mpsc::channel();
    assert_eq!(looks.ask(7, 70, waker(&woke)), GrokSignin::Reading);
    assert_eq!(
        wakes.recv_timeout(MUST_HAPPEN),
        Ok(()),
        "the first answer moved the wall: the session woke"
    );
    settled(&looks, 1);
    assert_eq!(looks.ask(7, 70, waker(&woke)), GrokSignin::Running);
    settled(&looks, 2);
    assert_eq!(
        wakes.try_recv(),
        Err(mpsc::TryRecvError::Empty),
        "the same answer again moved nothing"
    );
}

/// A look still out for a session forgotten (off its wall, then walled again) lands nowhere: the
/// newer entry keeps its own look's answer.
#[test]
fn an_answer_for_a_session_forgotten_lands_nowhere() {
    let (open_first, first) = mpsc::channel();
    let (script, looks) = Script::looks(
        vec![(Some(first), GrokSignin::SignedIn)],
        GrokSignin::Running,
    );
    assert_eq!(looks.ask(7, 70, || {}), GrokSignin::Reading);
    script.started(1);
    looks.forget(7);
    assert_eq!(looks.ask(7, 70, || {}), GrokSignin::Reading, "a new entry");
    settled(&looks, 1);
    open_first.send(()).expect("the first look waits");
    settled(&looks, 2);
    assert_eq!(
        looks.ask(7, 70, || {}),
        GrokSignin::Running,
        "the forgotten entry's late answer did not land over the newer one"
    );
}

/// A look still out for a program the session no longer runs (another grok on the wall) lands
/// nowhere: the wall waits on a look for the program it shows, which the look out owes as it ends.
#[test]
fn an_answer_for_a_program_the_session_no_longer_runs_lands_nowhere() {
    let (open_first, first) = mpsc::channel();
    let (open_second, second) = mpsc::channel();
    let (script, looks) = Script::looks(
        vec![
            (Some(first), GrokSignin::SignedIn),
            (Some(second), GrokSignin::Idle),
        ],
        GrokSignin::Running,
    );
    assert_eq!(looks.ask(7, 70, || {}), GrokSignin::Reading);
    script.started(1);
    assert_eq!(
        looks.ask(7, 80, || {}),
        GrokSignin::Reading,
        "another program"
    );
    open_first.send(()).expect("the first look waits");
    assert_eq!(script.started(2), [70, 80], "the look out owed one for 80");
    assert_eq!(
        looks.ask(7, 80, || {}),
        GrokSignin::Reading,
        "70's answer is not 80's"
    );
    open_second.send(()).expect("the second look waits");
    settled(&looks, 2);
    assert_ne!(looks.ask(7, 80, || {}), GrokSignin::SignedIn);
}
