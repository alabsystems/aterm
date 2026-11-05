// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `tab` marks (the Workers as subagents design (2026-11-04) §7, §8): one per act on a
//! tab, each by who acted, as `aterm ctl log kinds=tab` lists them.

use std::sync::{Arc, PoisonError};

use super::ActScope;
use crate::session_store::{ExitActor, ExitReason};
use crate::task_record::{TaskMark, TaskState};
use crate::{App, SessionCtx, WindowId, stub_session};

const WID: WindowId = WindowId(0);

/// `log kinds=tab` of `ctx`'s session: its `tab` marks' lines, oldest first.
fn tab_lines(ctx: &SessionCtx) -> Vec<String> {
    let reply = crate::control::cmd_log(ctx, "kinds=tab");
    assert!(reply.starts_with("OK "), "{reply}");
    reply.lines().skip(1).map(str::to_string).collect()
}

/// A person's new tab in the headless app's window, and its context.
fn person_tab(app: &mut App) -> (u64, Arc<SessionCtx>) {
    let id = app.next_session_id;
    app.push_stub_tab(WID, stub_session(id));
    app.sync_window(WID);
    (id, ctx(app, id))
}

/// A worker landed parked under the window's Workers tab.
fn parked_worker(app: &mut App) -> (u64, Arc<SessionCtx>) {
    let id = app.next_session_id;
    app.next_session_id += 1;
    let parent = app.new_tab_parent(WID);
    app.install_tab_session_parked(WID, stub_session(id), parent);
    app.sync_window(WID);
    (id, ctx(app, id))
}

fn ctx(app: &App, id: u64) -> Arc<SessionCtx> {
    Arc::clone(&app.pool.get(id).expect("its session").ctx)
}

fn sid_of(ctx: &SessionCtx) -> String {
    ctx.self_id.as_str().to_string()
}

/// `local` leaves the registry as `reason` by `actor`, through the close's one choke point.
fn deregister(app: &App, local: u64, reason: ExitReason, actor: ExitActor) {
    let _ = app
        .store
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .deregister_closing(local, reason, actor);
}

/// The value of `key=` on `line`.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(' ')
        .find_map(|token| token.strip_prefix(key)?.strip_prefix('='))
}

/// A PERSON'S NEW TAB OPENS `by=human why=new-tab`, naming its window, that it is shown and the
/// tab it was opened from, and `log kinds=tab` lists it. CONTROL: the same open inside an act of
/// aterm's own (a credit claim's worker) is that act's, `by=harness why=claim-credits`; another
/// kind's filter lists none of the marks.
#[test]
fn a_persons_new_tab_opens_by_human_and_an_acts_open_by_its_actor() {
    let mut app = App::headless_for_test();
    let first = sid_of(&ctx(&app, 0));
    let (_, person) = person_tab(&mut app);
    let lines = tab_lines(&person);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let open = &lines[0];
    assert!(
        open.contains(&format!(
            " mark - tab act=open why=new-tab parent={first} window=0 parked=0 by=human src=aterm conf=exact "
        )),
        "{open}"
    );
    let (_, claimed) = {
        let _act = ActScope::enter(ExitActor::Harness, "claim-credits");
        person_tab(&mut app)
    };
    let lines = tab_lines(&claimed);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(field(&lines[0], "why"), Some("claim-credits"), "{lines:?}");
    assert_eq!(field(&lines[0], "by"), Some("harness"), "{lines:?}");
    let reply = crate::control::cmd_log(&person, "kinds=mail-gap");
    assert!(!reply.contains(" tab "), "CONTROL: {reply}");
    assert_eq!(
        crate::session_log::LogQuery::parse("kinds=tabs").err(),
        Some(crate::session_log::LOG_USAGE),
        "CONTROL: a kind no log has is refused"
    );
}

