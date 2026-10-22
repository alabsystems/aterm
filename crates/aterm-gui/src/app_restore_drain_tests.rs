// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE COLD DRAIN ([`super::RestoreDrain`]): a cold launch's layout rebuild,
//! a slice per park. Every test drives the real slice driver with a zero
//! budget, so each slice lands exactly one window or tab: the order and the
//! interleaving with what a person does are the code's, never the clock's.

use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use crate::{App, WindowId, restore};

/// A one-pane terminal leaf named `title` by its operator title, which rides
/// onto whichever shell fills it (a fresh one or the grafted bootstrap) and
/// back into a capture — what these tests tell tabs apart by.
fn leaf(title: &str) -> restore::RestoredSplitTree {
    restore::RestoredSplitTree::leaf(restore::RestoredView::Terminal(
        restore::TerminalLeafRestore {
            sid: None,
            lineage: None,
            log_ep: None,
            cwd: Some("/tmp".to_string()),
            title: title.to_string(),
            profile: None,
            local_id: None,
            user_title: Some(title.to_string()),
            description: None,
            icon: None,
            role: None,
            attention: None,
            questions: None,
            autopilot: None,
            land_at: None,
            autopilot_by: None,
            theme: None,
            agent: None,
            supervised: None,
            lost: None,
            held: false,
            holds: Vec::new(),
        },
    ))
}

/// A window of one-pane tabs titled `titles`, `front` in front, as a quit
/// writes it.
fn window(titles: &[&str], front: usize) -> restore::WindowLayout {
    restore::WindowLayout {
        rows: 24,
        cols: 80,
        active_tab: 0,
        outer_x: None,
        outer_y: None,
        maximized: None,
        inner_px: None,
        presence_row: false,
        show: restore::WindowShow::UNKNOWN,
        tabs: Vec::new(),
        native_tabs: Vec::new(),
        tab_order: Vec::new(),
        active_item: Some(front),
        restored_tabs: titles
            .iter()
            .map(|title| restore::RestoredTab {
                root: leaf(title),
                focused_path: Vec::new(),
                zoomed: false,
            })
            .collect(),
    }
}

/// One slice of a desk whose windows after the first are logical windows
/// over a stub shell (a unit test has no event loop to create glass with).
fn slice(app: &mut App) -> bool {
    app.drain_restore_slice(Duration::ZERO, |app, first, wl, placed, filling| {
        if first {
            app.restore_bootstrap_window(wl, placed, Some(filling));
        } else {
            let wid = app.insert_logical_window(
                crate::stub_session(app.next_session_id),
                wl.rows,
                wl.cols,
            );
            placed.shown.push((wid, wl.show));
            app.fill_restored_window(wid, wl, Some(filling));
        }
        ControlFlow::Continue(())
    })
}

fn drain(app: &mut App) {
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while !slice(app) {
        assert!(Instant::now() < deadline, "the drain never ended");
    }
}

/// The operator title of the shell a tab shows (empty for one a person
/// opened).
fn named(app: &App, tab: &crate::tab_model::Tab) -> String {
    let session = app
        .view_store
        .get(tab.focus)
        .copied()
        .and_then(crate::tab_model::View::terminal_session)
        .expect("a terminal tab");
    app.pool
        .get(session)
        .expect("a live shell")
        .ctx
        .meta
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .user_title
        .clone()
        .unwrap_or_default()
}

fn titles(app: &App, wid: WindowId) -> Vec<String> {
    app.windows[&wid]
        .tab_set
        .tabs()
        .iter()
        .map(|tab| named(app, tab))
        .collect()
}

fn front(app: &App, wid: WindowId) -> String {
    named(
        app,
        app.windows[&wid].tab_set.active().expect("a front tab"),
    )
}

fn layout_titles(wl: &restore::WindowLayout) -> Vec<String> {
    wl.restored_tabs
        .iter()
        .map(|tab| match &tab.root {
            restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(leaf),
            } => leaf.user_title.clone().unwrap_or_default(),
            other => panic!("a one-pane terminal tab: {other:?}"),
        })
        .collect()
}

