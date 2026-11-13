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
    let id = if source
        .workstation_setup
        .developer_tools_mark
        .held()
        .is_some()
    {
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
    assert!(app.workstation_setup.developer_tools_mark.held().is_some());
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
    claimed: Option<&aterm_types::clt_mark::Claim>,
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
/// it holds as the choice someone made, never as the install tab's: the lane names what holds.
#[test]
fn clt_recovery_a_traveling_installer_beside_a_queued_choice_holds_as_that_choice() {
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
        Some(LaneHold::SetupChoice),
        "the queued choice holds, as the unsaved choice it is"
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
    assert!(
        receipt
            .claimed()
            .is_some_and(|claim| claim.asker.build == aterm_stamp::build_id()),
        "the receipt names the automatic landing's claim"
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
    assert!(
        !recovered.system_state_handoff_pending(),
        "a recovered installer holds no second update either"
    );
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
    // A successor's receipt of the same child names another claim.
    std::fs::remove_file(receipt.path()).unwrap();
    let mut other = receipt.claimed().cloned().unwrap();
    other.asker.build = "successor-test-build".to_owned();
    let successor_receipt = new_receipt(&mark, child, Some(&other));
    successor_receipt.write_exclusive().unwrap();
    assert!(!receipt.matches(), "the successor changed the claim");
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

/// An earlier build's tab and exact original receipt, as an update hands them over: the earlier
/// build's attempt holds the record, its asker running (this test's own process stands in for
/// the tab's program), and the receipt names that claim.
fn earlier_builds_tab(
    mark: &std::path::Path,
    earlier: &str,
) -> (crate::Session, std::fs::File, aterm_types::clt_mark::Claim) {
    let (session, read) = landed_child(mark);
    std::fs::remove_file(mark).unwrap();
    let asker = aterm_types::clt_mark::Asker::this_process(earlier).unwrap();
    let claim = aterm_types::clt_mark::claim(&aterm_types::clt_mark::Live, mark, &asker, unix())
        .unwrap()
        .unwrap();
    std::fs::remove_file(receipt_for(mark, &session).path()).unwrap();
    new_receipt(mark, &session, Some(&claim))
        .write_exclusive()
        .unwrap();
    (session, read, claim)
}

/// An adopted live child records its end with the exact claim its receipt names, across updates:
/// a failed password is unfinished (held until the next try is due), a proven preprompt end (Low
/// Data Mode, a deferral to another login or another installer, a program that never ran) gives
/// the claim back (an automatic ask may go at once). An unclaimed receipt ends no attempt, so an
/// older attempt's late release never erases the record's. No adopted end is a fresh reading of
/// the root steps: the hint the window holds stays.
#[test]
fn clt_recovery_an_update_records_the_adopted_attempts_end_with_its_exact_claim() {
    use aterm_pty::ChildExit;
    if !crate::test_alone::alone() {
        return;
    }
    let earlier = "0.1.0+gdeadbeef";
    let low_data = i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA);
    let not_own_login = i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_OWN_LOGIN);
    let installer_held = i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_INSTALLER_HELD);
    for (unclaimed, end, held) in [
        (false, ChildExit::Code(1), true),
        (false, ChildExit::Code(low_data), false),
        (false, ChildExit::Code(127), false),
        (false, ChildExit::Code(not_own_login), false),
        (false, ChildExit::Code(installer_held), false),
        (true, ChildExit::Code(1), true),
        (true, ChildExit::Code(not_own_login), true),
    ] {
        let directory = aterm_tempfile::tempdir().unwrap();
        let mark = directory.path().join("command-line-tools-tab");
        let (session, _read, claim) = earlier_builds_tab(&mark, earlier);
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
            (!unclaimed).then_some(&claim)
        );
        recovered.root_steps_owed(RootSteps::OTHER);
        recovered.developer_tools_install_ended(local, Some(end));
        assert_eq!(
            recovered.workstation_setup.developer_tools_root_owed,
            RootSteps::OTHER,
            "an adopted child's {end:?} is not a fresh root-work reading"
        );
        let read = Mark::at(mark.clone());
        assert_eq!(read.held().is_some(), held, "{unclaimed} {end:?}");
        assert!(read.coming(), "{unclaimed} {end:?}");
        assert!(recovered.exit_session_logical(local).is_empty());
        assert!(!recovered.developer_tools_install_active());
        assert!(!macos_dialog_up());
    }
}

