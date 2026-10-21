// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the digest's family pass (level-triggered; an entry a pass stops
//! naming ends once it has gone unnamed for ten minutes, at a look or at the
//! digest's own clock; words restated at most once in ten minutes) and the
//! seamless update's overlap (a dismissal the parent made after the successor
//! loaded the file).

use crate::Retired;
use crate::center::{MessageCenter, Outcome, PostOutcome};
use crate::exceptions::{Entry, Family, Mode};
use crate::log::{LogLine, MessageLog};
use crate::model::Message;
use crate::model::{Severity, WallStamp, tags};
use crate::need::{
    Fingerprint, LaterAsk, Mover, RESOLVE_AFTER, RESTATE_QUIET, Reason, Resurface, Then, Why,
};
use crate::{Duration, Instant};

fn t0() -> Instant {
    Instant::now()
}

fn stamp() -> WallStamp {
    WallStamp {
        unix_ms: 1_790_000_000_000,
    }
}

fn fresh(now: Instant) -> MessageCenter {
    MessageCenter::new(MessageLog::empty(), now)
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn own_work(tab: &str, holders: u32) -> Message {
    let ask = LaterAsk::person(
        Why::OnlyYou(Reason::new(format!(
            "aterm never ends running work, and {holders} programs under it are still running"
        ))),
        Then::step("Stop that work in the tab when it suits you"),
        Fingerprint::new("harness.upgrade", tab, "own-work"),
        Resurface::WAIT,
    );
    Message::new(
        tags::HARNESS,
        Severity::Info,
        format!("Claude in {tab} is behind"),
    )
    .later(ask)
}

fn fda() -> Message {
    let ask = LaterAsk::person(
        Why::OnlyYou(Reason::new("macOS forgets an Allow when aterm updates")),
        Then::press(
            Mover::open_system_pane("full-disk-access"),
            "Turn on aterm in Full Disk Access",
        ),
        Fingerprint::new("privacy", "fda", "needed"),
        Resurface::CHOICE,
    );
    Message::new(
        tags::PRIVACY,
        Severity::Info,
        "macOS keeps asking for access",
    )
    .later(ask)
}

const UPGRADE: &str = "harness.upgrade.";
const PRIVACY: &str = "privacy.";

fn posted_lines(c: &MessageCenter) -> usize {
    c.log().records().filter(|r| r.later).count()
}

/// A reporter states the whole of its family each look. A new fact posts, the same fact
/// is quiet, and a fact that stops being named ends only once it has gone unnamed for
/// `RESOLVE_AFTER`: a look of silence is a flicker, not an end; a look that names it
/// again clears the clock and writes no line.
#[test]
fn a_family_pass_ends_an_entry_only_after_ten_minutes_unnamed() {
    let now = t0();
    let mut c = fresh(now);
    let s = c.sync_digest(UPGRADE, vec![own_work("s-1", 16)], stamp(), now);
    assert_eq!((s.posted, s.resolved), (1, 0));
    assert_eq!(c.digest().len(), 1);
    // The same fact again: nothing new, nothing logged.
    let s = c.sync_digest(
        UPGRADE,
        vec![own_work("s-1", 16)],
        stamp(),
        now + ms(60_000),
    );
    assert_eq!((s.posted, s.restated, s.resolved), (0, 0, 0));
    assert_eq!(posted_lines(&c), 1);
    // A look that does not name it: its clock starts, it stays open.
    let s = c.sync_digest(UPGRADE, vec![], stamp(), now + ms(120_000));
    assert_eq!((s.resolved, s.pending_end), (0, 1));
    assert_eq!(c.digest().len(), 1);
    // A second empty look inside the clock changes nothing (two looks in a
    // burst are not two opinions).
    let s = c.sync_digest(UPGRADE, vec![], stamp(), now + ms(121_000));
    assert_eq!((s.resolved, s.pending_end), (0, 1));
    // It comes back: the same entry, the clock cleared, no second `posted`.
    let s = c.sync_digest(
        UPGRADE,
        vec![own_work("s-1", 16)],
        stamp(),
        now + ms(180_000),
    );
    assert_eq!((s.posted, s.resolved), (0, 0));
    assert_eq!(posted_lines(&c), 1, "a flicker writes no line");
    assert_eq!(c.digest_deadline(), None, "a named entry arms nothing");
    // Unnamed again, for the whole clock: ended, once, with its line.
    let _ = c.sync_digest(UPGRADE, vec![], stamp(), now + ms(240_000));
    let s = c.sync_digest(UPGRADE, vec![], stamp(), now + ms(240_000) + RESOLVE_AFTER);
    assert_eq!(s.resolved, 1);
    assert!(c.digest().is_empty());
    assert_eq!(posted_lines(&c), 1);
}

/// The upgrade's looks are sent only when its rows change, so a row that leaves in one
/// change (the owner quits Claude by hand, or closes the tab) gets one look, then
/// silence. The entry still ends, by the digest's own clock the host arms
/// (`digest_deadline`) and settles (`settle_digest`), and the log says when.
#[test]
fn an_entry_whose_row_left_in_one_change_ends_at_the_digests_own_clock() {
    let now = t0();
    let mut c = fresh(now);
    let _ = c.sync_digest(UPGRADE, vec![own_work("s-1", 16)], stamp(), now);
    // The one look that no longer names it.
    let gone = now + ms(60_000);
    let s = c.sync_digest(UPGRADE, vec![], stamp(), gone);
    assert_eq!((s.resolved, s.pending_end), (0, 1));
    // The loop is told when to come back.
    let due = c
        .digest_deadline()
        .expect("a missed entry arms the digest clock");
    assert_eq!(due, gone + RESOLVE_AFTER);
    // Before it: settling changes nothing. At it: the entry ends.
    assert!(!c.settle_digest(stamp(), due - ms(1)));
    assert_eq!(c.digest().len(), 1);
    assert!(c.settle_digest(stamp(), due));
    assert!(c.digest().is_empty());
    assert_eq!(c.digest_deadline(), None);
    let ended = c.log().records().find(|r| r.later).expect("its record");
    assert_eq!(
        ended.retired(),
        Some(&Retired::Resolved(Outcome::Ok)),
        "resolved, not dismissed: a false line the owner never has to file"
    );
}

/// The same fact, the same key, is news again once its cause has ended: a
/// `Resolved` entry is not quiet (it is not a filing), so a return posts a
/// fresh entry.
#[test]
fn a_resolved_entry_that_returns_is_news() {
    let now = t0();
    let mut c = fresh(now);
    let _ = c.sync_digest(UPGRADE, vec![own_work("s-1", 16)], stamp(), now);
    let _ = c.sync_digest(UPGRADE, vec![], stamp(), now + ms(1_000));
    assert!(c.settle_digest(stamp(), now + ms(1_000) + RESOLVE_AFTER));
    let s = c.sync_digest(
        UPGRADE,
        vec![own_work("s-1", 16)],
        stamp(),
        now + ms(1_000) + RESOLVE_AFTER + ms(1),
    );
    assert_eq!((s.posted, s.suppressed), (1, 0));
}

/// One producer, one mode: the upgrade rows' pass sweeps everything under
/// `harness.upgrade.`, so a fact the host raises by event (the state folder it cannot
/// use) has a prefix of its own and survives any number of the rows' passes.
#[test]
fn an_event_raised_sibling_survives_the_rows_family_pass() {
    let now = t0();
    let mut c = fresh(now);
    let host = Family::UpgradeHost;
    let e = Entry::new(
        host,
        "",
        "cannot-run",
        "The upgrade cannot run",
        Why::OnlyYou(Reason::new("the upgrade cannot use its state folder")),
        Then::step("fix the folder's permissions"),
    );
    let msg = e.message().expect("the entry is well formed");
    assert_eq!(c.post(msg, stamp(), now).outcome, PostOutcome::New);
    assert_eq!(e.key(), "harness.upgrade-host.cannot-run");
    for k in 1..=5u64 {
        let _ = c.sync_digest(
            &Family::Upgrade.prefix(),
            vec![own_work("s-1", 16)],
            stamp(),
            now + ms(60_000 * k),
        );
    }
    assert!(c.settle_digest(stamp(), now + ms(400_000)) == false);
    let keys: Vec<String> = c.digest().entries.iter().map(|e| e.key.clone()).collect();
    assert!(keys.contains(&"harness.upgrade-host.cannot-run".to_string()));
    assert_eq!(keys.len(), 2);
}

/// The roster: every family has one producer and one mode, and no family's prefix
/// starts another's (a pass over one must never sweep the other).
#[test]
fn every_family_has_one_mode_and_no_prefix_shadows_another() {
    let all = Family::ALL;
    for a in all {
        for b in all {
            if a != b {
                assert!(
                    !a.prefix().starts_with(&b.prefix()),
                    "{a:?}'s prefix {} starts {b:?}'s {}",
                    a.prefix(),
                    b.prefix()
                );
            }
        }
    }
    // The level families are the ones whose reporters state their whole answer
    // (the upgrade's and the resume loop's looks, the AI CLIs' look — each
    // states every CLI that needs a person — the need publication's API
    // outage, the window's long holds, the fleet glance's read, the disk
    // walker's raised huge folders); everything else raises by event and is
    // never swept.
    let level: Vec<Family> = all
        .into_iter()
        .filter(|f| f.mode() == Mode::Level)
        .collect();
    assert_eq!(
        level,
        vec![
            Family::Upgrade,
            Family::Resume,
            Family::Ai,
            Family::ApiOutage,
            Family::LongHold,
            Family::FleetUnreadable,
            Family::HugeDir
        ]
    );
    assert_eq!(Family::Ai.stem(), "ai");
    assert_eq!(Family::Ai.tag(), tags::HARNESS);
}

/// A pass for one family never sees another family's entries, so a reporter
/// that has not spoken yet (after an update) loses nothing to one that has.
#[test]
fn a_family_pass_never_touches_another_family() {
    let now = t0();
    let mut c = fresh(now);
    let _ = c.sync_digest(UPGRADE, vec![own_work("s-1", 16)], stamp(), now);
    let _ = c.sync_digest(PRIVACY, vec![fda()], stamp(), now);
    for k in 0..5u64 {
        // Five looks three minutes apart: the fifth is past the ten-minute clock.
        let _ = c.sync_digest(UPGRADE, vec![], stamp(), now + ms(180_000 * (k + 1)));
    }
    let keys: Vec<String> = c.digest().entries.iter().map(|e| e.key.clone()).collect();
    assert_eq!(keys, vec!["privacy.fda.needed".to_string()]);
    // A message that is not this family's exception is recorded, not posted.
    let before = c.demotions();
    let s = c.sync_digest(UPGRADE, vec![fda()], stamp(), now);
    assert_eq!(s.posted, 0);
    assert_eq!(c.demotions(), before + 1);
}

/// Holder counts and ages move every look. Words alone restate an entry at
/// most once per ten minutes; a new severity restates at once.
#[test]
fn a_words_only_change_restates_at_most_once_in_ten_minutes() {
    let now = t0();
    let mut c = fresh(now);
    let _ = c.sync_digest(UPGRADE, vec![own_work("s-1", 16)], stamp(), now);
    let s = c.sync_digest(
        UPGRADE,
        vec![own_work("s-1", 15)],
        stamp(),
        now + ms(60_000),
    );
    assert_eq!(s.restated, 0, "a count moved: not news yet");
    assert!(c.digest().entries[0].why.contains("16 programs"));
    let rev = c.digest_revision();
    let s = c.sync_digest(
        UPGRADE,
        vec![own_work("s-1", 15)],
        stamp(),
        now + RESTATE_QUIET + ms(1),
    );
    assert_eq!(s.restated, 1);
    assert!(c.digest().entries[0].why.contains("15 programs"));
    assert!(c.digest_revision() > rev);
    // A new severity is news at once.
    let mut louder = own_work("s-1", 15);
    louder.severity = Severity::Warn;
    let s = c.sync_digest(UPGRADE, vec![louder], stamp(), now + RESTATE_QUIET + ms(2));
    assert_eq!(s.restated, 1);
    assert_eq!(posted_lines(&c), 1, "restating writes no log line");
}

/// Words that move only with their fact (`LaterAsk::restated_at_once`: the
/// tabs an outage holds, counted as they join it) are restated at once, inside
/// the ten minutes; the same count change on an ordinary entry waits (the
/// negative control, `a_words_only_change_restates_at_most_once_in_ten_minutes`).
#[test]
fn words_that_move_with_their_fact_restate_at_once() {
    let now = t0();
    let mut c = fresh(now);
    let outage = |holders: u32| {
        let ask = LaterAsk::person(
            Why::OnlyYou(Reason::new(format!(
                "the network keeps {holders} programs from their API server"
            ))),
            Then::step("Check this Mac's network connection"),
            Fingerprint::new("harness.upgrade", "s-1", "own-work"),
            Resurface::WAIT,
        )
        .restated_at_once();
        Message::new(
            tags::HARNESS,
            Severity::Info,
            format!("{holders} can't reach it"),
        )
        .later(ask)
    };
    let _ = c.sync_digest(UPGRADE, vec![outage(1)], stamp(), now);
    let s = c.sync_digest(UPGRADE, vec![outage(2)], stamp(), now + ms(1_000));
    assert_eq!(s.restated, 1, "the count moved with its fact: news at once");
    assert!(c.digest().entries[0].why.contains("2 programs"));
    let s = c.sync_digest(UPGRADE, vec![outage(2)], stamp(), now + ms(2_000));
    assert_eq!(s.restated, 0, "the same words are a duplicate");
    assert_eq!(posted_lines(&c), 1, "restating writes no log line");
}

/// A NEW STEP IS NEWS AT ONCE: the one thing an entry tells the owner to do
/// (a renewal's command naming the next account) is never held for the ten
/// minutes that keep a moving count quiet, so the entry never asks for what no
/// longer helps. NEGATIVE CONTROL: the same entry whose reason alone moves
/// inside the ten minutes stands in its first words.
#[test]
fn a_new_step_restates_at_once_and_a_moving_reason_waits() {
    let now = t0();
    let mut c = fresh(now);
    let renew = |why: &str, label: &str| {
        let ask = LaterAsk::person(
            Why::OnlyYou(Reason::new(why)),
            Then::step(format!(
                "Run in a terminal: aterm harness accounts renew {label}"
            )),
            Fingerprint::new("harness.upgrade", "", "wrong-account"),
            Resurface::WAIT,
        );
        Message::new(
            tags::HARNESS,
            Severity::Info,
            "Claude renewal used another account",
        )
        .later(ask)
    };
    let first = "A renewal signed in a Claude account other than its roster row's";
    let _ = c.sync_digest(UPGRADE, vec![renew(first, "pool-a")], stamp(), now);
    // NEGATIVE CONTROL: the reason moves, the step does not.
    let s = c.sync_digest(
        UPGRADE,
        vec![renew(
            "Two renewals signed in other Claude accounts",
            "pool-a",
        )],
        stamp(),
        now + ms(1_000),
    );
    assert_eq!(s.restated, 0, "words alone: not news yet");
    assert_eq!(c.digest().entries[0].why, first);
    // The step moves: at once, and the entry says the new step.
    let s = c.sync_digest(
        UPGRADE,
        vec![renew(first, "pool-b")],
        stamp(),
        now + ms(2_000),
    );
    assert_eq!(s.restated, 1, "a new step is news at once");
    assert!(
        c.digest().entries[0].then.ends_with("renew pool-b"),
        "{:?}",
        c.digest().entries[0]
    );
    assert_eq!(posted_lines(&c), 1, "one entry");
}

/// The owner filed the batch: the next pass names the same facts and gets
/// quiet, and the missing-look counter does not run for a fact that never
/// opened.
#[test]
fn a_dismissed_fact_stays_quiet_through_the_next_passes() {
    let now = t0();
    let mut c = fresh(now);
    let _ = c.sync_digest(UPGRADE, vec![own_work("s-1", 16)], stamp(), now);
    let _ = c.sync_digest(PRIVACY, vec![fda()], stamp(), now);
    assert_eq!(c.file_digest(stamp(), now + ms(5_000)), 2);
    let s = c.sync_digest(
        UPGRADE,
        vec![own_work("s-1", 16)],
        stamp(),
        now + ms(60_000),
    );
    assert_eq!((s.posted, s.suppressed), (0, 1));
    let s = c.sync_digest(PRIVACY, vec![fda()], stamp(), now + ms(60_000));
    assert_eq!((s.posted, s.suppressed), (0, 1));
    assert!(c.digest().is_empty());
    assert_eq!(c.wanted_rows(), 0);
}

/// A verified grant ends its entry at once, with its line.
#[test]
fn an_explicit_clear_ends_the_entry_at_once() {
    let now = t0();
    let mut c = fresh(now);
    let _ = c.sync_digest(PRIVACY, vec![fda()], stamp(), now);
    assert!(c.clear_digest("privacy.fda.needed", now + ms(1_000)));
    assert!(c.digest().is_empty());
    assert!(
        !c.clear_digest("privacy.fda.needed", now + ms(2_000)),
        "once"
    );
    // An ended cause is not quiet: its return is news.
    let s = c.sync_digest(PRIVACY, vec![fda()], stamp(), now + ms(3_000));
    assert_eq!(s.posted, 1);
}

/// The Full Disk Access card's normal order: the owner presses the entry's button
/// (filed `Answered`, quiet for its span), and only then does the probe read the grant.
/// The verified grant ends the fact, so the next regression is news, in this process
/// and after a restart.
#[test]
fn a_grant_after_the_owners_answer_makes_the_next_regression_news() {
    use crate::model::ActionIndex;
    let now = t0();
    let mut c = fresh(now);
    let id = c.post(fda(), stamp(), now).id;
    assert!(
        c.act_digest(id, ActionIndex(0), stamp(), now + ms(1_000))
            .is_some()
    );
    assert!(c.digest().is_empty(), "answered: closed");
    // Still quiet before the grant: the same fact filed is not news.
    let quiet = c.post(fda(), stamp(), now + ms(2_000));
    assert_ne!(quiet.outcome, PostOutcome::New);
    assert!(c.digest().is_empty());
    // The verified grant: nothing is open, and the filing ends with its cause.
    assert!(!c.clear_digest("privacy.fda.needed", now + ms(3_000)));
    assert!(c.forget_filing("privacy.fda.needed", stamp(), now + ms(3_000)));
    assert!(
        !c.forget_filing("privacy.fda.needed", stamp(), now + ms(3_500)),
        "once: the latest word is a resolution now"
    );
    // The regression after the next update is news.
    let again = c.post(fda(), stamp(), now + ms(4_000));
    assert_eq!(again.outcome, PostOutcome::New);
    assert_eq!(c.digest().len(), 1);
    // ... and the resolution survives a restart, so a filing the log replays
    // does not keep a fact quiet whose cause the owner has since seen end.
    let mut c = fresh(now);
    let id = c.post(fda(), stamp(), now).id;
    let _ = c.act_digest(id, ActionIndex(0), stamp(), now + ms(1_000));
    assert!(c.forget_filing("privacy.fda.needed", stamp(), now + ms(2_000)));
    let lines: Vec<String> = c
        .drain_new_for_persist()
        .iter()
        .map(LogLine::encode)
        .collect();
    let mut log = MessageLog::empty();
    log.replay_all(lines.iter().filter_map(|l| LogLine::decode(l).ok()));
    let mut next = MessageCenter::new(log, now);
    assert_eq!(
        next.post(fda(), stamp(), now + ms(3_000)).outcome,
        PostOutcome::New
    );
}

// ---- the seamless update: a dismissal made while the two processes overlap ------

/// The lines a center wants on disk, encoded and decoded as the file would.
fn on_disk(c: &mut MessageCenter) -> Vec<LogLine> {
    c.drain_new_for_persist()
        .iter()
        .map(LogLine::encode)
        .filter_map(|l| LogLine::decode(&l).ok())
        .collect()
}

/// The successor loads the file at its launch and waits for Commit; the owner presses
/// `Dismiss all` in the parent meanwhile. At Commit the successor reads the file's tail
/// again, so the dismissed fact stays filed. Negative controls: a successor that trusts
/// its launch snapshot (`StaleSuccessor`) and a parent whose dismissal never reached the
/// file (`NoFlush`) both show the fact again.
#[test]
fn the_successor_honours_a_dismissal_the_parent_made_while_they_overlapped() {
    let now = t0();
    let wall = stamp();
    // The parent has one entry open; its lines are on disk.
    let mut parent = fresh(now);
    let _ = parent.sync_digest(UPGRADE, vec![own_work("s-1", 16)], wall, now);
    let file: Vec<LogLine> = on_disk(&mut parent);
    // The successor launches: it loads the file as it is now, and adopts.
    let load = |file: &[LogLine], at: Instant| {
        let mut log = MessageLog::empty();
        log.replay_all(file.iter().cloned());
        let mut c = MessageCenter::new(log, at);
        let n = c.adopt_digest(wall, at);
        assert_eq!(n, 1, "the open entry crosses the update");
        c
    };
    let mut successor = load(&file, now + ms(1_000));
    let mut stale = load(&file, now + ms(1_000));
    let mut lost = load(&file, now + ms(1_000));
    // The owner dismisses in the parent while the two overlap.
    assert_eq!(parent.file_digest(wall, now + ms(2_000)), 1);
    let filed = on_disk(&mut parent);
    assert!(!filed.is_empty(), "the filing is a line");
    let mut with_filing = file.clone();
    with_filing.extend(filed);

    // Commit: the successor reads the tail again and honours the filing.
    let found = successor.catch_up(with_filing.iter().cloned(), wall, now + ms(3_000));
    assert_eq!(found.closed, 1);
    assert!(
        successor.digest().is_empty(),
        "the filed fact is gone here too"
    );
    let s = successor.sync_digest(UPGRADE, vec![own_work("s-1", 16)], wall, now + ms(4_000));
    assert_eq!(
        (s.posted, s.suppressed),
        (0, 1),
        "and quiet, not raised again"
    );
    assert!(
        on_disk(&mut successor).is_empty(),
        "the successor writes no second filing and no second posting"
    );

    // StaleSuccessor: it trusts the snapshot it loaded and never reads the tail.
    let s = stale.sync_digest(UPGRADE, vec![own_work("s-1", 16)], wall, now + ms(4_000));
    assert_eq!(stale.digest().len(), 1, "the dismissed fact is shown again");
    assert_eq!(s.posted, 0);

    // NoFlush: the parent exited without flushing, so the file never learned.
    let found = lost.catch_up(file.iter().cloned(), wall, now + ms(3_000));
    assert_eq!(found.closed, 0);
    assert_eq!(
        lost.digest().len(),
        1,
        "a dismissal that never reached the file is lost"
    );
}

/// The catch-up may run twice (a Commit re-read after a Commit re-read): the
/// second finds nothing new and changes nothing.
#[test]
fn catching_up_twice_changes_nothing() {
    let now = t0();
    let wall = stamp();
    let mut parent = fresh(now);
    let _ = parent.sync_digest(UPGRADE, vec![own_work("s-1", 16)], wall, now);
    let mut file: Vec<LogLine> = on_disk(&mut parent);
    let mut log = MessageLog::empty();
    log.replay_all(file.iter().cloned());
    let mut successor = MessageCenter::new(log, now + ms(1_000));
    assert_eq!(successor.adopt_digest(wall, now + ms(1_000)), 1);
    assert_eq!(parent.file_digest(wall, now + ms(2_000)), 1);
    file.extend(on_disk(&mut parent));
    let first = successor.catch_up(file.iter().cloned(), wall, now + ms(3_000));
    assert_eq!(first.closed, 1);
    let again = successor.catch_up(file.iter().cloned(), wall, now + ms(4_000));
    assert_eq!((again.closed, again.adopted), (0, 0));
    assert!(successor.digest().is_empty());
    assert!(on_disk(&mut successor).is_empty(), "and it wrote nothing");
}

/// A posting is queued in the same call that makes the entry visible: no
/// surface can count an entry whose line is not on its way to the log
/// (`ExceptionsOnce`: `ShownImpliesQueued`).
#[test]
fn an_entry_is_never_visible_before_its_posting_is_queued() {
    let now = t0();
    let mut c = fresh(now);
    let s = c.sync_digest(UPGRADE, vec![own_work("s-1", 16)], stamp(), now);
    assert_eq!(
        s.posted_keys,
        vec!["harness.upgrade.s-1.own-work".to_string()]
    );
    assert_eq!(c.digest().len(), 1);
    let lines = on_disk(&mut c);
    assert!(
        lines
            .iter()
            .any(|l| matches!(l, LogLine::Posted(r) if r.later)),
        "the posting is pending the moment the entry shows: {lines:?}"
    );
}