/// A WORKER A MANAGER SPAWNED OPENS PARKED, BY THE MANAGER'S SESSION, AND A PERSON'S KEEP SHOWS
/// IT: `act=open why=task parked=1 by=<manager sid>`, then `act=keep parked=0 by=human` in the
/// same window under the same parent. CONTROL: a person's own tab is no worker's, so Keep as Tab
/// keeps nothing there and writes no mark.
#[test]
fn a_parked_worker_opens_by_its_manager_and_a_keep_shows_it_by_the_person() {
    let mut app = App::headless_for_test();
    let manager = sid_of(&ctx(&app, 0));
    let (worker, worker_ctx) = {
        let _act = ActScope::enter(ExitActor::Sid(manager.clone()), "task");
        parked_worker(&mut app)
    };
    let opened = tab_lines(&worker_ctx);
    assert_eq!(opened.len(), 1, "{opened:?}");
    for (key, want) in [
        ("act", "open"),
        ("why", "task"),
        ("parent", manager.as_str()),
        ("parked", "1"),
        ("by", manager.as_str()),
    ] {
        assert_eq!(field(&opened[0], key), Some(want), "{key}: {opened:?}");
    }
    assert!(app.keep_as_tab(worker));
    let lines = tab_lines(&worker_ctx);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[1].contains(&format!(
            " mark - tab act=keep parent={manager} window=0 parked=0 by=human src=aterm conf=exact "
        )),
        "{lines:?}"
    );
    let (person, person_ctx) = person_tab(&mut app);
    assert!(!app.keep_as_tab(person), "PRECONDITION: a person's tab");
    assert_eq!(tab_lines(&person_ctx).len(), 1, "CONTROL: its open alone");
}

/// A CLOSE NAMES ITS REASON AND ITS ACTOR AS THE SESSION'S CLOSE DOES, AND ITS TOTALS: the wall
/// time from the tab's open, and `unavailable` for what aterm keeps for no tab's life (CPU, peak
/// memory) or did not read (a shell's tokens and login). It comes before the session's close.
/// CONTROL: a session no tab ever held (registered without a place) closes with no `tab` mark;
/// none of the totals is written `0`.
#[test]
fn a_close_names_reason_actor_and_totals_unavailable_never_zero() {
    let mut app = App::headless_for_test();
    let (tab, tab_ctx) = person_tab(&mut app);
    deregister(&app, tab, ExitReason::UiClose, ExitActor::Human);
    let lines = tab_lines(&tab_ctx);
    assert_eq!(lines.len(), 2, "{lines:?}");
    let close = &lines[1];
    for (key, want) in [
        ("act", "close"),
        ("window", "0"),
        ("parked", "0"),
        ("out", "ui-close"),
        ("cpu_s", "unavailable"),
        ("peak_mb", "unavailable"),
        ("tokens", "unavailable"),
        ("account", "unavailable"),
        ("by", "human"),
    ] {
        assert_eq!(field(close, key), Some(want), "{key}: {close}");
    }
    assert!(
        field(close, "wall_s").is_some_and(|s| s.parse::<u64>().is_ok()),
        "{close}"
    );
    assert_eq!(
        field(close, "why"),
        None,
        "a person's close has no act's reason"
    );
    assert_eq!(field(close, "task"), None, "a shell's tab holds no task");
    let log = crate::control::cmd_log(&tab_ctx, "");
    let at = |needle: &str| log.lines().position(|l| l.contains(needle));
    assert!(
        at(" mark - tab act=close ") < at(" close - spawn "),
        "the tab's close first:\n{log}"
    );
    let bare = crate::stub_session(900);
    let bare_ctx = Arc::clone(&bare.ctx);
    App::register_session(&app.store, &bare, None);
    deregister(&app, 900, ExitReason::UiClose, ExitActor::Human);
    assert_eq!(
        tab_lines(&bare_ctx),
        Vec::<String>::new(),
        "CONTROL: no tab, no close"
    );
}

