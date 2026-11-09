// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A landed install's live child crossing the keeper-adoption pool boundary.

use super::*;
use aterm_types::macos_setup::{MacosDialog, macos_dialog_up};
use std::time::Duration;

// A recorded identity for an inert piped session, never an actual process to drive.
const CHILD_PID: i32 = 2_000_000_123;
const CHILD_BIRTH: u64 = 7_654_321;

fn dialog_reach() -> StepsReach {
    StepsReach {
        holds_dialog: true,
        dialog_up: macos_dialog_up,
        ..StepsReach::INERT
    }
}

fn landed_child(mark: &std::path::Path) -> (crate::Session, std::fs::File) {
    landed_child_for_owner(mark, 0)
}

/// The page whose install tab landed a live child for `owner` that travels with a handoff (its
/// receipt lets the successor recover it), still in this process, and the child's id and output.
fn landed_source(mark: &std::path::Path, owner: u64) -> (App, u64, std::fs::File) {
    let (mut source, wid) = page();
    source.next_session_id += owner;
    source.workstation_setup.developer_tools_mark = Mark::at(mark.to_path_buf());
    source.access_steps.set_reach(dialog_reach());
    let id = if source.workstation_setup.developer_tools_mark.opened() {
        source.workstation_setup.developer_tools = Install::Pressed;
        test_support::book(&mut source, wid, pressed_tab_then())
    } else {
        book(&mut source, wid, 4)
    };
    let (mut session, read) = registered_piped_session(id);
    session.pid = CHILD_PID + i32::try_from(owner).unwrap();
    session.shell_identity =
        aterm_pty::ShellIdentity::from_parts(session.pid, Some(CHILD_BIRTH + owner), false);
    assert!(source.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(session),
        reader_gate: None,
    }));
    assert_eq!(drain(&read), go_line());
    assert!(source.developer_tools_install_active());
    assert!(macos_dialog_up());
    assert!(mark.is_file());
    assert!(!source.system_state_handoff_pending());
    (source, id, read)
}

fn landed_child_for_owner(mark: &std::path::Path, owner: u64) -> (crate::Session, std::fs::File) {
    let (mut source, id, read) = landed_source(mark, owner);

    // Move the actual child rather than closing it: the old window's memory goes away,
    // while keeper custody leaves its process, SID, birth and PTY unchanged.
    let session = source.pool.sessions.remove(&id).unwrap().session;
    drop(source);
    assert!(!macos_dialog_up(), "the old process's guard is gone");
    (session, read)
}

fn recovered_page(mark: &std::path::Path) -> (App, WindowId) {
    let (mut app, wid) = page();
    app.workstation_setup.developer_tools_mark = Mark::at(mark.to_path_buf());
    app.access_steps.set_reach(dialog_reach());
    assert!(app.workstation_setup.developer_tools_mark.opened());
    assert!(!app.developer_tools_install_due(false));
    assert!(!app.developer_tools_install_active());
    (app, wid)
}

fn adopted(mut session: crate::Session, local: u64) -> crate::Session {
    session.handoff_local_id = Some(session.id);
    session.id = local;
    session
}

/// The receipt the landing of `session`'s tab wrote beside `mark`, read back as an adopting
/// launch reads it.
fn receipt_for(
    mark: &std::path::Path,
    session: &crate::Session,
) -> crate::clt_install::receipt::Receipt {
    crate::clt_install::receipt::Receipt::adopt(
        mark,
        session.ctx.self_id.as_str(),
        session.pid,
        session.shell_identity.birth,
    )
    .expect("the landing's receipt")
}

/// A receipt for `session`'s child beside `mark`, its landing claim having added `claimed`'s
/// line (none: no line), not yet written.
fn new_receipt(
    mark: &std::path::Path,
    session: &crate::Session,
    claimed: Option<&str>,
) -> crate::clt_install::receipt::Receipt {
    crate::clt_install::receipt::Receipt::for_child(
        mark,
        session.ctx.self_id.as_str(),
        session.pid,
        session.shell_identity.birth,
        claimed,
    )
    .unwrap()
}

