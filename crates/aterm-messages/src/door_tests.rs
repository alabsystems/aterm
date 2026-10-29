// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for `MessageCenter` admission rules (`need.rs`).

use crate::center::{MessageCenter, Outcome};
use crate::log::MessageLog;
use crate::model::{
    Decision, Hold, Intent, Message, Meter, Severity, UpgradeWord, WallStamp, tags,
};
use crate::need::{Ask, Mover, Reason, Span, StandingWhy, Then, Why};
use crate::{Duration, Instant, PROGRESS_GRACE};

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

fn sentence() -> Ask {
    Ask::standing(
        StandingWhy::OnlyYou(Reason::new("its own work holds the move")),
        Then::step("Stop that work in the tab when it suits you"),
    )
}

fn not_today() -> Intent {
    Intent::AgentUpgrade {
        tab: "s-3e79".into(),
        to: "2.1.284".into(),
        word: UpgradeWord::NotToday,
    }
}

fn skip_version() -> Intent {
    Intent::AgentUpgrade {
        tab: "s-3e79".into(),
        to: "2.1.284".into(),
        word: UpgradeWord::Skip,
    }
}

fn upgrade_row() -> Message {
    Message::new(
        tags::HARNESS,
        Severity::Warn,
        "Couldn't upgrade Claude in tab 1 yet",
    )
    .key("harness.upgrade.s-3e79")
}

/// A standing `Warn` row whose only actions are snoozes is recorded rather than shown.
#[test]
fn a_standing_row_whose_only_capsules_decline_is_recorded_never_shown() {
    let now = t0();
    let mut c = fresh(now);
    let row46 = upgrade_row()
        .action(not_today())
        .action(skip_version())
        .now(sentence());
    let posted = c.post(row46, stamp(), now);
    assert!(
        c.live(posted.id).is_none(),
        "the two snoozes are not an action"
    );
    assert_eq!(c.demotions(), 1, "and the door says so");
    let rec = c.log().get(posted.id).expect("the words are in the log");
    assert_eq!(rec.severity, Severity::Warn, "Problems still lists it");
    assert_eq!(c.wanted_rows(), 0);

    let mut c = fresh(now);
    let bare = c.post(upgrade_row().now(sentence()), stamp(), now).id;
    assert!(c.live(bare).is_some(), "words alone are an action");
    assert_eq!(c.demotions(), 0);
    let mut c = fresh(now);
    let mover = Ask::standing(
        StandingWhy::OnlyYou(Reason::new("a switch only you can turn")),
        Then::press(
            Mover::open_system_pane("full-disk-access"),
            "Turn on aterm in Full Disk Access",
        ),
    );
    let beside = c
        .post(
            upgrade_row()
                .action(not_today())
                .action(skip_version())
                .now(mover),
            stamp(),
            now,
        )
        .id;
    assert!(
        c.live(beside).is_some(),
        "a decline may ride beside a real action"
    );
    assert_eq!(c.demotions(), 0);
}

#[test]
fn is_snooze_names_exactly_the_snoozes() {
    let declines = [
        Intent::Details,
        Intent::NotNow {
            decision: Decision::FileAccess,
        },
        not_today(),
        skip_version(),
    ];
    for d in &declines {
        assert!(d.is_snooze(), "{d:?}");
    }
    let actions = [
        Intent::OpenSettings {
            route: "/updates".into(),
        },
        Intent::OpenConfigEditor { line: None },
        Intent::OpenPath { path: "/x".into() },
        Intent::OpenSystemPane {
            pane: "full-disk-access".into(),
        },
        Intent::OpenLink {
            url: "https://chatgpt.com/".into(),
        },
        Intent::ApplyUpdate { build: 7 },
        Intent::NewWindow,
        Intent::StopPaste { session: 1 },
        Intent::ShowTab { tab: 1, window: 1 },
        Intent::AgentUpgrade {
            tab: "s-1".into(),
            to: "2".into(),
            word: UpgradeWord::Now,
        },
        Intent::EndRecovered,
        Intent::ReviewVaultSync,
    ];
    for a in &actions {
        assert!(!a.is_snooze(), "{a:?}");
    }
}

