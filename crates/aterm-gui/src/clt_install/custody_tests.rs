// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Mac Setup tab's custody beyond its window's turn: what a quit carries, what a desk
//! manifest describes, and what holds setup's other prompts back while it runs.

use super::*;
use aterm_types::macos_setup::macos_dialog_up;

// A recorded identity for an inert piped session, never an actual process to drive.
const CHILD_PID: i32 = 2_000_000_321;
const CHILD_BIRTH: u64 = 7_654_987;

/// The terminal leaves of `manifest`'s recursive tabs and of its flat terminal tabs, by the
/// session each was captured from.
fn leaves(manifest: &crate::restore::RestoreManifest) -> Vec<Option<u64>> {
    fn walk(node: &crate::restore::RestoredSplitTree, out: &mut Vec<Option<u64>>) {
        match node {
            crate::restore::RestoredSplitTree::Leaf {
                view: crate::restore::RestoredView::Terminal(leaf),
            } => out.push(leaf.local_id),
            crate::restore::RestoredSplitTree::Leaf { .. } => {}
            crate::restore::RestoredSplitTree::Split { first, second, .. } => {
                walk(first, out);
                walk(second, out);
            }
        }
    }
    let mut out = Vec::new();
    for window in &manifest.windows {
        for tab in &window.restored_tabs {
            walk(&tab.root, &mut out);
        }
        for tab in &window.tabs {
            for leaf in tab.leaves() {
                if let crate::restore::PaneLayout::Leaf { local_id, .. } = leaf {
                    out.push(*local_id);
                }
            }
        }
    }
    out
}

/// A keeper that holds every session's master and keeps what a quit's BYE names
/// ([`crate::keeper_link::carries_at_quit`]).
fn keeper_carries(_session: u64) -> bool {
    true
}

/// The page with its Mac Setup tab landed over the exact child [`CHILD_PID`], its receipt beside
/// the account's record at `mark`, the window's dialog guard held, under a keeper that keeps
/// what a quit names (`keeps`) or none, and the read end of what the window typed into the tab.
fn landed(mark: &std::path::Path, keeps: bool) -> (App, WindowId, u64, std::fs::File) {
    assert!(
        !macos_dialog_up(),
        "premise: no earlier page's guard is left"
    );
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_mark = Mark::at(mark.to_path_buf());
    app.workstation_setup.developer_tools_keeper_carries =
        keeps.then_some(keeper_carries as fn(u64) -> bool);
    app.access_steps.set_reach(StepsReach {
        holds_dialog: true,
        dialog_up: macos_dialog_up,
        ..StepsReach::INERT
    });
    let id = book(&mut app, wid, 4);
    let (mut session, read) = registered_piped_session(id);
    session.pid = CHILD_PID;
    session.shell_identity =
        aterm_pty::ShellIdentity::from_parts(CHILD_PID, Some(CHILD_BIRTH), false);
    assert!(app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(session),
        reader_gate: None,
    }));
    assert_eq!(drain(&read), go_line());
    assert!(
        macos_dialog_up(),
        "premise: the landing holds the dialog slot"
    );
    assert!(
        !app.system_state_handoff_pending(),
        "premise: its receipt lets a successor adopt its child"
    );
    (app, wid, id, read)
}

/// A root run that `parent` started, alive under the placeholder it stamped: the run, `sudo`,
/// `parent`, launchd.
fn root_run_of(parent: i32) -> Reading {
    let mut run_chain = [0; RUN_CHAIN];
    run_chain[..4].copy_from_slice(&[
        2_000_000_901,
        2_000_000_900,
        u32::try_from(parent).unwrap(),
        1,
    ]);
    Reading {
        placeholder: true,
        live_run: true,
        run_chain,
        ..Reading::default()
    }
}

/// A desk manifest leaves the Mac Setup tab out (R3): its program is one install attempt, so the
/// next launch brought it back as a plain shell titled Mac Setup, one more after each attempt a
/// quit ended; an installer a quit carries comes back through its keeper custody instead, as a
/// tab of its own. An update's handoff manifest keeps it, since the successor fills that leaf
/// with the same PTY.
///
/// NEGATIVE CONTROL: the handoff manifest of the same window describes the tab.
#[test]
fn a_desk_manifest_leaves_the_mac_setup_tab_out() {
    let (mut app, wid) = page();
    let id = book(&mut app, wid, 4);
    let (landed, read) = land(&mut app, wid, id);
    assert!(landed);
    assert_eq!(drain(&read), go_line());
    let handoff = leaves(&app.capture_handoff_layout_for(false));
    assert!(
        handoff.contains(&Some(id)),
        "an update's successor fills the installer's leaf: {handoff:?}"
    );
    let desk = leaves(&app.capture_restore_manifest());
    assert!(
        !desk.contains(&Some(id)),
        "the next launch reopens no Mac Setup shell: {desk:?}"
    );
}