/// An installer that travels with the handoff holds none of it, and a queued setup choice beside
/// it holds as the setup worker's, never as the install tab's: the lane names what holds.
#[test]
fn clt_recovery_a_traveling_installer_beside_a_queued_choice_holds_as_system_state() {
    use crate::native_update_lane_hold::LaneHold;
    if !crate::test_alone::alone() {
        return;
    }
    let directory = aterm_tempfile::tempdir().unwrap();
    let mark = directory.path().join("command-line-tools-tab");
    let (mut source, _id, _read) = landed_source(&mark, 0);
    assert!(source.developer_tools_install_active(), "PRECONDITION");
    assert_eq!(
        source.system_state_handoff_hold(),
        None,
        "an installer that travels holds nothing"
    );
    source.receive_system_state_status(
        Some(source.system_state_host.status_generation_for_test()),
        crate::system_state::SetupStatus {
            queued: true,
            ..crate::system_state::SetupStatus::default()
        },
    );
    assert_eq!(
        source.system_state_handoff_hold(),
        Some(LaneHold::SystemState),
        "the queued choice holds, as the worker's"
    );
    assert!(source.system_state_handoff_pending());
}

#[test]
fn clt_recovery_a_live_adopted_installer_keeps_custody_without_retyping_its_command() {
    if !crate::test_alone::alone() {
        return;
    }
    assert!(!macos_dialog_up());
    let directory = aterm_tempfile::tempdir().unwrap();
    let mark = directory.path().join("command-line-tools-tab");
    let (session, read) = landed_child(&mark);
    let receipt = receipt_for(&mark, &session);
    assert!(receipt.matches());
    assert_eq!(
        receipt.claimed(),
        Some(aterm_stamp::build_id().as_str()),
        "the automatic landing's claim added this build's line"
    );
    let original_sid = session.ctx.self_id.clone();
    let original_identity = session.shell_identity;
    let (mut recovered, wid) = recovered_page(&mark);
    let local = recovered.next_session_id + 30;
    let session = adopted(session, local);
    assert_eq!(session.ctx.self_id, original_sid);
    assert_eq!(session.shell_identity, original_identity);
    App::register_session(&recovered.store, &session, None);
    recovered.pool_insert(session);

    assert!(
        recovered.developer_tools_install_active(),
        "a verified live child must recover its installer custody, not just its PTY"
    );
    assert!(step(&mut recovered).is_some());
    assert!(!recovered.system_state_handoff_pending());
    assert!(macos_dialog_up(), "other automatic prompts remain held");
    assert!(
        drain(&read).is_empty(),
        "adoption never retypes an installer command"
    );
    let before = tabs(&recovered);
    recovered.headless = false;
    let duplicate = recovered.press_developer_tools_install();
    recovered.headless = true;
    assert!(!duplicate, "the restored live child already owns Install");
    assert_eq!(tabs(&recovered), before);
    assert!(recovered.system_state_frontmost_in(wid));

    assert!(recovered.exit_session_logical(local).is_empty());
    assert!(!recovered.developer_tools_install_active());
    assert_eq!(step(&mut recovered), None);
    assert!(!recovered.system_state_handoff_pending());
    assert!(!macos_dialog_up());
    assert!(
        !receipt.matches(),
        "the exact child's exit retires its own receipt"
    );
    let available = MacosDialog::when_none_up(Duration::ZERO)
        .expect("the exact recovered child's exit releases custody");
    drop(available);
    assert!(drain(&read).is_empty(), "exit submits no further command");
}

