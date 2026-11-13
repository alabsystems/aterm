// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Setup Stop uses process control, with the admitted child's receipt and current image.

use super::*;
use crate::clt_install::stop::{Program, Reach, Running};
use aterm_uds::process::{Birth, Identity};
use std::cell::RefCell;
use std::collections::VecDeque;

const PID: i32 = 2_000_000_123;
const BIRTH: u64 = 7_654_321;

thread_local! {
    static READS: RefCell<VecDeque<Option<Running>>> = const { RefCell::new(VecDeque::new()) };
    static SIGNALS: RefCell<Vec<Identity>> = const { RefCell::new(Vec::new()) };
}

fn running() -> Running {
    Running {
        identity: Identity {
            pid: PID,
            birth: Birth(BIRTH),
            idversion: Some(4),
        },
        program: Some(Program::Tools),
    }
}

fn read(_: i32) -> Option<Running> {
    READS.with_borrow_mut(|reads| {
        if reads.len() > 1 {
            reads.pop_front().flatten()
        } else {
            reads.front().copied().flatten()
        }
    })
}

fn interrupt(identity: &Identity) -> std::io::Result<()> {
    SIGNALS.with_borrow_mut(|signals| signals.push(*identity));
    Ok(())
}

fn owned_install(mark: &std::path::Path) -> (App, u64, std::fs::File) {
    READS.with_borrow_mut(|reads| *reads = VecDeque::from([Some(running())]));
    SIGNALS.with_borrow_mut(Vec::clear);
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_mark = Mark::at(mark.to_path_buf());
    let id = book(&mut app, wid, 4);
    let (mut child, input) = registered_piped_session(id);
    child.pid = PID;
    child.shell_identity = aterm_pty::ShellIdentity::from_parts(PID, Some(BIRTH), false);
    assert!(app.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(child),
        reader_gate: None
    }));
    assert_eq!(drain(&input), go_line());
    app.workstation_setup.developer_tools_stop = Reach {
        read: Some(read),
        interrupt: Some(interrupt),
        #[cfg(target_os = "macos")]
        reads_secret: None,
    };
    app.workstation_setup.developer_tools_probe = Some(installing);
    (app, id, input)
}

#[test]
fn stop_interrupts_an_owned_installer_waiting_through_an_outage_once_without_reauthorization() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mark = root.path().join("command-line-tools-tab");
    let (mut app, id, input) = owned_install(&mark);
    assert!(app.developer_tools_install_active());
    assert_eq!(step(&mut app), Some(ToolsInstall::Installing));
    app.stop_initial_setup_permissions();
    app.stop_initial_setup_permissions();
    assert_eq!(SIGNALS.with_borrow(Clone::clone), [running().identity]);
    assert!(
        drain(&input).is_empty(),
        "Stop injects no terminal command or input"
    );
    assert_eq!(app.workstation_setup.developer_tools_want, None);
    assert_eq!(app.workstation_setup.developer_tools, Install::Opened);
    assert!(app.workstation_setup.developer_tools_mark.held().is_some());
    app.developer_tools_install_ended(
        id,
        Some(aterm_pty::ChildExit::Code(i32::from(
            aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED,
        ))),
    );
    app.want_developer_tools_install(4, true);
    assert!(
        !app.developer_tools_install_due(false),
        "a stopped install does not reopen by itself"
    );
    assert!(SIGNALS.with_borrow(|signals| signals.len()) == 1);
}

