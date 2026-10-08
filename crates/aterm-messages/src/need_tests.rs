// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the rule at the door ([`crate::need`]) and the digest
//! ([`crate::digest`]) — what each lock makes unwritable, that the digest can
//! never seat a band row, that a dismissal holds across a restart, and that an
//! exception survives one.

use crate::center::{MessageCenter, Outcome, PostOutcome};
use crate::log::{LogLine, LogState, MessageLog, Retired};
use crate::model::{
    ActionIndex, Hold, Intent, Message, MessageId, Severity, UpgradeWord, WallStamp, tags,
};
use crate::need::{
    Ask, DIGEST_CAP, DIGEST_EXPIRY, Fingerprint, Horizon, LaterAsk, MAX_TRANSIENT, Mover, Need,
    NoticeKind, RESTATE_QUIET, Reason, Resurface, Span, StandingWhy, Then, Why,
};
use crate::{Duration, Instant};

const DAY_MS: u64 = 86_400_000;

fn t0() -> Instant {
    Instant::now()
}

fn stamp(day_ms: u64) -> WallStamp {
    WallStamp {
        unix_ms: 1_790_000_000_000 + day_ms,
    }
}

fn fresh(now: Instant) -> MessageCenter {
    MessageCenter::new(MessageLog::empty(), now)
}

fn fda_ask() -> LaterAsk {
    LaterAsk::person(
        Why::Question(Reason::new(
            "macOS keeps asking: Allow covers one program only",
        )),
        Then::press(
            Mover::open_system_pane("full-disk-access"),
            "Turn on aterm in Full Disk Access",
        ),
        Fingerprint::new("privacy", "fda", "denied"),
        Resurface::CHOICE,
    )
}

fn fda() -> Message {
    Message::new(
        tags::PRIVACY,
        Severity::Info,
        "macOS keeps asking for access",
    )
    .later(fda_ask())
}

fn own_work_ask(tab: &str) -> LaterAsk {
    LaterAsk::outlasted(
        Reason::new("its own background work holds the move"),
        Horizon::OWN_WORK,
        Duration::from_secs(25 * 3_600),
        Then::step("Stop that work in the tab when it suits you"),
        Fingerprint::new("harness.upgrade", tab, "own-work"),
        Resurface::WAIT,
    )
    .expect("25 h outlasts a 24 h horizon")
}

fn own_work(tab: &str) -> Message {
    Message::new(
        tags::HARNESS,
        Severity::Warn,
        format!("Claude in {tab} is behind"),
    )
    .later(own_work_ask(tab))
}

fn flash() -> Message {
    Message::new(tags::CONFIG, Severity::Warn, "aterm.toml has a bad key").now(Ask::flash(
        Why::YourAction(Reason::new("you typed a key aterm does not know")),
        Then::press(Mover::open_config_editor(None), "Fix the key in aterm.toml"),
        Span::Warn,
    ))
}

fn agent_down() -> Message {
    Message::new(tags::HARNESS, Severity::Warn, "Claude stopped in tab 1")
        .key("harness.upgrade.s-1")
        .now(Ask::standing(
            StandingWhy::Spent {
                tried: Reason::new("asked it to resume"),
                reason: Reason::new("it did not come back"),
            },
            Then::step("Type claude --resume in the tab"),
        ))
}

fn working(title: &str) -> Message {
    Message::new(tags::UPDATE, Severity::Info, title)
        .working(Duration::from_secs(60))
        .in_flight()
        .key("update.progress")
}

// ---- the locks the types make -------------------------------------------------

/// An `Ask` derives its hold; the reporter never chooses one.
#[test]
fn an_ask_derives_its_hold_and_leads_with_the_reason_and_the_action() {
    let m = flash();
    assert_eq!(m.hold, Hold::For(Span::Warn.duration()));
    assert!(!m.persists());
    assert_eq!(
        m.detail[0], "you typed a key aterm does not know",
        "the band's excerpt IS the reason"
    );
    assert_eq!(
        m.detail[1], "Fix the key in aterm.toml",
        "Details opens on the action"
    );
    assert_eq!(
        m.actions,
        vec![Intent::OpenConfigEditor { line: None }],
        "the ask's button is the first capsule"
    );

    let s = agent_down();
    assert!(s.persists() && s.hold.is_standing());
    assert!(s.detail[0].starts_with("aterm asked it to resume; it did not come back"));
    assert_eq!(s.need().class().as_str(), "now");
}

/// Only whitelisted movers exist: none of them is a decline, a snooze, a
/// navigation to a tab or the log, or `Details`.
#[test]
fn no_mover_is_a_decline_or_a_snooze() {
    let movers = [
        Mover::open_settings("/updates"),
        Mover::open_system_pane("full-disk-access"),
        Mover::open_config_editor(Some(3)),
        Mover::apply_update(7),
        Mover::new_window(),
        Mover::upgrade_now("s-1", "2.1.284"),
    ];
    for m in &movers {
        let i = m.intent();
        assert!(!i.closes_row() && !i.is_ask(), "{i:?} declines");
        assert_ne!(*i, Intent::Details);
        assert!(
            !matches!(
                i,
                Intent::AgentUpgrade {
                    word: UpgradeWord::NotToday | UpgradeWord::Skip,
                    ..
                } | Intent::ShowTab { .. }
                    | Intent::OpenPath { .. }
                    | Intent::StopPaste { .. }
            ),
            "{i:?} is a snooze or a navigation, not the one action"
        );
    }
}

/// `LaterAsk::outlasted` is `None` until the fact has gone on for its named
/// horizon: the only way to show a self-healing fact is to wait it out.
#[test]
fn a_wait_is_not_an_exception_until_it_outlasts_its_horizon() {
    let mk = |age_h: u64| {
        LaterAsk::outlasted(
            Reason::new("its own work holds the move"),
            Horizon::OWN_WORK,
            Duration::from_secs(age_h * 3_600),
            Then::step("Stop that work in the tab"),
            Fingerprint::new("harness.upgrade", "s-1", "own-work"),
            Resurface::WAIT,
        )
    };
    assert!(mk(0).is_none() && mk(23).is_none());
    let ask = mk(24).expect("24 h reaches the horizon");
    assert_eq!(
        ask.why_words(),
        "its own work holds the move (for over 1 d)",
        "the reason says how long, in the horizon's words"
    );
}

/// A fingerprint is a key: held to the key cap, a long subject shortened by a
/// stable hash, two long subjects that differ at the end still differ.
#[test]
fn a_fingerprint_fits_the_key_cap_and_stays_distinct() {
    let long_a = "s-3e79b5bde172ad0088ad".repeat(3);
    let long_b = format!("{}X", "s-3e79b5bde172ad0088ad".repeat(3));
    let a = Fingerprint::new("harness.upgrade", &long_a, "own-work");
    let b = Fingerprint::new("harness.upgrade", &long_b, "own-work");
    assert!(
        a.as_str().chars().count() <= crate::KEY_CAP,
        "{}",
        a.as_str()
    );
    assert_ne!(a, b, "the hash keeps two long subjects apart");
    assert_eq!(
        a,
        Fingerprint::new("harness.upgrade", &long_a, "own-work"),
        "stable"
    );
    let short = Fingerprint::new("harness.upgrade", "s-3e79b5bde172ad0088ad", "own-work");
    assert_eq!(
        short.as_str(),
        "harness.upgrade.s-3e79b5bde172ad0088ad.own-work"
    );
    assert_eq!(short.as_str().chars().count(), 47);
}

// ---- the door -----------------------------------------------------------------

/// An UNCLASSIFIED hold past two minutes is cut to two minutes and counted as
/// a defect of its builder (`last_demoted` names it): only a class earns a
/// longer one (`Ask::flash(.., Span::Ten)`, the posture row).
#[test]
fn an_unclassified_hold_past_two_minutes_is_cut_and_named() {
    let now = t0();
    let mut c = fresh(now);
    let id = c
        .post(
            Message::new(tags::SYSTEM, Severity::Warn, "long")
                .key("packages.posture")
                .hold(Hold::For(Duration::from_secs(600))),
            stamp(0),
            now,
        )
        .id;
    assert_eq!(
        c.live(id).unwrap().msg.hold,
        Hold::For(MAX_TRANSIENT),
        "cut to the longest a row may hold"
    );
    assert_eq!(c.demotions(), 1);
    assert_eq!(c.last_demoted(), Some("packages.posture"));
}