/// A person opens a tab in `wid`, as the tests of a live window do.
fn open_tab(app: &mut App, wid: WindowId) -> u64 {
    let id = app.next_session_id;
    let session = crate::stub_session(id);
    App::register_session(&app.store, &session, None);
    app.pool.insert(session);
    app.next_session_id += 1;
    let tree = crate::pane::PaneTree::new(id);
    let tab = crate::register_terminal_tab(&mut app.tab_ids, &mut app.view_store, &tree)
        .expect("tab identity");
    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.layouts.push(tree);
    ws.tabs.add();
    ws.tab_set.push(tab).expect("fresh tab id");
    app.resync_active_or_window(wid);
    id
}

/// The window that has painted gets its bootstrap tab back at once, then the
/// tab it had in front, each in its place; one tab per slice after that, and
/// the desk ends exactly as it was written, with the rebuild owed until then.
#[test]
fn a_cold_drain_lands_the_front_tab_second_and_ends_as_recorded() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(
        &["A", "B", "C", "D", "E"],
        3,
    )]));

    assert!(!slice(&mut app));
    assert_eq!(titles(&app, wid), ["A"], "the bootstrap's own tab, at once");
    assert!(app.restore_pass_queued(), "the rest is owed");
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, wid), ["A", "D"]);
    assert_eq!(front(&app, wid), "D", "the tab on glass lands second");
    let mut slices = 2;
    while !slice(&mut app) {
        slices += 1;
        assert!(app.restore_pass_queued());
    }
    assert_eq!(slices + 1, 5, "one tab per slice");
    assert_eq!(titles(&app, wid), ["A", "B", "C", "D", "E"]);
    assert_eq!(front(&app, wid), "D");
    assert!(app.restore_drain.is_none() && !app.restore_pass_queued());
    assert!(app.restored_window_sessions_are_distinct(wid));
    assert!(
        app.structural_invariants_ok(),
        "{}",
        app.structural_invariant_violation().unwrap_or_default()
    );
}

/// A tab the person closes while the drain runs stays closed, and one they
/// open is kept, after the restored tabs it was opened beside; their choice
/// of front tab stands.
#[test]
fn a_person_s_tabs_outlive_the_drain_and_a_closed_tab_stays_closed() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(
        &["A", "B", "C", "D", "E"],
        3,
    )]));
    assert!(!slice(&mut app));
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, wid), ["A", "D"]);

    // The person closes D (not the window's last tab, so the window stays).
    assert!(!app.close_tab_at(wid, 1));
    assert_eq!(titles(&app, wid), ["A"]);
    let mine = open_tab(&mut app, wid);
    let opened = front(&app, wid);
    drain(&mut app);

    let after = titles(&app, wid);
    assert_eq!(
        &after[..4],
        ["A", "B", "C", "E"],
        "D is not rebuilt: {after:?}"
    );
    assert_eq!(after.len(), 5, "{after:?}");
    assert_eq!(front(&app, wid), opened, "the person's front tab stands");
    assert!(app.pool.get(mine).is_some(), "the person's shell lives");
    assert!(
        app.structural_invariants_ok(),
        "{}",
        app.structural_invariant_violation().unwrap_or_default()
    );
}

/// Each window lands in turn, its own tabs a slice at a time; a window the
/// person closes while its tabs land owes nothing more, and is not made
/// again.
#[test]
fn the_drain_takes_windows_in_turn_and_a_closed_window_owes_nothing() {
    let mut app = App::headless_for_test();
    app.pending_restore = Some(restore::RestoreManifest::new(vec![
        window(&["A", "B"], 0),
        window(&["P", "Q", "R"], 2),
        window(&["X", "Y"], 1),
    ]));
    assert!(!slice(&mut app));
    assert_eq!(app.windows.len(), 1, "window 0 first, alone");
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, WindowId(0)), ["A", "B"]);
    assert!(!slice(&mut app));
    let second = *app.windows.keys().last().expect("the second window");
    assert_eq!(titles(&app, second), ["P"]);
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, second), ["P", "R"]);
    let _ = app.close_window_logical(second);
    drain(&mut app);

    assert!(!app.windows.contains_key(&second), "not made again");
    assert_eq!(app.windows.len(), 2, "window 0 and the third");
    let third = *app.windows.keys().last().expect("the third window");
    assert_eq!(titles(&app, third), ["X", "Y"]);
    assert_eq!(front(&app, third), "Y");
}