#[test]
fn clt_recovery_only_exact_durable_installer_custody_allows_an_update() {
    if !crate::test_alone::alone() {
        return;
    }
    let directory = aterm_tempfile::tempdir().unwrap();
    let mark = directory.path().join("command-line-tools-tab");
    let (session, read) = landed_child(&mark);
    let receipt = receipt_for(&mark, &session);
    let (mut recovered, _) = recovered_page(&mark);
    let local = recovered.next_session_id + 30;
    recovered.pool_insert(adopted(session, local));
    assert!(recovered.developer_tools_install_active());
    assert!(!recovered.system_state_handoff_pending());
    let original = recovered.pool.get(local).unwrap().shell_identity;
    recovered
        .pool
        .sessions
        .get_mut(&local)
        .unwrap()
        .session
        .shell_identity =
        aterm_pty::ShellIdentity::from_parts(original.pid, Some(CHILD_BIRTH + 1), false);
    assert!(recovered.system_state_handoff_pending());
    recovered
        .pool
        .sessions
        .get_mut(&local)
        .unwrap()
        .session
        .shell_identity = original;
    assert!(!recovered.system_state_handoff_pending());
    let child = recovered.pool.get(local).unwrap();
    let mut successor_receipt = receipt_for(&mark, child);
    successor_receipt.claim("successor-test-build").unwrap();
    assert!(!receipt.matches(), "the successor changed the build claim");
    assert!(!recovered.system_state_handoff_pending());
    successor_receipt.remove_if_matching();
    assert!(recovered.system_state_handoff_pending());
    assert!(
        drain(&read).is_empty(),
        "handoff admission types no command"
    );
}

/// A launch that adopted live shells takes the shim's mark off with the rest an earlier launch
/// left, and the account's mark reads the tools unfinished; an install those shells carry on with
/// is one that runs, though: its want comes with it and new shells get the shim again while it
/// runs, until its end leaves nothing installing them.
/// NEGATIVE CONTROL: a recovery that brings no want, or keeps the tools unfinished, leaves new
/// shells with Apple's own dialog while the adopted install runs.
#[test]
fn clt_recovery_an_adopted_live_installer_puts_the_shim_back_on_new_shells() {
    if !crate::test_alone::alone() {
        return;
    }
    let directory = aterm_tempfile::tempdir().unwrap();
    let mark = directory.path().join("command-line-tools-tab");
    let (session, _read) = landed_child(&mark);
    let (mut recovered, _) = recovered_page(&mark);
    let shim = shim_in(&mut recovered, directory.path());
    recovered.workstation_setup.developer_tools_shim.reconcile();
    recovered.workstation_setup.developer_tools_unfinished = true;
    assert!(!installing_said(&shim));
    let local = recovered.next_session_id + 30;
    let session = adopted(session, local);
    App::register_session(&recovered.store, &session, None);
    recovered.pool_insert(session);
    assert!(recovered.developer_tools_install_active());
    assert_eq!(recovered.workstation_setup.developer_tools_want, Some(0));
    assert!(
        installing_said(&shim),
        "the adopted install runs on: the shim's line says so"
    );
    assert!(recovered.exit_session_logical(local).is_empty());
    assert!(
        !installing_said(&shim),
        "its end leaves nothing installing them"
    );
    assert!(wanted(&shim), "and the tools still missing");
    assert!(
        shim.is_dir(),
        "the directory stays for the shells that have it"
    );
}

/// An earlier build's tab and exact original receipt, as an update hands them over.
fn earlier_builds_tab(mark: &std::path::Path, earlier: &str) -> (crate::Session, std::fs::File) {
    let (session, read) = landed_child(mark);
    std::fs::write(mark, format!("opened {earlier}\n")).unwrap();
    std::fs::remove_file(receipt_for(mark, &session).path()).unwrap();
    new_receipt(mark, &session, Some(earlier))
        .write_exclusive()
        .unwrap();
    (session, read)
}

