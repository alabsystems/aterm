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

/// An earlier build's tab, as an update hands it over: the record names that build (and this
/// one too where `this_build_asked`, as a decline under this build or its follow-up's ask left
/// it), and the tab's receipt says its landing added the earlier build's line.
fn earlier_builds_tab(
    mark: &std::path::Path,
    earlier: &str,
    this_build_asked: bool,
) -> (crate::Session, std::fs::File) {
    let (session, read) = landed_child(mark);
    std::fs::write(mark, format!("opened {earlier}\n")).unwrap();
    if this_build_asked {
        this_builds_mark(mark);
    }
    std::fs::remove_file(receipt_for(mark, &session).path()).unwrap();
    new_receipt(mark, &session, Some(earlier))
        .write_exclusive()
        .unwrap();
    (session, read)
}

/// A live install an earlier build's tab carries on with across an update is the adopting build's
/// ask: adoption notes that build in the account's record, so a person's stop under it keeps the
/// opening spent (its next launch opens no tab by itself), while a failure takes it back. Where
/// the record named the adopting build already (a decline under it), the adoption's claim added
/// no line, and the tab's receipt claims only the earlier build's: a failure keeps the line.
/// NEGATIVE CONTROL: the failure leaves the record naming the earlier build alone, as before the
/// adoption; the earlier build's own line stays either way. The mutant that reads any build the
/// receipt claims as this one's (`receipt.claimed().is_some()` in place of `== Some(build_id)`)
/// takes back the line the decline kept.
#[test]
fn clt_recovery_an_install_adopted_across_an_update_is_the_adopting_builds_ask() {
    use aterm_pty::ChildExit;
    if !crate::test_alone::alone() {
        return;
    }
    let earlier = "0.1.0+gdeadbeef";
    let stopped = i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED);
    for (this_build_asked, end, kept) in [
        (false, ChildExit::Code(stopped), true),
        (false, ChildExit::Code(1), false),
        (true, ChildExit::Code(1), true),
    ] {
        let case = format!("this build asked: {this_build_asked}, {end:?}");
        let directory = aterm_tempfile::tempdir().unwrap();
        let mark = directory.path().join("command-line-tools-tab");
        let (session, _read) = earlier_builds_tab(&mark, earlier, this_build_asked);
        let (mut recovered, _) = page();
        recovered.workstation_setup.developer_tools_mark = Mark::at(mark.clone());
        recovered.access_steps.set_reach(dialog_reach());
        assert_eq!(
            recovered.workstation_setup.developer_tools_mark.opened(),
            this_build_asked,
            "{case}"
        );
        let local = recovered.next_session_id + 30;
        let session = adopted(session, local);
        App::register_session(&recovered.store, &session, None);
        recovered.pool_insert(session);
        assert!(recovered.developer_tools_install_active(), "{case}");
        assert!(
            Mark::at(mark.clone()).opened(),
            "{case}: the adopting build is claimed"
        );
        recovered.developer_tools_install_ended(local, Some(end));
        assert_eq!(Mark::at(mark.clone()).opened(), kept, "{case}");
        assert!(
            aterm_types::clt_mark::opened_on_build(&aterm_types::clt_mark::Live, &mark, earlier),
            "{case}: the earlier build's line stays"
        );
        assert!(recovered.exit_session_logical(local).is_empty());
        assert!(!recovered.developer_tools_install_active(), "{case}");
        assert!(!macos_dialog_up(), "{case}");
    }
}