/// Half a desk is never the crash journal's image: while the drain runs the
/// journal captures nothing, however much activity it is told of, and its
/// first image after the drain is the whole layout.
#[cfg(unix)]
#[test]
fn the_crash_journal_captures_nothing_until_the_drain_is_done() {
    let mut app = App::headless_for_test();
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(
        &["A", "B", "C"],
        0,
    )]));
    let journal_dir = crate::crash_journal::tests::scratch("cj-drain");
    app.crash_journal = crate::crash_journal::Lane::new(
        Some(journal_dir.path().to_path_buf()),
        crate::crash_journal::tests::reaped_pid(),
    );
    app.first_present_done = true;
    let journal = app.crash_journal.journal_id();

    assert!(!slice(&mut app));
    app.crash_journal.note_activity();
    app.tick_crash_journal(Instant::now());
    assert_eq!(app.crash_journal.captures, 0, "nothing captured mid-drain");
    drain(&mut app);
    app.crash_journal.note_activity();
    app.tick_crash_journal(Instant::now());
    assert_eq!(
        app.crash_journal.captures, 1,
        "the drained desk is captured"
    );
    let (_, image) = aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the journal's image",
        || {
            std::fs::read_to_string(journal_dir.path().join(journal.file_name()))
                .ok()
                .and_then(|text| crate::crash_journal::decode(&text).ok())
        },
    );
    assert_eq!(layout_titles(&image.windows[0]), ["A", "B", "C"]);
    app.crash_journal = crate::crash_journal::Lane::inert();
    crate::crash_journal::tests::await_released(&journal_dir.path().join(journal.lock_name()));
}

/// A quit in the drain's first instants keeps every tab and window it still
/// owed: the quit's layout is the rebuilt part with the owed part after it,
/// and it parses back.
#[test]
fn a_quit_inside_the_drain_keeps_every_tab_and_window_it_owed() {
    let mut app = App::headless_for_test();
    app.pending_restore = Some(restore::RestoreManifest::new(vec![
        window(&["A", "B", "C", "D"], 2),
        window(&["X", "Y"], 0),
    ]));
    assert!(!slice(&mut app));
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, WindowId(0)), ["A", "C"]);

    let layout = app.capture_restore_manifest();
    assert_eq!(layout.windows.len(), 2, "the window not begun is carried");
    assert_eq!(layout_titles(&layout.windows[0]), ["A", "B", "C", "D"]);
    assert_eq!(layout.windows[0].active_item, Some(2), "C stays in front");
    assert_eq!(layout_titles(&layout.windows[1]), ["X", "Y"]);
    let parsed = restore::RestoreManifest::from_toml(&layout.to_toml().expect("the layout writes"))
        .expect("the layout parses back");
    assert_eq!(parsed.windows.len(), 2);
    assert_eq!(layout_titles(&parsed.windows[0]), ["A", "B", "C", "D"]);
}

/// A mid-drain quit has the same tab order as finishing the drain: the
/// restored tabs retain their manifest positions around the live tabs, a
/// closed restored tab stays closed, and the person's selected tab stays
/// selected after owed tabs are inserted before it.
#[test]
fn a_mid_drain_quit_preserves_live_tabs_and_the_persons_selection() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(
        &["A", "B", "C", "D", "E"],
        3,
    )]));
    assert!(!slice(&mut app));
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, wid), ["A", "D"]);
    assert!(!app.close_tab_at(wid, 1));
    let mine = open_tab(&mut app, wid);
    let selected = app.windows[&wid].tab_set.active_id();

    let layout = app.capture_restore_manifest();
    assert_eq!(layout_titles(&layout.windows[0]), ["A", "B", "C", "E", ""]);
    assert_eq!(layout.windows[0].active_item, Some(4));
    let parsed = restore::RestoreManifest::from_toml(&layout.to_toml().expect("the layout writes"))
        .expect("the layout parses back");
    assert_eq!(
        layout_titles(&parsed.windows[0]),
        layout_titles(&layout.windows[0])
    );
    assert_eq!(parsed.windows[0].active_item, Some(4));

    drain(&mut app);
    assert_eq!(titles(&app, wid), layout_titles(&layout.windows[0]));
    assert_eq!(app.windows[&wid].tab_set.active_id(), selected);
    assert!(app.pool.get(mine).is_some(), "the person's shell lives");
}

