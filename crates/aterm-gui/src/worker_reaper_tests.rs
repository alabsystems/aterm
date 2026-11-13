// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Worker programs are bounded at their claim deadline; quota walls and concluded tasks keep
//! their panes. The helpers here serve the claim act's tests too.

use std::path::Path;
use std::sync::PoisonError;
use std::time::{Duration, Instant};

use aterm_agent::harness::credit_offers;
use aterm_agent::supervise::Autopilot;

use super::*;
use crate::harness_host::credit_claim::WORKER_FOR;
use crate::task_record::{TaskMark, WHY_LANDED, WHY_REPEATED, WHY_USAGE_LIMIT};

const WID: crate::WindowId = crate::WindowId(0);
const MOMENT: Duration = Duration::from_millis(1);

/// A live session's handle.
pub(crate) fn live(app: &crate::App, session: u64) -> crate::session_store::SessionHandle {
    let store = app.store.read().unwrap_or_else(PoisonError::into_inner);
    store
        .live_handles()
        .find(|h| h.local_id == session)
        .cloned()
        .expect("a live session")
}

/// The claim folder of a test's worker, as the harness state names it.
pub(crate) fn claim_folder() -> String {
    format!(
        "/state/{}/{}",
        aterm_agent::harness::cli::HARNESS,
        credit_offers::CLAIM_FOLDER
    )
}

/// Marks `session` a Claude Code worker trusted with `folder`, and returns when its record began.
pub(crate) fn set_task(app: &crate::App, session: u64, folder: &str) -> Instant {
    let h = live(app, session);
    crate::task_record::mark_session_task(&h.ctx, h.local_id, &TaskMark::new(TaskState::Working))
        .expect("unfenced");
    let mut timeline = h
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    timeline.set_task_worker(aterm_phase::Program::Claude, Some(folder.to_owned()));
    timeline.task().expect("its record").started
}

/// Marks `session`'s task `state`, `why`, and returns when it entered that state.
fn conclude(
    app: &crate::App,
    session: u64,
    state: TaskState,
    why: Option<&'static str>,
) -> Instant {
    let h = live(app, session);
    let mut mark = TaskMark::new(state);
    mark.why = why;
    crate::task_record::mark_session_task(&h.ctx, h.local_id, &mark).expect("unfenced");
    record(app, session).1
}

/// The steps a look took for `session` alone.
pub(crate) fn of(steps: Vec<(u64, Ending)>, session: u64) -> Vec<(u64, Ending)> {
    steps.into_iter().filter(|(s, _)| *s == session).collect()
}

/// Publishes `program` as the one in front of `session`'s tab, as the program reader does.
pub(crate) fn in_front(app: &crate::App, session: u64, program: &str) {
    let handle = live(app, session);
    let mut timeline = handle
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let group = 4_000 + i32::try_from(session).expect("a small session");
    timeline.note_foreground_group(group);
    timeline.set_program(group, Some(program.to_owned()));
}

/// `session`'s task state and when it entered it.
pub(crate) fn record(app: &crate::App, session: u64) -> (TaskState, Instant) {
    let task = live(app, session)
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .task()
        .cloned()
        .expect("its record");
    (task.state, task.since)
}

/// A person's new stub tab in the window: no mark, whatever task it runs.
pub(crate) fn new_tab(app: &mut crate::App) -> u64 {
    let session = app.next_session_id;
    app.push_stub_tab(WID, crate::stub_session(session));
    session
}

/// Gives the tab holding `session` the mark a worker's tab lands with, as a tab a worker landed in
/// that left the parked tabs some other way than Keep as Tab would have it.
pub(crate) fn mark_ephemeral(app: &mut crate::App, session: u64) {
    let at = app.windows[&WID]
        .tab_set
        .tabs()
        .iter()
        .position(|tab| app.tab_holds_session(tab, session))
        .expect("its tab");
    let ws = app.windows.get_mut(&WID).expect("its window");
    ws.tab_set.tab_at_mut(at).expect("its tab").ephemeral = true;
}

/// A worker's stub tab in the window's strip, not parked but marked.
pub(crate) fn worker_tab(app: &mut crate::App) -> u64 {
    let session = new_tab(app);
    mark_ephemeral(app, session);
    session
}