/// A persistent hold cannot be written by hand outside the crate (the seal);
/// one that reaches the door on an UNCLASSIFIED message anyway — a token
/// copied from another message's `hold` — stands on nothing: a record, counted
/// and named. A CLASSIFIED row cannot stand on declines either: the door
/// records it.
#[test]
fn a_persistent_hold_copied_from_another_message_is_a_record() {
    let now = t0();
    let mut c = fresh(now);
    let earned = Ask::standing(
        StandingWhy::OnlyYou(Reason::new("its own work holds the move")),
        Then::step("Stop that work in the tab"),
    )
    .hold();
    assert!(earned.persists());
    let row = || {
        let mut m = Message::new(
            tags::HARNESS,
            Severity::Warn,
            "Couldn't upgrade Claude in tab 1 yet",
        )
        .key("harness.upgrade.s-1")
        .action(Intent::AgentUpgrade {
            word: UpgradeWord::NotToday,
            tab: "s-1".into(),
            to: "2.1.284".into(),
        })
        .action(Intent::AgentUpgrade {
            word: UpgradeWord::Skip,
            tab: "s-1".into(),
            to: "2.1.284".into(),
        });
        // The field is public, so the copy is possible: the door is the lock.
        m.hold = earned;
        m
    };
    let posted = c.post(row(), stamp(0), now);
    assert!(
        c.live(posted.id).is_none(),
        "nothing stands on a copied token"
    );
    assert_eq!(c.demotions(), 1);
    assert_eq!(c.last_demoted(), Some("harness.upgrade.s-1"));
    assert_eq!(c.log().get(posted.id).unwrap().severity, Severity::Warn);
    // The same words CLASSIFIED (a standing ask with only snoozes for
    // capsules) are a record too: the classified door is strict.
    let classified = row().now(Ask::standing(
        StandingWhy::OnlyYou(Reason::new("its own work holds the move")),
        Then::step("Stop that work in the tab"),
    ));
    let mut d = fresh(now);
    let p = d.post(classified, stamp(0), now);
    assert!(d.live(p.id).is_none());
    assert_eq!(d.demotions(), 1);
}

/// A carried row from an older parent had no need: the parent admitted it, so
/// the successor adopts it (and counts no demotion).
#[test]
fn a_carried_standing_row_is_adopted_not_demoted() {
    let now = t0();
    let mut parent = fresh(now);
    let id = parent.post(agent_down(), stamp(0), now).id;
    // A standing row is born late (`reveal_after(PROGRESS_GRACE)`): a parent
    // hands the successor rows that are on its glass.
    let _ = parent.settle(now + crate::PROGRESS_GRACE, true);
    let mut carry = parent.carried();
    assert_eq!(carry.live.len(), 1);
    assert_eq!(carry.live[0].hold, "standing");
    // The successor's decoder has no `need` field to read (an older parent's
    // carry): the row arrives Unclassified + Carried.
    carry.live[0].id = id.raw();
    let mut child = fresh(now);
    child.seed_carried(&carry, stamp(1), now);
    assert_eq!(child.demotions(), 0);
    let live = child.live(id).expect("adopted");
    assert!(live.msg.hold.is_standing());
    assert!(matches!(live.msg.need(), Need::Now(_)));
}

/// A restatement may shorten a hold or end it; it cannot stretch a transient
/// row past its class's span.
#[test]
fn a_restatement_cannot_stretch_a_transient_row() {
    let now = t0();
    let mut c = fresh(now);
    let id = c.post(flash(), stamp(0), now).id;
    let r = crate::model::Restatement {
        hold: Some(Hold::For(Duration::from_secs(86_400))),
        ..Default::default()
    };
    assert!(c.restate(id, r, now));
    assert_eq!(
        c.live(id).unwrap().msg.hold,
        Hold::For(Span::Ten.duration()),
        "a decision may be asked for ten minutes, no more"
    );
    assert_eq!(c.demotions(), 1);
    let u = c
        .post(
            Message::new(tags::SYSTEM, Severity::Warn, "plain"),
            stamp(0),
            now,
        )
        .id;
    let r = crate::model::Restatement {
        hold: Some(Hold::For(Duration::from_secs(86_400))),
        ..Default::default()
    };
    assert!(c.restate(u, r, now));
    assert_eq!(
        c.live(u).unwrap().msg.hold,
        Hold::For(crate::need::MAX_TRANSIENT),
        "an UNCLASSIFIED row restates to no more than the door lets it hold"
    );
    assert_eq!(c.demotions(), 2, "and the cut is counted");
}

/// A `Now` whose reason or action says nothing is a record.
#[test]
fn an_ask_that_says_nothing_is_a_record() {
    let now = t0();
    let mut c = fresh(now);
    let m = Message::new(tags::SYSTEM, Severity::Warn, "Something happened").now(Ask::flash(
        Why::OnlyYou(Reason::new("")),
        Then::step(""),
        Span::Warn,
    ));
    let id = c.post(m, stamp(0), now).id;
    assert!(c.live(id).is_none());
    assert_eq!(c.demotions(), 1);
}

/// The notice kinds are the closed list; each earns its own span.
#[test]
fn a_notice_earns_its_kinds_span() {
    assert_eq!(NoticeKind::LastRun.span(), MAX_TRANSIENT);
    assert_eq!(NoticeKind::Script.span(), MAX_TRANSIENT);
    assert_eq!(
        NoticeKind::Answer.span(),
        MAX_TRANSIENT,
        "two minutes; then the log"
    );
    let m = Message::new(tags::CRASH, Severity::Error, "aterm crashed last time")
        .notice(NoticeKind::LastRun);
    assert_eq!(m.hold, Hold::For(MAX_TRANSIENT));
}

// ---- the digest never seats ------------------------------------------------------

/// An exception asks for no row, moves no band revision and arms no timer.
#[test]
fn a_later_never_wants_a_row() {
    let now = t0();
    let mut c = fresh(now);
    let before = (c.wanted_rows(), c.committed_rows(), c.revision());
    let posted = c.post(fda(), stamp(0), now);
    assert_eq!(posted.outcome, PostOutcome::New);
    assert_eq!(
        (c.wanted_rows(), c.committed_rows(), c.revision()),
        before,
        "the band cannot see it"
    );
    assert!(c.live(posted.id).is_none());
    assert_eq!(c.deadline(true), None, "no timer for an entry");
    assert_eq!(c.digest().len(), 1);
    assert_eq!(c.commit_rows(now + Duration::from_secs(10), 30), None);
    // the digest's own revision moved; nothing else did
    assert!(c.digest_revision() > 0);
}