/// An adopted live child retains its exact original claim across updates. A failed password
/// keeps that episode; a proven preprompt end can release it without touching a historical hold.
#[test]
fn clt_recovery_an_update_preserves_the_original_episode_and_exact_release_owner() {
    use aterm_pty::ChildExit;
    if !crate::test_alone::alone() {
        return;
    }
    let earlier = "0.1.0+gdeadbeef";
    let low_data = i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA);
    for (history, unclaimed, end, held) in [
        (false, false, ChildExit::Code(1), true),
        (false, false, ChildExit::Code(low_data), false),
        (true, false, ChildExit::Code(low_data), true),
        (false, false, ChildExit::Code(127), false),
        (false, true, ChildExit::Code(1), true),
        (false, false, ChildExit::Code(7), true),
        (false, false, ChildExit::Code(8), true),
    ] {
        let directory = aterm_tempfile::tempdir().unwrap();
        let mark = directory.path().join("command-line-tools-tab");
        let (session, _read) = earlier_builds_tab(&mark, earlier);
        if history {
            std::fs::write(&mark, format!("opened historical\nopened {earlier}\n")).unwrap();
        }
        if unclaimed {
            std::fs::remove_file(receipt_for(&mark, &session).path()).unwrap();
            new_receipt(&mark, &session, None)
                .write_exclusive()
                .unwrap();
        }
        let (mut recovered, _) = recovered_page(&mark);
        let local = recovered.next_session_id + 30;
        let session = adopted(session, local);
        App::register_session(&recovered.store, &session, None);
        recovered.pool_insert(session);
        assert!(recovered.developer_tools_install_active());
        assert_eq!(
            receipt_for(&mark, recovered.pool.get(local).unwrap()).claimed(),
            (!unclaimed).then_some(earlier)
        );
        if unclaimed {
            aterm_types::clt_mark::forget(&aterm_types::clt_mark::Live, &mark, earlier).unwrap();
            assert!(
                Mark::at(mark.clone()).opened(),
                "adopted explicit attempt survives older release"
            );
        }
        recovered.root_steps_owed(true);
        recovered.developer_tools_install_ended(local, Some(end));
        assert!(
            recovered.workstation_setup.developer_tools_root_owed,
            "an adopted child's {end:?} is not a fresh root-work reading"
        );
        assert_eq!(Mark::at(mark.clone()).opened(), held, "{history} {end:?}");
        if history {
            assert_eq!(
                std::fs::read_to_string(&mark).unwrap(),
                "opened historical\n"
            );
        }
        assert!(recovered.exit_session_logical(local).is_empty());
        assert!(!recovered.developer_tools_install_active());
        assert!(!macos_dialog_up());
    }
}

/// Repeated adoption after an update and crash preserves the original receipt owner and the
/// consumed episode even when the password or installer failed.
#[test]
fn clt_recovery_an_update_adopted_tab_keeps_its_episode_after_a_second_crash() {
    use aterm_pty::ChildExit;
    if !crate::test_alone::alone() {
        return;
    }
    let earlier = "0.1.0+gdeadbeef";
    let stopped = i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED);
    for (end, kept) in [(ChildExit::Code(1), true), (ChildExit::Code(stopped), true)] {
        let directory = aterm_tempfile::tempdir().unwrap();
        let mark = directory.path().join("command-line-tools-tab");
        let (session, _read) = earlier_builds_tab(&mark, earlier);

        // The update: this build adopts the earlier build's tab and claims its own line.
        let (mut first, _) = page();
        first.workstation_setup.developer_tools_mark = Mark::at(mark.clone());
        first.access_steps.set_reach(dialog_reach());
        assert!(first.workstation_setup.developer_tools_mark.opened());
        let local = first.next_session_id + 30;
        let session = adopted(session, local);
        App::register_session(&first.store, &session, None);
        first.pool_insert(session);
        assert!(first.developer_tools_install_active(), "{end:?}");
        assert!(
            Mark::at(mark.clone()).opened(),
            "{end:?}: the line is added"
        );
        assert_eq!(
            receipt_for(&mark, first.pool.get(local).unwrap()).claimed(),
            Some(earlier),
            "{end:?}: the receipt retains the original owner"
        );

        // The crash: that window goes, while the keeper keeps the child.
        let session = first.pool.sessions.remove(&local).unwrap().session;
        drop(first);
        assert!(!macos_dialog_up(), "{end:?}");

        // A launch of the same build adopts it again.
        let (mut second, _) = recovered_page(&mark);
        let local = second.next_session_id + 30;
        let session = adopted(session, local);
        App::register_session(&second.store, &session, None);
        second.pool_insert(session);
        assert!(second.developer_tools_install_active(), "{end:?}");
        second.developer_tools_install_ended(local, Some(end));
        assert_eq!(Mark::at(mark.clone()).opened(), kept, "{end:?}");
        assert!(
            aterm_types::clt_mark::spent(&aterm_types::clt_mark::Live, &mark),
            "{end:?}: the earlier build's line stays"
        );
        assert!(second.exit_session_logical(local).is_empty());
        assert!(!second.developer_tools_install_active(), "{end:?}");
        assert!(!macos_dialog_up(), "{end:?}");
    }
}