/// A worker landed parked, as a `spawn agent=` worker lands.
fn parked_worker(app: &mut crate::App) -> u64 {
    let session = app.next_session_id;
    app.next_session_id += 1;
    let parent = app.new_tab_parent(WID);
    app.install_tab_session_parked(WID, crate::stub_session(session), parent, false);
    app.sync_window(WID);
    session
}

fn autopilot(app: &crate::App, session: u64) -> Option<Autopilot> {
    live(app, session)
        .ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .autopilot_word()
}

/// Concluded tasks are never scheduled for retirement. An unfinished claim task alone expires.
#[test]
fn concluded_tasks_never_expire() {
    let t = Instant::now();
    for state in [TaskState::Done, TaskState::Failed, TaskState::Exited] {
        for why in [Some(WHY_LANDED), Some(WHY_USAGE_LIMIT), None] {
            let mut mark = TaskMark::new(state);
            mark.why = why;
            let (record, _) = mark.apply(None, &Default::default(), t, 1_000);
            assert!(settled(&record));
            assert_eq!(over(&record, t + LINGER * 100, None), None);
        }
    }
    let (mut record, _) =
        TaskMark::new(TaskState::Working).apply(None, &Default::default(), t, 1_000);
    record.trusted_cwd = Some(claim_folder());
    assert_eq!(over(&record, t + WORKER_FOR - MOMENT, None), None);
    assert_eq!(over(&record, t + WORKER_FOR, None), Some(Over::RanOut));
}

/// Updates and concluded tasks protect a worker before signals. An unfinished claim worker
/// still ends at its budget, with its pane preserved.
#[test]
fn a_worker_ends_without_raising_a_confirmation() {
    use Ending::{Kill, Leave, MarkExited, Term};
    let running = Stand {
        in_hand: false,
        concluded: false,
        exited: false,
        job: true,
        worker: true,
        updating: false,
        deaf: false,
    };
    let deaf = Stand {
        deaf: true,
        ..running
    };
    let ended = Stand {
        job: false,
        ..running
    };
    assert_eq!(ending(running), Term);
    assert_eq!(ending(deaf), Kill);
    for stand in [running, deaf, ended] {
        assert_eq!(
            ending(Stand {
                updating: true,
                ..stand
            }),
            Leave,
            "an update protects the worker before any signal or mark"
        );
    }
    assert_eq!(
        ending(Stand {
            concluded: true,
            ..running
        }),
        Leave,
        "a concluded worker keeps its program"
    );
    assert_eq!(
        ending(Stand {
            in_hand: true,
            ..deaf
        }),
        Leave
    );
    assert_eq!(
        ending(Stand {
            exited: true,
            ..deaf
        }),
        Leave,
        "not the worker's job"
    );
    assert_eq!(
        ending(Stand {
            worker: false,
            ..deaf
        }),
        Leave,
        "another program's job"
    );
    assert_eq!(ending(ended), MarkExited, "an open task is concluded first");
    let read = Stand {
        concluded: true,
        exited: true,
        ..ended
    };
    assert_eq!(ending(read), Leave);
    assert_eq!(
        ending(Stand {
            exited: false,
            ..read
        }),
        Leave,
        "a done task keeps its pane"
    );
    for (stand, why) in [
        (
            Stand {
                in_hand: true,
                ..ended
            },
            "someone has it",
        ),
        (
            Stand {
                in_hand: true,
                ..read
            },
            "someone has it",
        ),
        (
            Stand {
                updating: true,
                ..read
            },
            "a close calls it off",
        ),
        (
            Stand {
                updating: true,
                ..ended
            },
            "a mark waits on its fence",
        ),
    ] {
        assert_eq!(ending(stand), Leave, "{why}");
    }
}