#[test]
fn stop_never_interrupts_a_replaced_child_changed_receipt_or_another_user_program() {
    for changed in ["child", "birth", "program", "exec", "receipt", "finished"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join("command-line-tools-tab");
        let (mut app, id, input) = owned_install(&mark);
        match changed {
            "child" => app.pool.sessions.get_mut(&id).unwrap().session.pid += 1,
            "birth" => READS.with_borrow_mut(|reads| {
                let mut newer = running();
                newer.identity.birth = Birth(BIRTH + 1);
                *reads = VecDeque::from([Some(newer)]);
            }),
            "program" => READS.with_borrow_mut(|reads| {
                let mut other = running();
                other.program = None;
                *reads = VecDeque::from([Some(other)]);
            }),
            "exec" => READS.with_borrow_mut(|reads| {
                let mut other = running();
                other.identity.idversion = Some(5);
                *reads = VecDeque::from([Some(running()), Some(other)]);
            }),
            "receipt" => {
                let folder = mark.with_file_name("command-line-tools-tab.children");
                let path = std::fs::read_dir(folder)
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                std::fs::write(path, b"changed custody\n").unwrap();
            }
            "finished" => {
                app.pool
                    .get(id)
                    .unwrap()
                    .child_exit
                    .set(aterm_pty::ChildExit::Code(0))
                    .unwrap();
            }
            _ => unreachable!(),
        }
        app.stop_initial_setup_permissions();
        assert!(SIGNALS.with_borrow(Vec::is_empty), "{changed}");
        assert!(
            drain(&input).is_empty(),
            "{changed}: no input to a replacement"
        );
    }
}

/// Past the untouched-prompt backstop, by a second. The apply stops a prompt only on a Mac.
#[cfg(target_os = "macos")]
const PROMPT_OVERDUE: std::time::Duration =
    super::UNTOUCHED_PROMPT_BACKSTOP.saturating_add(std::time::Duration::from_secs(1));

/// An owned install whose receipt no longer adopts its child, so its tab cannot travel with an
/// update's handoff, served for `served` with nobody's gesture in it, or a person's last key
/// `typed` ago (now, or carried as an earlier gesture's age: this process's clock starts with it).
#[cfg(target_os = "macos")]
fn untouched_prompt(
    mark: &std::path::Path,
    served: std::time::Duration,
    typed: Option<std::time::Duration>,
) -> (App, u64, std::fs::File) {
    let (mut app, id, input) = owned_install(mark);
    let folder = mark.with_file_name("command-line-tools-tab.children");
    let receipt = std::fs::read_dir(folder)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(receipt, b"changed custody\n").unwrap();
    let now_us = crate::metrics::now_us();
    let stamp = &app.pool.get(id).unwrap().ctx.human_input;
    stamp.serve_from(u64::try_from(served.as_millis()).unwrap(), now_us);
    match typed {
        Some(typed) if typed.is_zero() => {
            stamp.note(now_us, crate::human_input::Gesture::Typed);
        }
        Some(typed) => stamp.carry(u64::try_from(typed.as_millis()).unwrap(), now_us),
        None => {}
    }
    app.workstation_setup.developer_tools_stop.reads_secret = Some(|_| true);
    assert!(
        app.developer_tools_install_holds_handoff(),
        "CONTROL: a tab whose receipt no longer adopts its child cannot travel"
    );
    assert_eq!(
        app.system_state_handoff_hold(),
        Some(crate::native_update_lane_hold::LaneHold::DeveloperTools),
        "CONTROL: the tab is what holds the update"
    );
    (app, id, input)
}

/// An update's apply, the lane's, with its attempt.
#[cfg(target_os = "macos")]
fn update_apply(app: &mut App) -> Result<(), crate::UpdateHandoffStartError> {
    app.apply_staged_update_now(
        crate::app_native::NativeUpdateSafetyToken::carrying_for_test(0, 0),
        crate::native_updater_service::ApplyMode::Automatic,
        Some(crate::native_updater_service::ApplyAttemptTicket::for_test(
            2,
            "0123456789ab",
            "00",
        )),
        None,
    )
}

#[cfg(target_os = "macos")]
fn deferred_words(
    result: &Result<(), crate::UpdateHandoffStartError>,
) -> Option<(crate::native_update_lane_hold::LaneHold, &str)> {
    match result {
        Err(crate::UpdateHandoffStartError::ActivityDeferred(hold, words)) => Some((*hold, words)),
        _ => None,
    }
}

/// AN UPDATE'S APPLY STOPS ATERM'S OWN SETUP TAB WHOSE PASSWORD PROMPT NOBODY TOUCHED FOR ITS
/// BACKSTOP: the attempt that stops it is still held by it, its exact child alone is interrupted
/// (no input is typed into the tab), and the record reads the attempt unfinished, asked again by
/// itself, never declined. Before, the tab held every attempt for as long as its prompt stood.
#[cfg(target_os = "macos")]
#[test]
fn an_updates_apply_stops_a_setup_tab_nobody_answers_at_its_password_prompt() {
    for typed in [None, Some(PROMPT_OVERDUE)] {
        nobody_answers(typed);
    }
}