/// The metamorphic property, over random operation sequences: two centers see
/// the same operations, but only one is also posted exceptions and dismissals.
/// The band-facing state never differs by a single row.
#[test]
fn the_digest_is_invisible_to_the_band() {
    let mut rng = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move |n: u64| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng % n
    };
    for seed_round in 0..40 {
        let base = t0();
        let mut with = fresh(base);
        let mut without = fresh(base);
        let mut ids_with: Vec<MessageId> = Vec::new();
        let mut ids_without: Vec<MessageId> = Vec::new();
        let mut clock = 0u64;
        for step in 0..300 {
            clock += next(3_000);
            let now = base + Duration::from_millis(clock);
            let wall = stamp(clock);
            match next(12) {
                0 | 1 => {
                    let title = format!("Working {step}");
                    ids_with.push(with.post(working(&title), wall, now).id);
                    ids_without.push(without.post(working(&title), wall, now).id);
                }
                2 => {
                    ids_with.push(with.post(flash(), wall, now).id);
                    ids_without.push(without.post(flash(), wall, now).id);
                }
                3 => {
                    ids_with.push(with.post(agent_down(), wall, now).id);
                    ids_without.push(without.post(agent_down(), wall, now).id);
                }
                4 => {
                    let title = format!("Plain {step}");
                    let m = || Message::new(tags::SYSTEM, Severity::Info, &title);
                    ids_with.push(with.post(m(), wall, now).id);
                    ids_without.push(without.post(m(), wall, now).id);
                }
                5 if !ids_with.is_empty() => {
                    let k = next(ids_with.len() as u64) as usize;
                    let r = || crate::model::Restatement {
                        title: Some(format!("Restated {step}")),
                        ..Default::default()
                    };
                    with.restate(ids_with[k], r(), now);
                    without.restate(ids_without[k], r(), now);
                }
                6 if !ids_with.is_empty() => {
                    let k = next(ids_with.len() as u64) as usize;
                    with.resolve(ids_with[k], Outcome::Ok, now);
                    without.resolve(ids_without[k], Outcome::Ok, now);
                }
                7 => {
                    let a = with.settle(now, true).glass_changed;
                    let b = without.settle(now, true).glass_changed;
                    assert_eq!(a, b, "round {seed_round} step {step}: settle");
                }
                8 => {
                    let afford = 2 + next(28) as u16;
                    assert_eq!(
                        with.commit_rows(now, afford),
                        without.commit_rows(now, afford),
                        "round {seed_round} step {step}: commit"
                    );
                }
                // The exceptions: only `with` sees them.
                9 => {
                    let tab = format!("tab {}", next(20));
                    with.post(own_work(&tab), wall, now);
                }
                10 => {
                    with.post(fda(), wall, now);
                }
                _ => match next(4) {
                    0 => {
                        with.file_digest(wall, now);
                    }
                    1 => {
                        if let Some(e) = with.digest().entries.first() {
                            with.file_digest_one(e.id, wall, now);
                        }
                    }
                    2 => {
                        with.settle_digest(stamp(clock + 15 * DAY_MS), now);
                    }
                    _ => {
                        if let Some(e) = with.digest().entries.first() {
                            with.act_digest(e.id, ActionIndex(0), wall, now);
                        }
                    }
                },
            }
            assert_eq!(
                with.wanted_rows(),
                without.wanted_rows(),
                "round {seed_round} step {step}: wanted_rows"
            );
            assert_eq!(
                with.committed_rows(),
                without.committed_rows(),
                "round {seed_round} step {step}: committed_rows"
            );
            assert_eq!(
                with.revision(),
                without.revision(),
                "round {seed_round} step {step}: the band's revision"
            );
            let titles = |c: &MessageCenter| -> Vec<String> {
                c.on_glass().map(|l| l.msg.title.clone()).collect()
            };
            assert_eq!(
                titles(&with),
                titles(&without),
                "round {seed_round} step {step}: glass"
            );
        }
    }
}

/// Negative control for the sweep above: a `Later` that WERE seated (posted as
/// the plain warning a broken router would make of it) is caught by the same
/// comparison on the very first step.
#[test]
fn a_seated_exception_would_be_seen() {
    let now = t0();
    let mut with = fresh(now);
    let mut without = fresh(now);
    let seated = Message::new(tags::HARNESS, Severity::Warn, "Claude in tab 1 is behind");
    with.post(seated, stamp(0), now);
    assert_ne!(with.wanted_rows(), without.wanted_rows());
    assert_ne!(with.revision(), without.revision());
    without.post(fda(), stamp(0), now);
    assert_eq!(
        without.wanted_rows(),
        0,
        "while the digest entry seats nothing"
    );
}

// ---- posting, dismissing, quiet ---------------------------------------------------

/// Same fact, same words: a duplicate. Same fact, new words: restated in
/// place (same entry, same place in the order). A new fact: a new entry.
#[test]
fn an_exception_is_deduplicated_restated_or_new() {
    let now = t0();
    let mut c = fresh(now);
    let a = c.post(own_work("tab 1"), stamp(0), now);
    assert_eq!(a.outcome, PostOutcome::New);
    let again = c.post(own_work("tab 1"), stamp(1_000), now);
    assert_eq!((again.id, again.outcome), (a.id, PostOutcome::Duplicate));
    assert_eq!(c.digest().len(), 1);
    let rev = c.digest_revision();
    let changed = Message::new(tags::HARNESS, Severity::Warn, "Claude in tab 1 is behind")
        .line("held by pid 3109 (zsh, 9m)")
        .later(own_work_ask("tab 1"));
    // Words alone restate an entry at most once per RESTATE_QUIET (a holder
    // count moves every look): inside it the change is a duplicate.
    let early = c.post(changed.clone(), stamp(2_000), now + Duration::from_secs(60));
    assert_eq!((early.id, early.outcome), (a.id, PostOutcome::Duplicate));
    let r = c.post(changed, stamp(2_000), now + RESTATE_QUIET);
    assert_eq!((r.id, r.outcome), (a.id, PostOutcome::Restated));
    assert!(
        c.digest_revision() > rev,
        "the words changed: observers republish"
    );
    assert_eq!(
        c.digest().entries[0].since,
        stamp(0),
        "the order never reshuffles"
    );
    let other = c.post(own_work("tab 3"), stamp(3_000), now);
    assert_eq!(other.outcome, PostOutcome::New);
    assert_eq!(c.digest().len(), 2);
    assert_eq!(c.digest().entries[0].id, a.id, "longest waiting first");
}

/// Worst first, then longest waiting.
#[test]
fn the_digest_reads_worst_first_then_oldest() {
    let now = t0();
    let mut c = fresh(now);
    let info = c.post(fda(), stamp(0), now).id;
    let warn_old = c.post(own_work("tab 1"), stamp(1), now).id;
    let warn_new = c.post(own_work("tab 2"), stamp(2), now).id;
    let ids: Vec<_> = c.digest().entries.iter().map(|e| e.id).collect();
    assert_eq!(ids, vec![warn_old, warn_new, info]);
    assert_eq!(c.digest().worst(), Some(Severity::Warn));
}

/// Dismiss all: one press files every entry at one wall stamp, each with its
/// own retirement line and its final words; the facts are quiet after it.
#[test]
fn dismiss_all_files_every_entry_to_the_log_in_one_batch() {
    let now = t0();
    let mut c = fresh(now);
    let a = c.post(own_work("tab 1"), stamp(0), now).id;
    let b = c.post(fda(), stamp(1), now).id;
    let n = c.file_digest(stamp(5_000), now);
    assert_eq!(n, 2);
    assert!(c.digest().is_empty());
    for id in [a, b] {
        let rec = c.log().get(id).unwrap();
        assert_eq!(rec.state, LogState::Retired(Retired::Dismissed));
        assert_eq!(
            rec.retired_unix_ms,
            Some(stamp(5_000).unix_ms),
            "one batch, one stamp"
        );
        assert!(!rec.title.is_empty(), "the final words are kept");
    }
    // both facts are quiet
    let again = c.post(own_work("tab 1"), stamp(6_000), now);
    assert_eq!(again.outcome, PostOutcome::Suppressed);
    assert_eq!(again.id, a, "the record that made it quiet");
    assert_eq!(
        c.post(fda(), stamp(6_000), now).outcome,
        PostOutcome::Suppressed
    );
    assert!(c.digest().is_empty());
}

/// A dismissal holds for the fact's resurface span, then the fact may be
/// shown again; a changed cause is a different fact and is never suppressed.
#[test]
fn a_dismissal_holds_for_its_span_and_a_new_cause_is_news() {
    let now = t0();
    let mut c = fresh(now);
    c.post(own_work("tab 1"), stamp(0), now);
    c.file_digest(stamp(1_000), now);
    let inside = c.post(own_work("tab 1"), stamp(6 * DAY_MS), now);
    assert_eq!(inside.outcome, PostOutcome::Suppressed, "7 days for a wait");
    let after = c.post(own_work("tab 1"), stamp(8 * DAY_MS), now);
    assert_eq!(
        after.outcome,
        PostOutcome::New,
        "a persisting fact may return after its span"
    );
    // another cause for the same tab is a different fingerprint
    let draft = LaterAsk::outlasted(
        Reason::new("a typed draft nobody sent"),
        Horizon::OWN_WORK,
        Duration::from_secs(30 * 3_600),
        Then::step("Send or clear the draft in the tab"),
        Fingerprint::new("harness.upgrade", "tab 1", "draft"),
        Resurface::WAIT,
    )
    .unwrap();
    c.file_digest(stamp(9 * DAY_MS), now);
    let news = c.post(
        Message::new(tags::HARNESS, Severity::Warn, "Claude in tab 1 is behind").later(draft),
        stamp(9 * DAY_MS + 5),
        now,
    );
    assert_eq!(news.outcome, PostOutcome::New);
}