/// A worker pane survives a concluded task, including a carried usage-limit failure.
/// Negative control: an unfinished claim worker still has its bounded execution time.
#[test]
fn concluded_workers_keep_their_panes_and_programs() {
    let mut app = crate::App::headless_for_test();
    for (state, why) in [
        (TaskState::Done, WHY_LANDED),
        (TaskState::Failed, WHY_USAGE_LIMIT),
        (TaskState::Failed, WHY_REPEATED),
        (TaskState::Exited, "exit"),
    ] {
        let worker = parked_worker(&mut app);
        set_task(&app, worker, "/workspace");
        let ended = conclude(&app, worker, state, Some(why));
        in_front(&app, worker, "claude");
        crate::feed_session_for_test(
            &app,
            worker,
            b"\x1b]133;A\x07PS> \x1b]133;B\x07claude\n\x1b]133;C\x07",
        );
        assert_eq!(of(app.reap_workers(ended + LINGER * 100), worker), []);
        assert!(app.pool.get(worker).is_some());
        assert!(app.worker_termed.is_empty());
        crate::feed_session_for_test(&app, worker, b"\x1b]133;D;0\x07");
        assert_eq!(of(app.reap_workers(ended + LINGER * 100), worker), []);
        assert!(app.pool.get(worker).is_some());
    }
    let worker = worker_tab(&mut app);
    let began = set_task(&app, worker, &claim_folder());
    assert_eq!(
        of(app.reap_workers(began + WORKER_FOR), worker),
        [(worker, Ending::MarkExited)]
    );
    assert!(app.pool.get(worker).is_some());
}

/// Keep as Tab removes the task-owned mark and the claim execution budget.
#[test]
fn a_kept_tab_is_a_persons_and_never_goes_for_its_tasks_end() {
    let mut app = crate::App::headless_for_test();
    let parked = parked_worker(&mut app);
    let shown = worker_tab(&mut app);
    let persons = new_tab(&mut app);
    assert!(app.session_is_ephemeral(parked) && app.session_is_ephemeral(shown));
    assert!(app.keep_as_tab(parked));
    assert!(app.keep_as_tab(shown));
    assert!(
        !app.keep_as_tab(persons),
        "a person's tab is already theirs"
    );
    assert!(!app.keep_as_tab(parked), "already kept");
    for session in [parked, shown] {
        assert!(!app.session_is_parked(session));
        assert!(!app.session_is_ephemeral(session));
    }
    let mut ended = Instant::now();
    for session in [parked, shown] {
        ended = ended.max(conclude(&app, session, TaskState::Done, Some(WHY_LANDED)));
    }
    assert_eq!(app.reap_workers(ended + LINGER * 100), []);
    let again = set_task(&app, shown, &claim_folder());
    assert_eq!(
        app.reap_workers(again + WORKER_FOR * 10),
        [],
        "nor a later task's time"
    );
    assert!(app.pool.get(parked).is_some() && app.pool.get(shown).is_some());
}

/// A claim worker that exhausts its execution budget is ended and marked exited. Its pane stays.
/// Negative controls: an occupied tab, another program, and a refused signal.
#[test]
fn the_look_preserves_a_claim_workers_pane_once_its_job_ended() {
    use Ending::{Leave, MarkExited, Term};
    let claim = claim_folder();
    let (time, deaf) = (WORKER_FOR, DEAF_FOR);
    let mut app = crate::App::headless_for_test();
    let front = 0;
    mark_ephemeral(&mut app, front);
    let worker = worker_tab(&mut app);
    let other = worker_tab(&mut app);
    let since = set_task(&app, worker, &claim);
    let project = set_task(&app, other, "/Users//me/project");
    crate::feed_session_for_test(
        &app,
        worker,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07claude\n\x1b]133;C\x07",
    );
    assert_eq!(
        app.reap_workers(since + time),
        [(worker, Leave)],
        "a job no reader named yet is not signalled"
    );
    in_front(&app, worker, "claude");

    assert_eq!(app.reap_workers(since + time - MOMENT), []);
    let driven = live(&app, worker);
    let drive = |on: bool| {
        *driven
            .ctx
            .turn_lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = on.then(|| crate::Lease::Drive {
            holder: crate::harness_host::holder(),
            expires_us: u64::MAX,
            conn: None,
            hard: true,
            here: false,
            holder_pid: None,
        });
    };
    drive(true);
    assert_eq!(app.reap_workers(since + time + deaf), [(worker, Leave)]);
    drive(false);
    assert_eq!(
        app.reap_workers(since + time + deaf),
        [(worker, Term)],
        "a program's first signal is SIGTERM, however long a hand held it past its time"
    );
    // A stub tab has no process group, so the verb refused that SIGTERM: no time to end counts
    // (a delivered one: `the_looks_delivered_sigterm_starts_the_time_its_sigkill_waits_for`).
    assert_eq!(
        app.reap_workers(since + time + 3 * deaf),
        [(worker, Term)],
        "a SIGTERM never delivered starts no time to end"
    );
    assert!(app.worker_termed.is_empty());
    assert!(
        app.pool.get(worker).is_some(),
        "a job's pane stays: closing it would raise a confirmation"
    );

    crate::feed_session_for_test(&app, worker, b"\x1b]133;D;0\x07");
    assert_eq!(app.reap_workers(since + time - MOMENT), []);
    assert_eq!(app.reap_workers(since + time), [(worker, MarkExited)]);
    let (state, exited) = record(&app, worker);
    assert_eq!(state, TaskState::Exited);
    assert_eq!(
        app.reap_workers(exited + LINGER - MOMENT),
        [],
        "its tab stays to be read"
    );
    assert!(app.pool.get(worker).is_some());
    assert_eq!(app.reap_workers(exited + LINGER), []);
    assert!(
        app.pool.get(worker).is_some(),
        "its job ended, its pane stays"
    );

    assert_eq!(app.reap_workers(project + time), []);
    assert!(app.pool.get(other).is_some(), "a tab of another folder");

    let second = set_task(&app, other, &claim);
    assert_eq!(app.reap_workers(second + time), [(other, MarkExited)]);
    let (_, exited) = record(&app, other);
    assert_eq!(app.reap_workers(exited + LINGER), []);
    let last = set_task(&app, front, &claim);
    assert_eq!(app.reap_workers(last + time), [(front, MarkExited)]);
    let (_, exited) = record(&app, front);
    assert_eq!(app.reap_workers(exited + LINGER), []);
    assert!(app.pool.get(front).is_some(), "a window's last tab asks");
}

