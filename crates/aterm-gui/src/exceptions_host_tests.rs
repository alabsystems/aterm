// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_messages::exceptions::{Entry, Family};
use aterm_messages::need::{Reason, Then, Why};

use crate::App;

fn tab_one() -> Entry {
    Entry::new(
        Family::Upgrade,
        "",
        "own-work",
        "Claude in tab 1 is still on 2.1.283",
        Why::OnlyYou(Reason::new(
            "aterm never ends running work, and a job it started 5 h ago (diskwatch.sh) still runs",
        )),
        Then::step("End that job in the tab"),
    )
}

/// A second family's entry for the digest fixtures, keyed `privacy.fda.needed`.
fn full_disk_access() -> Entry {
    Entry::new(
        Family::Privacy,
        "fda",
        "needed",
        "aterm cannot read your files",
        Why::OnlyYou(Reason::new(
            "only you can turn on Full Disk Access in System Settings",
        )),
        Then::step("Turn on Full Disk Access"),
    )
}

/// `HostNote::AgentPublished` schedules an immediate crash-journal capture without posting a digest entry.
#[cfg(unix)]
#[test]
fn an_agent_publication_owes_the_crash_journal_a_capture() {
    let mut app = App::headless_for_test();
    let dir = crate::crash_journal::tests::scratch("cj-published");
    app.crash_journal = crate::crash_journal::Lane::new(
        Some(dir.path().to_path_buf()),
        crate::crash_journal::tests::reaped_pid(),
    );
    let t0 = std::time::Instant::now();
    assert!(app.crash_journal.offer(
        crate::crash_journal::fixtures::layout(&[("/a", "claude")]),
        Vec::new(),
        t0
    ));
    let inside = t0 + std::time::Duration::from_millis(300);
    app.note_harness_host(crate::harness_host::HostNote::SupervisorSteady {
        sid: "s-x".to_string(),
    });
    assert!(!app.crash_journal.take_dirty(), "another note owes none");
    app.crash_journal.note_activity();
    assert!(
        !app.crash_journal.capture_due(inside),
        "plain activity waits for the interval"
    );
    let said = app.messages.digest().len();
    app.note_harness_host(crate::harness_host::HostNote::AgentPublished {
        sid: "s-x".to_string(),
    });
    assert!(
        app.crash_journal.capture_due(inside),
        "a capture owed now, inside the interval"
    );
    assert_eq!(app.messages.digest().len(), said, "nothing said");
    app.crash_journal = crate::crash_journal::Lane::inert();
}

/// Posting, restating, clearing, and dismissing digest entries never re-grid a window's chrome.
#[test]
fn a_digest_costs_no_regrid() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let rows = app.chrome_rows(wid);
    let _ = crate::app_presence::take_chrome_regrids_applied();
    app.sync_messages();
    app.except_replace(Family::Upgrade, &[tab_one()]);
    app.sync_messages();
    app.except(&full_disk_access());
    app.sync_messages();
    app.except_replace(Family::Upgrade, &[tab_one()]);
    app.sync_messages();
    assert_eq!(app.messages.digest().len(), 2);
    assert_eq!(app.messages.wanted_rows(), 0);
    app.except_clear("privacy.fda.needed");
    app.sync_messages();
    assert_eq!(app.dismiss_exceptions(), 1);
    app.sync_messages();
    assert!(app.messages.digest().is_empty());
    assert_eq!(
        crate::app_presence::take_chrome_regrids_applied(),
        Vec::new(),
        "no chrome re-grid"
    );
    assert_eq!(
        app.chrome_rows(wid),
        rows,
        "the pane's chrome rows never moved"
    );
}

/// `Dismiss` and `Dismiss all` bound their log-flush wait so a wedged writer thread never stalls the UI.
#[test]
fn a_dismissal_returns_though_the_log_writer_never_answers() {
    let mut app = App::headless_for_test();
    let dir = aterm_tempfile::tempdir().expect("a temp dir");
    let (writer, _thread) =
        crate::messages_store::Writer::stalled(&dir.path().join("messages.log"));
    app.messages_log = Some(writer);
    app.except_replace(Family::Upgrade, &[tab_one()]);
    app.sync_messages();
    let id = app.messages.digest().entries[0].id;
    assert!(app.dismiss_exception(id), "filed, the writer wedged");
    app.except(&full_disk_access());
    app.sync_messages();
    assert_eq!(app.dismiss_exceptions(), 1, "filed, the writer wedged");
    assert!(app.messages.digest().is_empty());
}