/// THE REAPER'S CLOSE OF A CONCLUDED WORKER IS aterm's ACT: `why=concluded out=reaper by=harness`
/// with the task as it stood, `failed:could-not-finish`, and the exit ledger's row says `reaper`
/// too. CONTROL: a person closing the same kind of tab says neither `concluded` nor `reaper`.
#[test]
fn the_reapers_close_is_harness_concluded_with_the_task_it_ended() {
    let mut app = App::headless_for_test();
    let (worker, worker_ctx) = parked_worker(&mut app);
    let mut mark = TaskMark::new(TaskState::Failed);
    mark.why = Some(crate::task_record::WHY_COULD_NOT_FINISH);
    crate::task_record::mark_session_task(&worker_ctx, worker, &mark).expect("unfenced");
    let (_, since) = crate::worker_reaper::tests::record(&app, worker);
    let steps = app.reap_workers(since + crate::worker_reaper::LINGER);
    assert_eq!(
        steps.len(),
        1,
        "PRECONDITION: the reaper closed it: {steps:?}"
    );
    let lines = tab_lines(&worker_ctx);
    let close = lines.last().expect("its close");
    for (key, want) in [
        ("act", "close"),
        ("why", "concluded"),
        ("parked", "1"),
        ("out", "reaper"),
        ("task", "failed:could-not-finish"),
        ("by", "harness"),
    ] {
        assert_eq!(field(close, key), Some(want), "{key}: {close}");
    }
    let exits = crate::control::cmd_exits(&app.store, "", &[]);
    assert!(exits.contains(" reason=reaper "), "{exits}");

    let (person_worker, person_ctx) = parked_worker(&mut app);
    crate::task_record::mark_session_task(&person_ctx, person_worker, &mark).expect("unfenced");
    deregister(&app, person_worker, ExitReason::UiClose, ExitActor::Human);
    let close = tab_lines(&person_ctx).pop().expect("its close");
    assert_eq!(field(&close, "why"), None, "CONTROL: {close}");
    assert_eq!(field(&close, "out"), Some("ui-close"), "CONTROL: {close}");
    assert_eq!(field(&close, "task"), Some("failed:could-not-finish"));
}

/// A WORKER WHOSE SPAWN NOBODY TOOK CLOSES `by=harness why=unstarted`, the act that closed it.
/// CONTROL: an open's act (`spawn`) is never a close's reason.
#[test]
fn an_unstarted_workers_close_says_unstarted_and_an_opens_reason_never_closes() {
    let mut app = App::headless_for_test();
    let (worker, worker_ctx) = parked_worker(&mut app);
    {
        let _act = ActScope::closing(ExitActor::Harness, "unstarted");
        deregister(&app, worker, ExitReason::CtlClose, ExitActor::Harness);
    }
    let close = tab_lines(&worker_ctx).pop().expect("its close");
    assert_eq!(field(&close, "why"), Some("unstarted"), "{close}");
    assert_eq!(field(&close, "by"), Some("harness"), "{close}");

    let (other, other_ctx) = parked_worker(&mut app);
    {
        let _act = ActScope::enter(ExitActor::Harness, "spawn");
        deregister(&app, other, ExitReason::CtlClose, ExitActor::Harness);
    }
    let close = tab_lines(&other_ctx).pop().expect("its close");
    assert_eq!(field(&close, "act"), Some("close"));
    assert_eq!(field(&close, "why"), None, "CONTROL: {close}");
}