/// A fact that resolved (its cause ended) is not filed by the owner, so its
/// return is news, not a suppression.
#[test]
fn a_resolved_exception_is_not_quiet() {
    let now = t0();
    let mut c = fresh(now);
    c.post(own_work("tab 1"), stamp(0), now);
    let n = c.resolve_key_prefix("harness.upgrade.tab 1", Outcome::Ok, now);
    assert_eq!(n, 1);
    assert!(c.digest().is_empty());
    assert_eq!(
        c.post(own_work("tab 1"), stamp(1_000), now).outcome,
        PostOutcome::New
    );
}

/// The cap files the oldest lowest-severity entry, with its log line.
#[test]
fn the_cap_files_the_oldest_lowest_severity_entry() {
    let now = t0();
    let mut c = fresh(now);
    let info = c.post(fda(), stamp(0), now).id; // the lowest severity, oldest
    for i in 0..DIGEST_CAP {
        c.post(own_work(&format!("tab {i}")), stamp(10 + i as u64), now);
    }
    assert_eq!(c.digest().len(), DIGEST_CAP, "bounded");
    let rec = c.log().get(info).unwrap();
    assert_eq!(
        rec.state,
        LogState::Retired(Retired::Evicted),
        "filed, not dropped"
    );
}

/// An entry nobody looked at files itself after two weeks.
#[test]
fn an_entry_nobody_reviewed_files_itself_after_two_weeks() {
    let now = t0();
    let mut c = fresh(now);
    let id = c.post(own_work("tab 1"), stamp(0), now).id;
    assert!(!c.settle_digest(stamp(13 * DAY_MS), now));
    assert!(c.settle_digest(
        stamp(u64::try_from(DIGEST_EXPIRY.as_millis()).unwrap()),
        now
    ));
    assert_eq!(
        c.log().get(id).unwrap().state,
        LogState::Retired(Retired::Unseen)
    );
}

/// Pressing an entry's capsule logs the press; a decline that closes the row
/// answers the entry.
#[test]
fn a_capsule_press_on_an_entry_is_logged_and_a_decline_answers_it() {
    let now = t0();
    let mut c = fresh(now);
    let m = Message::new(
        tags::PRIVACY,
        Severity::Info,
        "macOS keeps asking for access",
    )
    .action(Intent::NotNow {
        decision: crate::model::Decision::FileAccess,
    })
    .later(fda_ask());
    let id = c.post(m, stamp(0), now).id;
    assert_eq!(
        c.digest().entries[0].actions[0].label(),
        "Open Settings",
        "the ask's button leads"
    );
    // The entry's ONE action, done: it is put away ANSWERED (not `Resolved`:
    // nobody has seen the cause end; not `Dismissed`: the owner acted).
    let open = c.act_digest(id, ActionIndex(0), stamp(1), now).unwrap();
    assert!(matches!(open, Intent::OpenSystemPane { .. }));
    assert!(c.digest().is_empty(), "the owner did what it asked");
    assert!(matches!(
        c.log().get(id).unwrap().state,
        LogState::Retired(Retired::Answered { ref label }) if label == "Open Settings"
    ));
    // ... and stays put away for its quiet span, exactly as a dismissal would.
    assert_eq!(
        c.post(
            Message::new(
                tags::PRIVACY,
                Severity::Info,
                "macOS keeps asking for access"
            )
            .later(fda_ask()),
            stamp(3),
            now
        )
        .outcome,
        PostOutcome::Suppressed
    );
}

/// A decline the entry carries answers it too, and a NAVIGATION beside the
/// action (`Show tab`) is neither: the entry stays until the cause or the owner
/// ends it.
#[test]
fn a_decline_answers_an_entry_and_a_navigation_leaves_it() {
    let now = t0();
    let mut c = fresh(now);
    let with_decline = Message::new(tags::PRIVACY, Severity::Info, "macOS keeps asking")
        .action(Intent::NotNow {
            decision: crate::model::Decision::FileAccess,
        })
        .later(fda_ask());
    let id = c.post(with_decline, stamp(0), now).id;
    let no = c.act_digest(id, ActionIndex(1), stamp(2), now).unwrap();
    assert!(no.closes_row());
    assert!(c.digest().is_empty());

    let mut c = fresh(now);
    let nav = Message::new(tags::HARNESS, Severity::Info, "Claude in tab 1 is behind")
        .action(Intent::ShowTab { tab: 1, window: 1 })
        .later(own_work_ask("tab 1"));
    let id = c.post(nav, stamp(0), now).id;
    let shown = c.act_digest(id, ActionIndex(0), stamp(1), now).unwrap();
    assert!(matches!(shown, Intent::ShowTab { .. }));
    assert_eq!(
        c.digest().len(),
        1,
        "a navigation leaves the entry until the cause or the owner ends it"
    );
}

/// One fact, one place: a row that takes over a key retires the entry, and an
/// entry that takes over a key retires the row.
#[test]
fn a_fact_lives_in_one_place() {
    let now = t0();
    let mut c = fresh(now);
    let e = c.post(own_work("s-1"), stamp(0), now).id;
    let key = c.digest().entries[0].key.clone();
    let row = Message::new(tags::HARNESS, Severity::Warn, "Claude stopped in tab 1")
        .key(&key)
        .now(Ask::standing(
            StandingWhy::OnlyYou(Reason::new("it needs a sign-in")),
            Then::step("Sign in in the tab"),
        ));
    let r = c.post(row, stamp(1), now).id;
    assert!(c.digest().is_empty(), "the entry moved to a row");
    assert_eq!(
        c.log().get(e).unwrap().state,
        LogState::Retired(Retired::Superseded { by: r })
    );
    let back = c.post(own_work("s-1"), stamp(2), now);
    assert_eq!(back.outcome, PostOutcome::New);
    assert!(c.live(r).is_none(), "the row was retired by the entry");
    assert_eq!(c.wanted_rows(), 0);
}

// ---- across a restart ---------------------------------------------------------------

fn restart(c: &mut MessageCenter, now: Instant) -> MessageCenter {
    let lines: Vec<String> = c
        .drain_new_for_persist()
        .iter()
        .map(LogLine::encode)
        .collect();
    let mut log = MessageLog::empty();
    log.replay_all(lines.iter().filter_map(|l| LogLine::decode(l).ok()));
    MessageCenter::new(log, now)
}

/// An entry open at quit is still open at launch (every other posted-and-never-
/// retired line loads as Stale), under its old id, and its reporter's first
/// look replaces the words in place.
#[test]
fn an_open_exception_survives_a_restart() {
    let now = t0();
    let mut c = fresh(now);
    let id = c.post(own_work("tab 1"), stamp(0), now).id;
    let mut next = restart(&mut c, now);
    assert!(
        next.digest().is_empty(),
        "not adopted until the host says so"
    );
    assert_eq!(next.adopt_digest(stamp(60_000), now), 1);
    let view = next.digest();
    assert_eq!(view.entries[0].id, id);
    assert_eq!(view.entries[0].key, "harness.upgrade.tab 1.own-work");
    assert_eq!(
        view.entries[0].why,
        "its own background work holds the move (for over 1 d)"
    );
    // the reporter's first look re-asserts it: same words -> a duplicate
    let again = next.post(own_work("tab 1"), stamp(120_000), now);
    assert_eq!((again.id, again.outcome), (id, PostOutcome::Duplicate));
    assert_eq!(next.wanted_rows(), 0);
}

/// An entry adopted after a restart carries a stand-in ask (no `Mover`); the
/// reporter's next look brings the real one, and the owner pressing the entry's
/// own button must still put it away ANSWERED, not leave it open.
#[test]
fn a_press_after_a_restart_answers_the_entry_on_the_reporters_ask() {
    let now = t0();
    let mut c = fresh(now);
    let id = c.post(fda(), stamp(0), now).id;
    let mut next = restart(&mut c, now);
    assert_eq!(next.adopt_digest(stamp(60_000), now), 1);
    let again = next.post(fda(), stamp(120_000), now);
    assert_eq!((again.id, again.outcome), (id, PostOutcome::Duplicate));
    let open = next
        .act_digest(id, ActionIndex(0), stamp(130_000), now)
        .unwrap();
    assert!(matches!(open, Intent::OpenSystemPane { .. }));
    assert!(next.digest().is_empty(), "the press answered the entry");
    assert!(matches!(
        next.log().get(id).unwrap().state,
        LogState::Retired(Retired::Answered { .. })
    ));
}