/// A script's dismissal moves the surfaces (`aterm ctl notice dismiss review`
/// and `dismiss <id>` file through the engine): the menu-bar item and
/// Settings follow the same call, and the log is flushed before the reply.
#[test]
fn a_scripted_dismissal_moves_the_surfaces() {
    use super::DIGEST_MOVES;
    use aterm_messages::wire::NoticeRequest;
    let moves = || DIGEST_MOVES.with(std::cell::Cell::get);
    let mut app = App::headless_for_test();
    app.except_replace(Family::Upgrade, &[tab_one()]);
    app.except(&full_disk_access());
    let id = app.messages.digest().entries[0].id;
    let before = moves();
    let one = app.take_notice(NoticeRequest::Dismiss(id));
    assert!(one.starts_with("OK"), "{one}");
    assert_eq!(app.messages.digest().len(), 1);
    assert_eq!(
        moves(),
        before + 1,
        "dismiss <id> moved the digest's readers"
    );
    let all = app.take_notice(NoticeRequest::DismissReview);
    assert!(all.contains("filed=1"), "{all}");
    assert!(app.messages.digest().is_empty());
    assert_eq!(moves(), before + 2, "dismiss review did too");
    let none = app.take_notice(NoticeRequest::DismissReview);
    assert!(none.contains("filed=0"), "{none}");
    assert_eq!(moves(), before + 2, "nothing to file, nothing to announce");
}

/// A dev build that writes the release's setup is one entry, said once.
#[test]
fn a_dev_build_that_writes_the_setup_is_one_entry() {
    let mut app = App::headless_for_test();
    let writes = || crate::DevSharedWrites {
        what: "the PATH links",
        repair: "`aterm pkg repair`",
    };
    app.note_dev_build_writes(writes());
    app.note_dev_build_writes(writes());
    assert_eq!(app.messages.digest().len(), 1);
    assert_eq!(app.messages.wanted_rows(), 0);
}

/// At Commit the successor folds the parent's log tail in: a `Dismiss all` the parent made
/// after this process loaded the file holds here, and the surfaces move.
#[cfg(unix)]
#[test]
fn the_successor_folds_in_what_the_parent_filed() {
    use super::DIGEST_MOVES;
    use crate::messages_host::wall_stamp_now;
    use aterm_messages::{LogLine, MessageCenter, MessageLog};
    use std::time::Instant;
    let mut app = App::headless_for_test();
    app.except(&full_disk_access());
    assert_eq!(app.messages.digest().len(), 1);
    let posted: Vec<LogLine> = app
        .messages
        .drain_shelved_for_persist()
        .into_iter()
        .map(|(line, _)| line)
        .collect();
    let mut log = MessageLog::empty();
    log.replay_all(posted);
    let now = Instant::now();
    let mut parent = MessageCenter::new(log, now);
    assert_eq!(parent.adopt_digest(wall_stamp_now(), now), 1);
    assert_eq!(parent.file_digest(wall_stamp_now(), now), 1);
    let filed: Vec<LogLine> = parent
        .drain_shelved_for_persist()
        .into_iter()
        .map(|(line, _)| line)
        .collect();
    let before = DIGEST_MOVES.with(std::cell::Cell::get);
    app.catch_up_messages_with(filed);
    assert!(
        app.messages.digest().is_empty(),
        "the parent's filing holds"
    );
    assert_eq!(DIGEST_MOVES.with(std::cell::Cell::get), before + 1);
}

/// The owner's dismissal is on disk before the call returns and holds after
/// the same facts are asserted again (the next look, the next process).
#[test]
fn a_dismissal_holds_when_the_reporter_asserts_the_fact_again() {
    let mut app = App::headless_for_test();
    app.except_replace(Family::Upgrade, &[tab_one()]);
    assert_eq!(app.dismiss_exceptions(), 1);
    app.except_replace(Family::Upgrade, &[tab_one()]);
    assert!(
        app.messages.digest().is_empty(),
        "a filed fact is quiet for its span"
    );
}

/// A reporter that words an entry against the rule shows the owner nothing:
/// the entry is recorded in `aterm.log`, not posted (the roster test fails
/// in a test run first).
#[test]
fn an_entry_that_breaks_the_rule_is_never_shown() {
    let mut app = App::headless_for_test();
    let bad = Entry::new(
        Family::Upgrade,
        "",
        "own-work",
        "Claude is behind",
        Why::OnlyYou(Reason::new("it is waiting for its work")),
        Then::step("Stop that work or skip the build"),
    );
    app.except(&bad);
    assert!(app.messages.digest().is_empty());
}