/// A tab that lands behind the one in front leaves that tab as the person
/// has it: a find bar they open while the drain runs stays open as the rest
/// of the desk lands.
#[test]
fn tabs_landing_behind_the_front_one_leave_its_find_bar_open() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.pending_restore = Some(restore::RestoreManifest::new(vec![window(
        &["A", "B", "C"],
        0,
    )]));
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, wid), ["A"]);
    app.search_enter();
    assert!(app.windows[&wid].search.is_some(), "find opens mid-drain");

    drain(&mut app);
    assert_eq!(titles(&app, wid), ["A", "B", "C"]);
    assert_eq!(front(&app, wid), "A");
    assert!(
        app.windows[&wid].search.is_some(),
        "find stays open while B and C land"
    );
}

/// An agent a leaf carries, as an earlier crash's or restart's layout names
/// it: its conversation `session`.
fn carried_agent(session: &str) -> Box<restore::AgentRestore> {
    Box::new(restore::AgentRestore {
        pid: 4242,
        start: "Sat Sep 27 01:02:03 2026".to_string(),
        program: "/opt/claude/bin/claude".to_string(),
        argv: vec!["/opt/claude/bin/claude".to_string()],
        session: Some(session.to_string()),
        cwd: "/tmp".to_string(),
        version: None,
        home: None,
        login: None,
        run: None,
        word: String::new(),
        cause: None,
        tab: None,
    })
}

/// Tab `at` of `wl` as the previous launch's layout wrote it: its pool id
/// then, the owner's hold and the agent it carries.
fn written_as(
    wl: &mut restore::WindowLayout,
    at: usize,
    id: u64,
    hold: Option<&str>,
    agent: Option<Box<restore::AgentRestore>>,
) {
    let restore::RestoredSplitTree::Leaf {
        view: restore::RestoredView::Terminal(leaf),
    } = &mut wl.restored_tabs[at].root
    else {
        panic!("a one-pane terminal tab");
    };
    leaf.local_id = Some(id);
    leaf.holds = hold.map(str::to_string).into_iter().collect();
    leaf.agent = agent;
}

/// Each tab of `wl`: its title, pool id, its holds (`,`-joined, none: `None`)
/// and its agent's conversation.
type Carried = (String, Option<u64>, Option<String>, Option<String>);

fn carried(wl: &restore::WindowLayout) -> Vec<Carried> {
    wl.restored_tabs
        .iter()
        .map(|tab| match &tab.root {
            restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(leaf),
            } => (
                leaf.user_title.clone().unwrap_or_default(),
                leaf.local_id,
                (!leaf.holds.is_empty()).then(|| leaf.holds.join(",")),
                leaf.agent.as_ref().and_then(|agent| agent.session.clone()),
            ),
            other => panic!("a one-pane terminal tab: {other:?}"),
        })
        .collect()
}