/// A dismissal stamped under a wall clock that ran years ahead (and was
/// corrected) does not keep the fact quiet until that future time.
#[test]
fn a_filing_from_a_clock_that_ran_ahead_keeps_nothing_quiet() {
    let now = t0();
    let mut c = fresh(now);
    c.post(fda(), stamp(0), now);
    c.file_digest(stamp(5 * 365 * DAY_MS), now);
    assert_eq!(
        c.post(fda(), stamp(DAY_MS), now).outcome,
        PostOutcome::New,
        "the filing is from the future of this clock: it is news again"
    );
}

/// A dismissal made before a restart still holds after it: the quiet span is
/// read from the replayed ring, with no state of its own.
#[test]
fn a_dismissal_holds_across_a_restart() {
    let now = t0();
    let mut c = fresh(now);
    c.post(fda(), stamp(0), now);
    c.file_digest(stamp(1_000), now);
    let mut next = restart(&mut c, now);
    assert_eq!(next.adopt_digest(stamp(2_000), now), 0, "nothing left open");
    assert_eq!(
        next.post(fda(), stamp(3_000), now).outcome,
        PostOutcome::Suppressed
    );
    assert_eq!(
        next.post(fda(), stamp(31 * DAY_MS), now).outcome,
        PostOutcome::New,
        "30 days for a fact about the owner's own choice"
    );
}

/// A decoded record of an exception is `need=later`; a record of anything else
/// carries no such key (an older build ignores the key it does not know).
#[test]
fn the_log_marks_only_exceptions() {
    let now = t0();
    let mut c = fresh(now);
    c.post(fda(), stamp(0), now);
    c.post(flash(), stamp(0), now);
    let lines: Vec<String> = c
        .drain_new_for_persist()
        .iter()
        .map(LogLine::encode)
        .collect();
    let posted: Vec<&String> = lines.iter().filter(|l| l.contains("kind=posted")).collect();
    assert_eq!(posted.len(), 2);
    assert_eq!(
        posted
            .iter()
            .filter(|l| l.contains("\tneed=later\t"))
            .count(),
        1
    );
    assert!(
        posted[0].contains("key=privacy.fda.denied"),
        "{}",
        posted[0]
    );
}

/// A live row and a record are what they were: the class helpers derive their
/// hold and none leaves the digest a hold.
#[test]
fn every_class_constructor_is_coherent() {
    let w = working("Downloading");
    assert!(matches!(w.hold, Hold::Live { .. }) && matches!(w.need(), Need::Working));
    let r = Message::new(tags::UPDATE, Severity::Info, "done").record();
    assert_eq!(
        (r.hold, r.need().class().as_str()),
        (Hold::LogOnly, "record")
    );
    let n = Message::new(tags::SYSTEM, Severity::Warn, "warned").notice(NoticeKind::Script);
    assert_eq!(n.hold, Hold::For(MAX_TRANSIENT));
    let l = fda();
    assert_eq!(
        (l.hold, l.need().class().as_str()),
        (Hold::LogOnly, "later")
    );
    assert_eq!(
        l.key.as_deref(),
        Some("privacy.fda.denied"),
        "the key IS the fingerprint"
    );
}

// ---- over the wire: `aterm ctl messages` / `notice dismiss` --------------------------

fn wire_rows(c: &MessageCenter, args: &str) -> Vec<String> {
    let q = crate::wire::parse_read_args(args).expect("a query");
    let enc = |s: &str| s.replace(' ', "%20");
    crate::wire::message_rows(c, &q, stamp(0).unix_ms + 10_000, &enc)
}

/// The introspection invariant: the digest is readable over the wire in the
/// same grammar as every other row — `state=review` while open, `need=later`
/// open and filed, the CURRENT words after a restatement — and `need=later`
/// selects exactly the entries.
#[test]
fn the_digest_is_readable_over_the_wire() {
    let now = t0();
    let mut c = fresh(now);
    let a = c.post(own_work("tab 1"), stamp(0), now).id;
    c.post(fda(), stamp(1), now);
    c.post(flash(), stamp(2), now);
    c.post(
        Message::new(tags::SYSTEM, Severity::Info, "plain"),
        stamp(3),
        now,
    );

    let later = wire_rows(&c, "need=later");
    assert_eq!(later.len(), 2, "{later:#?}");
    for row in &later {
        assert!(row.contains(" state=review glass=- "), "{row}");
        assert!(row.ends_with(" need=later"), "{row}");
    }
    let live = wire_rows(&c, "live");
    assert!(
        !live.iter().any(|r| r.contains("state=review")),
        "`live` is what is on or queued for the band: an open exception never is"
    );
    let open = wire_rows(&c, "need=later live");
    assert_eq!(
        open.len(),
        2,
        "`need=later live` is exactly the open entries"
    );
    assert!(
        live.iter()
            .any(|r| r.contains("aterm.toml%20has%20a%20bad%20key") && r.ends_with(" need=now")),
        "a typed live row names its class: {live:#?}"
    );
    assert!(
        live.iter()
            .any(|r| r.contains("title=plain") && !r.contains(" need=")),
        "an unclassified row says nothing"
    );

    // a restatement is never logged: the row reads the entry's CURRENT words
    let changed = Message::new(tags::HARNESS, Severity::Warn, "Claude in tab 1 is behind")
        .line("held by pid 3109 (zsh, 9m)")
        .later(own_work_ask("tab 1"));
    c.post(changed, stamp(4), now + RESTATE_QUIET);
    let row = wire_rows(&c, "need=later")
        .into_iter()
        .find(|r| r.starts_with(&format!("message {a} ")))
        .unwrap();
    assert!(row.contains("held%20by%20pid%203109"), "{row}");

    // filed entries stay `need=later`, with the way they left
    c.file_digest(stamp(9_000), now);
    let filed = wire_rows(&c, "need=later");
    assert_eq!(filed.len(), 2);
    for row in &filed {
        assert!(row.contains(" state=dismissed "), "{row}");
        assert!(
            row.ends_with(" need=later") || row.contains(" need=later "),
            "{row}"
        );
    }
}

/// `notice dismiss review` files them all; `notice dismiss <id>` files one.
#[test]
fn notice_dismiss_files_the_digest() {
    use crate::wire::{NoticeRequest, WireGate, apply};
    let now = t0();
    let mut c = fresh(now);
    let mut gate = WireGate::default();
    let a = c.post(own_work("tab 1"), stamp(0), now).id;
    c.post(fda(), stamp(1), now);
    let one = NoticeRequest::parse(&format!("dismiss {a}")).expect("parses");
    let r = apply(&mut c, &mut gate, one, stamp(5), now);
    assert_eq!(r.reply, format!("OK dismissed={a}"));
    assert_eq!(c.digest().len(), 1);
    let all = NoticeRequest::parse("dismiss review").expect("parses");
    assert_eq!(all, NoticeRequest::DismissReview);
    let r = apply(&mut c, &mut gate, all, stamp(6), now);
    assert_eq!(r.reply, "OK dismissed=review filed=1");
    assert!(c.digest().is_empty());
    let r = apply(
        &mut c,
        &mut gate,
        NoticeRequest::parse("dismiss 999").unwrap(),
        stamp(7),
        now,
    );
    assert!(
        r.reply.starts_with("ERR notice: no live message"),
        "{}",
        r.reply
    );
    assert!(NoticeRequest::parse("dismiss review please").is_err());
}

/// `aterm review` reads a REPLAYED log — no center, no window — and lists the
/// exceptions in the words the glass used: open ones first, then what was
/// filed, and nothing else in the log.
#[test]
fn the_exceptions_report_reads_a_replayed_log() {
    let now = t0();
    let mut c = fresh(now);
    c.post(own_work("tab 1"), stamp(0), now);
    c.post(fda(), stamp(1), now);
    c.post(flash(), stamp(2), now); // not an exception: never in the report
    c.file_digest_one(c.digest().entries[1].id, stamp(5_000), now);
    let lines: Vec<String> = c
        .drain_new_for_persist()
        .iter()
        .map(LogLine::encode)
        .collect();
    let mut log = MessageLog::empty();
    log.replay_all(lines.iter().filter_map(|l| LogLine::decode(l).ok()));

    let open = crate::exceptions_report(&log, crate::ReportScope::Open, 0, 0);
    assert_eq!(open[0], "To look at (1)");
    assert!(
        open.iter().any(|l| l.contains("Claude in tab 1 is behind")),
        "{open:#?}"
    );
    assert!(
        open.iter()
            .any(|l| l.trim_start().starts_with("why") && l.contains("background work"))
    );
    assert!(
        open.iter()
            .any(|l| l.trim_start().starts_with("then") && l.contains("Stop that work"))
    );
    assert!(!open.iter().any(|l| l.contains("macOS keeps asking")));
    assert!(
        !open.iter().any(|l| l.contains("bad key")),
        "a Now row is not an exception"
    );

    let all = crate::exceptions_report(&log, crate::ReportScope::All, 0, 0);
    let filed_at = all.iter().position(|l| l.starts_with("Filed")).unwrap();
    assert_eq!(all[filed_at], "Filed (1)");
    assert!(
        all[filed_at..]
            .iter()
            .any(|l| l.contains("macOS keeps asking") || l.contains("dismissed"))
    );
}

