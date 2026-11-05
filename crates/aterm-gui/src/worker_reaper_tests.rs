// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The reaper of workers' tabs ([`super`]): a tab a task opened for itself goes once its task
//! concluded and nobody holds it, a claim worker whose task never concludes is ended at its time,
//! and a person's tab never goes. The helpers here serve the claim act's tests too.

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

/// Who the session log says closed `session`.
fn closed_by(app: &crate::App, session: u64) -> String {
    use aterm_slog::{Node, Op};
    let source = app
        .store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .closed_logs()
        .by_local(session)
        .expect("its closed log")
        .source();
    let log = source.into_log().expect("its log");
    let close = log
        .window()
        .rev()
        .find(|r| r.op == Op::Close && r.node == Node::SESSION)
        .expect("its close");
    close.by.word().to_string()
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
    app.install_tab_session_parked(WID, crate::stub_session(session), parent);
    app.sync_window(WID);
    session
}

/// The retired row the reaper kept for `session`.
fn retired(app: &crate::App, session: u64) -> Option<crate::retired_workers::Retired> {
    app.store
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .retired_workers()
        .by_local(session)
        .cloned()
}

fn autopilot(app: &crate::App, session: u64) -> Option<Autopilot> {
    live(app, session)
        .ctx
        .meta
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .autopilot_word()
}

/// A CONCLUDED TASK IS OVER ONCE ITS END STOOD ITS LINGER, OR AT ONCE WHERE ITS PROGRAM WAS ASKED
/// TO END: done, failed and exited alike, a failure at a usage limit too (nothing types into it
/// again, so its tab would only stand blocked), unless a person typed in its tab since that end.
/// An open claim worker's runs out at its time. CONTROL: a moment short of the linger; typing
/// older than the end; a stall, which may move on; an open task of another folder, which never
/// runs out.
#[test]
fn a_concluded_task_is_over_after_its_linger_unless_a_person_took_it_up() {
    assert_eq!(LINGER, Duration::from_secs(3 * 60));
    let t0 = Instant::now();
    let of_state = |state: TaskState, why: Option<&'static str>, folder: Option<String>| {
        let mut mark = TaskMark::new(state);
        mark.why = why;
        let (mut record, _) = mark.apply(None, false, t0, 1_000);
        record.trusted_cwd = folder;
        record
    };
    for state in [TaskState::Done, TaskState::Failed, TaskState::Exited] {
        let record = of_state(state, None, None);
        assert!(settled(&record), "{state:?}");
        assert_eq!(
            over(&record, t0 + LINGER - MOMENT, None, false),
            None,
            "{state:?}"
        );
        assert_eq!(
            over(&record, t0 + LINGER, None, false),
            Some(Over::Concluded),
            "{state:?}"
        );
        assert_eq!(
            over(&record, t0 + MOMENT, None, true),
            Some(Over::Concluded),
            "{state:?}: its program was asked to end"
        );
        assert_eq!(
            over(&record, t0 + LINGER, Some(LINGER - MOMENT), false),
            None,
            "{state:?}: typed since its end"
        );
        assert_eq!(
            over(&record, t0 + LINGER, Some(LINGER - MOMENT), true),
            None,
            "{state:?}: typed since its end, its program asked to end or not"
        );
        assert_eq!(
            over(&record, t0 + LINGER, Some(LINGER + MOMENT), false),
            Some(Over::Concluded),
            "{state:?}: CONTROL: typed before its end"
        );
    }
    let a_day = Duration::from_secs(24 * 3600);
    let stalled = of_state(TaskState::Stalled, Some("held"), None);
    assert!(!settled(&stalled));
    assert_eq!(over(&stalled, t0 + a_day, None, false), None, "a stall");
    let limited = of_state(TaskState::Failed, Some(WHY_USAGE_LIMIT), None);
    assert!(settled(&limited), "a usage limit's failure is a conclusion");
    assert_eq!(
        over(&limited, t0 + LINGER - MOMENT, None, false),
        None,
        "a usage limit, a moment short of its linger"
    );
    assert_eq!(
        over(&limited, t0 + LINGER, None, false),
        Some(Over::Concluded),
        "a usage limit's failure goes after its linger, as any failure does"
    );
    let working = of_state(TaskState::Working, None, Some("/Users//me/project".into()));
    assert_eq!(
        over(&working, t0 + a_day, None, false),
        None,
        "another folder"
    );
    let claim = of_state(TaskState::Working, None, Some(claim_folder()));
    assert_eq!(over(&claim, t0 + WORKER_FOR - MOMENT, None, false), None);
    assert_eq!(
        over(&claim, t0 + WORKER_FOR, None, false),
        Some(Over::RanOut)
    );
    let limited_claim = of_state(
        TaskState::Failed,
        Some(WHY_USAGE_LIMIT),
        Some(claim_folder()),
    );
    assert_eq!(
        over(&limited_claim, t0 + LINGER, None, false),
        Some(Over::Concluded),
        "a claim worker failed at a usage limit concluded: it waits no WORKER_FOR"
    );
}