/// A `Hold::Live` row at `Warn`+ or via `.working()` requires a progress indicator.
#[test]
fn a_blocked_live_row_at_warn_is_a_record_and_work_needs_its_indicator() {
    let now = t0();
    let live = Hold::Live {
        stale_after: Duration::from_secs(3600),
    };
    let mut c = fresh(now);
    let blocked = Message::new(tags::UPDATE, Severity::Warn, "Update waits for you")
        .key("update.block")
        .hold(live);
    let id = c.post(blocked, stamp(), now).id;
    assert!(
        c.live(id).is_none(),
        "a Warn Live row with no indicator is a record"
    );
    assert_eq!(c.demotions(), 1);

    let mut c = fresh(now);
    let info = Message::new(tags::UPDATE, Severity::Info, "Waiting on the editor")
        .key("update.block")
        .hold(live);
    let id = c.post(info, stamp(), now).id;
    assert!(c.live(id).is_some(), "Info is not a claim on the owner");
    assert_eq!(c.demotions(), 0);

    let mut c = fresh(now);
    let bare = Message::new(tags::UPDATE, Severity::Info, "Working")
        .key("w")
        .working(ms(60_000));
    let id = c.post(bare, stamp(), now).id;
    assert!(
        c.live(id).is_none(),
        "`working` without an indicator is a record"
    );
    assert_eq!(c.demotions(), 1);

    let mut c = fresh(now);
    let ok = Message::new(tags::UPDATE, Severity::Warn, "Working")
        .key("w")
        .working(ms(60_000))
        .in_flight();
    let id = c.post(ok, stamp(), now).id;
    assert!(c.live(id).is_some());
    let fill = Message::new(tags::UPDATE, Severity::Info, "Fill")
        .key("f")
        .working(ms(60_000))
        .meter(Meter {
            fill_permille: Some(500),
            ..Meter::default()
        });
    let id = c.post(fill, stamp(), now).id;
    assert!(c.live(id).is_some());
    assert_eq!(c.demotions(), 0);
}

/// A standing ask resolved within `PROGRESS_GRACE` is never revealed.
#[test]
fn a_standing_ask_waits_out_the_progress_grace() {
    let now = t0();
    let mut c = fresh(now);
    let msg = Message::new(tags::HARNESS, Severity::Warn, "Claude stopped in tab 3")
        .key("harness.upgrade.s-3")
        .now(sentence());
    assert_eq!(msg.reveal_after, Some(PROGRESS_GRACE));
    let id = c.post(msg, stamp(), now).id;
    assert_eq!(c.wanted_rows(), 0, "not yet");
    assert_eq!(c.commit_rows(now, 3), None);
    assert!(c.resolve(id, Outcome::Ok, now + ms(800)));
    let _ = c.settle(now + PROGRESS_GRACE + ms(1), true);
    assert_eq!(c.wanted_rows(), 0);
    assert_eq!(c.commit_rows(now + PROGRESS_GRACE + ms(1), 3), None);

    let msg = Message::new(tags::HARNESS, Severity::Warn, "Claude stopped in tab 4")
        .key("harness.upgrade.s-4")
        .now(sentence());
    let id = c.post(msg, stamp(), now + ms(10_000)).id;
    let _ = c.settle(now + ms(10_000) + PROGRESS_GRACE, true);
    assert_eq!(c.wanted_rows(), 1);
    assert!(c.live(id).is_some_and(|l| l.revealed));
}

/// Classified decision rows keep a 10-minute hold; unclassified rows are capped at `MAX_TRANSIENT`.
#[test]
fn a_decision_row_keeps_its_ten_minutes_and_an_unclassified_one_is_cut() {
    let now = t0();
    let mut c = fresh(now);
    let posture = Message::new(tags::UPDATE, Severity::Warn, "Move aterm to Applications")
        .key("packages.posture")
        .now(Ask::flash(
            Why::OnlyYou(Reason::new(
                "aterm runs from the disk image and cannot update itself",
            )),
            Then::step("Drag aterm into Applications"),
            Span::Ten,
        ));
    let id = c.post(posture, stamp(), now).id;
    assert_eq!(
        c.live(id).unwrap().msg.hold,
        Hold::For(Duration::from_secs(600))
    );
    assert_eq!(c.demotions(), 0);

    let question = Message::new(tags::PRIVACY, Severity::Info, "Allow file access?")
        .key("privacy.ask")
        .now(Ask::question(
            Reason::new("aterm cannot read this folder until you say yes"),
            Then::press(Mover::open_system_pane("full-disk-access"), "Turn on aterm"),
        ));
    let id = c.post(question, stamp(), now).id;
    assert_eq!(
        c.live(id).unwrap().msg.hold,
        Hold::asking(Duration::from_secs(600)),
        "a question is a decision row"
    );
    assert!(c.mark_seen(id, now));
    assert!(
        c.live(id).is_some(),
        "reading a question does not answer it: only Not now, a supersede or its ten minutes do"
    );
    assert_eq!(c.demotions(), 0);

    let by_hand = Message::new(tags::UPDATE, Severity::Warn, "Move aterm to Applications")
        .key("packages.posture2")
        .hold(Hold::For(Duration::from_secs(600)));
    let id = c.post(by_hand, stamp(), now).id;
    assert_eq!(
        c.live(id).unwrap().msg.hold,
        Hold::For(crate::need::MAX_TRANSIENT),
        "cut to the longest a row may hold"
    );
    assert_eq!(c.demotions(), 1);
    assert_eq!(c.last_demoted(), Some("packages.posture2"));
}