/// A tab an earlier build opened, adopted across an update, and adopted again by a launch of the
/// adopting build once the window that adopted it went (a crash the PTY keeper outlived), is
/// still that build's ask by the first adoption's claim: the claim rewrote the tab's receipt to
/// say so, so a failure (exit 1) under the second adoption takes the line back and the next launch
/// asks by itself again, while a person's stop keeps it.
/// NEGATIVE CONTROL: the stop keeps the line. The mutant whose adoption leaves the receipt as the
/// earlier build wrote it (no `Receipt::claim` call in `recover_developer_tools_install`) fails
/// the receipt's look after the first adoption, and without that look keeps the line on exit 1.
#[test]
fn clt_recovery_an_update_adopted_tab_adopted_again_after_a_crash_takes_back_its_line() {
    use aterm_pty::ChildExit;
    if !crate::test_alone::alone() {
        return;
    }
    let earlier = "0.1.0+gdeadbeef";
    let build = aterm_stamp::build_id();
    let stopped = i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED);
    for (end, kept) in [
        (ChildExit::Code(1), false),
        (ChildExit::Code(stopped), true),
    ] {
        let directory = aterm_tempfile::tempdir().unwrap();
        let mark = directory.path().join("command-line-tools-tab");
        let (session, _read) = earlier_builds_tab(&mark, earlier, false);

        // The update: this build adopts the earlier build's tab and claims its own line.
        let (mut first, _) = page();
        first.workstation_setup.developer_tools_mark = Mark::at(mark.clone());
        first.access_steps.set_reach(dialog_reach());
        assert!(!first.workstation_setup.developer_tools_mark.opened());
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
            Some(build.as_str()),
            "{end:?}: the receipt says the adoption's claim"
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
            aterm_types::clt_mark::opened_on_build(&aterm_types::clt_mark::Live, &mark, earlier),
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

/// A live install a tab of this same build carries on with once the window that opened it went
/// (a crash the PTY keeper outlived) is still that tab's ask: its receipt says its landing's claim
/// added this build's line, so a failure (exit 1) takes the line back and the next launch asks by
/// itself again, as the window that opened it would have; a person's stop keeps it.
/// NEGATIVE CONTROL: a tab whose own claim added no line (a person's Install… pressed after this
/// build asked already) keeps the line when it fails, adopted as before.
#[test]
fn clt_recovery_a_same_build_adoption_takes_back_only_the_line_its_own_landing_added() {
    use aterm_pty::ChildExit;
    if !crate::test_alone::alone() {
        return;
    }
    let stopped = i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED);
    for (asked_before, end, kept) in [
        (false, ChildExit::Code(1), false),
        (false, ChildExit::Code(stopped), true),
        (true, ChildExit::Code(1), true),
    ] {
        let case = format!("asked before: {asked_before}, {end:?}");
        let directory = aterm_tempfile::tempdir().unwrap();
        let mark = directory.path().join("command-line-tools-tab");
        if asked_before {
            this_builds_mark(&mark);
        }
        // The automatic tab where this build had not asked, else a person's press.
        let (session, _read) = landed_child(&mark);
        assert_eq!(
            receipt_for(&mark, &session).claimed().is_some(),
            !asked_before,
            "{case}"
        );
        let (mut recovered, _) = recovered_page(&mark);
        let local = recovered.next_session_id + 30;
        let session = adopted(session, local);
        App::register_session(&recovered.store, &session, None);
        recovered.pool_insert(session);
        assert!(recovered.developer_tools_install_active(), "{case}");
        recovered.developer_tools_install_ended(local, Some(end));
        assert_eq!(Mark::at(mark.clone()).opened(), kept, "{case}");
        assert!(recovered.exit_session_logical(local).is_empty());
        assert!(!recovered.developer_tools_install_active(), "{case}");
        assert!(!macos_dialog_up(), "{case}");
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

/// A tab its window took down before the PTY keeper held it (a new Mac's first launch killed in
/// its first seconds: E3's kill-stage row, 2026-11-05) is gone with its end unread. The next
/// launch of the build takes back the line the tab's landing claimed, and the tab's receipt, so
/// that launch opens the tab by itself again. A tab still running (the keeper's, for its
/// adoption) keeps both; one gone whose claim added another build's line leaves that line and its
/// receipt to that build; one gone whose claim added none goes, taking back no line.
/// NEGATIVE CONTROL: the mutant whose launch takes nothing back reads this build's ask as spent,
/// so setup's one Mac password is never asked again (the kill-stage guest's `debugger`, owed for
/// good).
#[test]
fn clt_recovery_a_tab_gone_with_its_end_unread_is_asked_again_by_the_next_launch() {
    if !crate::test_alone::alone() {
        return;
    }
    let directory = aterm_tempfile::tempdir().unwrap();
    let mark = directory.path().join("command-line-tools-tab");
    let build = aterm_stamp::build_id();
    let (child, _read) = landed_child(&mark);
    let receipt = receipt_for(&mark, &child);
    assert_eq!(
        receipt.claimed(),
        Some(build.as_str()),
        "its landing claimed this build's ask"
    );

    let running = |pid: i32, birth: u64| pid == CHILD_PID && birth == CHILD_BIRTH;
    assert!(
        Mark::launched(mark.clone(), running).opened(),
        "a running tab is the keeper's"
    );
    assert!(receipt.matches(), "left for its adoption");

    let next = Mark::launched(mark.clone(), |_, _| false);
    assert!(!next.opened(), "gone unread: this launch asks again");
    assert!(launch_install(true, true, true, next.opened()).want);
    assert!(!Mark::at(mark.clone()).opened(), "the line left the record");
    assert!(!receipt.matches(), "and the receipt went with it");

    const EARLIER: &str = "0.1.0+gdeadbeef";
    let live = aterm_types::clt_mark::Live;
    assert!(aterm_types::clt_mark::claim(&live, &mark, EARLIER).unwrap());
    this_builds_mark(&mark);
    for (claimed, kept) in [(Some(EARLIER), true), (None, false)] {
        let orphan = new_receipt(&mark, &child, claimed);
        orphan.write_exclusive().unwrap();
        assert!(
            Mark::launched(mark.clone(), |_, _| false).opened(),
            "{claimed:?}: this build's line is no gone tab's"
        );
        assert!(
            aterm_types::clt_mark::opened_on_build(&live, &mark, EARLIER),
            "{claimed:?}"
        );
        assert_eq!(orphan.matches(), kept, "{claimed:?}");
        orphan.remove_if_matching();
    }
}