/// A WORKER'S END TAKES NOTHING FROM ANYONE AND ASKS NOTHING OF A PERSON: a tab in someone's hand
/// and a job that is not the worker's are left, a running program is asked to end and then made
/// to before its pane closes (a job in a closing pane raises a confirmation), an open task is
/// marked `exited` once its program is gone, a concluded one's pane closes, and a window's last
/// tab, a program someone ran there since and a seamless update under way keep it.
#[test]
fn a_worker_ends_without_raising_a_confirmation() {
    use Ending::{ClosePane, Kill, Leave, MarkExited, Term};
    let running = Stand {
        in_hand: false,
        concluded: false,
        exited: false,
        job: true,
        worker: true,
        ends_window: false,
        updating: false,
        deaf: false,
        reading: false,
    };
    let last = Stand {
        ends_window: true,
        ..running
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
    assert_eq!(ending(last), Term, "even its last tab");
    assert_eq!(ending(deaf), Kill);
    assert_eq!(
        ending(Stand {
            concluded: true,
            ..running
        }),
        Term,
        "a done worker's program still running is asked to end first"
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
    assert_eq!(
        ending(Stand {
            ends_window: true,
            ..ended
        }),
        MarkExited,
        "a mark closes nothing"
    );
    let read = Stand {
        concluded: true,
        exited: true,
        ..ended
    };
    assert_eq!(ending(read), ClosePane);
    assert_eq!(
        ending(Stand {
            exited: false,
            ..read
        }),
        ClosePane,
        "a done task's tab goes with no exit mark"
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
                ends_window: true,
                ..read
            },
            "a last tab asks",
        ),
        (
            Stand {
                reading: true,
                ..read
            },
            "the output of a program someone ran there is still to be read",
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

/// A CONCLUDED WORKER'S TAB GOES AFTER ITS LINGER, PARKED OR NOT, AND WHAT IT LEFT STAYS: a parked
/// worker that landed and a worker's tab in the strip that failed each close in the harness's
/// name once their ends have stood [`LINGER`], each leaving a retired row with its end, which the
/// Workers page lists under Finished and which keeps the Workers tab while it stands. CONTROL: a
/// moment short of the linger; a person's tab with the same end; a worker stalled, whose tab
/// stays.
#[test]
fn a_concluded_workers_tab_goes_after_its_linger_parked_or_not() {
    use Ending::ClosePane;
    let mut app = crate::App::headless_for_test();
    let parked = parked_worker(&mut app);
    let shown = worker_tab(&mut app);
    let persons = new_tab(&mut app);
    let limited = worker_tab(&mut app);
    let stalled = worker_tab(&mut app);
    let ends = [
        conclude(&app, parked, TaskState::Done, Some(WHY_LANDED)),
        conclude(&app, shown, TaskState::Failed, Some(WHY_REPEATED)),
        conclude(&app, persons, TaskState::Done, Some(WHY_LANDED)),
        conclude(&app, limited, TaskState::Failed, Some(WHY_USAGE_LIMIT)),
        conclude(&app, stalled, TaskState::Stalled, Some("held")),
    ];
    let first = ends.into_iter().min().expect("ends");
    let last = ends.into_iter().max().expect("ends");
    assert_eq!(app.reap_workers(first + LINGER - MOMENT), []);
    assert!(app.pool.get(parked).is_some() && app.pool.get(shown).is_some());

    let mut steps = app.reap_workers(last + LINGER);
    steps.sort_unstable_by_key(|(session, _)| *session);
    let mut closed = vec![
        (parked, ClosePane),
        (shown, ClosePane),
        (limited, ClosePane),
    ];
    closed.sort_unstable_by_key(|(session, _)| *session);
    assert_eq!(steps, closed);
    for (session, state) in [
        (parked, TaskState::Done),
        (shown, TaskState::Failed),
        (limited, TaskState::Failed),
    ] {
        assert!(app.pool.get(session).is_none(), "its tab went");
        assert_eq!(closed_by(&app, session), "harness");
        let row = retired(&app, session).expect("its retired row");
        assert_eq!((row.window, row.record.state), (Some(WID), state));
    }
    for session in [persons, stalled] {
        assert!(app.pool.get(session).is_some(), "CONTROL: {session} stays");
        assert!(retired(&app, session).is_none());
    }
    let rows = app.worker_rows(WID);
    assert_eq!(
        rows.iter()
            .filter(|row| row.finished)
            .map(|row| row.session)
            .collect::<Vec<_>>(),
        [parked, shown, limited],
        "its Finished rows, oldest first"
    );
    assert!(
        app.windows[&WID].tab_set.home().is_some(),
        "the Workers tab stays while a Finished row does"
    );
    assert_eq!(
        app.reap_workers(last + LINGER * 100),
        [],
        "a person's tab never goes, nor a stalled worker's"
    );
}

/// A CONCLUDED WORKER SOMEONE HAS STAYS: on screen in a focused window, typed in since its end,
/// driven, or its window's last tab, it is left; a person's typed turn reopens its task, so its
/// next end counts anew. CONTROL: the same worker in front of a window nobody focuses goes, and
/// so does one typed in only before its end.
#[test]
fn a_concluded_worker_someone_has_stays() {
    use Ending::{ClosePane, Leave};
    let mut app = crate::App::headless_for_test();
    let ws = app.windows.get_mut(&WID).expect("its window");
    ws.focused = true;
    ws.os_window_for_test = true;
    let watched = worker_tab(&mut app);
    let ended = conclude(&app, watched, TaskState::Done, Some(WHY_LANDED));
    assert_eq!(app.focused_session_id(WID), Some(watched), "PRECONDITION");
    assert_eq!(app.reap_workers(ended + LINGER), [(watched, Leave)]);
    app.windows.get_mut(&WID).expect("its window").focused = false;
    assert_eq!(
        app.reap_workers(ended + LINGER),
        [(watched, ClosePane)],
        "CONTROL: nobody looks at it"
    );

    let typed = worker_tab(&mut app);
    let ended = conclude(&app, typed, TaskState::Done, Some(WHY_LANDED));
    live(&app, typed)
        .ctx
        .human_input
        .note(crate::metrics::now_us(), crate::human_input::Gesture::Typed);
    assert_eq!(
        app.reap_workers(ended + LINGER * 10),
        [],
        "typed since its end"
    );
    let reopened = conclude(&app, typed, TaskState::Working, Some("turn"));
    assert_eq!(
        app.reap_workers(reopened + LINGER * 10),
        [],
        "its turn reopened the task"
    );
    let ended = conclude(&app, typed, TaskState::Done, Some(WHY_LANDED));
    assert_eq!(
        app.reap_workers(ended + LINGER - MOMENT),
        [],
        "its next end counts anew"
    );

    let before = worker_tab(&mut app);
    let ended = conclude(&app, before, TaskState::Done, Some(WHY_LANDED));
    let linger_ms = u64::try_from(LINGER.as_millis()).expect("small");
    live(&app, before)
        .ctx
        .human_input
        .carry_typed_awake(linger_ms + 60_000, crate::metrics::now_us());
    assert_eq!(
        of(app.reap_workers(ended + LINGER), before),
        [(before, ClosePane)],
        "CONTROL: typed only before its end"
    );

    let driven = worker_tab(&mut app);
    let ended = conclude(&app, driven, TaskState::Done, Some(WHY_LANDED));
    *live(&app, driven)
        .ctx
        .turn_lease
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(crate::Lease::Drive {
        holder: crate::harness_host::holder(),
        expires_us: u64::MAX,
        conn: None,
        hard: true,
        here: false,
        holder_pid: None,
    });
    assert_eq!(
        of(app.reap_workers(ended + LINGER), driven),
        [(driven, Leave)]
    );

    let front = 0;
    mark_ephemeral(&mut app, front);
    assert!(app.close_session_tab_for_test(typed));
    assert!(app.close_session_tab_for_test(driven));
    let ended = conclude(&app, front, TaskState::Done, Some(WHY_LANDED));
    assert_eq!(
        app.reap_workers(ended + LINGER),
        [(front, Leave)],
        "a window's last tab asks before it goes"
    );
    assert!(app.pool.get(front).is_some());
}

/// KEEP AS TAB MAKES A WORKER'S TAB A PERSON'S: it leaves the parked tabs and loses its mark, so
/// its task's end never closes it, nor does the mark come back with a later record. CONTROL:
/// before it, the same end closes a worker's tab
/// (`a_concluded_workers_tab_goes_after_its_linger_parked_or_not`); a person's tab has no Keep
/// as Tab.
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

/// THE LOOK ENDS A WORKER WHOSE PROCESS NEVER KNEW IT: once a claim worker's open record has stood
/// its time the worker's program is asked to end (`SIGTERM` first, however long a hand held it
/// past its time; `SIGKILL` follows only one that reached it, which a stub tab's never does);
/// with its program gone and no hand having marked its task, its task is marked `exited`, which
/// concludes it, and its tab goes in the harness's name once that end has stood [`LINGER`].
/// CONTROL: a moment before each; a tab a driver has; a `SIGTERM` the verb refused; a tab of
/// another folder; a window's last tab.
#[test]
fn the_look_closes_a_claim_workers_tab_once_its_job_ended() {
    use Ending::{ClosePane, Leave, MarkExited, Term};
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
    assert_eq!(app.reap_workers(exited + LINGER), [(worker, ClosePane)]);
    assert!(
        app.pool.get(worker).is_none(),
        "its job ended, its tab went"
    );
    assert_eq!(closed_by(&app, worker), "harness");

    assert_eq!(app.reap_workers(project + time), []);
    assert!(app.pool.get(other).is_some(), "a tab of another folder");

    let second = set_task(&app, other, &claim);
    assert_eq!(app.reap_workers(second + time), [(other, MarkExited)]);
    let (_, exited) = record(&app, other);
    assert_eq!(app.reap_workers(exited + LINGER), [(other, ClosePane)]);
    let last = set_task(&app, front, &claim);
    assert_eq!(app.reap_workers(last + time), [(front, MarkExited)]);
    let (_, exited) = record(&app, front);
    assert_eq!(app.reap_workers(exited + LINGER), [(front, Leave)]);
    assert!(app.pool.get(front).is_some(), "a window's last tab asks");
}

/// A PERSON'S HAND IN A WORKER'S TAB IS THEIRS: their typing gives a claim worker its time again,
/// a claim worker that ran out has its Autopilot Off even with its tab left in front, and a job in
/// the tab of a worker whose program exited is theirs and never signalled, nor is another
/// program's job in a worker's tab that nothing marked exited. CONTROL: a concluded worker whose
/// tab stays keeps its Autopilot; it goes Off only as its program is asked to end.
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
    assert_eq!(app.reap_workers(at + LINGER), [(exited, Leave)]);
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
/// concluded already, so its end stands, its reply and its `why=` with it, and its tab goes as
/// concluded; a landed worker likewise. CONTROL: a record that moved since the look read it is not
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
        [(limited, Ending::ClosePane)],
        "concluded at its wall: no exit mark, its tab goes"
    );
    let kept = retired(&app, limited).expect("its retired row").record;
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
        [(landed, Ending::ClosePane)],
        "a landed worker's tab goes as it landed"
    );
    assert_eq!(
        retired(&app, landed).map(|row| row.record.state),
        Some(TaskState::Done)
    );

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

/// A PROGRAM SOMEONE RUNS IN AN ENDED WORKER'S TAB KEEPS ITS OUTPUT TO BE READ: every look sees it
/// running, its tab's linger out or not, and the tab closes only [`LINGER`] after the last look
/// that saw it, not as soon as it ends. CONTROL: a tab with nothing run there closes once its
/// exit mark has stood its linger.
#[test]
fn a_program_run_in_an_ended_workers_tab_keeps_its_output_to_be_read() {
    use Ending::{ClosePane, Leave, MarkExited};
    let claim = claim_folder();
    let minute = Duration::from_secs(60);
    let mut app = crate::App::headless_for_test();
    let used = worker_tab(&mut app);
    let idle = worker_tab(&mut app);
    let began = set_task(&app, used, &claim);
    let idle_began = set_task(&app, idle, &claim);
    let marked = app.reap_workers(began.max(idle_began) + WORKER_FOR);
    for tab in [used, idle] {
        assert_eq!(of(marked.clone(), tab), [(tab, MarkExited)], "PRECONDITION");
    }
    let exited = record(&app, used).1;
    let idle_exited = record(&app, idle).1;
    crate::feed_session_for_test(
        &app,
        used,
        b"\x1b]133;A\x07PS> \x1b]133;B\x07make\n\x1b]133;C\x07",
    );
    let seen = exited + LINGER - minute;
    assert_eq!(
        of(app.reap_workers(seen), used),
        [],
        "its linger is not out, and the look sees the program"
    );
    crate::feed_session_for_test(&app, used, b"\x1b]133;D;0\x07");
    let steps = app.reap_workers(exited.max(idle_exited) + LINGER);
    assert_eq!(
        of(steps.clone(), used),
        [(used, Leave)],
        "ended within its linger: its output is still to be read"
    );
    assert_eq!(
        of(steps, idle),
        [(idle, ClosePane)],
        "CONTROL: nothing run there"
    );
    assert_eq!(
        of(app.reap_workers(seen + LINGER), used),
        [(used, ClosePane)]
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