/// A CLOSE BY A CONTROL CLIENT NAMES IT AS THE EXIT LEDGER DOES: a fleet drive `fleet:<host>` in
/// both the tab's mark and `exits`, a manager's session by its sid. CONTROL: an unidentified
/// client is `ctl`, and a close attribution's actor opens a tab as that actor with the place's
/// own reason (a control client's `tab` gesture that opens one under no act).
#[test]
fn a_control_clients_close_names_it_as_the_exit_ledger_does() {
    let mut app = App::headless_for_test();
    let (driven, driven_ctx) = person_tab(&mut app);
    let by = {
        let _drive = crate::fabric::DrivingAs::enter(crate::fabric::Driver {
            node: "n-m9".to_string(),
            host: "m9".to_string(),
        });
        crate::control::caller_actor_of(crate::control::Scope::Owner, None, &app.store)
    };
    assert_eq!(by, ExitActor::Fleet("fleet:m9".to_string()));
    deregister(&app, driven, ExitReason::CtlClose, by);
    let close = tab_lines(&driven_ctx).pop().expect("its close");
    assert_eq!(field(&close, "by"), Some("fleet:m9"), "{close}");
    let exits = crate::control::cmd_exits(&app.store, "", &[]);
    assert!(exits.contains(" by=fleet:m9 "), "{exits}");
    assert_eq!(
        crate::control::caller_actor_of(crate::control::Scope::Owner, None, &app.store),
        ExitActor::Ctl,
        "CONTROL: no drive, no tab: an unidentified client"
    );

    let manager = sid_of(&ctx(&app, 0));
    let (asked, asked_ctx) = {
        let _closing = crate::session_store::CloseAttribution::enter(
            ExitReason::CtlClose,
            ExitActor::Sid(manager.clone()),
        );
        person_tab(&mut app)
    };
    let open = tab_lines(&asked_ctx).pop().expect("its open");
    assert_eq!(field(&open, "why"), Some("new-tab"), "{open}");
    assert_eq!(field(&open, "by"), Some(manager.as_str()), "{open}");
    deregister(
        &app,
        asked,
        ExitReason::CtlClose,
        ExitActor::Sid(manager.clone()),
    );
    let close = tab_lines(&asked_ctx).pop().expect("its close");
    assert_eq!(field(&close, "by"), Some(manager.as_str()), "{close}");
}

/// AN OWNER CLIENT RUN FROM A TAB IS THAT TAB'S SESSION: its process, or the nearest ancestor of
/// it, is a tab's shell. CONTROL: a process no tab's shell is an ancestor of is in no tab, and
/// aterm's own harness token is the harness, not `ctl`.
#[test]
fn an_owner_client_run_from_a_tab_is_that_tabs_session() {
    let app = App::headless_for_test();
    let mut handle = crate::session_store::test_handle(77);
    handle.shell_identity.pid = 6999;
    let tab_sid = handle.sid.as_str().to_string();
    app.store
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .register(handle);
    let shell = 6999;
    let ancestry = move |pid: i32| match pid {
        7001 => Some(7000),
        7000 => Some(shell),
        _ => None,
    };
    let client = crate::typed_by::Who::client_with(Some(7001), 1, &ancestry, &|_| None, &|_| {
        Some("aterm".to_string())
    });
    assert_eq!(
        crate::control::owner_tab_sid(&client, &app.store),
        Some(tab_sid)
    );
    let stranger =
        crate::typed_by::Who::client_with(Some(8001), 1, &|_| None, &|_| None, &|_| None);
    assert_eq!(
        crate::control::owner_tab_sid(&stranger, &app.store),
        None,
        "CONTROL: a process in no tab"
    );
    let harness = {
        let _principal =
            crate::control::ServingPrincipal::enter(crate::control::Principal::Harness);
        crate::control::caller_actor_of(crate::control::Scope::Owner, None, &app.store)
    };
    assert_eq!(harness, ExitActor::Harness, "CONTROL: aterm's own token");
}

/// Land the spawn `app` booked last (its app books unsent, `TabSpawns::unsent`), as the spawner
/// thread's answer would: the tab it opened and its context.
fn land_booked(app: &mut App) -> (u64, Arc<SessionCtx>) {
    let id = app.next_session_id - 1;
    assert!(
        app.install_spawned_tab(crate::tab_spawn::TabSpawned {
            id,
            window: WID,
            session: Ok(stub_session(id)),
            reader_gate: None,
        }),
        "PRECONDITION: the booked tab landed"
    );
    (id, ctx(app, id))
}