/// The report lists the open entries worst first, marking a warning, and a filed
/// entry whose cause ended says "fixed" in a person's words with no action left.
#[test]
fn the_exceptions_report_ranks_warnings_first_and_drops_a_fixed_entrys_action() {
    let now = t0();
    let mut c = fresh(now);
    c.post(fda(), stamp(0), now);
    c.post(own_work("tab 1"), stamp(1), now);
    // The file so far, replayed into a fresh log as a reader would.
    let mut file: Vec<String> = Vec::new();
    let mut replay = |c: &mut MessageCenter| {
        file.extend(c.drain_new_for_persist().iter().map(LogLine::encode));
        let mut log = MessageLog::empty();
        log.replay_all(file.iter().filter_map(|l| LogLine::decode(l).ok()));
        log
    };
    let log = replay(&mut c);
    let open = crate::exceptions_report(&log, crate::ReportScope::Open, 0, 0);
    let titles: Vec<&String> = open
        .iter()
        .filter(|l| l.starts_with("  ") && !l.starts_with("      "))
        .collect();
    assert!(
        titles[0].contains("warning: Claude in tab 1 is behind"),
        "{open:#?}"
    );
    assert!(
        titles[1].contains("macOS keeps asking") && !titles[1].contains("warning"),
        "{open:#?}"
    );

    assert_eq!(c.resolve_digest_prefix("privacy", Outcome::Ok, now), 1);
    let log = replay(&mut c);
    let filed = crate::exceptions_report(&log, crate::ReportScope::Filed, 0, 0);
    assert!(
        filed.iter().any(|l| l.contains("\u{b7} fixed")),
        "{filed:#?}"
    );
    assert!(
        !filed.iter().any(|l| l.trim_start().starts_with("then")),
        "{filed:#?}"
    );
    assert!(
        !filed.iter().any(|l| l.contains("resolved-ok")),
        "{filed:#?}"
    );

    // A cause that ended unfixed still stands: its action stays.
    let mut c = fresh(now);
    c.post(fda(), stamp(0), now);
    assert_eq!(c.resolve_digest_prefix("privacy", Outcome::Warn, now), 1);
    let mut log = MessageLog::empty();
    log.replay_all(
        c.drain_new_for_persist()
            .iter()
            .filter_map(|l| LogLine::decode(&l.encode()).ok()),
    );
    let filed = crate::exceptions_report(&log, crate::ReportScope::Filed, 0, 0);
    assert!(
        filed.iter().any(|l| l.contains("\u{b7} ended unfixed")),
        "{filed:#?}"
    );
    assert!(
        filed.iter().any(|l| l.trim_start().starts_with("then")),
        "{filed:#?}"
    );
}

/// A log read a few days later says WHEN: the time of today's entries bare, an
/// earlier day's with its date, on the reader's own clock (`offset_s`), so a
/// filing the owner opens a week on is not a bare `3:13 PM` of no day.
#[test]
fn the_exceptions_report_says_the_day_of_an_earlier_entry() {
    let now = t0();
    let mut c = fresh(now);
    // 2026-09-21 14:13:20 UTC, a Monday; the reader is one hour east.
    let at = stamp(0).unix_ms;
    c.post(own_work("tab 1"), stamp(0), now);
    c.file_digest(stamp(60_000), now);
    let lines: Vec<String> = c
        .drain_new_for_persist()
        .iter()
        .map(LogLine::encode)
        .collect();
    let mut log = MessageLog::empty();
    log.replay_all(lines.iter().filter_map(|l| LogLine::decode(l).ok()));
    let said = |now_ms: u64| {
        crate::exceptions_report(&log, crate::ReportScope::Filed, 3_600, now_ms)
            .into_iter()
            .find(|l| l.trim_start().starts_with("since"))
            .expect("a filed entry has a since line")
    };
    // The same day: the clock alone, in the reader's zone.
    assert_eq!(
        said(at + 3_600_000),
        "      since  3:13 PM \u{b7} dismissed 3:14 PM"
    );
    // The next day: the date words lead.
    assert!(
        said(at + DAY_MS).contains("Yesterday, 3:13 PM"),
        "{}",
        said(at + DAY_MS)
    );
    // A week on: the calendar date.
    assert!(
        said(at + 8 * DAY_MS).contains("Mon, 21 Sep, 3:13 PM"),
        "{}",
        said(at + 8 * DAY_MS)
    );
}

/// The default report lists the open entries and only the newest filings
/// (newest first) and says how many it left out; the whole history is `All`.
#[test]
fn the_latest_report_caps_the_filed_list_at_the_newest() {
    let now = t0();
    let mut c = fresh(now);
    for i in 0..12_u64 {
        let tab = format!("tab {i}");
        c.post(own_work(&tab), stamp(i * 1_000), now);
        c.file_digest(stamp(i * 1_000 + 500), now);
    }
    c.post(fda(), stamp(20_000), now);
    let lines: Vec<String> = c
        .drain_new_for_persist()
        .iter()
        .map(LogLine::encode)
        .collect();
    let mut log = MessageLog::empty();
    log.replay_all(lines.iter().filter_map(|l| LogLine::decode(l).ok()));
    let latest = crate::exceptions_report(&log, crate::ReportScope::Latest, 0, 0);
    assert_eq!(latest[0], "To look at (1)");
    let filed_at = latest.iter().position(|l| l.starts_with("Filed")).unwrap();
    assert_eq!(latest[filed_at], "Filed (12, the newest 10 shown)");
    let titles: Vec<&String> = latest[filed_at..]
        .iter()
        .filter(|l| l.starts_with("  ") && !l.starts_with("      "))
        .collect();
    assert_eq!(titles.len(), 10);
    assert!(
        titles[0].contains("tab 11"),
        "newest filing first: {titles:?}"
    );
    assert!(titles[9].contains("tab 2"), "{titles:?}");
    let all = crate::exceptions_report(&log, crate::ReportScope::All, 0, 0);
    assert!(all.iter().any(|l| l == "Filed (12)"), "{all:#?}");
}

// ---- a standing row is raised once ---------------------------------------------------

/// The reporters' own once-only code, in one place: a standing row the owner
/// read (it folded) is not raised again inside a day, and a return after the
/// span is news.
#[test]
fn a_standing_row_the_owner_read_is_not_raised_again_for_a_day() {
    let now = t0();
    let mut c = fresh(now);
    let id = c.post(agent_down(), stamp(0), now).id;
    assert!(c.live(id).is_some() && c.live(id).unwrap().msg.hold.is_standing());
    assert!(c.mark_seen(id, now), "a read folds a standing row");
    assert!(c.live(id).is_none());
    let again = c.post(agent_down(), stamp(3_600_000), now);
    assert_eq!((again.id, again.outcome), (id, PostOutcome::Suppressed));
    assert_eq!(c.wanted_rows(), 0, "nothing was seated");
    let next_day = c.post(agent_down(), stamp(DAY_MS + 1), now);
    assert_eq!(
        next_day.outcome,
        PostOutcome::New,
        "still down a day later: news"
    );
}