/// A PERSON'S HAND IN A WORKER'S TAB IS THEIRS: their typing gives a claim worker its time again,
/// a claim worker that ran out has its Autopilot Off even with its tab left in front, and a job in
/// the tab of a worker whose program exited is theirs and never signalled, nor is another
/// program's job in a worker's tab that nothing marked exited. CONTROL: a concluded worker whose
/// tab stays keeps its Autopilot.
#[test]
fn the_look_leaves_what_a_person_has_in_a_workers_tab() {
    use Ending::Leave;
    let claim = claim_folder();
    let mut app = crate::App::headless_for_test();
    let typed = worker_tab(&mut app);
    let since = set_task(&app, typed, &claim);
    live(&app, typed)
        .ctx
        .human_input
        .note(crate::metrics::now_us(), crate::human_input::Gesture::Typed);
    assert_eq!(app.reap_workers(since + WORKER_FOR), []);

    let exited = worker_tab(&mut app);
    set_task(&app, exited, &claim);
    let at = conclude(&app, exited, TaskState::Exited, None);
    crate::feed_session_for_test(
        &app,
        exited,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07make\n\x1b]133;C\x07",
    );
    assert_eq!(app.reap_workers(at + LINGER), []);
    assert_ne!(
        autopilot(&app, exited),
        Some(Autopilot::Off),
        "CONTROL: a concluded worker's tab left as it is"
    );

    let taken = worker_tab(&mut app);
    let began = set_task(&app, taken, &claim);
    crate::feed_session_for_test(
        &app,
        taken,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07npm run dev\n\x1b]133;C\x07",
    );
    // npm's leader names itself `node`, and the worker's last Claude Code frame, still on the
    // screen, names the reader.
    in_front(&app, taken, "node");
    {
        let handle = live(&app, taken);
        let mut timeline = handle
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        timeline.publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
        assert_eq!(
            crate::harness_host::identified_agent(timeline.agent()),
            Some(aterm_phase::Program::Claude),
            "PRECONDITION: the screen names Claude Code for the runtime in front"
        );
    }
    assert_eq!(
        of(app.reap_workers(began + WORKER_FOR + DEAF_FOR), taken),
        [(taken, Leave)],
        "a person's own job, with nothing marked exited before it and the worker's frame on screen"
    );
    assert_eq!(record(&app, taken).0, TaskState::Working);
    assert_eq!(autopilot(&app, taken), Some(Autopilot::Off));
}