/// A CONTROL CLIENT'S `tab new` OPENS BY THAT CALLER, across the spawn that lands it later:
/// `aterm ctl tab new` from a manager's tab says `by=<manager> why=new-tab`, never `by=human`.
/// CONTROL: the person's own new tab, booked the same way under no act, is `by=human`.
#[test]
fn a_control_clients_tab_new_opens_by_the_caller_across_its_spawn() {
    let mut app = App::headless_for_test();
    app.tab_spawns.unsent = true;
    let manager = sid_of(&ctx(&app, 0));
    let (reply, _answer) = std::sync::mpsc::channel();
    app.tab_new_answered(Ok(WID), ExitActor::Sid(manager.clone()), reply);
    let (_, asked) = land_booked(&mut app);
    let open = tab_lines(&asked).pop().expect("its open");
    assert_eq!(field(&open, "act"), Some("open"), "{open}");
    assert_eq!(field(&open, "why"), Some("new-tab"), "{open}");
    assert_eq!(field(&open, "by"), Some(manager.as_str()), "{open}");

    app.open_tab_off_loop(WID, None, crate::tab_spawn::TabSpawnThen::Nothing);
    let (_, own) = land_booked(&mut app);
    let open = tab_lines(&own).pop().expect("its open");
    assert_eq!(field(&open, "by"), Some("human"), "CONTROL: {open}");
}

/// A GESTURE A CONTROL CLIENT'S `invoke` DRIVES IS THAT CLIENT'S: a tab a gesture opens under
/// `ActScope::gesture` (Summarize, a sign-in, an install) names the invoking caller.
/// CONTROL: the same gesture from the person (no invoke under way) is `by=human`.
#[test]
fn a_gesture_an_invoke_drives_opens_by_the_invoking_client() {
    let mut app = App::headless_for_test();
    let manager = sid_of(&ctx(&app, 0));
    let invoked = app.invoke_attributed(ExitActor::Sid(manager.clone()), |app| {
        let _act = ActScope::gesture("signin");
        Ok(sid_of(&person_tab(app).1))
    });
    let invoked = invoked.expect("the gesture ran");
    let ctx_of = |app: &App, sid: &str| {
        let g = app.store.read().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            &g.by_sid(&aterm_session::SessionId::new(sid))
                .expect("its tab")
                .ctx,
        )
    };
    let open = tab_lines(&ctx_of(&app, &invoked)).pop().expect("its open");
    assert_eq!(field(&open, "why"), Some("signin"), "{open}");
    assert_eq!(field(&open, "by"), Some(manager.as_str()), "{open}");
    let (_, own) = {
        let _act = ActScope::gesture("signin");
        person_tab(&mut app)
    };
    let open = tab_lines(&own).pop().expect("its open");
    assert_eq!(field(&open, "by"), Some("human"), "CONTROL: {open}");
}

/// THE LAUNCH'S FIRST TAB OPENS: built before the App, it is marked once the App stands, by the
/// person (`why=new-window`), or by aterm's restore where a manifest is to place it
/// (`by=harness why=restored`), and its close then closes it. CONTROL: before that, the first tab
/// had no open, and its close wrote no `tab` mark.
#[test]
fn the_launchs_first_tab_opens_by_who_launched_or_by_the_restore() {
    for (restoring, by, why) in [
        (false, "human", "new-window"),
        (true, "harness", "restored"),
    ] {
        let app = App::headless_for_test();
        let first = ctx(&app, 0);
        assert!(tab_lines(&first).is_empty(), "PRECONDITION: no open yet");
        app.note_launch_tab(restoring);
        let lines = tab_lines(&first);
        assert_eq!(lines.len(), 1, "{lines:?}");
        for (key, want) in [
            ("act", "open"),
            ("why", why),
            ("window", "0"),
            ("parked", "0"),
            ("by", by),
        ] {
            assert_eq!(field(&lines[0], key), Some(want), "{key}: {lines:?}");
        }
        deregister(&app, 0, ExitReason::UiClose, ExitActor::Human);
        let close = tab_lines(&first).pop().expect("its close");
        assert_eq!(field(&close, "act"), Some("close"), "{close}");
    }
}

/// A fake process table: shell 100, its agent 200 (alive), the agent's build 300 (alive), and a
/// grandchild 400 whose counters the kernel refuses; 200 lists itself once more, which is read
/// once.
fn tree_children(pid: u32) -> Option<Vec<u32>> {
    match pid {
        100 => Some(vec![200]),
        200 => Some(vec![300, 200]),
        300 => Some(vec![400]),
        _ => Some(Vec::new()),
    }
}