/// A TAB THE DRAIN STILL OWES KEEPS ITS OWN AGENT AND THE OWNER'S HOLD
/// THROUGH A RESTART'S QUIT, AND TAKES NO LIVE TAB'S (RV-SURV-1). Its leaf
/// names the previous launch's pool id, which this launch mints again from
/// 0, so the fills that read a leaf's session by its id
/// (`RestoreManifest::fill_agents`, `fill_holds`) took the owed tab's own
/// agent and hold off it — and where that id named a live tab of this
/// launch, put that tab's on it: two leaves naming one conversation, the
/// wrong tab reopened held. So did every leaf of a window not begun. The
/// owed leaves name no id of this launch's, and keep what they carry.
/// NEGATIVE CONTROLS: the live tab keeps its own hold and agent, read off
/// its session as before.
#[test]
fn a_system_quit_inside_the_drain_keeps_each_owed_tabs_own_agent_and_hold() {
    const C_AGENT: &str = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";
    const X_AGENT: &str = "7a1d2e3f-4b5c-4d6e-8f70-112233445566";
    const B_AGENT: &str = "11111111-2222-4333-8444-555555555555";
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut first = window(&["A", "B", "C", "D"], 0);
    written_as(&mut first, 0, 10, None, None);
    written_as(&mut first, 1, 11, None, None);
    written_as(
        &mut first,
        2,
        12,
        Some("local pause"),
        Some(carried_agent(C_AGENT)),
    );
    // D's old id is the one B's live shell gets in this launch.
    written_as(&mut first, 3, 1, None, None);
    // A window not begun, its old id A's live bootstrap's.
    let mut second = window(&["X"], 0);
    written_as(
        &mut second,
        0,
        0,
        Some("local mine-too"),
        Some(carried_agent(X_AGENT)),
    );
    app.pending_restore = Some(restore::RestoreManifest::new(vec![first, second]));
    assert!(!slice(&mut app));
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, wid), ["A", "B"]);
    assert_eq!(app.windows.len(), 1, "X is not begun");
    // B's live shell: the owner's hold on it, and the agent its host
    // publishes.
    let b = app
        .view_store
        .get(app.windows[&wid].tab_set.tabs()[1].focus)
        .copied()
        .and_then(crate::tab_model::View::terminal_session)
        .expect("B's shell");
    assert_eq!(b, 1, "D's old id names B's live shell");
    {
        let session = app.pool.get(b).expect("live");
        session.ctx.fabric.seed_holds(
            crate::fabric::parse_holds(["local mine"]),
            &session.ctx.timeline,
        );
    }
    let sid = app.session_sid(b).expect("B's sid");
    let published = aterm_agent::harness::relaunch::Snapshot {
        stored_login: false,
        profile: None,
        tab: sid.clone(),
        pid: 5151,
        start: "Sat Sep 27 02:02:03 2026".into(),
        shell: 4343,
        program: std::path::PathBuf::from("/opt/claude/bin/claude"),
        argv: vec!["/opt/claude/bin/claude".into()],
        session: Some(B_AGENT.into()),
        cwd: "/tmp".into(),
        version: None,
        run: aterm_agent::harness::relaunch::AgentRun::Claude,
        dialect: None,
        login: None,
        home: None,
        claude_dir: None,
    };
    crate::harness_host::publish_snapshot_for_test(&sid, Some(&published));

    let mut layout = app.capture_restore_manifest();
    app.quit_agents(&mut layout, true);
    crate::harness_host::publish_snapshot_for_test(&sid, None);

    let first = carried(&layout.windows[0]);
    let mine = |s: &str| Some(s.to_string());
    assert_eq!(
        first[1],
        ("B".into(), Some(b), mine("local mine"), mine(B_AGENT)),
        "the live tab's own"
    );
    assert_eq!(
        first[2],
        ("C".into(), None, mine("local pause"), mine(C_AGENT)),
        "an owed tab keeps its own"
    );
    assert_eq!(
        first[3],
        ("D".into(), None, None, None),
        "an owed tab takes no live tab's"
    );
    assert_eq!(
        carried(&layout.windows[1]),
        [("X".into(), None, mine("local mine-too"), mine(X_AGENT))],
        "a window not begun keeps its own"
    );
}