/// A live tab an earlier aterm opened whose recovery record this launch cannot read (none, an
/// earlier format, another folder) gives no custody here, but it still runs the Mac Setup
/// tab's program under aterm's pinned title, its `sudo` perhaps asking now with nothing a
/// probe could see: the automatic tab waits for it, at its due check and at its landing, and
/// opens once that tab's program has ended.
/// NEGATIVE CONTROL: an adopted live tab under another title, or none, holds nothing. The mutant
/// whose due check skips `developer_tools_tab_adopted` reads the automatic tab due beside it,
/// and the one whose landing check skips it lands the tab beside it.
#[test]
fn clt_recovery_an_earlier_aterms_tab_with_no_custody_here_holds_the_automatic_tab() {
    for title in [Some(TAB_TITLE), Some("zsh"), None] {
        let (mut app, wid) = page();
        let local = app.next_session_id + 30;
        let (mut session, _read) = registered_piped_session(local);
        session.handoff_local_id = Some(3);
        if let Some(title) = title {
            let _ = session
                .ctx
                .meta
                .lock()
                .unwrap()
                .set("title", Some(title.to_owned()));
        }
        App::register_session(&app.store, &session, None);
        app.pool_insert(session);
        assert!(
            !app.developer_tools_install_active(),
            "{title:?}: no custody"
        );
        if title != Some(TAB_TITLE) {
            assert_retry_lands_once(&mut app, wid, 4);
            continue;
        }
        assert!(
            !app.developer_tools_install_due(false),
            "it holds the due check"
        );
        let id = book(&mut app, wid, 4);
        assert_deferred(&mut app, wid, id, Install::Wanted(4));
        app.pool
            .get(local)
            .unwrap()
            .child_exit
            .set(aterm_pty::ChildExit::Code(1))
            .unwrap();
        assert_retry_lands_once(&mut app, wid, 4);
    }
}

#[test]
fn clt_recovery_a_cold_or_mismatched_child_never_inherits_installer_custody() {
    if !crate::test_alone::alone() {
        return;
    }
    for case in [
        "cold",
        "unadopted",
        "pid",
        "birth",
        "sid",
        "exited",
        "reaped",
    ] {
        let directory = aterm_tempfile::tempdir().unwrap();
        let mark = directory.path().join("command-line-tools-tab");
        let (original, original_read) = landed_child(&mark);
        let (mut recovered, _) = recovered_page(&mark);
        let local = recovered.next_session_id + 30;
        let mut session = adopted(original, local);
        let mut replacement_read = None;
        match case {
            "unadopted" => session.handoff_local_id = None,
            "pid" => {
                session.pid += 1;
                session.shell_identity.pid = session.pid;
            }
            "birth" => session.shell_identity.birth = Some(CHILD_BIRTH + 1),
            "cold" | "sid" => {
                let identity = session.shell_identity;
                let previous = session.handoff_local_id;
                let (mut replacement, read) = registered_piped_session(local);
                if case == "sid" {
                    replacement.pid = identity.pid;
                    replacement.shell_identity = identity;
                    replacement.handoff_local_id = previous;
                } else {
                    replacement.pid = CHILD_PID + 10;
                    replacement.shell_identity = aterm_pty::ShellIdentity::from_parts(
                        replacement.pid,
                        Some(CHILD_BIRTH + 10),
                        false,
                    );
                }
                session = replacement;
                replacement_read = Some(read);
            }
            "exited" => {
                session
                    .child_exit
                    .set(aterm_pty::ChildExit::Code(1))
                    .unwrap();
            }
            "reaped" => session
                .child_reaped
                .store(true, std::sync::atomic::Ordering::Release),
            _ => unreachable!(),
        }
        App::register_session(&recovered.store, &session, None);
        recovered.pool_insert(session);
        assert!(!recovered.developer_tools_install_active(), "{case}");
        assert_eq!(step(&mut recovered), None, "{case}");
        assert!(!recovered.system_state_handoff_pending(), "{case}");
        assert!(!macos_dialog_up(), "{case}");
        assert!(
            drain(&original_read).is_empty(),
            "{case}: no command replay"
        );
        if let Some(read) = replacement_read {
            assert!(
                drain(&read).is_empty(),
                "{case}: no command for another SID"
            );
        }
    }
}