fn tree_counters(pid: u32) -> Option<aterm_sysprobe::jobs::Counters> {
    let c = |cpu_s: u64, reaped_s: u64, peak_mib: u64| aterm_sysprobe::jobs::Counters {
        cpu_ns: cpu_s * 1_000_000_000,
        child_cpu_ns: Some(reaped_s * 1_000_000_000),
        peak_footprint: Some(peak_mib * 1024 * 1024),
        ..aterm_sysprobe::jobs::Counters::default()
    };
    match pid {
        100 => Some(c(1, 2, 5)),
        200 => Some(c(30, 10, 6144)),
        300 => Some(c(5, 0, 1024)),
        _ => None,
    }
}

const TREE: super::TreeProbe = super::TreeProbe {
    children: tree_children,
    counters: tree_counters,
};

/// A TAB'S TREE SPENT WHAT ITS COUNTERS HOLD: each live process's own CPU and what it reaped of
/// its children, once each, and the largest footprint any of them reached. CONTROL: a shell the
/// kernel cannot read gives `None` for both (never `0`), and so does a session with no shell.
#[test]
fn a_tabs_tree_spent_each_live_processs_own_and_reaped_time_once() {
    let spent = super::tree_spent(100, &TREE);
    assert_eq!(spent.cpu_ns, Some(48 * 1_000_000_000), "{spent:?}");
    assert_eq!(spent.peak_bytes, Some(6144 * 1024 * 1024), "{spent:?}");
    let gone = super::tree_spent(999, &TREE);
    assert_eq!(gone, crate::session_log::TreeSpent::default(), "CONTROL");
    assert_eq!(
        super::tree_spent(-1, &TREE),
        crate::session_log::TreeSpent::default(),
        "CONTROL: no shell"
    );
}

/// THE REAPER'S CLOSE STAMPS WHAT THE WORKER'S TREE SPENT, read before its terminal closed: its
/// CPU-seconds and peak MiB, and its agent's tokens and login though the agent had left the front
/// (Stop, then the close). CONTROL: with a probe that reads nothing, both process totals are
/// `unavailable`.
#[test]
fn the_reapers_close_stamps_the_workers_cpu_peak_and_tokens() {
    for (probe, cpu, peak) in [
        (TREE, "48", "6144"),
        (super::TreeProbe::INERT, "unavailable", "unavailable"),
    ] {
        let mut app = App::headless_for_test();
        app.tree_probe = probe;
        let id = app.next_session_id;
        app.next_session_id += 1;
        let parent = app.new_tab_parent(WID);
        let mut session = stub_session(id);
        session.pid = 100;
        app.install_tab_session_parked(WID, session, parent);
        app.sync_window(WID);
        let worker_ctx = ctx(&app, id);
        {
            let mut tl = worker_ctx
                .timeline
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            assert!(tl.note_foreground_group(4242));
            let facts = aterm_agent::harness::footer::FooterFacts {
                usage: Some(aterm_agent::harness::session_usage::UsageFacts {
                    models: vec![aterm_agent::harness::session_usage::ModelTokens {
                        label: "opus".to_string(),
                        input: 40_000,
                        output: 8_211,
                    }],
                    complete: true,
                    ..Default::default()
                }),
                account: Some("m9-2".to_string()),
                ..Default::default()
            };
            assert!(tl.set_claude_footer(4242, Some(9), Some(facts)));
            assert!(tl.note_foreground_group(100), "the agent left the front");
        }
        let mut mark = TaskMark::new(TaskState::Failed);
        mark.why = Some(crate::task_record::WHY_COULD_NOT_FINISH);
        crate::task_record::mark_session_task(&worker_ctx, id, &mark).expect("unfenced");
        let (_, since) = crate::worker_reaper::tests::record(&app, id);
        let steps = app.reap_workers(since + crate::worker_reaper::LINGER);
        assert_eq!(steps.len(), 1, "PRECONDITION: reaped: {steps:?}");
        let close = tab_lines(&worker_ctx).pop().expect("its close");
        assert_eq!(field(&close, "cpu_s"), Some(cpu), "{close}");
        assert_eq!(field(&close, "peak_mb"), Some(peak), "{close}");
        assert_eq!(field(&close, "tokens"), Some("48211"), "{close}");
        assert_eq!(field(&close, "account"), Some("m9-2"), "{close}");
    }
}