/// A PERSON'S OWN QUIT INSIDE THE DRAIN KEEPS NO AGENT AND NO HOLD
/// (RV-SURV-1): an owed tab carried the agent and the owner's hold its
/// earlier layout — a crash's, a restart's — named, and the quit wrote them
/// on, so the next launch relaunched an agent the person's own quit had
/// ended. Both fields' rule: a person's own quit writes neither.
#[test]
fn a_persons_own_quit_inside_the_drain_keeps_no_agent_and_no_hold() {
    const C_AGENT: &str = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";
    let mut app = App::headless_for_test();
    let mut wl = window(&["A", "B", "C"], 0);
    written_as(
        &mut wl,
        2,
        12,
        Some("local pause"),
        Some(carried_agent(C_AGENT)),
    );
    app.pending_restore = Some(restore::RestoreManifest::new(vec![wl]));
    assert!(!slice(&mut app));
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, WindowId(0)), ["A", "B"]);

    let mut layout = app.capture_restore_manifest();
    app.quit_agents(&mut layout, false);
    for (title, _, hold, agent) in carried(&layout.windows[0]) {
        assert_eq!((hold, agent), (None, None), "{title}");
    }
    // The next launch relaunches nothing from it.
    let wire = layout.to_toml().expect("the layout writes");
    let back = restore::RestoreManifest::from_toml(&wire).expect("the layout parses back");
    let mut next = App::headless_for_test();
    next.restore_into_window(WindowId(0), back.windows[0].clone());
    for (&id, pooled) in next.pool.sessions.iter_mut() {
        pooled.session.pid = 40_000 + i32::try_from(id).expect("a small id");
    }
    let handed = next.take_restored_agents();
    assert!(
        handed.is_empty(),
        "{:?}",
        handed
            .iter()
            .map(|(sid, snap, ..)| (sid.clone(), snap.session.clone()))
            .collect::<Vec<_>>()
    );
}