/// One producer, one mode: the host refuses the wrong call, so a pass over the upgrade's rows
/// never ends the fact its host raised by event.
#[test]
fn a_level_family_is_never_raised_by_event_and_an_event_family_is_never_swept() {
    let mut app = App::headless_for_test();
    app.except(&tab_one());
    assert!(
        app.messages.digest().is_empty(),
        "the upgrade's rows speak by pass, not by event"
    );
    let cannot_run = Entry::new(
        Family::UpgradeHost,
        "",
        "cannot-run",
        "The upgrade cannot run",
        Why::OnlyYou(Reason::new("the upgrade cannot use its state folder")),
        Then::step("fix the folder's permissions"),
    );
    app.except(&cannot_run);
    assert_eq!(app.messages.digest().len(), 1);
    app.except_replace(Family::UpgradeHost, &[]);
    app.except_replace(Family::Upgrade, &[tab_one()]);
    app.except_replace(Family::Upgrade, &[]);
    assert_eq!(
        app.messages.digest().len(),
        2,
        "neither pass ended an entry it does not own"
    );
}

/// An omitted level-family entry resolves via `messages_deadline` and `settle_messages` without a second pass.
#[test]
fn an_entry_its_reporter_stopped_naming_ends_without_a_second_look() {
    use std::time::{Duration, Instant};
    let mut app = App::headless_for_test();
    app.except_replace(Family::Upgrade, &[tab_one()]);
    assert_eq!(app.messages.digest().len(), 1);
    assert_eq!(app.messages_deadline(), None, "a named entry arms nothing");
    app.except_replace(Family::Upgrade, &[]);
    assert_eq!(app.messages.digest().len(), 1, "one look is a flicker");
    let due = app
        .messages_deadline()
        .expect("the loop is told when to come back");
    assert!(due > Instant::now(), "and not before the clock has run");
    let _ = app.settle_messages(due + Duration::from_secs(1));
    assert!(
        app.messages.digest().is_empty(),
        "no second look was needed"
    );
}

/// A refused restatement keeps the existing entry open in its previous wording without starting a resolve timer.
#[test]
fn a_refused_restatement_leaves_its_entry_open() {
    use aterm_messages::need::RESOLVE_AFTER;
    use std::time::{Duration, Instant};
    let mut app = App::headless_for_test();
    app.except_replace(Family::Upgrade, &[tab_one()]);
    let id = app.messages.digest().entries[0].id;
    let shown = app.messages.digest_entry(id).expect("open").detail;
    let refused = Entry::new(
        Family::Upgrade,
        "",
        "own-work",
        "Claude in tab 1 is still on 2.1.283",
        Why::OnlyYou(Reason::new(
            "aterm never ends running work, and it is waiting on background work of its own",
        )),
        Then::step("Stop that work in the tab when it suits you"),
    );
    assert_eq!(refused.key(), tab_one().key(), "the same fact");
    assert!(refused.message().is_err(), "words the door refuses");
    for _ in 0..3 {
        app.except_replace(Family::Upgrade, std::slice::from_ref(&refused));
    }
    assert_eq!(
        app.messages.digest_deadline(),
        None,
        "a fact still named starts no clock"
    );
    let _ = app.settle_messages(Instant::now() + RESOLVE_AFTER + Duration::from_secs(1));
    let open = app
        .messages
        .digest_entry(id)
        .expect("still open past the clock");
    assert_eq!(open.detail, shown, "in the words it was shown in");
    app.except_replace(Family::Upgrade, &[]);
    assert!(app.messages.digest_deadline().is_some());
}

/// Settings, `aterm ctl messages need=later live`, and the menu-bar status item read from the same digest.
#[test]
fn the_surfaces_read_one_digest() {
    let mut app = App::headless_for_test();
    app.except_replace(Family::Upgrade, &[tab_one()]);
    let id = app.messages.digest().entries[0].id;
    let state = app.messages_state();
    let page = state
        .entries
        .iter()
        .find(|e| e.id == id.raw())
        .expect("the entry stands on the page");
    let wire = app.messages.digest_entry(id).expect("open");
    assert_eq!(page.title, wire.title);
    assert_eq!(page.detail, wire.detail);
    let open =
        app.read_messages(&aterm_messages::wire::parse_read_args("need=later live").unwrap());
    assert_eq!(open.len(), 1);
    let live = app.read_messages(&aterm_messages::wire::parse_read_args("live").unwrap());
    assert!(
        live.is_empty(),
        "an open exception is never a live row: {live:?}"
    );
    assert_eq!(
        app.status_item_chrome_line(),
        "statusitem title=\u{276f} needs=0 stuck=0 exceptions=1 done=0 fleet=off access=read \
         shown=read",
        "the menu-bar cue an agent can read"
    );
}