/// A CLAIM WORKER'S EXIT MARK TAKES DOWN A HELD ASK, AND A CONCLUDED WORKER GETS NONE: the mark
/// takes down the held ask a program that ended no longer owes; a worker failed at a usage limit
/// concluded already, so its end stands, its reply and its `why=` with it, and its pane stays;
/// a landed worker likewise. CONTROL: a record that moved since the look read it is not
/// marked.
#[test]
fn the_looks_exit_mark_takes_down_a_held_ask_and_spares_a_concluded_end() {
    let claim = claim_folder();
    let mut app = crate::App::headless_for_test();
    let limited = worker_tab(&mut app);
    let began = set_task(&app, limited, &claim);
    let h = live(&app, limited);
    let mut limit = TaskMark::new(TaskState::Failed);
    limit.why = Some(WHY_USAGE_LIMIT);
    limit.result_bytes = 23;
    limit.result_path = Some(std::path::PathBuf::from("/s/tasks/s-2-1.result.md"));
    crate::task_record::mark_session_task(&h.ctx, limited, &limit).expect("unfenced");
    assert_eq!(
        of(app.reap_workers(began + WORKER_FOR), limited),
        [],
        "a carried wall failure keeps its pane and record"
    );
    let kept = live(&app, limited)
        .ctx
        .timeline
        .lock()
        .unwrap()
        .task()
        .cloned()
        .unwrap();
    assert_eq!(
        (kept.state, kept.why, kept.result_bytes),
        (TaskState::Failed, Some(WHY_USAGE_LIMIT), 23)
    );
    assert_eq!(
        kept.result_path.as_deref(),
        Some(Path::new("/s/tasks/s-2-1.result.md"))
    );

    let landed = worker_tab(&mut app);
    let began = set_task(&app, landed, &claim);
    conclude(&app, landed, TaskState::Done, Some(WHY_LANDED));
    assert_eq!(
        of(app.reap_workers(began + WORKER_FOR), landed),
        [],
        "a landed worker keeps its pane"
    );
    assert_eq!(Some(record(&app, landed).0), Some(TaskState::Done));

    let held = worker_tab(&mut app);
    let began = set_task(&app, held, &claim);
    let h = live(&app, held);
    let mut stalled = TaskMark::new(TaskState::Stalled);
    stalled.why = Some("held");
    let ask = format!("{}a box", aterm_agent::supervise::HELD_HEAD);
    crate::task_record::mark_session_task_with(
        &h.ctx,
        held,
        Some(&stalled),
        crate::task_record::AttentionEdit::Raise(&ask),
        Duration::ZERO,
        |_, _| true,
    )
    .expect("unfenced");
    let attention = |ctx: &crate::SessionCtx| {
        ctx.meta
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attention_owners
            .get(crate::harness_host::ATTENTION_OWNER)
            .map(str::to_owned)
    };
    assert_eq!(
        attention(&h.ctx).as_deref(),
        Some(ask.as_str()),
        "PRECONDITION"
    );
    assert_eq!(
        of(app.reap_workers(began + WORKER_FOR), held),
        [(held, Ending::MarkExited)]
    );
    assert_eq!(attention(&h.ctx), None, "the ask went with its program");
    let ended = live(&app, held)
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .task()
        .cloned()
        .expect("its record");
    assert_eq!(
        (ended.state, ended.why),
        (TaskState::Exited, Some("held")),
        "it ended where it stalled, and its report says so"
    );
    assert_eq!(
        crate::task_record::failed_words(ended.state, ended.why),
        "held at a screen aterm does not know"
    );

    let moved = worker_tab(&mut app);
    set_task(&app, moved, &claim);
    let h = live(&app, moved);
    let read = h
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .task()
        .cloned()
        .expect("its record");
    conclude(&app, moved, TaskState::Working, Some("turn"));
    assert_eq!(super::mark_ran_out(&h.ctx, moved, &read), Ok(None));
    assert_eq!(record(&app, moved).0, TaskState::Working);
}

/// A WORKER YOU CAN SEE IS LEFT ALONE: in a focused window, a claim worker in a split beside the
/// focused pane is on screen and is not ended. CONTROL: the same worker in a tab behind.
#[test]
fn a_claim_worker_in_a_visible_split_is_left_alone() {
    let claim = claim_folder();
    let mut app = crate::App::headless_for_test();
    let ws = app.windows.get_mut(&WID).expect("its window");
    ws.focused = true;
    ws.os_window_for_test = true;
    let worker = 0;
    mark_ephemeral(&mut app, worker);
    let began = set_task(&app, worker, &claim);
    let beside = app.split_active_stub_tab(WID);
    assert_eq!(app.focused_session_id(WID), Some(beside), "PRECONDITION");
    assert_eq!(
        app.reap_workers(began + WORKER_FOR),
        [(worker, Ending::Leave)]
    );

    let behind = worker_tab(&mut app);
    let began = set_task(&app, behind, &claim);
    app.switch_tab_in(WID, 0);
    assert_eq!(
        of(app.reap_workers(began + WORKER_FOR), behind),
        [(behind, Ending::MarkExited)]
    );
}