/// A LANDED HELD TAB KEEPS ITS AGENT DOWN THROUGH A RESTART INSIDE THE
/// DRAIN, WHATEVER PUT ITS HOLD BACK (the merge review's GUI-M7-1; ruling 23
/// (ii)). The drain holds a landed held tab's agent (`App::restored_held`)
/// until its last slice, and a restart's quit names it on the tab's leaf
/// (`App::unhanded_restored_agent`). The hold that kept it down is often not
/// on the session: a FLEET row is never put back on a fresh or bootstrap
/// shell, and the owner's is refused while an update commits the session.
/// Read off the session alone, the leaf named the agent with no hold, and
/// the next launch relaunched it into the tab. The leaf now carries the rows
/// the restore could not put back (`App::unhanded_restored_holds`), and the
/// next launch keeps the agent the holder's. NEGATIVE CONTROLS: the owner's
/// hold put back rides the session's own rows; the unheld tab's agent is owed
/// its relaunch throughout.
///
/// PUT BACK AND THEN LIFTED BY THE OWNER, the hold stays lifted (theirs to
/// lift) and the agent stays down (ruling 23 (ii): a held tab's agent is not
/// relaunched when the hold lifts LC-8): the leaf names no
/// agent, and the next launch neither holds the tab nor relaunches into it,
/// as after the drain's last slice. It named the agent with no hold, and the
/// next launch relaunched it and typed its carry-on into the tab. AND THE
/// RECORD THAT NAMES THE AGENT AND THE LINE THAT RESUMES IT is said once, as
/// the lift's wake is read (`App::let_go_lifted_held`; the review's KR-J-5),
/// or, lifted on the last turn with its wake unread, as the exit closes the
/// message log before the quit writes its layout
/// (`App::close_messages_at_exit`): said only at the pass's end, which the
/// restart came before, it was said nowhere, the next launch having no held
/// tab to say. A tab whose hold still stands says nothing before the pass's
/// end: the next launch says it.
#[test]
fn a_restart_inside_the_drain_keeps_a_landed_held_tabs_agent_down() {
    use crate::fabric::apply_hold_for_test;
    const X: &str = "0b6f3c1e-8a4d-4b61-9d52-7f1e2c3a4b5c";
    const Y: &str = "7a1d2e3f-4b5c-4d6e-8f70-112233445566";
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Then {
        Fenced,
        Lifted,
        LiftedUnread,
        Nothing,
    }
    let mine = |s: &str| Some(s.to_string());
    for (case, row, then, held_next) in [
        ("a fleet row", "fleet fabric-lost", Then::Nothing, true),
        ("the owner's, fenced", "local pause", Then::Fenced, true),
        (
            "control: the owner's, put back",
            "local pause",
            Then::Nothing,
            true,
        ),
        ("the owner's, lifted", "local pause", Then::Lifted, false),
        (
            "the owner's, lifted on the last turn",
            "local pause",
            Then::LiftedUnread,
            false,
        ),
    ] {
        let mut app = App::headless_for_test();
        // A host that relaunches restored agents: the hold is what keeps a
        // held tab's down, and its record names the line that resumes it.
        let host = crate::harness_host::HostHandle::for_restored_test(std::sync::Arc::new(
            |_, _, _, _| "wait:shell-prompt".to_string(),
        ));
        app.harness = Some(host.clone());
        let records = app.messages.log().records().count();
        let resume = carried_agent(X).resume_line().expect("X's resume line");
        let naming_x = |app: &App, from: usize| {
            app.messages
                .log()
                .records()
                .skip(from)
                .filter(|record| record.detail.iter().any(|line| line.contains(&resume)))
                .count()
        };
        let wid = WindowId(0);
        // The window's bootstrap shell is the first tab's (session 0).
        let bootstrap = std::sync::Arc::clone(&app.pool.get(0).expect("the bootstrap").ctx);
        if then == Then::Fenced {
            let _ = bootstrap.fabric.fence_hold(true);
        }
        let mut wl = window(&["A", "B"], 0);
        written_as(&mut wl, 0, 0, Some(row), Some(carried_agent(X)));
        // Another process than X's: one agent two leaves name is relaunched
        // once.
        let mut y = carried_agent(Y);
        y.pid = 4343;
        written_as(&mut wl, 1, 1, None, Some(y));
        app.pending_restore = Some(restore::RestoreManifest::new(vec![wl]));
        assert!(!slice(&mut app), "{case}");
        assert_eq!(titles(&app, wid), ["A"], "{case}: B is owed");
        assert_eq!(
            app.restored_held
                .iter()
                .map(|(id, agent, ..)| (*id, agent.session.clone()))
                .collect::<Vec<_>>(),
            vec![(0, mine(X))],
            "{case}: A's agent is the holder's"
        );
        if then == Then::Fenced {
            bootstrap.fabric.lift_hold_fence();
        }
        if matches!(then, Then::Lifted | Then::LiftedUnread) {
            assert!(apply_hold_for_test(&bootstrap, None), "{case}: lifted");
        }
        if then == Then::Lifted {
            // The lift's wake, as the loop reads it.
            app.on_fabric_changed(&bootstrap.self_id);
            assert_eq!(
                naming_x(&app, records),
                1,
                "{case}: the record naming X and its resume line, said at the lift's wake"
            );
        }
        // The exit's order (`main_entry`): the message log closes, then the
        // quit writes its layout.
        let _ = app.close_messages_at_exit();
        assert_eq!(
            naming_x(&app, records),
            usize::from(!held_next),
            "{case}: said once the hold is lifted, at the latest as the log closes"
        );
        let mut layout = app.capture_restore_manifest();
        app.quit_agents(&mut layout, true);
        assert_eq!(
            naming_x(&app, records),
            usize::from(!held_next),
            "{case}: the quit says nothing more"
        );
        // Lifted, nothing stands to lift: the record names no lift.
        let lift = format!("hold {} off", bootstrap.self_id);
        assert!(
            !app.messages
                .log()
                .records()
                .skip(records)
                .any(|record| record.detail.iter().any(|line| line.contains(&lift))),
            "{case}: no lift is named for a hold already lifted"
        );
        let kept = carried(&layout.windows[0]);
        assert_eq!(
            kept[0],
            (
                "A".into(),
                Some(0),
                held_next.then(|| row.to_string()),
                held_next.then(|| X.to_string())
            ),
            "{case}: the leaf names the agent beside what keeps it down, and \
             no agent once nothing does"
        );
        assert_eq!(
            kept[1],
            ("B".into(), None, None, mine(Y)),
            "{case}: the owed tab keeps its own"
        );
        // The next launch: the held tab's agent is said again, never handed.
        let wire = layout.to_toml().expect("the layout writes");
        let back = restore::RestoreManifest::from_toml(&wire).expect("the layout parses back");
        let mut next = App::headless_for_test();
        next.harness = Some(host.clone());
        next.restore_into_window(WindowId(0), back.windows[0].clone());
        for (&id, pooled) in next.pool.sessions.iter_mut() {
            pooled.session.pid = 40_000 + i32::try_from(id).expect("a small id");
        }
        let held: Vec<Option<String>> = next
            .restored_held
            .iter()
            .map(|(_, agent, ..)| agent.session.clone())
            .collect();
        let handed: Vec<Option<String>> = next
            .take_restored_agents()
            .into_iter()
            .map(|(_, snap, ..)| snap.session)
            .collect();
        if held_next {
            assert_eq!(held, vec![mine(X)], "{case}: still the holder's");
        } else {
            assert!(held.is_empty(), "{case}: {held:?}");
        }
        assert_eq!(
            handed,
            vec![mine(Y)],
            "{case}: X is not handed, held or lifted"
        );
        // Said again by the next launch only while held: once in all.
        let next_records = next.messages.log().records().count();
        next.say_restored_held();
        assert_eq!(
            naming_x(&next, next_records),
            usize::from(held_next),
            "{case}: the next launch says X while it is held"
        );
        host.shutdown_and_join();
    }
}