/// A standing row's fact that is NEW — the occurrence the owner read ended and
/// another began, with no live row between them to withdraw (round eight of
/// the update audit, the review of lead U38: a person's unsaved work holding an
/// update, read, saved, the update landed, and the next build held again) — is
/// news once the reporter says so (`end_standing_quiet`): the read no longer
/// keeps the key quiet, and it is on record as withdrawn. CONTROL: without the
/// word, the read keeps the next post quiet; with a live row, or with no read
/// to end, the call changes nothing.
#[test]
fn a_standing_row_whose_read_occurrence_ended_is_news_when_another_begins() {
    let now = t0();
    let mut c = fresh(now);
    assert!(
        !c.end_standing_quiet("harness.upgrade.s-1", stamp(0), now),
        "no word on the key: nothing to end"
    );
    let id = c.post(agent_down(), stamp(0), now).id;
    assert!(
        !c.end_standing_quiet("harness.upgrade.s-1", stamp(1), now),
        "a live row keeps its own quiet"
    );
    assert!(c.mark_seen(id, now));
    assert_eq!(
        c.post(agent_down(), stamp(1_000), now).outcome,
        PostOutcome::Suppressed,
        "PRECONDITION: the read keeps the same occurrence quiet"
    );
    assert!(c.end_standing_quiet("harness.upgrade.s-1", stamp(2_000), now));
    assert!(matches!(
        c.log().get(id).and_then(|rec| rec.retired()),
        Some(Retired::Withdrawn)
    ));
    assert!(
        !c.end_standing_quiet("harness.upgrade.s-1", stamp(2_500), now),
        "said once: the latest word keeps no quiet now"
    );
    let next = c.post(agent_down(), stamp(3_000), now);
    assert_eq!(next.outcome, PostOutcome::New, "a new occurrence is news");
    assert_ne!(next.id, id);
}

/// A row whose cause ended (resolved or withdrawn) is not filed by the owner:
/// its return is news at once.
#[test]
fn a_standing_row_that_ended_is_news_when_it_returns() {
    let now = t0();
    let mut c = fresh(now);
    let id = c.post(agent_down(), stamp(0), now).id;
    assert!(c.resolve(id, Outcome::Ok, now));
    assert_eq!(
        c.post(agent_down(), stamp(1_000), now).outcome,
        PostOutcome::New
    );
}

/// A standing row with no key cannot be superseded, resolved or remembered:
/// the door records it.
#[test]
fn a_standing_row_without_a_key_is_a_record() {
    let now = t0();
    let mut c = fresh(now);
    let m = Message::new(tags::HARNESS, Severity::Warn, "Claude stopped").now(Ask::standing(
        StandingWhy::OnlyYou(Reason::new("it needs a sign-in")),
        Then::step("Sign in in the tab"),
    ));
    let id = c.post(m, stamp(0), now).id;
    assert!(c.live(id).is_none());
    assert_eq!(c.demotions(), 1);
}

// ---- the words that say nothing ---------------------------------------------------------

/// A snooze in words, two actions in a sentence, "it is waiting" as a reason:
/// none is a Need. The door records the message and counts it, so a roster of
/// every reporter's messages (each asserting `demotions() == 0`) fails on the
/// author who wrote one.
#[test]
fn a_snooze_a_pair_of_actions_or_a_wait_is_no_need() {
    let now = t0();
    let cases: [(&str, Why, &str); 7] = [
        (
            "empty reason",
            Why::OnlyYou(Reason::new("")),
            "Fix it in the tab",
        ),
        (
            "waiting",
            Why::OnlyYou(Reason::new("aterm is waiting for its work to end")),
            "Stop that work in the tab",
        ),
        (
            "retrying",
            Why::Spent {
                tried: Reason::new("tried to resume"),
                reason: Reason::new("it will retry in a minute"),
            },
            "Type claude --resume in the tab",
        ),
        (
            "a snooze",
            Why::OnlyYou(Reason::new("a build waits on you")),
            "Skip this build",
        ),
        (
            "a snooze 2",
            Why::OnlyYou(Reason::new("a build is stuck")),
            "Not today",
        ),
        (
            "two actions",
            Why::OnlyYou(Reason::new("its work holds the move")),
            "Stop that work in the tab or skip 2.1.284",
        ),
        (
            "two actions 2",
            Why::OnlyYou(Reason::new("its work holds the move")),
            "Stop that work; skip 2.1.284",
        ),
    ];
    for (name, why, then) in cases {
        let mut c = fresh(now);
        let m = Message::new(tags::HARNESS, Severity::Warn, "Claude in tab 1 is behind")
            .key("harness.upgrade.s-1")
            .now(Ask::standing(
                match why.clone() {
                    Why::OnlyYou(r) => StandingWhy::OnlyYou(r),
                    Why::Spent { tried, reason } => StandingWhy::Spent { tried, reason },
                    other => unreachable!("{other:?}"),
                },
                Then::step(then),
            ));
        let id = c.post(m, stamp(0), now).id;
        assert!(c.live(id).is_none(), "{name}: not seated");
        assert_eq!(c.demotions(), 1, "{name}: counted");
        assert_eq!(c.wanted_rows(), 0, "{name}");
    }
    // ...and the same words in a digest entry
    let mut c = fresh(now);
    let later = LaterAsk::person(
        Why::OnlyYou(Reason::new("a build waits on you")),
        Then::step("Dismiss it"),
        Fingerprint::new("x", "y", "z"),
        Resurface::WAIT,
    );
    c.post(
        Message::new(tags::HARNESS, Severity::Warn, "x").later(later),
        stamp(0),
        now,
    );
    assert!(c.digest().is_empty());
    assert_eq!(c.demotions(), 1);
    // the wordings this design ships pass
    for ask in [own_work_ask("tab 1"), fda_ask()] {
        assert_eq!(ask.fault(), None, "{ask:?}");
    }
}

// ---- the memory of a filing (review 2026-09-29, F4 / issue 9 / the back-off) ------------

fn unrelated(i: usize) -> Message {
    Message::new(tags::UPDATE, Severity::Info, format!("record {i}")).record()
}

/// A DISMISSAL IS REMEMBERED FOR ITS SPAN AFTER THE DISMISSAL, not for 512
/// records after the entry's POSTING. The entry stands while 505 unrelated
/// records arrive (an owner who has not looked at the menu bar for a while),
/// is dismissed, then 1,000 more arrive: the ring has turned over twice and the
/// fact is still quiet. A filing older than the longest quiet span (90 days)
/// is let go like any other record.
#[test]
fn a_dismissal_outlives_512_later_records() {
    let now = t0();
    let mut c = fresh(now);
    c.post(fda(), stamp(0), now);
    for i in 0..505 {
        c.post(unrelated(i), stamp(10 + i as u64), now);
    }
    assert_eq!(c.file_digest(stamp(1_000_000), now), 1);
    for i in 0..1_000 {
        c.post(unrelated(1_000 + i), stamp(1_000_200 + i as u64), now);
    }
    assert_eq!(c.log().len(), crate::LOG_CAP, "the ring has turned over");
    assert_eq!(
        c.post(fda(), stamp(2_000_000), now).outcome,
        PostOutcome::Suppressed,
        "a fact dismissed half an hour ago (span 30 days) is still quiet"
    );
    // ... and 181 days on, with the ring turning again, the filing is let go.
    for i in 0..600 {
        c.post(unrelated(3_000 + i), stamp(181 * DAY_MS + i as u64), now);
    }
    assert_eq!(
        c.post(fda(), stamp(181 * DAY_MS + 1_000), now).outcome,
        PostOutcome::New
    );
}

/// THE LATEST WORD IS THE LATEST BY TIME. A RECORD (or any other fact's
/// retirement) under an entry's key, posted after the entry opened and before
/// the owner dismissed it, sits later in the ring but is an OLDER word; it
/// must neither erase the dismissal nor count as one.
#[test]
fn a_record_under_the_key_never_erases_a_dismissal() {
    let now = t0();
    let key = "privacy.fda.denied";
    let record = || {
        Message::new(tags::PRIVACY, Severity::Info, "a wait, recorded")
            .key(key)
            .record()
    };
    // The record BEFORE the entry.
    let mut before = fresh(now);
    before.post(record(), stamp(0), now);
    before.post(fda(), stamp(1), now);
    assert_eq!(before.file_digest(stamp(2), now), 1);
    assert_eq!(
        before.post(fda(), stamp(3), now).outcome,
        PostOutcome::Suppressed
    );
    // The record BETWEEN the entry's posting and the dismissal.
    let mut between = fresh(now);
    between.post(fda(), stamp(1), now);
    between.post(record(), stamp(2), now);
    assert_eq!(between.file_digest(stamp(3), now), 1);
    assert_eq!(
        between.post(fda(), stamp(4), now).outcome,
        PostOutcome::Suppressed,
        "the dismissal is the latest word on the fact"
    );
    // A record AFTER the dismissal is another fact's word: still quiet.
    between.post(record(), stamp(5), now);
    assert_eq!(
        between.post(fda(), stamp(6), now).outcome,
        PostOutcome::Suppressed
    );
}