/// A CLICK IS NOT TYPING: a claim worker's time starts again when a person types in its tab, not
/// when they click or scroll there, after a seamless update from a build that carried no awake
/// typing row too, and the typing age is awake time, as the record's is, a typing carried across
/// a seamless update after the Mac slept too. CONTROL: a key press.
#[test]
fn only_typing_gives_a_claim_worker_its_time_again() {
    use crate::human_input::Gesture;
    let claim = claim_folder();
    let mut app = crate::App::headless_for_test();
    for (gesture, after) in [
        (Gesture::Other, vec![Ending::MarkExited]),
        (Gesture::Typed, vec![]),
    ] {
        let tab = worker_tab(&mut app);
        let since = set_task(&app, tab, &claim);
        live(&app, tab)
            .ctx
            .human_input
            .note(crate::metrics::now_us(), gesture);
        assert_eq!(
            of(app.reap_workers(since + WORKER_FOR), tab)
                .into_iter()
                .map(|(_, step)| step)
                .collect::<Vec<_>>(),
            after,
            "{gesture:?}"
        );
    }
    // Carried across a seamless update that followed a night's sleep: the gesture and typing ages
    // the outgoing process carried (the typing's counting the sleep), its awake typing row where
    // it wrote one, and a gesture made here; the worker's record crossed on awake time.
    const SLEPT_MS: u64 = 8 * 3600 * 1000;
    let minutes = |n: u64| n * 60_000;
    let mut carried =
        |gesture_ms: u64, typed_ms: u64, awake_ms: Option<u64>, here: Option<Gesture>| {
            let tab = worker_tab(&mut app);
            let since = set_task(&app, tab, &claim);
            let now_us = crate::metrics::now_us();
            let input = &live(&app, tab).ctx.human_input;
            input.carry(gesture_ms, now_us);
            input.carry_typed_at(typed_ms + SLEPT_MS, now_us, None);
            if let Some(awake_ms) = awake_ms {
                input.carry_typed_awake(awake_ms, now_us);
            }
            if let Some(gesture) = here {
                input.note(now_us, gesture);
            }
            of(app.reap_workers(since + WORKER_FOR), tab)
                .into_iter()
                .map(|(_, step)| step)
                .collect::<Vec<_>>()
        };
    assert_eq!(
        carried(minutes(1), minutes(1), Some(minutes(1)), None),
        [],
        "typed a minute ago in awake time, whatever the sleep before the update"
    );
    assert_eq!(
        carried(10_000, minutes(31), Some(minutes(31)), None),
        [Ending::MarkExited],
        "a click younger than the typing: the awake row, not the click, ends the worker"
    );
    assert_eq!(
        carried(10_000, minutes(31), None, None),
        [],
        "CONTROL: no awake row (an older outgoing build): the typing is bounded by the click it \
         carried, so the worker lives longer, never ends early"
    );
    assert_eq!(
        carried(minutes(31), minutes(31), None, Some(Gesture::Other)),
        [Ending::MarkExited],
        "a click made here after an older build's carry is no typing"
    );
    assert_eq!(
        carried(minutes(31), minutes(31), None, Some(Gesture::Typed)),
        [],
        "CONTROL: typing here"
    );
}