/// Settings page actions offer per-entry `Dismiss` and, when multiple entries are open, `Dismiss all` on the first entry.
#[test]
fn the_page_dismisses_one_entry_or_all_of_them() {
    use crate::native_app::MessageActOutcome;
    use aterm_messages::page::{DISMISS_ACTION, DISMISS_ALL_ACTION};
    let mut app = App::headless_for_test();
    app.except_replace(Family::Upgrade, &[tab_one()]);
    app.except(&full_disk_access());
    assert_eq!(app.messages.digest().len(), 2);
    let state = app.messages_state();
    let open: Vec<_> = state
        .entries
        .iter()
        .filter(|e| e.state == "review")
        .collect();
    assert_eq!(open.len(), 2);
    for e in &open {
        assert!(
            e.actions
                .iter()
                .any(|a| a.index == DISMISS_ACTION && a.still_actionable),
            "every open entry offers Dismiss: {:?}",
            e.actions
        );
    }
    let all_on: Vec<_> = open
        .iter()
        .filter(|e| e.actions.iter().any(|a| a.index == DISMISS_ALL_ACTION))
        .collect();
    assert_eq!(all_on.len(), 1, "only the first offers Dismiss all");
    assert!(std::ptr::eq(*all_on[0], open[0]));

    let (first, second) = (open[0].id, open[1].id);
    let outcome = app.perform_message_act(crate::WindowId(0), first, DISMISS_ACTION);
    assert!(
        matches!(&outcome, MessageActOutcome::Performed { .. }),
        "{outcome:?}"
    );
    assert_eq!(app.messages.digest().len(), 1);
    assert!(matches!(
        app.perform_message_act(crate::WindowId(0), first, DISMISS_ACTION),
        MessageActOutcome::Gone
    ));
    let state = app.messages_state();
    let filed = state
        .entries
        .iter()
        .find(|e| e.id == first)
        .expect("filed record");
    assert!(filed.actions.iter().all(|a| a.index != DISMISS_ACTION));
    let last = state
        .entries
        .iter()
        .find(|e| e.id == second)
        .expect("still open");
    assert!(
        last.actions.iter().all(|a| a.index != DISMISS_ALL_ACTION),
        "one open entry has nothing to dismiss all of"
    );
    let outcome = app.perform_message_act(crate::WindowId(0), second, DISMISS_ALL_ACTION);
    assert!(
        matches!(&outcome, MessageActOutcome::Performed { .. }),
        "{outcome:?}"
    );
    assert!(app.messages.digest().is_empty());
}

/// The rule's counter is said in `aterm.log` from every posting path, once per change: here a
/// plain post the door records.
#[test]
fn the_rules_counter_is_said_once_per_change() {
    use aterm_messages::need::{Ask, Reason, StandingWhy, Then};
    use aterm_messages::{Intent, Message, Severity, UpgradeWord, tags};
    let mut app = App::headless_for_test();
    let decline = |word| Intent::AgentUpgrade {
        tab: "s-1".into(),
        to: "2.1.284".into(),
        word,
    };
    let classified = || {
        Message::new(
            tags::HARNESS,
            Severity::Warn,
            "Couldn't upgrade Claude in tab 1 yet",
        )
        .key("harness.upgrade.s-1")
        .action(decline(UpgradeWord::NotToday))
        .action(decline(UpgradeWord::Skip))
        .now(Ask::standing(
            StandingWhy::OnlyYou(Reason::new("its own work holds the move")),
            Then::step("Stop that work in the tab"),
        ))
    };
    app.post_message(classified());
    assert_eq!(
        app.demotions_logged, 1,
        "row 46's classified twin is recorded and said"
    );
    app.post_message(classified());
    assert_eq!(app.demotions_logged, 2);
    app.post_message(
        Message::new(tags::UPDATE, Severity::Warn, "checks failing").hold(
            aterm_messages::Hold::For(std::time::Duration::from_secs(600)),
        ),
    );
    assert_eq!(app.demotions_logged, 3, "an over-long hold is cut and said");
}