/// A fact the owner keeps putting away is one they have decided to live with:
/// a week, then four weeks, then 90 days — and it STAYS at 90 days while they
/// keep putting it away — never for good.
#[test]
fn a_fact_dismissed_again_backs_off() {
    let now = t0();
    let mut c = fresh(now);
    let at = |day: u64| stamp(day * DAY_MS + 1);
    assert_eq!(
        c.post(own_work("tab 1"), at(0), now).outcome,
        PostOutcome::New
    );
    c.file_digest(at(0), now);
    let mut day = 0u64;
    let mut quiet_days = Vec::new();
    for _ in 0..5 {
        // How many days until it is news again?
        let mut d = 1;
        while c.post(own_work("tab 1"), at(day + d), now).outcome == PostOutcome::Suppressed {
            d += 1;
        }
        quiet_days.push(d);
        day += d;
        c.file_digest(at(day), now);
    }
    assert_eq!(
        quiet_days,
        vec![7, 28, 90, 90, 90],
        "a week, a month, then three months"
    );
    assert_eq!(Resurface::WAIT.backed_off(1), 7 * DAY_MS);
    assert_eq!(Resurface::WAIT.backed_off(2), 28 * DAY_MS);
    assert_eq!(Resurface::WAIT.backed_off(3), 90 * DAY_MS);
    assert_eq!(Resurface::CHOICE.backed_off(1), 30 * DAY_MS);
    assert_eq!(Resurface::CHOICE.backed_off(2), 90 * DAY_MS);
    assert_eq!(Resurface::WAIT.backed_off(u32::MAX), 90 * DAY_MS);
}

/// The log the owner reads later must give the real time an entry ended. An
/// entry adopted after a restart is anchored at the wall clock of its
/// adoption, not at its old ingress stamp plus the minutes since: it stood
/// three days and must not read as ending five minutes after it began. Every
/// way an entry ends without a stamp of its own to hand (a later family look,
/// the reporter's clear, a key-prefix sweep) is held to it.
#[test]
fn an_adopted_entry_is_ended_with_the_real_wall_time() {
    let now = t0();
    let mut c = fresh(now);
    let ids: Vec<_> = ["tab 1", "tab 2", "tab 3"]
        .iter()
        .map(|t| c.post(own_work(t), stamp(0), now).id)
        .collect();
    let mut lines: Vec<String> = c
        .drain_new_for_persist()
        .iter()
        .map(LogLine::encode)
        .collect();
    let mut log = MessageLog::empty();
    log.replay_all(lines.drain(..).filter_map(|l| LogLine::decode(&l).ok()));
    let mut next = MessageCenter::new(log, now);
    assert_eq!(next.adopt_digest(stamp(3 * DAY_MS), now), 3);
    let real = |secs: u64| stamp(3 * DAY_MS).unix_ms + secs * 1_000;
    let logged = |c: &MessageCenter, id| c.log().get(id).unwrap().retired_unix_ms.unwrap();
    let near = |a: u64, b: u64| a.abs_diff(b) < 10_000;
    // 1. A family look that names only tab 3: tab 1 and tab 2's clocks start at
    //    +300 s, and a look 600 s later ends them.
    let at = |secs: u64| now + Duration::from_secs(secs);
    let only_three = || vec![own_work("tab 3")];
    next.sync_digest(
        "harness.upgrade.",
        only_three(),
        stamp(3 * DAY_MS + 300_000),
        at(300),
    );
    let s = next.sync_digest(
        "harness.upgrade.",
        only_three(),
        stamp(3 * DAY_MS + 900_000),
        at(900),
    );
    assert_eq!(s.resolved, 2);
    assert!(
        near(logged(&next, ids[0]), real(900)),
        "a later look: real time"
    );
    assert!(near(logged(&next, ids[1]), real(900)));
    // 2. The reporter's clear.
    let key = next.digest().entries[0].key.clone();
    assert!(next.clear_digest(&key, at(2_000)));
    assert!(
        near(logged(&next, ids[2]), real(2_000)),
        "the clear: real time"
    );
    // 3. A key-prefix sweep, on a fresh adoption.
    let mut c2 = fresh(now);
    let id = c2.post(own_work("tab 9"), stamp(0), now).id;
    let mut lines: Vec<String> = c2
        .drain_new_for_persist()
        .iter()
        .map(LogLine::encode)
        .collect();
    let mut log = MessageLog::empty();
    log.replay_all(lines.drain(..).filter_map(|l| LogLine::decode(&l).ok()));
    let mut third = MessageCenter::new(log, now);
    assert_eq!(third.adopt_digest(stamp(3 * DAY_MS), now), 1);
    assert_eq!(
        third.resolve_key_prefix("harness.upgrade.", Outcome::Ok, at(60)),
        1
    );
    assert!(
        near(logged(&third, id), real(60)),
        "a sweep: real time, not 72 h early"
    );
}

/// A reporter that changed a family from a standing row to a record never
/// re-asserts the row its parent carried across the update. The newer build's
/// CLASSIFIED record under the adopted row's key retires it.
#[test]
fn a_classified_record_under_an_adopted_rows_key_retires_it() {
    let now = t0();
    let key = "update.dev-build-writes";
    let mut parent = fresh(now);
    parent.post(
        Message::new(
            tags::UPDATE,
            Severity::Warn,
            "Release setup lost to dev build",
        )
        .key(key)
        .now(Ask::standing(
            StandingWhy::OnlyYou(Reason::new("this dev build overwrote the release setup")),
            Then::step("Run aterm pkg repair from the release"),
        )),
        stamp(0),
        now,
    );
    let later = now + Duration::from_secs(5);
    let _ = parent.settle(later, true);
    let _ = parent.commit_rows(later, 3);
    let carry = parent.carried();
    let mut s = fresh(later);
    s.seed_carried(&carry, stamp(10), later);
    s.after_handoff_commit(later);
    assert_eq!(s.live_rows().count(), 1, "the carried row stands");
    s.post(
        Message::new(
            tags::UPDATE,
            Severity::Warn,
            "Release setup lost to dev build",
        )
        .key(key)
        .record(),
        stamp(11),
        later + Duration::from_secs(1),
    );
    assert_eq!(s.live_rows().count(), 0, "the newer build's record ends it");
    // ... but a row the CURRENT build posted is not retired by a record.
    s.post(
        Message::new(
            tags::UPDATE,
            Severity::Warn,
            "Release setup lost to dev build",
        )
        .key(key)
        .now(Ask::standing(
            StandingWhy::OnlyYou(Reason::new("this dev build overwrote the release setup")),
            Then::step("Run aterm pkg repair from the release"),
        )),
        stamp(12),
        later + Duration::from_secs(2),
    );
    s.post(
        Message::new(tags::UPDATE, Severity::Warn, "a log line")
            .key(key)
            .record(),
        stamp(13),
        later + Duration::from_secs(3),
    );
    assert_eq!(
        s.live_rows().count(),
        1,
        "only an ADOPTED row is retired by a record"
    );
}

/// A builder that wrote its own detail lines keeps them: the ask's reason and
/// action lead the detail only for a message that brought none (PROBE7 of the
/// review: `gpu_lost`'s four lines and the screen-reader row's `retry` line
/// moved).
#[test]
fn a_builders_own_detail_lines_are_not_reordered_by_the_ask() {
    let m = Message::new(tags::A11Y, Severity::Error, "Screen reader access lost")
        .line("restart aterm to retry")
        .line("bus gone")
        .key("a11y.publisher")
        .now(Ask::standing(
            StandingWhy::Spent {
                tried: Reason::new("tried to publish the screen reader tree"),
                reason: Reason::new("the accessibility thread died, and a process gets one"),
            },
            Then::step("restart aterm to retry"),
        ));
    assert_eq!(m.detail, vec!["restart aterm to retry", "bus gone"]);
    // ... and a message that brought none gets the shape.
    let bare = agent_down();
    assert_eq!(bare.detail.len(), 2);
    assert_eq!(bare.detail[1], "Type claude --resume in the tab");
}