/// THE TABS THE DRAIN HAS LANDED KEEP THEIR AGENTS THROUGH A RESTART'S QUIT
/// TOO (F1): each one's agent waits in the restore's own list
/// (`App::restored_agents`) until the drain's last slice hands it to the
/// host, so neither the host's queue nor an agent it publishes names it
/// yet, and the quit's fill (`App::fill_desk_agents`) wrote no agent on
/// any landed tab — a restart in a cold drain's first instants relaunched
/// none of the desk's agents at the next launch. The owner's hold on a
/// landed tab is read off its session, where the restore put it back.
/// NEGATIVE CONTROLS: the owed tabs keep their own (RV-SURV-1), and no
/// leaf names another's agent.
#[test]
fn a_system_quit_inside_the_drain_keeps_the_landed_tabs_agents_not_yet_handed() {
    let agents = [
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
        "33333333-3333-4333-8333-333333333333",
        "44444444-4444-4444-8444-444444444444",
    ];
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut wl = window(&["A", "B", "C", "D"], 2);
    for (at, agent) in agents.iter().enumerate() {
        // The earlier launch's pool ids: 0 to 3, this launch's own.
        let hold = matches!(at, 0 | 1).then_some("local -");
        let id = u64::try_from(at).expect("a small index");
        let mut agent = carried_agent(agent);
        // A cause no durable layout should carry: never written on.
        agent.cause = (at == 0).then(|| "asked".to_string());
        written_as(&mut wl, at, id, hold, Some(agent));
    }
    app.pending_restore = Some(restore::RestoreManifest::new(vec![wl]));
    assert!(!slice(&mut app));
    assert!(!slice(&mut app));
    assert_eq!(titles(&app, wid), ["A", "C"], "B and D are owed");

    let mut layout = app.capture_restore_manifest();
    app.quit_agents(&mut layout, true);

    let kept: Vec<_> = carried(&layout.windows[0])
        .into_iter()
        .map(|(title, _, hold, agent)| (title, hold, agent))
        .collect();
    let mine = |s: &str| Some(s.to_string());
    assert_eq!(
        kept,
        [
            ("A".into(), mine("local -"), mine(agents[0])),
            ("B".into(), mine("local -"), mine(agents[1])),
            ("C".into(), None, mine(agents[2])),
            ("D".into(), None, mine(agents[3])),
        ]
    );
    // An explicit restart's cause rides a handoff's layout alone.
    for window in &layout.windows {
        for tab in &window.restored_tabs {
            if let restore::RestoredSplitTree::Leaf {
                view: restore::RestoredView::Terminal(leaf),
            } = &tab.root
            {
                assert!(leaf.agent.as_ref().is_none_or(|a| a.cause.is_none()));
            }
        }
    }
}