#[test]
fn clt_recovery_each_live_child_keeps_custody_until_its_own_exit() {
    if !crate::test_alone::alone() {
        return;
    }
    assert!(!macos_dialog_up());
    let directory = aterm_tempfile::tempdir().unwrap();
    let mark = directory.path().join("command-line-tools-tab");
    let (first, first_read) = landed_child_for_owner(&mark, 0);
    let (second, second_read) = landed_child_for_owner(&mark, 100);
    let first_receipt = receipt_for(&mark, &first);
    let second_receipt = receipt_for(&mark, &second);
    assert!(first_receipt.matches());
    assert!(
        second_receipt.matches(),
        "another GUI's child must not replace the first receipt"
    );
    assert_ne!(first.ctx.self_id, second.ctx.self_id);
    assert_ne!(first.pid, second.pid);
    assert_ne!(first.shell_identity.birth, second.shell_identity.birth);
    let (mut recovered, _) = recovered_page(&mark);
    let first_local = recovered.next_session_id + 30;
    let second_local = first_local + 1;
    for session in [adopted(first, first_local), adopted(second, second_local)] {
        App::register_session(&recovered.store, &session, None);
        recovered.pool_insert(session);
    }
    assert!(recovered.developer_tools_install_active());
    assert!(!recovered.system_state_handoff_pending());
    assert!(macos_dialog_up());
    assert!(drain(&first_read).is_empty());
    assert!(drain(&second_read).is_empty());

    assert!(recovered.exit_session_logical(first_local).is_empty());
    assert!(!first_receipt.matches());
    assert!(
        second_receipt.matches(),
        "the first exit cannot remove another child's receipt"
    );
    assert!(
        recovered.developer_tools_install_active(),
        "the second child still owns custody"
    );
    assert!(step(&mut recovered).is_some());
    assert!(!recovered.system_state_handoff_pending());
    assert!(macos_dialog_up());
    recovered.headless = false;
    let duplicate = recovered.press_developer_tools_install();
    recovered.headless = true;
    assert!(!duplicate);
    assert!(
        recovered.pool.get(first_local).is_some(),
        "the first result remains available"
    );

    assert!(recovered.exit_session_logical(second_local).is_empty());
    assert!(!second_receipt.matches());
    assert!(!recovered.developer_tools_install_active());
    assert!(!recovered.system_state_handoff_pending());
    assert!(!macos_dialog_up());
    assert!(
        recovered.pool.get(second_local).is_some(),
        "the second result remains available"
    );
    assert!(drain(&first_read).is_empty());
    assert!(drain(&second_read).is_empty());
}