/// Repeated adoption after an update and a crash keeps the receipt's exact claim, so the second
/// adopter records the attempt's end: unfinished after a failed password, declined after a Stop.
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
        let (session, _read, claim) = earlier_builds_tab(&mark, earlier);

        // The update: this build adopts the earlier build's tab, claiming nothing.
        let (mut first, _) = page();
        first.workstation_setup.developer_tools_mark = Mark::at(mark.clone());
        first.access_steps.set_reach(dialog_reach());
        assert!(
            first
                .workstation_setup
                .developer_tools_mark
                .held()
                .is_some()
        );
        let local = first.next_session_id + 30;
        let session = adopted(session, local);
        App::register_session(&first.store, &session, None);
        first.pool_insert(session);
        assert!(first.developer_tools_install_active(), "{end:?}");
        assert!(
            Mark::at(mark.clone()).held().is_some(),
            "{end:?}: the attempt still runs"
        );
        assert_eq!(
            receipt_for(&mark, first.pool.get(local).unwrap()).claimed(),
            Some(&claim),
            "{end:?}: the receipt keeps the exact claim"
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
        assert_eq!(Mark::at(mark.clone()).held().is_some(), kept, "{end:?}");
        assert!(
            matches!(
                aterm_types::clt_mark::reading(
                    &aterm_types::clt_mark::Live,
                    &mark,
                    super::unix_now()
                ),
                aterm_types::clt_mark::Asked::Unfinished { tries: 1, .. }
                    | aterm_types::clt_mark::Asked::Declined { .. }
            ),
            "{end:?}: the end is recorded"
        );
        assert!(second.exit_session_logical(local).is_empty());
        assert!(!second.developer_tools_install_active(), "{end:?}");
        assert!(!macos_dialog_up(), "{end:?}");
    }
}

/// An update from a build whose receipts named only the build that claimed (`v2`) and whose
/// record said only `opened <build>` hands this build that build's live Mac Setup tab, its
/// `sudo` perhaps asking now (review finding: such a receipt was refused, so the tab's end was
/// recorded nowhere and its `opened` line read as a try long since due). The launch takes
/// custody of the tab, and once the handoff is in carries its attempt into the record as the
/// tab's program's and rewrites the receipt as this build's: the record then reads it asking, so
/// no automatic ask goes beside it, and its end is recorded against that claim, a Stop as a
/// decline that holds the next automatic ask for its long pause. A carry the record refused (an
/// asker holding its lock past the window's wait) is tried again by itself on its schedule, the
/// window waking for it.
/// NEGATIVE CONTROL: the earlier build's record alone lets an automatic ask go at once, and until
/// the handoff is in nothing is carried and the receipt stays as the earlier build wrote it; a
/// carry that failed changes neither, and none is tried again before its pause has passed.
#[test]
fn clt_recovery_an_earlier_formats_live_tab_carries_its_attempt_and_records_its_end() {
    use aterm_types::clt_mark::Asked;
    if !crate::test_alone::alone() {
        return;
    }
    let earlier = "0.193.0+g5a297ec3fd7c";
    let stopped = i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED);
    let reading = |mark: &std::path::Path| {
        aterm_types::clt_mark::reading(&aterm_types::clt_mark::Live, mark, unix())
    };
    for claimed in [true, false] {
        let directory = aterm_tempfile::tempdir().unwrap();
        let mark = directory.path().join("command-line-tools-tab");
        let (session, read) = landed_child(&mark);
        // What the earlier build left: its record's line, and its receipt for the same child.
        std::fs::write(&mark, format!("opened {earlier}\n")).unwrap();
        let path = receipt_for(&mark, &session).path().to_path_buf();
        let line = if claimed {
            format!("claimed {earlier}")
        } else {
            "unclaimed".to_owned()
        };
        let v2 = format!(
            "v2\n{}\n{}\n{}\n{line}\n",
            session.ctx.self_id.as_str(),
            session.pid,
            session.shell_identity.birth.unwrap()
        );
        std::fs::write(&path, &v2).unwrap();
        assert!(
            Mark::at(mark.clone()).held().is_none(),
            "{claimed}: the earlier record alone lets an automatic ask go now"
        );

        let (mut recovered, _) = page();
        recovered.workstation_setup.developer_tools_mark = Mark::at(mark.clone());
        recovered.access_steps.set_reach(dialog_reach());
        let local = recovered.next_session_id + 30;
        let session = adopted(session, local);
        App::register_session(&recovered.store, &session, None);
        recovered.pool_insert(session);
        assert!(
            recovered.developer_tools_install_active(),
            "{claimed}: custody of the earlier build's live tab"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            v2,
            "{claimed}: the receipt stays the earlier build's until the handoff is in"
        );

        let now = std::time::Instant::now();
        let stalled =
            aterm_types::clt_mark::Files::lock(&aterm_types::clt_mark::Live, &mark).unwrap();
        recovered.carry_earlier_developer_tools_tabs(now);
        drop(stalled);
        let historical = format!("opened {earlier}\n");
        assert_eq!(std::fs::read_to_string(&mark).unwrap(), historical);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), v2);
        let retry = now + LOOK_AGAIN.after(1);
        assert_eq!(
            recovered.workstation_setup.developer_tools_carry_retry,
            Some(retry),
            "{claimed}: a carry that failed is tried again on its schedule"
        );
        assert!(
            recovered
                .workstation_setup
                .deadline(now)
                .is_some_and(|wake| wake <= retry),
            "{claimed}: the window wakes for it"
        );
        recovered.carry_earlier_developer_tools_tabs(retry - Duration::from_secs(1));
        assert_eq!(
            std::fs::read_to_string(&mark).unwrap(),
            historical,
            "{claimed}: not before its pause has passed"
        );

        recovered.carry_earlier_developer_tools_tabs(retry);
        assert!(
            matches!(reading(&mark), Asked::Asking),
            "{claimed}: the record reads the carried tab asking"
        );
        assert_eq!(
            recovered.workstation_setup.developer_tools_carry_retry,
            None
        );
        assert!(
            recovered
                .workstation_setup
                .developer_tools_mark
                .held()
                .is_some(),
            "{claimed}: no automatic ask beside it"
        );
        let carried = receipt_for(&mark, recovered.pool.get(local).unwrap());
        assert!(
            carried.earlier().is_none() && carried.claimed().is_some(),
            "{claimed}: the receipt is this build's, naming the claim it carried"
        );
        let asking = std::fs::read_to_string(&mark).unwrap();
        recovered.carry_earlier_developer_tools_tabs(retry);
        assert_eq!(
            std::fs::read_to_string(&mark).unwrap(),
            asking,
            "{claimed}: carried once"
        );

        recovered.developer_tools_install_ended(local, Some(aterm_pty::ChildExit::Code(stopped)));
        assert!(
            matches!(reading(&mark), Asked::Declined { declines: 1, .. }),
            "{claimed}: its Stop is recorded as a decline"
        );
        assert!(recovered.exit_session_logical(local).is_empty());
        assert!(!recovered.developer_tools_install_active());
        assert!(!path.exists(), "{claimed}: its exit retires the receipt");
        assert!(drain(&read).is_empty(), "adoption types nothing");
        assert!(!macos_dialog_up());
    }
}