/// aterm's quit leaves the Mac Setup tab's installer running for the next launch (H9): the close
/// of its last window keeps the tab's session in the pool, its teardown the keeper's (no hang-up
/// of this process's), and its receipt in place for the launch that adopts it; the carry the
/// run's end makes names it for its BYE ([`App::carry_developer_tools_install_at_exit`], whose
/// set `keeper_link::bye` turns into the masters the keeper keeps:
/// `keeper_link::tests::a_quits_bye_names_the_masters_it_carries`), and the quit's manifest
/// describes no shell in its place. The next launch, offered the kept master with no leaf of
/// that manifest to fill, adopts it as a tab of its own ([`App::land_orphan_tab`]): its receipt
/// gives that launch the installer's custody under the landing's claim, and the tab lands
/// behind the one the window shows.
///
/// NEGATIVE CONTROL: under a keeper that would not keep it, the same close drops the session and
/// retires its receipt, the attempt cut off; and a plain shell the next launch adopts lands in
/// front.
#[test]
fn a_quit_carries_the_installer_to_the_next_launch() {
    if !crate::test_alone::alone() {
        return;
    }
    for keeps in [true, false] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mark = dir.path().join("command-line-tools-tab");
        let (mut app, wid, id, _read) = landed(&mark, keeps);
        let (sid, pid, birth) = {
            let child = app.pool.get(id).expect("the tab's session");
            (
                child.ctx.self_id.as_str().to_owned(),
                child.pid,
                child.shell_identity.birth,
            )
        };
        let receipt =
            || crate::clt_install::receipt::Receipt::adopt(&mark, &sid, pid, birth).is_some();
        assert!(receipt(), "premise: the landing's receipt");
        assert_eq!(
            app.developer_tools_custody_at_exit(),
            if keeps { vec![id] } else { Vec::new() },
            "keeps: {keeps}"
        );
        assert_eq!(app.close_window_logical(wid), crate::CloseOutcome::Exit);
        if keeps {
            let child = app
                .pool
                .get(id)
                .expect("the quit leaves the installer's session for the keeper");
            assert!(
                child
                    .teardown_started
                    .load(std::sync::atomic::Ordering::Acquire),
                "no teardown of this process's hangs it up"
            );
            assert!(receipt(), "its receipt stays for the launch that adopts it");
            assert_eq!(
                app.carry_developer_tools_install_at_exit(),
                vec![id],
                "the run's end names it in its BYE"
            );
            let quit = app.quit_capture.as_ref().expect("the quit's manifest");
            assert!(
                !leaves(quit).contains(&Some(id)),
                "and its manifest reopens no shell in its place"
            );
            let claim = crate::clt_install::receipt::Receipt::adopt(&mark, &sid, pid, birth)
                .and_then(|receipt| receipt.claimed().cloned());
            assert!(claim.is_some(), "premise: the landing's claim");
            let mut carried = app.pool.sessions.remove(&id).expect("carried").session;
            drop(app);
            assert!(!macos_dialog_up(), "premise: the quit process is gone");

            let (mut next, next_wid) = page();
            next.workstation_setup.developer_tools_mark = Mark::at(mark.clone());
            let front = |app: &App| {
                app.windows
                    .get(&next_wid)
                    .and_then(|ws| ws.tab_set.active_id())
            };
            let shown = front(&next);
            assert!(shown.is_some(), "premise: the window shows a tab");
            let local = next.next_session_id + 30;
            carried.handoff_local_id = Some(carried.id);
            carried.id = local;
            next.land_orphan_tab(next_wid, carried);
            assert!(
                next.developer_tools_install_active() && next.is_developer_tools_session(local),
                "the next launch has the installer's custody"
            );
            assert_eq!(
                crate::clt_install::receipt::Receipt::adopt(&mark, &sid, pid, birth)
                    .and_then(|receipt| receipt.claimed().cloned()),
                claim,
                "under the landing's claim"
            );
            assert_eq!(front(&next), shown, "its tab lands behind the one shown");
            let (mut plain, _plain_read) = registered_piped_session(local + 1);
            plain.handoff_local_id = Some(7);
            next.land_orphan_tab(next_wid, plain);
            assert_ne!(
                front(&next),
                shown,
                "CONTROL: a plain shell the launch adopts lands in front"
            );
            drop(next);
        } else {
            assert!(
                app.pool.get(id).is_none(),
                "CONTROL: with no keeper to keep it, the quit hangs it up"
            );
            assert!(!receipt(), "CONTROL: and retires its receipt");
        }
    }
}