/// The real name-rule landing holds an update until its exact untouched prompt is stopped
/// and its exit arrives. Another command, new process, human input or non-password work is
/// never interrupted; name repair acquires no tools-install outcome.
#[cfg(target_os = "macos")]
#[test]
fn an_update_releases_an_abandoned_name_rule_prompt_only_through_its_exact_child() {
    for changed in [
        "none",
        "typed",
        "child",
        "birth",
        "tools",
        "exec",
        "working",
        "other-tab",
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join("command-line-tools-tab");
        let (mut app, wid) = page();
        app.workstation_setup.developer_tools_mark = Mark::at(mark.clone());
        let tools = app.workstation_setup.developer_tools;
        let attempt = app.workstation_setup.developer_tools_attempt;
        app.headless = false;
        app.frontmost_window = Some(wid);
        app.machine_sets = 7;
        let origin = app.palette_menu_origin(wid).unwrap();
        let id = test_support::book_unasked(&mut app, wid, name_rule_tab_then(7, origin));
        let (mut child, input) = registered_piped_session(id);
        child.pid = PID;
        child.shell_identity = aterm_pty::ShellIdentity::from_parts(PID, Some(BIRTH), false);
        assert!(app.install_spawned_tab(TabSpawned {
            id,
            window: wid,
            session: Ok(child),
            reader_gate: None,
        }));
        assert_eq!(drain(&input), go_line());
        assert!(app.developer_tools_install_holds_handoff());
        let current = Running {
            program: Some(Program::NameRule),
            ..running()
        };
        READS.with_borrow_mut(|reads| *reads = VecDeque::from([Some(current)]));
        SIGNALS.with_borrow_mut(Vec::clear);
        app.workstation_setup.developer_tools_stop = Reach {
            read: Some(read),
            interrupt: Some(interrupt),
            reads_secret: Some(|_| true),
        };
        let now = crate::metrics::now_us();
        app.pool
            .get(id)
            .unwrap()
            .ctx
            .human_input
            .serve_from(u64::try_from(PROMPT_OVERDUE.as_millis()).unwrap(), now);
        match changed {
            "typed" => {
                app.pool
                    .get(id)
                    .unwrap()
                    .ctx
                    .human_input
                    .note(now, crate::human_input::Gesture::Typed);
            }
            "child" => app.pool.sessions.get_mut(&id).unwrap().session.pid += 1,
            "birth" => READS.with_borrow_mut(|reads| {
                *reads = VecDeque::from([Some(Running {
                    identity: Identity {
                        birth: Birth(BIRTH + 1),
                        ..current.identity
                    },
                    ..current
                })]);
            }),
            "tools" => READS.with_borrow_mut(|reads| *reads = VecDeque::from([Some(running())])),
            "exec" => READS
                .with_borrow_mut(|reads| *reads = VecDeque::from([Some(current), Some(running())])),
            "working" => app.workstation_setup.developer_tools_stop.reads_secret = Some(|_| false),
            "other-tab" => app
                .workstation_setup
                .developer_tools_session
                .push(Terminal::running(id + 1000, false, None)),
            "none" => {}
            _ => unreachable!(),
        }
        let held = update_apply(&mut app);
        assert!(
            matches!(
                deferred_words(&held),
                Some((crate::native_update_lane_hold::LaneHold::DeveloperTools, _))
            ),
            "{changed}: {held:?}"
        );
        assert_eq!(
            SIGNALS.with_borrow(Vec::len),
            usize::from(changed == "none"),
            "{changed}"
        );
        assert!(drain(&input).is_empty(), "{changed}: no input is typed");
        if changed == "none" {
            assert!(
                app.developer_tools_install_holds_handoff(),
                "wait for the actual exit"
            );
            let _ = update_apply(&mut app);
            assert_eq!(SIGNALS.with_borrow(Vec::len), 1, "one interrupt per child");
            app.developer_tools_install_ended(id, Some(aterm_pty::ChildExit::Code(130)));
            assert!(!app.developer_tools_install_holds_handoff());
            assert!(
                app.machine_unset.is_some(),
                "repair did not authorize a name change"
            );
        }
        assert_eq!(app.workstation_setup.developer_tools, tools, "{changed}");
        assert_eq!(
            app.workstation_setup.developer_tools_attempt, attempt,
            "{changed}"
        );
        assert!(
            !mark.exists(),
            "{changed}: no tools-install decision recorded"
        );
    }
}