/// A live tab an earlier aterm opened whose recovery record this launch cannot read (none, a
/// malformed one, another folder) gives no custody here, but it still runs the Mac Setup
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
        // No custody: no password, restored tab or install is claimed for it; at most the row
        // says aterm's own ask comes, on the record's schedule.
        let step = step(&mut recovered);
        assert!(
            matches!(step, None | Some(ToolsInstall::Coming)),
            "{case}: {step:?}"
        );
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
    asking_now(&mark);
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
        !source
            .workstation_setup
            .developer_tools_mark
            .held()
            .is_some()
            && !Mark::at(mark.clone()).held().is_some(),
        "an install that never started takes its opening back: the next launch asks again"
    );
    assert!(source.developer_tools_install_active());
    assert!(macos_dialog_up());
    assert!(receipt.matches());
    assert!(source.exit_session_logical(id).is_empty());
    assert!(
        !Mark::at(mark.clone()).held().is_some(),
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

/// H8: an orphan (its tab's program gone with no end any window saw) was cut off: the record
/// its program held reads due at once, so a launch wants the tools again by itself, and the
/// orphan's receipt goes. A live child's receipt stays for adoption.
#[test]
fn clt_recovery_an_orphans_attempt_was_cut_off_and_is_asked_again() {
    if !crate::test_alone::alone() {
        return;
    }
    let directory = aterm_tempfile::tempdir().unwrap();
    let mark = directory.path().join("command-line-tools-tab");
    let (child, _read) = landed_child(&mark);
    let receipt = receipt_for(&mark, &child);
    // The record as the tab's program held it: that program is gone.
    std::fs::remove_file(&mark).unwrap();
    let program = aterm_types::clt_mark::Asker {
        build: aterm_stamp::build_id(),
        pid: CHILD_PID,
        birth: CHILD_BIRTH,
    };
    aterm_types::clt_mark::claim(&aterm_types::clt_mark::Live, &mark, &program, unix())
        .unwrap()
        .unwrap();
    Mark::launched(mark.clone(), false, |pid, birth| {
        pid == CHILD_PID && birth == CHILD_BIRTH
    });
    assert!(receipt.matches(), "a live child's receipt stays");
    let next = Mark::launched(mark.clone(), false, |_, _| false);
    assert_eq!(next.held(), None, "cut off: asked again at once");
    assert!(launch_install(true, true, true, !next.coming()).want);
    assert!(!receipt.matches(), "the orphan's receipt goes");
}

/// AN UPDATE'S SUCCESSOR RETIRES NO TAB ITS PARENT STILL HOLDS: a program that ended while a descendant
/// holds its PTY reads as no longer running, and its parent has not seen the end. Retiring it took
/// the receipt the parent's handoff was admitted on, which then refused every later attempt for as
/// long as that descendant lived, and made the parent's own release find no line. NEGATIVE CONTROL: a
/// launch with no parent reads it as an orphan.
#[test]
fn clt_recovery_an_update_s_successor_retires_no_tab_its_parent_holds() {
    if !crate::test_alone::alone() {
        return;
    }
    let directory = aterm_tempfile::tempdir().unwrap();
    let mark = directory.path().join("command-line-tools-tab");
    let (child, _read) = landed_child(&mark);
    let receipt = receipt_for(&mark, &child);
    let record = std::fs::read(&mark).unwrap();
    Mark::launched(mark.clone(), true, |_, _| false);
    assert!(receipt.matches(), "the parent's receipt stays");
    assert_eq!(std::fs::read(&mark).unwrap(), record, "and the record");
    Mark::launched(mark.clone(), false, |_, _| false);
    assert!(!receipt.matches(), "NEGATIVE CONTROL");
}