/// `SIGKILL` IS NEVER A PROGRAM'S FIRST SIGNAL: the time to end counts from the first `SIGTERM`
/// the verb delivered to the process group now in front, for the record now standing. CONTROL:
/// a refused one records nothing; a later one to the same group keeps the first; another
/// program come to the front, or a new record, starts again; no foreground group is never deaf.
#[test]
fn the_time_to_end_counts_from_the_first_delivered_sigterm() {
    let mut termed = super::TermedBySession::new();
    let t0 = Instant::now();
    let (record, newer) = (t0, t0 + Duration::from_secs(1));
    let at = t0 + WORKER_FOR;
    let deaf = |termed: &super::TermedBySession, session, record, group, now| {
        super::deaf(termed.get(&session), record, group, now)
    };
    super::note_term(
        &mut termed,
        7,
        record,
        at,
        "ERR no foreground process group\n",
    );
    assert!(termed.is_empty(), "a refused SIGTERM");
    assert!(!deaf(&termed, 7, record, 4001, at + DEAF_FOR * 9));

    super::note_term(
        &mut termed,
        7,
        record,
        at,
        "OK signalled pgrp 4001 discarded=3\n",
    );
    super::note_term(
        &mut termed,
        7,
        record,
        at + DEAF_FOR / 2,
        "OK signalled pgrp 4001\n",
    );
    assert_eq!(
        termed.get(&7),
        Some(&super::Termed {
            record,
            group: 4001,
            at
        })
    );
    assert!(!deaf(&termed, 7, record, 4001, at + DEAF_FOR - MOMENT));
    assert!(
        deaf(&termed, 7, record, 4001, at + DEAF_FOR),
        "from the first"
    );
    assert!(
        !deaf(&termed, 8, record, 4001, at + DEAF_FOR),
        "another tab"
    );
    assert!(
        !deaf(&termed, 7, newer, 4001, at + DEAF_FOR),
        "a new record's program was sent nothing"
    );
    assert!(
        !deaf(&termed, 7, record, 4002, at + DEAF_FOR),
        "another program came to the front: it was sent nothing"
    );
    assert!(
        !deaf(&termed, 7, record, -1, at + DEAF_FOR),
        "no foreground group"
    );

    let later = at + DEAF_FOR * 3;
    super::note_term(&mut termed, 7, record, later, "OK signalled pgrp 4002\n");
    assert!(!deaf(&termed, 7, record, 4002, later + DEAF_FOR - MOMENT));
    assert!(
        deaf(&termed, 7, record, 4002, later + DEAF_FOR),
        "the new program's own first SIGTERM"
    );
    super::note_term(&mut termed, 7, newer, later, "OK signalled pgrp 4002\n");
    assert!(deaf(&termed, 7, newer, 4002, later + DEAF_FOR));
    assert!(!deaf(&termed, 7, record, 4002, later + DEAF_FOR));
    super::note_term(&mut termed, 7, newer, later, "OK signalled pid 4003\n");
    assert_eq!(
        termed.get(&7).map(|sent| sent.group),
        Some(4002),
        "a reply that names no group records nothing"
    );
}

/// THE LOOK'S OWN DELIVERED SIGTERM STARTS THE TIME ITS SIGKILL WAITS FOR: against a real pty
/// whose foreground program ignores `SIGTERM`, the look sends `SIGTERM` first and records the
/// group it reached, sends it again until [`DEAF_FOR`] has passed since, then `SIGKILL`, which
/// ends it; once a person types in the tab, nothing of that signal stays.
#[test]
#[cfg(target_os = "macos")]
fn the_looks_delivered_sigterm_starts_the_time_its_sigkill_waits_for() {
    use Ending::{Kill, Term};
    let claim = claim_folder();
    let (master, slave, mut job, group) =
        crate::input_stall::tests::ready_job("trap '' TERM; printf R; exec /bin/sleep 120");
    let mut app = crate::App::headless_for_test();
    let worker = app.next_session_id;
    let mut stub = crate::stub_session_with_sink(
        worker,
        std::sync::Arc::new(aterm_session::sink::SinkWriter::new(master)),
    );
    stub.master = master;
    app.push_stub_tab(WID, stub);
    mark_ephemeral(&mut app, worker);
    assert_eq!(live(&app, worker).master, master, "PRECONDITION");
    let since = set_task(&app, worker, &claim);
    crate::feed_session_for_test(
        &app,
        worker,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07claude\n\x1b]133;C\x07",
    );
    in_front(&app, worker, "claude");

    let termed = since + WORKER_FOR;
    assert_eq!(app.reap_workers(termed), [(worker, Term)]);
    assert_eq!(
        app.worker_termed.get(&worker),
        Some(&super::Termed {
            record: since,
            group,
            at: termed
        }),
        "the group the SIGTERM reached"
    );
    assert_eq!(
        job.0.try_wait().expect("its state"),
        None,
        "it ignores SIGTERM"
    );
    assert_eq!(
        app.reap_workers(termed + DEAF_FOR - MOMENT),
        [(worker, Term)]
    );
    assert_eq!(app.reap_workers(termed + DEAF_FOR), [(worker, Kill)]);
    let ended = job.0.wait().expect("reaped");
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&ended),
        Some(libc::SIGKILL)
    );

    live(&app, worker)
        .ctx
        .human_input
        .note(crate::metrics::now_us(), crate::human_input::Gesture::Typed);
    assert_eq!(app.reap_workers(termed + DEAF_FOR), []);
    assert!(
        app.worker_termed.is_empty(),
        "a worker a person typed back into its time owes no signal"
    );
    // SAFETY: the test's own pty fds, open until here and used by nothing after.
    unsafe {
        libc::close(slave);
        libc::close(master);
    }
}