#[cfg(target_os = "macos")]
fn nobody_answers(typed: Option<std::time::Duration>) {
    let root = aterm_tempfile::tempdir().unwrap();
    let mark = root.path().join("command-line-tools-tab");
    let (mut app, id, input) = untouched_prompt(&mark, PROMPT_OVERDUE, typed);
    let held = update_apply(&mut app);
    assert_eq!(
        deferred_words(&held),
        Some((
            crate::native_update_lane_hold::LaneHold::DeveloperTools,
            "an untouched Mac Setup password prompt was stopped; the next attempt goes on"
        )),
        "{held:?}"
    );
    assert_eq!(SIGNALS.with_borrow(Clone::clone), [running().identity]);
    assert!(
        drain(&input).is_empty(),
        "the stop types nothing into the tab"
    );
    let again = update_apply(&mut app);
    assert_eq!(
        deferred_words(&again),
        Some((
            crate::native_update_lane_hold::LaneHold::DeveloperTools,
            "a Mac Setup tab is still running"
        )),
        "until its program ends, the tab still holds: {again:?}"
    );
    assert_eq!(SIGNALS.with_borrow(Vec::len), 1, "one stop per child");
    app.developer_tools_install_ended(
        id,
        Some(aterm_pty::ChildExit::Code(i32::from(
            aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED,
        ))),
    );
    assert!(!app.developer_tools_install_holds_handoff());
    assert!(
        matches!(
            aterm_types::clt_mark::reading(&aterm_types::clt_mark::Live, &mark, unix()),
            aterm_types::clt_mark::Asked::Unfinished { tries: 1, .. }
        ),
        "nobody said no: the attempt is unfinished, asked again by itself"
    );
}

/// A SETUP TAB THE APPLY DOES NOT STOP HOLDS IT: one a person typed in within the backstop, one
/// still within its backstop, one whose program reads no password now, and any apply that
/// carries no update attempt.
#[cfg(target_os = "macos")]
#[test]
fn a_typed_prompt_a_fresh_one_and_a_tab_past_its_prompt_hold_the_update() {
    let mut stopped = Vec::new();
    for case in ["typed", "typed-earlier", "fresh", "no-prompt", "no-attempt"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join("command-line-tools-tab");
        let (served, typed) = match case {
            "typed" => (PROMPT_OVERDUE, Some(std::time::Duration::ZERO)),
            "typed-earlier" => (
                PROMPT_OVERDUE,
                Some(super::UNTOUCHED_PROMPT_BACKSTOP - std::time::Duration::from_secs(5)),
            ),
            "fresh" => (
                super::UNTOUCHED_PROMPT_BACKSTOP - std::time::Duration::from_secs(5),
                None,
            ),
            _ => (PROMPT_OVERDUE, None),
        };
        let (mut app, _, _input) = untouched_prompt(&mark, served, typed);
        if case == "no-prompt" {
            app.workstation_setup.developer_tools_stop.reads_secret = Some(|_| false);
        }
        let held = if case == "no-attempt" {
            app.apply_staged_update_now(
                crate::app_native::NativeUpdateSafetyToken::carrying_for_test(0, 0),
                crate::native_updater_service::ApplyMode::Immediate,
                None,
                None,
            )
        } else {
            update_apply(&mut app)
        };
        if deferred_words(&held)
            != Some((
                crate::native_update_lane_hold::LaneHold::DeveloperTools,
                "a Mac Setup tab is still running",
            ))
            || !SIGNALS.with_borrow(Vec::is_empty)
        {
            stopped.push(format!("{case}: {held:?}"));
        }
    }
    assert!(stopped.is_empty(), "each of these was stopped: {stopped:?}");
}