#[test]
fn clt_recovery_receipt_before_command_bytes_restores_custody_without_starting_the_install() {
    if !crate::test_alone::alone() {
        return;
    }
    let directory = aterm_tempfile::tempdir().unwrap();
    let mark = directory.path().join("command-line-tools-tab");
    this_builds_mark(&mark);
    let (mut child, read) = registered_piped_session(37);
    child.pid = CHILD_PID;
    child.shell_identity =
        aterm_pty::ShellIdentity::from_parts(CHILD_PID, Some(CHILD_BIRTH), false);
    let receipt = new_receipt(&mark, &child, None);
    receipt.write_exclusive().unwrap();
    assert!(
        drain(&read).is_empty(),
        "the old window ended before submitting command bytes"
    );

    let (mut recovered, _) = recovered_page(&mark);
    let local = recovered.next_session_id + 30;
    let child = adopted(child, local);
    App::register_session(&recovered.store, &child, None);
    recovered.pool_insert(child);
    assert!(recovered.developer_tools_install_active());
    assert!(step(&mut recovered).is_some());
    assert!(!recovered.system_state_handoff_pending());
    assert!(macos_dialog_up());
    assert!(
        drain(&read).is_empty(),
        "recovery is custody, never command authority"
    );
    recovered.retire_developer_tools_install(local);
    assert!(!receipt.matches());
    assert!(!recovered.developer_tools_install_active());
    assert!(!macos_dialog_up());
    assert!(drain(&read).is_empty());
}

#[test]
fn clt_recovery_a_receipt_collision_refuses_dispatch_and_keeps_the_existing_record() {
    if !crate::test_alone::alone() {
        return;
    }
    let directory = aterm_tempfile::tempdir().unwrap();
    let mark = directory.path().join("command-line-tools-tab");
    let (mut source, wid) = page();
    source.workstation_setup.developer_tools_mark = Mark::at(mark.clone());
    source.access_steps.set_reach(dialog_reach());
    let id = book(&mut source, wid, 4);
    let (mut child, read) = registered_piped_session(id);
    child.pid = CHILD_PID;
    child.shell_identity =
        aterm_pty::ShellIdentity::from_parts(CHILD_PID, Some(CHILD_BIRTH), false);
    let receipt = new_receipt(&mark, &child, None);
    receipt.write_exclusive().unwrap();

    assert!(source.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(child),
        reader_gate: None,
    }));
    assert_eq!(
        drain(&read),
        format!("{TAB_STOP}\r").into_bytes(),
        "no go line after recovery-state failure: the program ends having started nothing"
    );
    let shown: Vec<String> = {
        let term = crate::term_lock(&source.pool.get(id).expect("the result tab").term);
        (0..200).filter_map(|row| term.row_text(row)).collect()
    };
    assert!(
        shown.concat().contains(&not_started_line()),
        "the result tab says why, in plain words: {shown:?}"
    );
    assert!(
        !source.workstation_setup.developer_tools_mark.opened() && !Mark::at(mark.clone()).opened(),
        "an install that never started takes its opening back: the next launch asks again"
    );
    assert!(source.developer_tools_install_active());
    assert!(macos_dialog_up());
    assert!(receipt.matches());
    assert!(source.exit_session_logical(id).is_empty());
    assert!(
        !Mark::at(mark.clone()).opened(),
        "its end keeps it taken back"
    );
    assert!(!source.developer_tools_install_active());
    assert!(!macos_dialog_up());
    assert!(
        receipt.matches(),
        "a refused dispatch never acquired the occupied record"
    );
    assert!(drain(&read).is_empty());
}

/// An orphan with no terminal outcome retains its automatic hold. A missing record is rebuilt
/// from exact receipt custody; live children remain available for adoption.
#[test]
fn clt_recovery_an_unknown_orphan_never_reauthorizes_an_automatic_password_request() {
    if !crate::test_alone::alone() {
        return;
    }
    let directory = aterm_tempfile::tempdir().unwrap();
    let mark = directory.path().join("command-line-tools-tab");
    let (child, _read) = landed_child(&mark);
    let receipt = receipt_for(&mark, &child);
    assert!(
        Mark::launched(mark.clone(), |pid, birth| pid == CHILD_PID
            && birth == CHILD_BIRTH)
        .opened()
    );
    assert!(receipt.matches());
    std::fs::remove_file(&mark).unwrap();
    let next = Mark::launched(mark.clone(), |_, _| false);
    assert!(next.opened());
    assert!(!launch_install(true, true, true, next.opened()).want);
    assert!(!receipt.matches());
    assert!(Mark::at(mark).opened());
}
