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

fn landed_child_for_owner(mark: &std::path::Path, owner: u64) -> (crate::Session, std::fs::File) {
    let (mut source, wid) = page();
    source.next_session_id += owner;
    source.workstation_setup.developer_tools_mark = Mark::at(mark.to_path_buf());
    source.access_steps.set_reach(dialog_reach());
    let id = if source.workstation_setup.developer_tools_mark.opened() {
        source.workstation_setup.developer_tools = Install::Pressed;
        test_support::book(&mut source, wid, pressed_tab_then(LINE.into()))
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
    assert_eq!(drain(&read), format!("exec {LINE}; exit $?\r").as_bytes());
    assert!(source.developer_tools_install_active());
    assert!(macos_dialog_up());
    assert!(mark.is_file());

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
    assert!(!app.developer_tools_install_due(true, false, false));
    assert!(!app.developer_tools_install_active());
    (app, wid)
}

fn adopted(mut session: crate::Session, local: u64) -> crate::Session {
    session.handoff_local_id = Some(session.id);
    session.id = local;
    session
}

fn receipt_for(
    mark: &std::path::Path,
    session: &crate::Session,
) -> crate::clt_install::receipt::Receipt {
    crate::clt_install::receipt::Receipt::for_child(
        mark,
        session.ctx.self_id.as_str(),
        session.pid,
        session.shell_identity.birth,
    )
    .unwrap()
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
    assert!(recovered.developer_tools_install_step().is_some());
    assert!(recovered.system_state_handoff_pending());
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
    assert_eq!(recovered.developer_tools_install_step(), None);
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
        assert_eq!(recovered.developer_tools_install_step(), None, "{case}");
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
    assert!(recovered.system_state_handoff_pending());
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
    assert!(recovered.developer_tools_install_step().is_some());
    assert!(recovered.system_state_handoff_pending());
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
    std::fs::write(&mark, b"opened\n").unwrap();
    let (mut child, read) = registered_piped_session(37);
    child.pid = CHILD_PID;
    child.shell_identity =
        aterm_pty::ShellIdentity::from_parts(CHILD_PID, Some(CHILD_BIRTH), false);
    let receipt = receipt_for(&mark, &child);
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
    assert!(recovered.developer_tools_install_step().is_some());
    assert!(recovered.system_state_handoff_pending());
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
    let receipt = receipt_for(&mark, &child);
    receipt.write_exclusive().unwrap();

    assert!(source.install_spawned_tab(TabSpawned {
        id,
        window: wid,
        session: Ok(child),
        reader_gate: None,
    }));
    let submitted = String::from_utf8(drain(&read)).unwrap();
    assert!(
        !submitted.contains(LINE),
        "no installer command after recovery-state failure"
    );
    assert!(
        submitted.contains("/usr/bin/printf"),
        "the result tab receives the refusal"
    );
    assert!(
        submitted.ends_with("; exit 1\r"),
        "the error child ends after its result"
    );
    assert!(source.developer_tools_install_active());
    assert!(macos_dialog_up());
    assert!(receipt.matches());
    assert!(source.exit_session_logical(id).is_empty());
    assert!(!source.developer_tools_install_active());
    assert!(!macos_dialog_up());
    assert!(
        receipt.matches(),
        "a refused dispatch never acquired the occupied record"
    );
    assert!(drain(&read).is_empty());
}