/// THE APPLY'S STOP REACHES ONLY THE TAB'S EXACT INSTALLER: a replaced child, another birth,
/// another program, or an exec between its two reads gets no interrupt and no input, and the
/// tab still holds the update.
#[cfg(target_os = "macos")]
#[test]
fn an_updates_apply_never_interrupts_a_replaced_child_or_another_program() {
    let mut reached = Vec::new();
    for changed in ["child", "birth", "program", "exec"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mark = root.path().join("command-line-tools-tab");
        let (mut app, id, input) = untouched_prompt(&mark, PROMPT_OVERDUE, None);
        match changed {
            "child" => app.pool.sessions.get_mut(&id).unwrap().session.pid += 1,
            "birth" => READS.with_borrow_mut(|reads| {
                let mut newer = running();
                newer.identity.birth = Birth(BIRTH + 1);
                *reads = VecDeque::from([Some(newer)]);
            }),
            "program" => READS.with_borrow_mut(|reads| {
                let mut other = running();
                other.program = None;
                *reads = VecDeque::from([Some(other)]);
            }),
            "exec" => READS.with_borrow_mut(|reads| {
                let mut other = running();
                other.identity.idversion = Some(5);
                *reads = VecDeque::from([Some(running()), Some(other)]);
            }),
            _ => unreachable!(),
        }
        let held = update_apply(&mut app);
        if deferred_words(&held)
            != Some((
                crate::native_update_lane_hold::LaneHold::DeveloperTools,
                "a Mac Setup tab is still running",
            ))
            || !SIGNALS.with_borrow(Vec::is_empty)
            || !drain(&input).is_empty()
        {
            reached.push(format!("{changed}: {held:?}"));
        }
    }
    assert!(reached.is_empty(), "each of these was reached: {reached:?}");
}

/// ANOTHER HOLDING SETUP TAB KEEPS AN UNTOUCHED PROMPT FROM BEING STOPPED: the apply stops
/// untouched prompts only when they are all that holds it, since any other would still hold
/// the attempt.
#[cfg(target_os = "macos")]
#[test]
fn an_untouched_prompt_beside_another_holding_tab_is_not_stopped() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mark = root.path().join("command-line-tools-tab");
    let (mut app, id, input) = untouched_prompt(&mark, PROMPT_OVERDUE, None);
    assert_eq!(
        app.developer_tools_prompts_untouched(crate::metrics::now_us()),
        Some(vec![id]),
        "CONTROL: alone, the untouched prompt is the one to stop"
    );
    // A second holding tab, not at its prompt: its session is none the pool serves.
    app.workstation_setup
        .developer_tools_session
        .push(Terminal::running(id + 1_000, false, None));
    assert_eq!(
        app.developer_tools_prompts_untouched(crate::metrics::now_us()),
        None
    );
    let held = update_apply(&mut app);
    assert_eq!(
        deferred_words(&held),
        Some((
            crate::native_update_lane_hold::LaneHold::DeveloperTools,
            "a Mac Setup tab is still running"
        )),
        "{held:?}"
    );
    assert!(SIGNALS.with_borrow(Vec::is_empty), "nothing is stopped");
    assert!(drain(&input).is_empty());
}

/// The backstop itself: a prompt is overdue from the backstop on, never a moment before, never
/// one whose terminal reads no password, and never with no reading of its session's gestures.
#[test]
fn an_untouched_prompt_is_overdue_from_its_backstop_on() {
    use crate::clt_install::stop::untouched_prompt_overdue;
    let backstop = u64::try_from(super::UNTOUCHED_PROMPT_BACKSTOP.as_millis()).unwrap();
    assert!(untouched_prompt_overdue(true, Some(backstop)));
    assert!(untouched_prompt_overdue(true, Some(u64::MAX)));
    assert!(!untouched_prompt_overdue(true, Some(backstop - 1)));
    assert!(!untouched_prompt_overdue(false, Some(u64::MAX)));
    assert!(!untouched_prompt_overdue(true, None));
}