/// Past its password, the Mac Setup tab's installer holds no page back (H25): once the watch sees
/// the root run its own program started alive ([`Reading::run_chain`]), which `sudo` lets run
/// only past the password, and a quit would carry it ([`App::developer_tools_custody_at_exit`]),
/// the tab is no gesture up, its dialog slot is free, and the walk's start does not wait on its
/// run ([`App::developer_tools_install_elsewhere`]), though the tab is still this launch's
/// install. Apple's installer window, an installer that is not that run's, and an installer state
/// that could not be read each take the slot back while they last.
///
/// NEGATIVE CONTROL: a root run another program started, one whose parents went unread, and a
/// keeper that would not keep the run past a quit each leave the tab holding the pages back.
#[test]
fn an_installer_past_its_password_holds_no_page_back() {
    if !crate::test_alone::alone() {
        return;
    }
    let now = std::time::Instant::now();
    let dir = aterm_tempfile::tempdir().unwrap();
    let (mut app, _wid, _id, _read) = landed(&dir.path().join("command-line-tools-tab"), true);
    app.settle_access_steps(now);
    assert!(
        app.access_gesture_up_beside_walk(now),
        "premise: at its password the tab holds the pages back"
    );
    app.developer_tools_watch(Some(root_run_of(CHILD_PID)));
    app.settle_access_steps(now);
    assert!(
        app.developer_tools_install_active(),
        "the install is still this launch's tab's"
    );
    assert!(
        !app.access_gesture_up_beside_walk(now),
        "an installer past its password holds no page back"
    );
    assert!(!macos_dialog_up(), "its dialog slot is free");
    assert!(
        !app.developer_tools_install_elsewhere(),
        "nor does its own run hold the walk's start"
    );
    for (reading, why) in [
        (
            Reading {
                apple_window: true,
                ..root_run_of(CHILD_PID)
            },
            "Apple's installer window up beside the run",
        ),
        (
            Reading {
                placeholder: true,
                install_process: true,
                ..Reading::default()
            },
            "an installer that is not the run's",
        ),
        (
            Reading {
                unread: true,
                ..Reading::default()
            },
            "an installer state that could not be read",
        ),
    ] {
        app.developer_tools_watch(Some(reading));
        app.settle_access_steps(now);
        assert!(
            app.access_gesture_up_beside_walk(now),
            "{why} holds the pages back again"
        );
        assert!(macos_dialog_up(), "{why}: the slot is held again");
        app.developer_tools_watch(Some(root_run_of(CHILD_PID)));
        app.settle_access_steps(now);
        assert!(!macos_dialog_up(), "{why} gone, the slot is free again");
    }
    drop(app);

    let mut unread_parents = root_run_of(CHILD_PID);
    unread_parents.run_chain = [0; RUN_CHAIN];
    for (keeps, reading, why) in [
        (
            true,
            root_run_of(CHILD_PID + 1),
            "a root run another program started",
        ),
        (true, unread_parents, "a root run whose parents went unread"),
        (
            false,
            root_run_of(CHILD_PID),
            "a keeper that would not keep the run past a quit",
        ),
    ] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (mut app, _wid, _id, _read) = landed(&dir.path().join("command-line-tools-tab"), keeps);
        app.developer_tools_watch(Some(reading));
        app.settle_access_steps(now);
        assert!(
            app.access_gesture_up_beside_walk(now),
            "CONTROL: {why} holds the pages back"
        );
        assert!(macos_dialog_up(), "CONTROL: {why}: the slot stays held");
    }
}

/// The quit question tells what a quit does to the Mac Setup tab's install (H9): one the quit
/// carries keeps running and comes back with the next launch, so the question says so and not
/// that its process ends ([`crate::quit_safety::quit_prompt_carrying`]).
///
/// NEGATIVE CONTROL: under a keeper that would not keep it, the quit ends it, and the question
/// says a process will end.
#[test]
fn the_quit_question_says_a_carried_install_keeps_running() {
    if !crate::test_alone::alone() {
        return;
    }
    for keeps in [true, false] {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (app, wid, _id, _read) = landed(&dir.path().join("command-line-tools-tab"), keeps);
        let reopens = app.config.restore_session_or_default();
        let asked = app
            .destructive_prompt(wid, true, true, crate::close_confirm::CloseReplay::Quit)
            .expect("a quit asks");
        if keeps {
            assert_eq!(
                asked,
                crate::quit_safety::quit_prompt_carrying(false, reopens),
                "the install the quit carries keeps running"
            );
        } else {
            assert_eq!(
                Some(asked),
                crate::quit_safety::confirm_prompt(true, true, reopens),
                "CONTROL: an install the quit ends is a process that ends"
            );
        }
    }
}