/// THE LOOK CONCLUDES A PROGRAM WORKER NO LAUNCHER WATCHES ON THE BLOCK ITS LAUNCHER TYPED INTO
/// ([`crate::App::conclude_unwatched_program`]): its exit status and output tail, as its launcher
/// would. NEGATIVE CONTROL: one whose launcher stalled before typing, in whose tab a person then ran
/// a command, is left as its stall said.
#[test]
fn the_look_concludes_a_program_worker_on_its_typed_block_alone() {
    let mut app = crate::App::headless_for_test();
    let typed = worker_tab(&mut app);
    let stalled = worker_tab(&mut app);
    for session in [typed, stalled] {
        let h = live(&app, session);
        crate::task_record::mark_session_task(
            &h.ctx,
            h.local_id,
            &TaskMark::new(TaskState::Working),
        )
        .expect("unfenced");
        h.ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_task_worker(aterm_phase::Program::Generic, None);
        let mut term = h.term.lock().unwrap_or_else(PoisonError::into_inner);
        term.process(b"\x1b]133;A\x07% \x1b]133;B\x07");
        let block = term.all_blocks().last().expect("its prompt").id;
        term.process(b"make\r\n\x1b]133;C\x07built\r\n\x1b]133;D;0\x07\x1b]133;A\x07% ");
        drop(term);
        if session == typed {
            assert!(crate::task_record::note_program_block(
                &h.ctx,
                block,
                Duration::ZERO
            ));
        } else {
            let mut stall = TaskMark::new(TaskState::Stalled);
            stall.why = Some("startup");
            crate::task_record::mark_session_task(&h.ctx, h.local_id, &stall).expect("unfenced");
        }
    }
    let _ = app.reap_workers(Instant::now());
    let task = |session| {
        live(&app, session)
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .task()
            .cloned()
            .expect("its record")
    };
    let done = task(typed);
    assert_eq!(
        (done.state, done.why, done.exit),
        (TaskState::Done, Some("exit-status"), Some(0))
    );
    assert_eq!(
        crate::task_record::cmd_result(&live(&app, typed).ctx, typed, ""),
        "OK 1 source=output bytes=5\nbuilt\n"
    );
    let left = task(stalled);
    assert_eq!(
        (left.state, left.why),
        (TaskState::Stalled, Some("startup")),
        "CONTROL: a command its launcher never typed concludes nothing"
    );
}

/// A quota wall protects even a claim worker past its execution budget. Negative control:
/// a worker running after that budget is still ended, and its pane stays when the program exits.
#[test]
fn a_claim_workers_quota_wall_is_waited_out() {
    let mut app = crate::App::headless_for_test();
    let worker = worker_tab(&mut app);
    let began = set_task(&app, worker, &claim_folder());
    in_front(&app, worker, "claude");
    crate::feed_session_for_test(
        &app,
        worker,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07claude\n\x1b]133;C\x07",
    );
    let h = live(&app, worker);
    for word in [
        "wall:usage-session",
        "wall:usage-weekly",
        "wall:model-bucket",
    ] {
        h.ctx.timeline.lock().unwrap().publish_agent(
            word,
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
        assert_eq!(app.reap_workers(began + WORKER_FOR * 100), []);
        assert!(app.pool.get(worker).is_some());
        assert!(app.worker_termed.is_empty());
        assert_ne!(autopilot(&app, worker), Some(Autopilot::Off));
    }
    h.ctx.timeline.lock().unwrap().publish_agent(
        "busy",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        crate::session_timeline::AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq: 2 },
            fp: 2,
        },
    );
    assert_eq!(
        app.reap_workers(began + WORKER_FOR * 100),
        [(worker, Ending::Term)]
    );
}