/// A TAB'S CLOSE NAMES WHERE IT STANDS AT ITS CLOSE: a tab moved to a new window closes there,
/// a pane split inside a parked worker's tab opens and closes parked with it, and Keep as Tab
/// keeps every session of the worker's tab. CONTROL: the tab that stayed closes in its window.
#[test]
fn a_tabs_close_names_the_window_and_parking_it_stands_in_at_its_close() {
    let mut app = App::headless_for_test();
    let (stays, stays_ctx) = person_tab(&mut app);
    let (moved, moved_ctx) = person_tab(&mut app);
    let to = app.detach_active_tab_logical().expect("a new window");
    assert_ne!(to, WID);
    deregister(&app, moved, ExitReason::UiClose, ExitActor::Human);
    let close = tab_lines(&moved_ctx).pop().expect("its close");
    assert_eq!(
        field(&close, "window"),
        Some(to.0.to_string().as_str()),
        "{close}"
    );
    deregister(&app, stays, ExitReason::UiClose, ExitActor::Human);
    let close = tab_lines(&stays_ctx).pop().expect("its close");
    assert_eq!(field(&close, "window"), Some("0"), "CONTROL: {close}");

    let mut app = App::headless_for_test();
    let (worker, worker_ctx) = parked_worker(&mut app);
    assert!(
        app.focus_session_preferring(Some(WID), worker),
        "the worker shows"
    );
    let pane = app.next_session_id;
    app.next_session_id += 1;
    app.install_split_session(WID, crate::pane::SplitDir::Vertical, stub_session(pane))
        .expect("split");
    let pane_ctx = ctx(&app, pane);
    let open = tab_lines(&pane_ctx).pop().expect("its open");
    assert_eq!(field(&open, "parked"), Some("1"), "{open}");
    assert!(app.keep_as_tab(worker));
    for ctx in [&worker_ctx, &pane_ctx] {
        let keep = tab_lines(ctx).pop().expect("its keep");
        assert_eq!(field(&keep, "act"), Some("keep"), "{keep}");
        assert_eq!(field(&keep, "parked"), Some("0"), "{keep}");
    }
}

/// A QUIT CLOSES EVERY TAB STILL OPEN, by whoever asked for it (`out=app-quit`), with its totals,
/// once. CONTROL: an unattended quit (`App::on_unattended_quit`'s actor) names nobody, and a
/// session no tab held writes nothing.
#[test]
fn a_quit_closes_every_open_tab_by_who_asked_once() {
    let mut app = App::headless_for_test();
    let (_, tab_ctx) = person_tab(&mut app);
    let bare = crate::stub_session(901);
    let bare_ctx = Arc::clone(&bare.ctx);
    App::register_session(&app.store, &bare, None);
    app.note_quit_by();
    app.close_tabs_at_quit();
    app.close_tabs_at_quit();
    let lines = tab_lines(&tab_ctx);
    assert_eq!(lines.len(), 2, "one open, one close: {lines:?}");
    for (key, want) in [
        ("act", "close"),
        ("out", "app-quit"),
        ("by", "human"),
        ("cpu_s", "unavailable"),
    ] {
        assert_eq!(field(&lines[1], key), Some(want), "{key}: {lines:?}");
    }
    assert!(tab_lines(&bare_ctx).is_empty(), "CONTROL: no tab, no close");

    let mut app = App::headless_for_test();
    let (_, tab_ctx) = person_tab(&mut app);
    app.quit_by = Some(ExitActor::Unknown);
    app.note_quit_by();
    app.close_tabs_at_quit();
    let close = tab_lines(&tab_ctx).pop().expect("its close");
    assert_eq!(field(&close, "out"), Some("app-quit"), "{close}");
    assert_eq!(
        field(&close, "by"),
        None,
        "CONTROL: nobody is named: {close}"
    );
}
