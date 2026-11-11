// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::log::{LogState, Retired};
use crate::model::{Decision, Origin, tags};
use crate::text::char_width;
use crate::{HOLD_INFO, HOLD_SUCCESS, HOLD_WARN};

fn t0() -> Instant {
    Instant::now()
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn stamp(unix_ms: u64) -> WallStamp {
    WallStamp { unix_ms }
}

fn fresh(now: Instant) -> MessageCenter {
    MessageCenter::new(MessageLog::empty(), now)
}

fn info(title: &str) -> Message {
    Message::new(tags::SYSTEM, Severity::Info, title)
}

fn standing(title: &str) -> Message {
    Message::new(tags::RENDER, Severity::Info, title).hold(Hold::STANDING)
}

fn ask(title: &str) -> Message {
    Message::new(tags::PRIVACY, Severity::Info, title)
        .action(Intent::OpenSystemPane {
            pane: "full-disk-access".into(),
        })
        .action(Intent::NotNow {
            decision: Decision::FileAccess,
        })
        .hold(Hold::asking(crate::HOLD_ASK))
}

fn glass_ids(c: &MessageCenter) -> Vec<MessageId> {
    c.on_glass().map(|l| l.id).collect()
}

#[test]
fn exhausted_entry_confirmations_never_wrap_or_allow_conditional_settlement() {
    use crate::wire::{NoticeRequest, WireGate};
    let now = t0();
    let mut c = MessageCenter::with_confirmations(
        MessageLog::empty(),
        now,
        Arc::new(crate::ConfirmationSource::new(Some(u64::MAX - 1))),
    );
    let NoticeRequest::Later(later) = NoticeRequest::parse(
        "later k Renew the key -- only you hold its passphrase -- Renew the key",
    )
    .unwrap() else {
        panic!("a later entry");
    };
    let msg = later.into_message();
    let id = c.post(msg.clone(), stamp(0), now).id;
    assert_eq!(c.digest_confirmation(id), Some(u64::MAX - 1));
    let before = (c.revision(), c.digest_revision(), c.projection_revision());
    for _ in 0..2 {
        assert_eq!(
            c.post(msg.clone(), stamp(0), now).outcome,
            PostOutcome::Duplicate
        );
    }
    let rows = crate::wire::message_rows(
        &c,
        &crate::wire::parse_read_args("live need=later").unwrap(),
        0,
        &str::to_string,
    );
    assert!(rows[0].ends_with(&format!(" rev={}", u64::MAX)));
    let mut gate = WireGate::default();
    for revision in [u64::MAX - 1, u64::MAX] {
        let req = NoticeRequest::parse(&format!("settle k {id} {revision}")).unwrap();
        let result = crate::wire::apply(&mut c, &mut gate, req, stamp(0), now);
        assert_eq!(result.reply, "OK settled=- how=gone");
        assert_eq!(
            (result.paint, result.note),
            (crate::wire::Paint::None, false)
        );
    }
    assert!(c.digest_entry(id).is_some());
    assert_eq!(
        (c.revision(), c.digest_revision(), c.projection_revision()),
        before
    );
}

fn pending_retired(c: &MessageCenter) -> Vec<(MessageId, Retired, String, u32)> {
    c.log()
        .pending_lines()
        .filter_map(|l| match l {
            LogLine::Retired {
                id,
                how,
                title,
                repeats,
                ..
            } => Some((*id, how.clone(), title.clone(), *repeats)),
            _ => None,
        })
        .collect()
}

#[test]
fn bounded_live_set() {
    let now = t0();
    let mut c = fresh(now);
    let ask_id = c.post(ask("File access not confirmed"), stamp(1), now).id;
    let standing_id = c.post(standing("GPU lost"), stamp(2), now).id;
    for i in 0..(MAX_LIVE + 50) {
        c.post(
            info(&format!("notice {i}")),
            stamp(10 + i as u64),
            now + ms(i as u64),
        );
    }
    assert_eq!(c.live_rows().count(), MAX_LIVE);
    assert!(c.live(ask_id).is_some(), "the ask survives");
    assert!(c.live(standing_id).is_some(), "the standing row survives");
    let evicted: Vec<_> = pending_retired(&c)
        .into_iter()
        .filter(|(_, how, _, _)| *how == Retired::Evicted)
        .collect();
    assert_eq!(
        evicted.len(),
        52,
        "two more than MAX_LIVE + 50 - MAX_LIVE: the ask and standing rows hold two seats"
    );
    assert_eq!(evicted[0].2, "notice 0", "the oldest held row goes first");
}

#[test]
fn a_flood_takes_every_queued_row_before_one_on_glass() {
    let now = t0();
    let tail = |severity, title: &str| {
        Message::new(tags::SYSTEM, severity, title).hold(Hold::Live {
            stale_after: crate::STALE_TAILED,
        })
    };
    let evicted = |c: &MessageCenter| -> Vec<String> {
        pending_retired(c)
            .into_iter()
            .filter(|(_, how, _, _)| *how == Retired::Evicted)
            .map(|(_, _, title, _)| title)
            .collect()
    };
    let mut c = fresh(now);
    let warn = c
        .post(
            Message::new(tags::PACKAGES, Severity::Warn, "Disk nearly full"),
            stamp(1),
            now,
        )
        .id;
    c.commit_rows(now, 1);
    assert_eq!(glass_ids(&c), vec![warn]);
    for i in 0..MAX_LIVE {
        let (severity, title) = if i == 30 {
            (Severity::Success, "quiet tail".to_string())
        } else {
            (Severity::Info, format!("tail {i}"))
        };
        c.post(
            tail(severity, &title),
            stamp(10 + i as u64),
            now + ms(1 + i as u64),
        );
    }
    assert_eq!(c.live_rows().count(), MAX_LIVE);
    assert_eq!(
        glass_ids(&c),
        vec![warn],
        "the held warning on the glass outlives a flood of queued Live rows"
    );
    assert_eq!(
        evicted(&c),
        ["quiet tail"],
        "the lowest-severity queued row goes first, though `tail 0` is older"
    );
    c.post(tail(Severity::Info, "one more"), stamp(999), now + ms(999));
    assert_eq!(glass_ids(&c), vec![warn]);
    assert_eq!(
        evicted(&c),
        ["quiet tail", "tail 0"],
        "then the oldest queued row"
    );

    let mut c = fresh(now);
    let g_live = c.post(tail(Severity::Info, "g live"), stamp(1), now).id;
    let g_held = c.post(info("g held"), stamp(2), now + ms(1)).id;
    let x = c
        .post(
            Message::new(tags::PACKAGES, Severity::Success, "x"),
            stamp(3),
            now + ms(2),
        )
        .id;
    c.commit_rows(now + ms(2), 3);
    assert_eq!(glass_ids(&c), vec![g_live, g_held, x]);
    for i in 0..(MAX_LIVE - 3) {
        c.post(
            standing(&format!("standing {i}")),
            stamp(10 + i as u64),
            now + ms(3 + i as u64),
        );
    }
    assert_eq!(
        glass_ids(&c),
        vec![g_live, g_held],
        "the overflow row took the third slot; `x` is queued"
    );
    let later = |c: &mut MessageCenter, n: u64| {
        c.post(
            standing(&format!("late {n}")),
            stamp(900 + n),
            now + ms(900 + n),
        );
    };
    later(&mut c, 1);
    assert_eq!(evicted(&c), ["x"], "the queued held row first");
    assert_eq!(glass_ids(&c), vec![g_live, g_held]);
    later(&mut c, 2);
    assert_eq!(
        evicted(&c),
        ["x", "g held"],
        "nothing queued is evictable: the held row on glass before the Live one"
    );
    later(&mut c, 3);
    assert_eq!(evicted(&c), ["x", "g held", "g live"]);
    later(&mut c, 4);
    assert_eq!(
        evicted(&c),
        ["x", "g held", "g live", "late 4"],
        "only asks and standing rows are left: the newcomer itself goes"
    );
    assert!(!c.has_live_capacity(), "the protected queue has no vacancy");
    let retired = c.live_rows().next().expect("a protected row").id;
    assert!(c.withdraw(retired, now + ms(1000)));
    assert!(
        c.has_live_capacity(),
        "an actual retirement opens admission"
    );
    let next = c
        .post(
            info("owed row after a vacancy"),
            stamp(1001),
            now + ms(1001),
        )
        .id;
    assert!(
        c.live(next).is_some(),
        "the admitted new row survives the cap"
    );
    assert!(!c.has_live_capacity(), "the vacancy has been consumed");
}

#[test]
fn a_queued_held_row_goes_before_a_queued_live_row_of_lower_severity() {
    let now = t0();
    let mut c = fresh(now);
    let glass = c
        .post(
            Message::new(tags::PACKAGES, Severity::Error, "Install failed"),
            stamp(1),
            now,
        )
        .id;
    c.commit_rows(now, 1);
    let live_success = c
        .post(
            Message::new(tags::SYSTEM, Severity::Success, "Indexing done soon").hold(Hold::Live {
                stale_after: crate::STALE_TAILED,
            }),
            stamp(2),
            now + ms(1),
        )
        .id;
    let held_info = c.post(info("held info"), stamp(3), now + ms(2)).id;
    let route_info = c
        .post(
            info("route info").hold(Hold::For(crate::HOLD_ROUTE)),
            stamp(4),
            now + ms(3),
        )
        .id;
    for i in 0..(MAX_LIVE - 4) {
        c.post(
            standing(&format!("standing {i}")),
            stamp(10 + i as u64),
            now + ms(4 + i as u64),
        );
    }
    assert_eq!(c.live_rows().count(), MAX_LIVE);
    assert_eq!(glass_ids(&c), vec![glass]);
    for id in [live_success, held_info, route_info] {
        assert!(c.live(id).unwrap().is_queued());
    }
    let evicted = |c: &MessageCenter| -> Vec<MessageId> {
        pending_retired(c)
            .into_iter()
            .filter(|(_, how, _, _)| *how == Retired::Evicted)
            .map(|(id, _, _, _)| id)
            .collect()
    };
    c.post(standing("one more"), stamp(998), now + ms(998));
    assert_eq!(
        evicted(&c),
        vec![held_info],
        "the queued held row goes before the queued Live row, though that one is \
         lower in severity and older"
    );
    c.post(standing("two more"), stamp(999), now + ms(999));
    assert_eq!(
        evicted(&c),
        vec![held_info, route_info],
        "a `For` row is held: it goes before the Live row too"
    );
    assert!(c.live(live_success).is_some());
    assert_eq!(glass_ids(&c), vec![glass]);
}

#[test]
fn an_unrevealed_row_counts_as_queued_and_goes_before_a_row_on_glass() {
    let now = t0();
    let mut c = fresh(now);
    let warn = c
        .post(
            Message::new(tags::PACKAGES, Severity::Warn, "Disk nearly full"),
            stamp(1),
            now,
        )
        .id;
    c.commit_rows(now, 1);
    let unrevealed = c
        .post(
            Message::new(tags::SYSTEM, Severity::Error, "Build failing")
                .hold(Hold::Live {
                    stale_after: crate::STALE_TAILED,
                })
                .in_flight()
                .reveal_after(Duration::from_secs(5)),
            stamp(2),
            now + ms(1),
        )
        .id;
    for i in 0..(MAX_LIVE - 2) {
        c.post(
            standing(&format!("standing {i}")),
            stamp(10 + i as u64),
            now + ms(2 + i as u64),
        );
    }
    assert_eq!(c.live_rows().count(), MAX_LIVE);
    assert_eq!(glass_ids(&c), vec![warn]);
    let row = c.live(unrevealed).unwrap();
    assert!(
        !row.revealed && row.is_queued(),
        "still in its reveal grace"
    );
    c.post(standing("one more"), stamp(999), now + ms(999));
    let evicted: Vec<MessageId> = pending_retired(&c)
        .into_iter()
        .filter(|(_, how, _, _)| *how == Retired::Evicted)
        .map(|(id, _, _, _)| id)
        .collect();
    assert_eq!(
        evicted,
        vec![unrevealed],
        "the unrevealed Error is queued: it goes before the held warning on the glass"
    );
    assert_eq!(glass_ids(&c), vec![warn]);
}

#[test]
fn hold_anchors_at_first_glass_and_a_displaced_row_is_released() {
    let now = t0();
    let mut c = fresh(now);
    let ids: Vec<MessageId> = (0..4)
        .map(|i| c.post(info(&format!("r{i}")), stamp(i), now + ms(i)).id)
        .collect();
    assert_eq!(c.wanted_rows(), 3);
    assert_eq!(c.commit_rows(now + ms(10), 3), Some(3));
    assert_eq!(
        glass_ids(&c),
        vec![ids[0], ids[1]],
        "two rows plus the overflow row"
    );
    assert_eq!(c.queued(), 2);
    assert_eq!(
        c.live(ids[0]).unwrap().fold_at,
        Some(now + ms(10) + HOLD_INFO),
        "anchored at commit, not at post"
    );
    assert_eq!(c.live(ids[2]).unwrap().fold_at, None, "queued: no anchor");
    assert!(c.live(ids[2]).unwrap().is_queued());
    let promotion = now + ms(500);
    assert!(c.resolve(ids[0], Outcome::Ok, promotion));
    assert_eq!(
        glass_ids(&c),
        vec![ids[1], ids[2], ids[3]],
        "three eligible rows fill the three committed rows; the overflow row is gone"
    );
    assert_eq!(c.live(ids[2]).unwrap().on_glass_since, Some(promotion));
    assert_eq!(c.live(ids[2]).unwrap().fold_at, Some(promotion + HOLD_INFO));
    assert_eq!(c.live(ids[3]).unwrap().fold_at, Some(promotion + HOLD_INFO));
    assert_eq!(
        c.live(ids[1]).unwrap().fold_at,
        Some(now + ms(10) + HOLD_INFO),
        "the survivor keeps its anchor"
    );
    let mut c = fresh(now);
    let ids: Vec<MessageId> = (0..3)
        .map(|i| c.post(info(&format!("r{i}")), stamp(i), now).id)
        .collect();
    c.commit_rows(now, 3);
    assert_eq!(glass_ids(&c), ids);
    let fourth = c.post(info("r3"), stamp(3), now + ms(1)).id;
    assert_eq!(
        glass_ids(&c),
        vec![ids[0], ids[1]],
        "the third slot is the overflow row"
    );
    let displaced = c.live(ids[2]).unwrap();
    assert!(displaced.is_queued() && displaced.fold_at.is_none());
    assert_eq!(displaced.on_glass_since, None);
    assert_eq!(
        c.settle(now + HOLD_INFO, true).retired,
        vec![(ids[0], Retired::Folded), (ids[1], Retired::Folded)],
        "the displaced row does not fold hidden"
    );
    assert_eq!(glass_ids(&c), vec![ids[2], fourth]);
    assert_eq!(
        c.live(ids[2]).unwrap().fold_at,
        Some(now + HOLD_INFO + HOLD_INFO),
        "re-anchored on its return"
    );
}

#[test]
fn supersede_keeps_the_slot_and_logs_the_last_words() {
    let now = t0();
    let mut c = fresh(now);
    let top = c.post(info("above"), stamp(1), now).id;
    let a = c
        .post(
            Message::new(tags::UPDATE, Severity::Info, "Checking for updates")
                .key("update.progress"),
            stamp(2),
            now,
        )
        .id;
    let below = c.post(info("below"), stamp(3), now).id;
    c.commit_rows(now, 3);
    assert_eq!(glass_ids(&c), vec![top, a, below]);
    c.restate(
        a,
        Restatement {
            title: Some("Downloading aterm v0.91.0".into()),
            ..Restatement::default()
        },
        now + ms(5),
    );
    let posted = c.post(
        Message::new(tags::UPDATE, Severity::Success, "aterm v0.91.0 is ready")
            .key("update.progress"),
        stamp(4),
        now + ms(10),
    );
    assert_eq!(posted.outcome, PostOutcome::Superseded(a));
    assert_eq!(glass_ids(&c), vec![top, posted.id, below], "the same slot");
    assert!(c.live(a).is_none());
    assert_eq!(
        c.live(posted.id).unwrap().fold_at,
        Some(now + ms(10) + HOLD_SUCCESS),
        "a fresh anchor"
    );
    let old = c.log().get(a).unwrap();
    assert_eq!(
        old.state,
        LogState::Retired(Retired::Superseded { by: posted.id })
    );
    assert_eq!(
        old.title, "Downloading aterm v0.91.0",
        "the record keeps the final words"
    );
    assert_eq!(c.live_by_key("update.progress").unwrap().id, posted.id);
    let mut c = fresh(now);
    let q = c.post(info("q").key("k"), stamp(1), now).id;
    let q2 = c.post(info("q2").key("k"), stamp(2), now).id;
    assert!(c.live(q).is_none() && c.live(q2).unwrap().fold_at.is_none());
}

#[test]
fn duplicates_bump_repeats_and_reanchor_the_hold() {
    let now = t0();
    let mut c = fresh(now);
    let first = c.post(info("Same words").line("same detail"), stamp(1), now);
    c.commit_rows(now, 3);
    let anchored = c.live(first.id).unwrap().fold_at.unwrap();
    let again = c.post(
        info("Same words").line("same detail"),
        stamp(2),
        now + ms(700),
    );
    assert_eq!(
        again,
        Posted {
            id: first.id,
            outcome: PostOutcome::Duplicate
        }
    );
    assert_eq!(
        c.live(first.id).unwrap().fold_at,
        Some(anchored + ms(700)),
        "the hold re-anchors"
    );
    assert_eq!(c.log().pending_len(), 1, "a duplicate logs nothing new");
    assert_eq!(
        c.post(info("Same words").line("other"), stamp(3), now)
            .outcome,
        PostOutcome::New
    );
    let k = c.post(
        info("K").key("dup").action(Intent::NewWindow),
        stamp(4),
        now,
    );
    assert_eq!(
        c.post(
            info("K").key("dup").action(Intent::NewWindow),
            stamp(5),
            now
        )
        .outcome,
        PostOutcome::Duplicate
    );
    assert_eq!(
        c.post(info("K").key("dup"), stamp(6), now).outcome,
        PostOutcome::Superseded(k.id)
    );
    let folds = c.settle(anchored + ms(700), true);
    assert!(
        folds
            .retired
            .iter()
            .any(|(id, how)| *id == first.id && *how == Retired::Folded),
        "{folds:?}"
    );
    let line = pending_retired(&c)
        .into_iter()
        .find(|(id, _, _, _)| *id == first.id)
        .unwrap();
    assert_eq!(line.3, 2, "rep=2 on the Retired line");
}

#[test]
fn restatements_never_write_the_log() {
    let now = t0();
    let mut c = fresh(now);
    let id = c
        .post(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing")
                .line("trust \u{00b7} downloading")
                .meter(Meter {
                    fill_permille: Some(0),
                    stats: String::new(),
                    ..Meter::default()
                })
                .hold(Hold::Live {
                    stale_after: crate::STALE_TAILED,
                }),
            stamp(1),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    let before = c.log().pending_len();
    for i in 0..1000u16 {
        let at = now + ms(u64::from(i));
        assert!(c.restate(
            id,
            Restatement {
                meter: Some(Some(Meter {
                    fill_permille: Some(i),
                    stats: format!("{i} / 1000"),
                    ..Meter::default()
                })),
                detail: Some(vec![format!("trust \u{00b7} step {i}")]),
                ..Restatement::default()
            },
            at
        ));
    }
    assert_eq!(c.log().pending_len(), before, "no lines for restatements");
    c.restate(
        id,
        Restatement {
            title: Some("Installed".into()),
            ..Restatement::default()
        },
        now + ms(1000),
    );
    let settled = c.settle(now + ms(1000) + crate::STALE_TAILED, true);
    assert_eq!(settled.retired, vec![(id, Retired::Stale)]);
    let line = c.log().pending_lines().last().cloned().unwrap();
    match line {
        LogLine::Retired { title, detail, .. } => {
            assert_eq!(title, "Installed");
            assert_eq!(detail, vec!["trust \u{00b7} step 999".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    assert!(!c.restate(id, Restatement::default(), now), "gone");
}

#[test]
fn an_identical_restatement_moves_no_revision() {
    let now = t0();
    let mut c = fresh(now);
    let words = Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab toolchain")
        .line("trust")
        .meter(Meter {
            fill_permille: Some(420),
            stats: "3 of 10 programs".into(),
            ..Meter::default()
        })
        .hold(Hold::Live {
            stale_after: crate::STALE_TAILED,
        });
    let id = c.post(words.clone(), stamp(1), now).id;
    c.commit_rows(now, 3);
    let (rev, fp) = (c.revision(), c.fingerprint(120));
    let same = Restatement {
        title: Some(words.title.clone()),
        detail: Some(words.detail.clone()),
        meter: Some(words.meter.clone()),
        hold: Some(words.hold),
        ..Restatement::default()
    };
    assert!(
        c.restate(id, same.clone(), now + ms(2000)),
        "the row is live"
    );
    assert_eq!(c.revision(), rev, "nothing the row says changed");
    assert_eq!(c.fingerprint(120), fp);
    assert_eq!(
        c.live(id).unwrap().stale_at,
        Some(now + ms(2000) + crate::STALE_TAILED),
        "the heartbeat still re-arms the cap"
    );
    let moved = Restatement {
        meter: Some(Some(Meter {
            fill_permille: Some(430),
            stats: "4 of 10 programs".into(),
            ..Meter::default()
        })),
        ..same
    };
    assert!(c.restate(id, moved, now + ms(4000)));
    assert!(c.revision() > rev, "a moved meter is a change");
    assert_ne!(c.fingerprint(120), fp);
}

#[test]
fn a_heartbeat_that_moves_the_estimate_is_a_change() {
    use crate::model::{Amount, Unit};
    let now = t0();
    let mut c = fresh(now);
    let meter = |done: u64| Meter {
        fill_permille: Some(u16::try_from(done * 1000 / 40).unwrap()),
        stats: "3 of 10 programs".into(),
        amount: Some(Amount {
            series: 1,
            done,
            total: 40,
            unit: Unit::Steps,
        }),
        ..Meter::default()
    };
    let id = c
        .post(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab toolchain")
                .meter(meter(0))
                .hold(Hold::Live {
                    stale_after: crate::STALE_TAILED,
                }),
            stamp(1),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    let restate = |c: &mut MessageCenter, done: u64, at: Instant| {
        c.restate(
            id,
            Restatement {
                meter: Some(Some(meter(done))),
                ..Restatement::default()
            },
            at,
        )
    };
    let mut t = now;
    for k in 1..=20u64 {
        t += ms(500);
        assert!(restate(&mut c, k, t));
    }
    assert!(
        matches!(c.live(id).unwrap().track.eta(t), Eta::Remaining(_)),
        "latched"
    );
    let (mut moved, mut still) = (0, 0);
    for _ in 0..30 {
        t += ms(2000);
        let (rev, eta) = (c.revision(), c.live(id).unwrap().track.eta(t));
        assert!(restate(&mut c, 20, t));
        let after = c.live(id).unwrap().track.eta(t);
        if after == eta {
            still += 1;
            assert_eq!(c.revision(), rev, "nothing moved: no repaint");
        } else {
            moved += 1;
            assert!(c.revision() > rev, "{eta:?} → {after:?}: a repaint");
        }
    }
    assert!(moved > 0 && still > 0, "moved {moved}, still {still}");
}

#[test]
fn frozen_holds_only_retire_on_staleness() {
    let now = t0();
    let mut c = fresh(now);
    let held = c
        .post(
            Message::new(tags::UPDATE, Severity::Success, "Updated")
                .line("your shells kept running"),
            stamp(1),
            now,
        )
        .id;
    let live = c
        .post(
            Message::new(tags::UPDATE, Severity::Info, "Installing aterm v0.76.0")
                .in_flight()
                .hold(Hold::Live {
                    stale_after: STALE_HANDOFF,
                }),
            stamp(2),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    assert_eq!(c.deadline(true), Some(now + HOLD_SUCCESS));
    assert_eq!(
        c.deadline(false),
        Some(now + STALE_HANDOFF),
        "frozen: only the cap is armed"
    );
    let frozen = c.settle(now + HOLD_SUCCESS, false);
    assert!(
        frozen.retired.is_empty() && !frozen.glass_changed,
        "the freeze keeps the words: {frozen:?}"
    );
    assert!(c.live(held).is_some());
    let capped = c.settle(now + STALE_HANDOFF, false);
    assert_eq!(capped.retired, vec![(live, Retired::Stale)]);
    assert!(capped.glass_changed);
    assert!(c.live(held).is_some(), "still frozen, still up");
    let faded = now + STALE_HANDOFF + EchoKind::Vanish.span(0);
    assert_eq!(c.deadline(false), Some(faded), "the fade's end");
    assert!(c.settle(faded, false).glass_changed, "the fade ended");
    assert_eq!(
        c.deadline(false),
        None,
        "nothing a frozen settle would act on"
    );
    assert_eq!(c.deadline(true), Some(now + HOLD_SUCCESS));
    let thawed = c.settle(faded, true);
    assert_eq!(thawed.retired, vec![(held, Retired::Folded)]);
    assert_eq!(c.deadline(true), None);
}

#[test]
fn hysteresis_grows_now_and_shrinks_after_quiet() {
    let now = t0();
    let mut c = fresh(now);
    let a = c.post(standing("a"), stamp(1), now).id;
    let b = c.post(standing("b"), stamp(2), now).id;
    assert_eq!(c.commit_rows(now, 3), Some(2));
    let d = c.post(standing("c"), stamp(3), now + ms(1)).id;
    assert_eq!(c.commit_rows(now + ms(1), 3), Some(3), "grow is immediate");
    assert_eq!(c.deadline(true), None);
    c.resolve(b, Outcome::Ok, now + ms(2));
    c.resolve(d, Outcome::Ok, now + ms(2));
    assert_eq!(c.wanted_rows(), 1);
    assert_eq!(c.commit_rows(now + ms(2), 3), None, "shrink waits");
    assert_eq!(c.committed_rows(), 3);
    assert_eq!(
        c.deadline(true),
        Some(now + ms(2) + SHRINK_QUIET),
        "the deadline names the shrink"
    );
    let e = c.post(standing("e"), stamp(4), now + ms(500)).id;
    let f = c.post(standing("f"), stamp(5), now + ms(500)).id;
    assert_eq!(
        c.commit_rows(now + ms(500), 3),
        None,
        "3 wanted, 3 committed: nothing moves"
    );
    assert_eq!(c.deadline(true), None, "no shrink pending");
    c.resolve(e, Outcome::Ok, now + ms(600));
    c.resolve(f, Outcome::Ok, now + ms(600));
    assert_eq!(c.commit_rows(now + ms(600), 3), None);
    assert_eq!(
        c.commit_rows(now + ms(600) + SHRINK_QUIET, 3),
        Some(1),
        "the shrink lands on time"
    );
    assert_eq!(c.committed_rows(), 1);
    assert_eq!(glass_ids(&c), vec![a]);
}

#[test]
fn a_pending_quiet_shrink_is_a_thawed_deadline_only() {
    let now = t0();
    let mut c = fresh(now);
    let a = c.post(standing("a"), stamp(1), now).id;
    assert_eq!(c.commit_rows(now, 3), Some(1));
    let later = now + Duration::from_secs(2);
    assert!(c.keep_as_record(a, &standing("a"), later));
    assert_eq!(c.wanted_rows(), 0);
    assert_eq!(c.commit_rows(later, 3), None, "the shrink waits");
    assert_eq!(c.committed_rows(), 1);
    assert_eq!(c.quiet_shrink_at(), Some(later + SHRINK_QUIET));
    assert_eq!(
        c.deadline(true),
        Some(later + SHRINK_QUIET),
        "thawed: the quiet shrink is armed"
    );
    assert_eq!(
        c.deadline(false),
        None,
        "frozen: a pending shrink names no instant"
    );
}

#[test]
fn stable_order_a_row_never_reshuffles_under_the_pointer() {
    let now = t0();
    let mut c = fresh(now);
    let r: Vec<MessageId> = (0..3)
        .map(|i| c.post(standing(&format!("r{i}")), stamp(i), now + ms(i)).id)
        .collect();
    c.commit_rows(now + ms(3), 3);
    assert_eq!(glass_ids(&c), vec![r[0], r[1], r[2]]);
    let r3 = c.post(standing("r3"), stamp(3), now + ms(4)).id;
    c.commit_rows(now + ms(4), 3);
    assert_eq!(
        glass_ids(&c),
        vec![r[0], r[1]],
        "the fourth queues behind the overflow row"
    );
    assert!(c.live(r3).unwrap().is_queued());
    assert_eq!(c.queued(), 2);
    let warn = c
        .post(
            Message::new(tags::UPDATE, Severity::Warn, "Update failed").hold(Hold::STANDING),
            stamp(4),
            now + ms(5),
        )
        .id;
    c.commit_rows(now + ms(5), 3);
    assert_eq!(
        glass_ids(&c),
        vec![warn, r[0]],
        "a Warn goes above; the survivor keeps its place"
    );
    c.resolve(warn, Outcome::Warn, now + ms(6));
    assert_eq!(
        glass_ids(&c),
        vec![r[0], r[1]],
        "rows move up in order; the best queued row fills the bottom"
    );
    c.resolve(r[0], Outcome::Ok, now + ms(7));
    assert_eq!(
        glass_ids(&c),
        vec![r[1], r[2], r3],
        "three eligible rows: no overflow row, the queue drains in rank order"
    );
    c.resolve(r[2], Outcome::Ok, now + ms(8));
    assert_eq!(glass_ids(&c), vec![r[1], r3]);
    let late = c.post(standing("late"), stamp(9), now + ms(9)).id;
    assert_eq!(glass_ids(&c), vec![r[1], r3, late]);
}

#[test]
fn overflow_patience_folds_an_unseen_row() {
    let now = t0();
    let mut c = fresh(now);
    c.post(standing("a"), stamp(1), now);
    c.post(standing("b"), stamp(2), now);
    c.commit_rows(now, 2);
    let s = c
        .post(
            Message::new(tags::PACKAGES, Severity::Success, "installed"),
            stamp(3),
            now + ms(1),
        )
        .id;
    c.commit_rows(now + ms(1), 2);
    assert!(c.live(s).unwrap().is_queued());
    assert_eq!(
        c.live(s).unwrap().patience_at,
        now + ms(1) + OVERFLOW_PATIENCE_MIN,
        "max(8 s, 60 s)"
    );
    assert_eq!(c.deadline(true), Some(now + ms(1) + OVERFLOW_PATIENCE_MIN));
    assert!(
        c.settle(now + ms(1) + OVERFLOW_PATIENCE_MIN - ms(1), true)
            .retired
            .is_empty()
    );
    let settled = c.settle(now + ms(1) + OVERFLOW_PATIENCE_MIN, true);
    assert_eq!(settled.retired, vec![(s, Retired::Unseen)]);
    assert_eq!(
        c.log().get(s).unwrap().state,
        LogState::Retired(Retired::Unseen),
        "the log keeps it"
    );
    let w = c
        .post(
            Message::new(tags::PACKAGES, Severity::Success, "long")
                .hold(Hold::For(Duration::from_secs(100))),
            stamp(4),
            now,
        )
        .id;
    assert_eq!(
        c.live(w).unwrap().patience_at,
        now + Duration::from_secs(100),
        "a long hold extends the patience (a transient hold is at most two minutes: the door)"
    );
}

fn glassless(now: Instant) -> MessageCenter {
    MessageCenter::glassless(
        MessageLog::empty(),
        now,
        Arc::new(crate::ConfirmationSource::new(None)),
    )
}

/// A GLASS-LESS HOST'S ROW IS AT THE FRONT ONCE REVEALED (the native app, which paints no
/// band): its hold anchors at its reveal, a held-back reveal's included, and ends it, Folded,
/// on no glass; the center commits no rows, and only a row not yet revealed is queued. A band
/// host's same row, which no commit placed, waits in the queue past its hold and retires
/// Unseen once its patience runs out.
/// NEGATIVE CONTROL (scratch): arming nothing in `rebalance`'s glass-less arm leaves the row
/// live past its hold; dropping that arm's `l.revealed` filter puts the unrevealed row at
/// the front at its post.
#[test]
fn a_glassless_row_arms_its_hold_at_its_reveal_and_folds_when_it_ends() {
    let now = t0();
    let mut c = glassless(now);
    assert_eq!(c.glass(), Glass::Glassless);
    assert_eq!(
        fresh(now).glass(),
        Glass::Band,
        "a center is a band host by default"
    );
    let id = c.post(info("installed"), stamp(1), now).id;
    let row = c.live(id).unwrap();
    assert_eq!(row.on_glass_since, Some(now), "at the front at its post");
    assert_eq!(
        row.fold_at,
        Some(now + HOLD_INFO),
        "its hold runs from there"
    );
    assert_eq!(c.commit_rows(now, 3), None, "no rows to commit");
    assert_eq!(c.committed_rows(), 0);
    assert!(glass_ids(&c).is_empty(), "nothing on any glass");
    assert_eq!(c.deadline(true), Some(now + HOLD_INFO));
    assert!(c.settle(now + HOLD_INFO - ms(1), true).retired.is_empty());
    assert_eq!(
        c.settle(now + HOLD_INFO, true).retired,
        vec![(id, Retired::Folded)],
        "its hold ends it"
    );

    let mut later = info("revealed later");
    later.reveal_after = Some(ms(2_000));
    let late = c.post(later, stamp(2), now).id;
    assert!(
        c.live(late).unwrap().is_queued(),
        "an unrevealed row is the only queued kind"
    );
    assert_eq!(
        c.live(late).unwrap().fold_at,
        None,
        "no hold before its reveal"
    );
    let revealed = now + ms(2_000);
    assert!(c.settle(revealed, true).retired.is_empty());
    assert!(
        !c.live(late).unwrap().is_queued(),
        "at the front at its reveal"
    );
    assert!(glass_ids(&c).is_empty(), "still nothing on any glass");
    assert_eq!(c.committed_rows(), 0);
    assert_eq!(
        c.live(late).unwrap().fold_at,
        Some(revealed + HOLD_INFO),
        "the hold anchors at the reveal"
    );

    let mut band = fresh(now);
    let queued = band.post(info("installed"), stamp(1), now).id;
    assert!(
        band.live(queued).unwrap().is_queued(),
        "CONTROL: no commit placed it"
    );
    assert!(
        band.settle(now + HOLD_INFO, true).retired.is_empty(),
        "CONTROL: it waits"
    );
    assert_eq!(
        band.settle(now + OVERFLOW_PATIENCE_MIN, true).retired,
        vec![(queued, Retired::Unseen)],
        "CONTROL: a band host's queued row ages out unseen"
    );
}

/// NO GLASS-LESS ROW SPENDS PATIENCE: a row whose reveal the first settle meets only past its
/// patience (the host slept through it) is revealed and held from that settle, a standing row
/// outlives a day, the patience a band host's queue spends on it, with no deadline asked for
/// it, and a busy update row its reporter keeps alive outlives its patience; a band host
/// retires all three Unseen.
/// NEGATIVE CONTROL (scratch): dropping `spends_patience`'s `Glass::Band` term retires the
/// slept-through row Unseen.
#[test]
fn a_glassless_row_spends_no_patience() {
    let now = t0();
    let mut slept = info("revealed in a sleep");
    slept.reveal_after = Some(ms(2_000));
    let woke = now + ms(2_000) + OVERFLOW_PATIENCE_MIN;
    for (host, mut c) in [("glass-less", glassless(now)), ("band", fresh(now))] {
        let id = c.post(slept.clone(), stamp(1), now).id;
        let retired = c.settle(woke, true).retired;
        if host == "band" {
            assert_eq!(retired, vec![(id, Retired::Unseen)], "CONTROL: {host}");
            continue;
        }
        assert!(retired.is_empty(), "{host}: {retired:?}");
        assert_eq!(
            c.live(id).unwrap().fold_at,
            Some(woke + HOLD_INFO),
            "{host}"
        );
    }
    let day = STANDING_PATIENCE;
    for (host, mut c) in [("glass-less", glassless(now)), ("band", fresh(now))] {
        let still = c.post(standing("Disk nearly full"), stamp(1), now).id;
        let work = c.post(busy("Installing the update"), stamp(2), now).id;
        let mut t = now;
        let mut retired = Vec::new();
        while t < now + day + ms(1) {
            t += Duration::from_mins(10);
            c.restate(work, Restatement::default(), t);
            retired.extend(c.settle(t, true).retired);
        }
        if host == "band" {
            assert_eq!(
                retired,
                vec![(work, Retired::Unseen), (still, Retired::Unseen)],
                "CONTROL: {host}"
            );
            continue;
        }
        assert!(retired.is_empty(), "{host}: {retired:?}");
        assert!(c.live(still).is_some() && c.live(work).is_some(), "{host}");
        c.withdraw(work, t);
        assert_eq!(c.deadline(true), None, "{host}: no patience is asked for");
    }
}

/// A GLASS-LESS SUCCESSOR'S CARRIED ROW HOLDS FROM THE COMMIT: seeded at the front, it waits
/// out the freeze, then its hold runs from the handoff commit and ends it, Folded.
#[test]
fn a_glassless_successors_carried_row_holds_from_the_commit() {
    let now = t0();
    let mut parent = glassless(now);
    let id = parent.post(info("carried"), stamp(1), now).id;
    let mut child = glassless(now);
    child.seed_carried(&parent.carried(), stamp(1), now);
    let freeze = Duration::from_mins(2);
    assert!(
        child.settle(now + freeze, false).retired.is_empty(),
        "the freeze"
    );
    let commit = now + freeze;
    child.after_handoff_commit(commit);
    assert_eq!(child.live(id).unwrap().fold_at, Some(commit + HOLD_INFO));
    assert!(
        child
            .settle(commit + HOLD_INFO - ms(1), true)
            .retired
            .is_empty()
    );
    assert_eq!(
        child.settle(commit + HOLD_INFO, true).retired,
        vec![(id, Retired::Folded)]
    );
}

/// A FLOOD ON A GLASS-LESS HOST TAKES A ROW NOT YET REVEALED BEFORE ONE AT THE FRONT: there
/// a revealed row is at the front, so a Live row still in its grace goes before every older
/// held row. CONTROL: a band host, whose same rows no commit placed, holds every one queued,
/// so the held rows go first there, the oldest of them before the Live one.
/// NEGATIVE CONTROL (scratch): keying `evict_past_cap` on the glass (`self.glass.contains`)
/// in place of `!is_queued` makes the glass-less half evict the oldest held row.
#[test]
fn a_glassless_flood_takes_an_unrevealed_row_before_a_revealed_one() {
    let now = t0();
    let evicted = |c: &MessageCenter| -> Vec<String> {
        pending_retired(c)
            .into_iter()
            .filter(|(_, how, _, _)| *how == Retired::Evicted)
            .map(|(_, _, title, _)| title)
            .collect()
    };
    for (mut c, gone, kept, host) in [
        (glassless(now), "in its grace", "info 0", "glass-less"),
        (fresh(now), "info 0", "in its grace", "band"),
    ] {
        for i in 0..(MAX_LIVE - 1) {
            let at = u64::try_from(i).expect("a small count");
            c.post(info(&format!("info {i}")), stamp(at), now + ms(at));
        }
        let grace = c
            .post(
                Message::new(tags::SYSTEM, Severity::Info, "in its grace")
                    .hold(Hold::Live {
                        stale_after: crate::STALE_TAILED,
                    })
                    .in_flight()
                    .reveal_after(crate::PROGRESS_GRACE),
                stamp(900),
                now + ms(900),
            )
            .id;
        let row = c.live(grace).unwrap();
        assert!(
            !row.revealed && row.is_queued(),
            "{host}: PRECONDITION: the Live row is in its grace: {row:?}"
        );
        assert_eq!(c.live_rows().count(), MAX_LIVE, "{host}");
        c.post(info("one more"), stamp(901), now + ms(901));
        assert_eq!(evicted(&c), [gone], "{host}");
        assert!(
            c.live_rows().any(|l| l.msg.title == kept),
            "{host}: {kept:?} stays"
        );
    }
}

#[test]
fn act_returns_the_intent_and_details_marks_seen() {
    let now = t0();
    let mut c = fresh(now);
    let id = c.post(ask("File access not confirmed"), stamp(1), now).id;
    c.commit_rows(now, 3);
    let intent = c.act(id, ActionIndex(0), now + ms(1));
    assert_eq!(
        intent,
        Some(Intent::OpenSystemPane {
            pane: "full-disk-access".into()
        })
    );
    assert!(c.live(id).is_some(), "Open Settings leaves the row (R16)");
    assert!(
        matches!(c.log().pending_lines().last(), Some(LogLine::Acted { label, .. }) if label == "Open Settings")
    );
    assert_eq!(c.act(id, ActionIndex(5), now), None, "no such action");
    let held = c.post(info("read me"), stamp(2), now).id;
    c.commit_rows(now, 3);
    for (row, folds) in [(held, true), (id, false)] {
        assert_eq!(
            c.act(row, ActionIndex::DETAILS, now + ms(4)),
            Some(Intent::Details)
        );
        assert_eq!(c.live(row).is_none(), folds, "{row}");
    }
    assert_eq!(
        c.log().get(held).unwrap().state,
        LogState::Retired(Retired::Folded)
    );
    assert!(c.live(id).unwrap().seen);
}

fn busy(title: &str) -> Message {
    Message::new(tags::UPDATE, Severity::Info, title)
        .line("installs within a minute")
        .meter(Meter::busy(""))
        .hold(Hold::Live {
            stale_after: crate::STALE_UPDATE,
        })
        .key("update.progress")
}

#[test]
fn a_carried_row_never_reopens_another_writers_record_minted_at_load() {
    let now = t0();
    let live = |title: &str, key: &str| {
        Message::new(tags::TOOLCHAIN, Severity::Info, title)
            .meter(Meter::busy(""))
            .hold(Hold::Live {
                stale_after: crate::STALE_ANNOUNCE,
            })
            .key(key)
    };
    let mut parent = fresh(now);
    let a = parent
        .post(live("Installing ALab tools", "a"), stamp(1), now)
        .id;
    let b = parent.post(live("Indexing docs", "b"), stamp(2), now).id;
    let c = parent.post(live("Building docs", "c"), stamp(3), now).id;
    parent.commit_rows(now, 3);
    let carry = parent.carried();
    assert!(
        carry.live.iter().any(|m| m.id == c.raw()),
        "{:?}",
        carry.live
    );
    let rec = |id: MessageId| parent.log().get(id).cloned().unwrap();
    let other = LogRecord {
        stamp: stamp(9),
        title: "Another writer's work".into(),
        ..rec(b)
    };
    let mut log = MessageLog::empty();
    log.replay_all([
        LogLine::Posted(rec(a)),
        LogLine::Posted(rec(b)),
        LogLine::Posted(other),
    ]);
    let squatter = log.records().find(|r| r.stamp == stamp(9)).map(|r| r.id);
    assert_eq!(squatter, Some(c), "the collision minted the carried id");
    let mut successor = MessageCenter::new(log, now);
    successor.seed_carried(&carry, stamp(10), now);
    let log = successor.log();
    let mut ids: Vec<u64> = log.records().map(|r| r.id.raw()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), log.len(), "one id, one record");
    let at_c = log.get(c).unwrap();
    assert_eq!(at_c.title, "Building docs");
    assert_eq!(at_c.state, LogState::Posted);
    let moved = log.records().find(|r| r.stamp == stamp(9)).unwrap();
    assert_eq!(moved.title, "Another writer's work", "keeps its own words");
    assert_eq!(moved.state, LogState::Retired(Retired::Stale));
    assert!(moved.id.raw() >= carry.next_id, "above every carried id");
    let at_b = log.get(b).unwrap();
    assert_eq!(
        (at_b.title.as_str(), &at_b.state),
        ("Indexing docs", &LogState::Posted)
    );
}

#[test]
fn carry_round_trips_busy_and_commit_stills_unrestated_rows() {
    let now = t0();
    let mut parent = fresh(now);
    let flow = parent.post(busy("Updating to aterm v1"), stamp(1), now).id;
    let other = parent
        .post(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab tools")
                .meter(Meter::busy(""))
                .hold(Hold::Live {
                    stale_after: crate::STALE_ANNOUNCE,
                })
                .key("toolchain.pass"),
            stamp(2),
            now,
        )
        .id;
    parent.commit_rows(now, 3);
    let carry = parent.carried();
    assert!(carry.live.iter().all(|m| m.busy), "{:?}", carry.live);
    let mut successor = fresh(now);
    successor.seed_carried(&carry, stamp(0), now);
    assert!(
        successor.live_rows().all(Live::is_busy),
        "busy crosses on every carried row"
    );
    assert!(successor.restate(
        flow,
        Restatement {
            detail: Some(vec!["almost done".into()]),
            meter: Some(Some(Meter::busy(""))),
            ..Restatement::default()
        },
        now,
    ));
    successor.after_handoff_commit(now);
    let busy_of = |c: &MessageCenter, id| {
        c.live(id)
            .and_then(|l| l.msg.meter.as_ref())
            .is_some_and(|m| m.busy)
    };
    assert!(busy_of(&successor, flow), "restated: this process's own");
    assert!(!busy_of(&successor, other), "not restated: shown still");
    let mut old = carry.clone();
    for m in &mut old.live {
        m.busy = false;
    }
    let mut older = fresh(now);
    older.seed_carried(&old, stamp(0), now);
    assert!(!busy_of(&older, flow));
}

#[test]
fn carry_round_trips_and_folds_after_commit() {
    let now = t0();
    let mut parent = fresh(now);
    let held = parent
        .post(
            Message::new(tags::UPDATE, Severity::Success, "Updated")
                .line("your shells kept running"),
            stamp(1),
            now,
        )
        .id;
    let live = parent
        .post(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Installing")
                .line("trust \u{00b7} extracting")
                .meter(Meter {
                    fill_permille: Some(420),
                    stats: "512 MB / 1.2 GB".into(),
                    ..Meter::default()
                })
                .action(Intent::OpenSettings {
                    route: "/packages".into(),
                })
                .hold(Hold::Live {
                    stale_after: crate::STALE_TAILED,
                })
                .key("toolchain.pass"),
            stamp(2),
            now,
        )
        .id;
    let standing = parent.post(standing("GPU lost"), stamp(3), now).id;
    let queued = parent
        .post(
            Message::new(tags::PACKAGES, Severity::Warn, "queued warn"),
            stamp(4),
            now + ms(1),
        )
        .id;
    parent.commit_rows(now + ms(1), 2);
    let carry = parent.carried();
    assert_eq!(carry.next_id, 5);
    assert_eq!(
        carry.live.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![held.raw(), live.raw(), standing.raw(), queued.raw()],
        "every revealed row, glass or queue, in the parent's own order"
    );
    let carried_live = &carry.live[1];
    assert_eq!(carried_live.hold, "live:30");
    assert_eq!(carried_live.actions, vec!["open-settings:/packages"]);
    assert_eq!(carried_live.fill_permille, Some(420));

    let later = now + Duration::from_secs(5);
    let mut child = fresh(later);
    child.seed_carried(&carry, stamp(5_000), later);
    assert_eq!(child.live_rows().count(), 4);
    assert_eq!(
        child.log().next_id().raw(),
        5,
        "ids continue past the parent's"
    );
    assert_eq!(
        child.log().pending_len(),
        0,
        "no line crosses: the parent's record is its own to the end"
    );
    for id in [held, live, standing, queued] {
        let row = child.live(id).unwrap();
        assert_eq!(
            row.stale_at,
            Some(later + STALE_HANDOFF),
            "{id}: the handoff cap"
        );
        assert_eq!(row.fold_at, None);
        assert_eq!(row.msg.origin, Origin::Carried);
        assert!(child.log().get(id).is_some_and(|r| r.is_live()));
    }
    assert_eq!(
        child.committed_rows(),
        0,
        "the seed takes no row: only the host's commit does"
    );
    assert!(
        glass_ids(&child).is_empty(),
        "every carried row waits queued"
    );
    assert_eq!(
        child.carried(),
        carry,
        "the successor hands on what it took"
    );
    child.commit_rows(later, 3);
    assert_eq!(
        glass_ids(&child),
        vec![queued, live],
        "a host's commit places them: the progress row and the best queued row; the third \
         is the overflow row"
    );
    assert_eq!(
        child.live(live).unwrap().stale_at,
        Some(later + STALE_HANDOFF),
        "on the glass, its hold still waits for the handoff commit"
    );
    let mut junk = carry.clone();
    junk.live[1].severity = "loud".into();
    junk.live[2].tag = "Not A Tag".into();
    junk.live[3].hold = "forever".into();
    junk.next_id = 900;
    let mut c = fresh(later);
    c.seed_carried(&junk, stamp(5_000), later);
    assert_eq!(c.live_rows().count(), 1);
    assert_eq!(c.log().next_id().raw(), 900);
    let commit = later + Duration::from_secs(20);
    assert!(child.settle(commit, true).retired.is_empty());
    let restated = later + Duration::from_secs(1);
    assert!(child.restate(
        queued,
        Restatement {
            title: Some("queued warn, restated".into()),
            ..Restatement::default()
        },
        restated
    ));
    assert_eq!(
        child.live(queued).unwrap().stale_at,
        None,
        "the handoff cap is gone"
    );
    assert_eq!(
        child.live(queued).unwrap().fold_at,
        Some(restated + HOLD_WARN),
        "its own hold, anchored at the restate"
    );
    child.after_handoff_commit(commit);
    assert_eq!(
        child.live(queued).unwrap().fold_at,
        Some(restated + HOLD_WARN)
    );
    assert_eq!(
        child.live(live).unwrap().stale_at,
        Some(commit + crate::STALE_TAILED)
    );
    for id in [held, standing] {
        let row = child.live(id).unwrap();
        assert_eq!((row.stale_at, row.fold_at), (None, None), "{id}");
        assert_eq!(
            row.patience_at,
            commit + row.hold_span().max(OVERFLOW_PATIENCE_MIN),
            "{id}: its patience runs from the commit"
        );
    }
}

#[test]
fn a_carried_row_waits_out_a_two_minute_freeze_with_its_deadline_ahead() {
    let now = t0();
    let mut parent = fresh(now);
    let ids: Vec<MessageId> = (0..4)
        .map(|i| {
            parent
                .post(
                    Message::new(tags::SYSTEM, Severity::Info, format!("row {i}")),
                    stamp(i),
                    now,
                )
                .id
        })
        .collect();
    let carry = parent.carried();
    let mut child = fresh(now);
    child.seed_carried(&carry, stamp(0), now);
    let freeze = Duration::from_mins(2);
    assert!(
        OVERFLOW_PATIENCE_MIN < freeze && freeze < STALE_HANDOFF,
        "PRECONDITION: the freeze outlasts the patience, not the handoff cap"
    );
    let mut t = now;
    while t <= now + freeze {
        let settled = child.settle(t, false);
        assert!(
            settled.retired.is_empty(),
            "{:?} into the freeze: {:?}",
            t - now,
            settled.retired
        );
        let due = child.deadline(false).expect("the handoff cap is pending");
        assert!(
            due > t,
            "{:?} into the freeze: a wake due {:?} ago would spin the event loop",
            t - now,
            t.saturating_duration_since(due)
        );
        t += Duration::from_secs(1);
    }
    assert!(ids.iter().all(|id| child.live(*id).is_some()));
    let commit = now + freeze;
    child.after_handoff_commit(commit);
    let patience = child
        .live(ids[0])
        .unwrap()
        .hold_span()
        .max(OVERFLOW_PATIENCE_MIN);
    assert_eq!(child.deadline(true), Some(commit + patience));
    assert!(
        child
            .settle(commit + patience - ms(1), true)
            .retired
            .is_empty(),
        "a whole patience from the commit"
    );
    let retired = child.settle(commit + patience, true).retired;
    assert_eq!(
        retired.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        ids,
        "then each, unseen"
    );
    assert!(retired.iter().all(|(_, how)| *how == Retired::Unseen));
}

#[test]
fn a_problem_folded_before_its_fix_still_closes_as_fixed() {
    let now = t0();
    let mut c = fresh(now);
    let warn = |title: &str, key: &str| {
        Message::new(tags::CONFIG, Severity::Warn, title)
            .line("windw_padding \u{2192} window_padding")
            .finished_as("Misspelled setting fixed")
            .key(key)
    };
    let post = |c: &mut MessageCenter, m: Message, n: u64| c.post(m, stamp(n), now).id;
    let folded = post(&mut c, warn("Misspelled setting", "config.ignored-keys"), 1);
    let no_words = post(
        &mut c,
        Message::new(tags::CONFIG, Severity::Warn, "Font not found").key("config.fonts"),
        2,
    );
    let elsewhere = post(&mut c, warn("Misspelled setting", "privacy.x"), 3);
    let taken_over = post(
        &mut c,
        warn("Misspelled setting", "config.unaccepted-values"),
        4,
    );
    c.commit_rows(now, 3);
    for id in [folded, no_words, elsewhere, taken_over] {
        assert!(c.mark_seen(id, now + ms(1)), "the press folds it");
        assert_eq!(
            c.log().get(id).unwrap().state,
            LogState::Retired(Retired::Folded)
        );
    }
    let repeat = post(
        &mut c,
        warn("Misspelled setting", "config.ignored-keys").hold(Hold::LogOnly),
        5,
    );
    let changed = post(
        &mut c,
        warn("2 misspelled settings", "config.unaccepted-values"),
        6,
    );
    let bystander = post(&mut c, warn("Misspelled setting", "privacy.fda"), 7);
    let fixed_at = now + ms(10);
    assert_eq!(
        c.resolve_key_prefix("config.", Outcome::Ok, fixed_at),
        3,
        "the changed row (live) and the folded warning and its repeat (closed)"
    );
    assert!(c.live(bystander).is_some());
    let words = |c: &MessageCenter, id| {
        let rec = c.log().get(id).unwrap();
        (
            rec.title.clone(),
            rec.detail.first().cloned(),
            rec.severity,
            rec.glyph.ch(),
        )
    };
    let fixed = (
        "Misspelled setting fixed".to_string(),
        Some("Misspelled setting".to_string()),
        Severity::Success,
        '\u{2713}',
    );
    assert_eq!(words(&c, folded), fixed);
    assert_eq!(words(&c, repeat), fixed);
    assert_eq!(words(&c, changed).2, Severity::Success, "fixed while live");
    for id in [no_words, elsewhere, taken_over] {
        assert_eq!(words(&c, id).2, Severity::Warn, "{id:?} keeps its \u{26a0}");
    }
    assert_eq!(
        c.log().get(folded).unwrap().state,
        LogState::Retired(Retired::Resolved(Outcome::Ok))
    );
    assert_eq!(c.fix_closed("config.", fixed_at + ms(1)), 0, "once");
    let lines = c.log.drain_pending();
    let mut replayed = MessageLog::empty();
    replayed.replay_all(lines);
    let rec = replayed
        .records()
        .find(|r| r.stamp == stamp(1))
        .expect("the folded warning's record");
    assert_eq!(
        (rec.title.as_str(), rec.severity),
        ("Misspelled setting fixed", Severity::Success)
    );
    let rec = replayed.records().find(|r| r.stamp == stamp(3)).unwrap();
    assert_eq!(rec.severity, Severity::Warn, "the control replays unfixed");
}

#[test]
fn a_record_is_reworded_in_place_and_replays_so() {
    let now = t0();
    let mut c = fresh(now);
    let waits = |age: &str| {
        info("Couldn't upgrade Claude in its tab yet")
            .key("harness.upgrade.s-a")
            .line(format!("behind for {age}: its own work runs"))
            .hold(Hold::LogOnly)
    };
    let id = c.post(waits("6 h"), stamp(1), now).id;
    let before = c.log().get(id).unwrap().clone();
    let lines_before = c.log().pending_len();
    let revision = c.revision();
    assert!(!c.reword_record(id, &waits("6 h")), "the same words");
    assert_eq!(c.log().pending_len(), lines_before, "nothing written");
    assert_eq!(c.revision(), revision, "nothing moved");

    assert!(c.reword_record(id, &waits("9 h")));
    assert_ne!(c.revision(), revision);
    let rec = c.log().get(id).unwrap();
    assert_eq!(rec.detail, vec!["behind for 9 h: its own work runs"]);
    assert_eq!(rec.title, before.title);
    assert_eq!(rec.state, LogState::Retired(Retired::Recorded));
    assert_eq!(rec.retired_unix_ms, before.retired_unix_ms);
    assert_eq!(rec.retired_at, before.retired_at, "nothing finished again");

    let lines = c.log.drain_pending();
    let mut replayed = MessageLog::empty();
    replayed.replay_all(lines.iter().map(|l| LogLine::decode(&l.encode()).unwrap()));
    let keyed: Vec<&LogRecord> = replayed
        .records()
        .filter(|r| r.key.as_deref() == Some("harness.upgrade.s-a"))
        .collect();
    assert_eq!(keyed.len(), 1);
    assert_eq!(keyed[0].detail, vec!["behind for 9 h: its own work runs"]);
    assert_eq!(keyed[0].state, LogState::Retired(Retired::Recorded));

    let live = c.post(info("Rebuilding"), stamp(2), now).id;
    assert!(!c.reword_record(live, &info("Rebuilt")));
    assert_eq!(c.live(live).unwrap().msg.title, "Rebuilding");
    c.commit_rows(now, 3);
    assert!(c.dismiss(live, now + ms(1)));
    assert!(!c.reword_record(live, &info("Rebuilt")));
    assert_eq!(c.log().get(live).unwrap().title, "Rebuilding");
    let unknown = c.log().next_id();
    assert!(!c.reword_record(unknown, &info("x")), "no such record");
}

fn upgrade_word(word: crate::UpgradeWord) -> Intent {
    Intent::AgentUpgrade {
        tab: "s-a".to_string(),
        to: "2.1.284".to_string(),
        word,
    }
}

#[test]
fn a_record_is_never_reworded_for_capsules_it_does_not_have() {
    use crate::UpgradeWord::{NotToday, Now, Skip};
    let now = t0();
    let mut c = fresh(now);
    let said = |age: &str, remedy: &str, words: [crate::UpgradeWord; 2]| {
        words.into_iter().fold(
            info("Couldn't upgrade Claude in its tab yet")
                .key("harness.upgrade.s-a")
                .line(format!("behind for {age}: its own work runs"))
                .line(remedy)
                .hold(Hold::LogOnly),
            |m, w| m.action(upgrade_word(w)),
        )
    };
    let own_work = "Upgrade now cannot move it past its own work; Skip version keeps it";
    let id = c
        .post(said("7 h", own_work, [NotToday, Skip]), stamp(1), now)
        .id;
    let lines = c.log().pending_len();
    let revision = c.revision();
    let other = said(
        "8 h",
        "Upgrade now asks it at its next pause; Skip This Version in the tab's menu keeps it",
        [Now, NotToday],
    );
    assert!(!c.reword_record(id, &other), "words for other capsules");
    assert_eq!(c.log().pending_len(), lines, "nothing written");
    assert_eq!(c.revision(), revision, "nothing moved");
    assert_eq!(
        c.log().get(id).unwrap().detail[0],
        "behind for 7 h: its own work runs"
    );
    assert!(c.reword_record(id, &said("8 h", own_work, [NotToday, Skip])));
    assert_eq!(
        c.log().get(id).unwrap().detail[0],
        "behind for 8 h: its own work runs"
    );
}

#[test]
fn a_record_replaced_by_its_row_says_so_and_replays_so() {
    let now = t0();
    let mut c = fresh(now);
    let record = c
        .post(
            info("Couldn't upgrade Claude in its tab yet")
                .key("harness.upgrade.s-a")
                .line("behind for 23 h: its own work runs")
                .hold(Hold::LogOnly),
            stamp(1),
            now,
        )
        .id;
    let before = c.log().get(record).unwrap().clone();
    let row = c
        .post(
            Message::new(tags::HARNESS, Severity::Warn, "Couldn't upgrade Claude yet")
                .key("harness.upgrade.s-a")
                .line("behind for 24 h: its own work runs")
                .hold(Hold::STANDING),
            stamp(2),
            now,
        )
        .id;
    let revision = c.revision();
    assert!(c.supersede_record(record, row, now));
    assert_ne!(c.revision(), revision);
    let rec = c.log().get(record).unwrap();
    assert_eq!(
        rec.state,
        LogState::Retired(Retired::Superseded { by: row })
    );
    assert_eq!((&rec.title, &rec.detail), (&before.title, &before.detail));
    assert_eq!(rec.retired_unix_ms, before.retired_unix_ms);
    assert_eq!(rec.retired_at, before.retired_at, "nothing finished again");
    let lines = c.log().pending_len();
    assert!(!c.supersede_record(record, row, now), "replaced already");
    assert!(
        !c.supersede_record(row, record, now),
        "a live row, by an earlier record: never told back"
    );
    assert!(!c.supersede_record(record, record, now));
    assert!(
        !c.supersede_record(c.log().next_id(), row, now),
        "no such record"
    );
    c.commit_rows(now, 3);
    assert!(c.withdraw(row, now + ms(1)));
    assert!(
        !c.supersede_record(row, record, now),
        "a row withdrawn, by an earlier record"
    );
    assert_eq!(c.log().pending_len(), lines + 1, "only the withdraw's line");

    let lines = c.log.drain_pending();
    let mut replayed = MessageLog::empty();
    replayed.replay_all(lines.iter().map(|l| LogLine::decode(&l.encode()).unwrap()));
    let rec = replayed
        .records()
        .find(|r| r.stamp == stamp(1))
        .expect("the record");
    assert_eq!(
        rec.state,
        LogState::Retired(Retired::Superseded { by: row })
    );
    assert_eq!(rec.detail, before.detail);
}

#[test]
fn a_row_or_a_read_record_is_told_on_under_another_key_and_an_ended_story_is_not() {
    let now = t0();
    let mut c = fresh(now);
    let warn = |title: &str, key: &str| {
        Message::new(tags::HARNESS, Severity::Warn, title)
            .key(key)
            .hold(Hold::STANDING)
    };
    let si = c.post(warn("Login expired", "n.s-1.si"), stamp(1), now).id;
    c.commit_rows(now, 3);
    let ca = c
        .post(warn("Sign-in cancelled", "n.s-1.ca"), stamp(2), now)
        .id;
    assert!(c.supersede_record(si, ca, now + ms(1)), "a live row");
    assert!(c.live(si).is_none(), "it left the glass");
    assert_eq!(
        c.log().get(si).unwrap().retired(),
        Some(&Retired::Superseded { by: ca })
    );
    assert!(c.live(ca).is_some());
    assert!(c.mark_seen(ca, now + ms(2)));
    assert_eq!(c.log().get(ca).unwrap().retired(), Some(&Retired::Folded));
    let nr = c.post(warn("Login expired", "n.s-1.nr"), stamp(3), now).id;
    assert!(c.supersede_record(ca, nr, now + ms(3)), "a read record");
    assert_eq!(
        c.log().get(ca).unwrap().retired(),
        Some(&Retired::Superseded { by: nr })
    );
    assert!(c.resolve(nr, Outcome::Ok, now + ms(4)));
    let next = c.post(warn("Login expired", "n.s-1.si"), stamp(4), now).id;
    assert!(
        !c.supersede_record(nr, next, now),
        "a story its reporter resolved"
    );
    let asked = c.post(ask("Allow access?"), stamp(5), now).id;
    assert!(c.answer(asked, "Not now", now + ms(5)));
    assert!(!c.supersede_record(asked, next, now), "the owner's answer");
    let entry = c
        .post(
            info("A fact for later").later(crate::need::LaterAsk::person(
                crate::need::Why::OnlyYou(crate::need::Reason::new("only you can")),
                crate::need::Then::step("Do the thing"),
                crate::need::Fingerprint::new("n", "", "later"),
                crate::need::Resurface::WAIT,
            )),
            stamp(6),
            now,
        )
        .id;
    let after = c.post(info("Later still"), stamp(7), now).id;
    assert!(!c.supersede_record(entry, after, now), "an open exception");
    assert!(
        !c.supersede_record(next, c.log().next_id(), now),
        "no such successor"
    );
    assert!(c.live(next).is_some(), "nothing moved");
}

#[test]
fn a_record_fixed_by_name_reads_fixed_and_ends_its_quiet() {
    let now = t0();
    let mut c = fresh(now);
    let warn = |key: &str| {
        use crate::need::{Ask, Reason, StandingWhy, Then};
        Message::new(tags::HARNESS, Severity::Warn, "Login expired")
            .now(Ask::standing(
                StandingWhy::OnlyYou(Reason::new("Claude Code's login expired")),
                Then::step("Type /login in the tab"),
            ))
            .key(key)
    };
    let gone = c.post(warn("n.s-1.nr"), stamp(1_000), now).id;
    assert!(c.withdraw(gone, now + ms(1)));
    assert!(c.fix_record(gone, "Login back", stamp(9_000), now + ms(2)));
    let rec = c.log().get(gone).unwrap();
    assert_eq!(rec.retired(), Some(&Retired::Resolved(Outcome::Ok)));
    assert_eq!(rec.title, "Login back");
    assert_eq!(
        rec.detail,
        [
            "Login expired",
            "Claude Code's login expired",
            "Type /login in the tab"
        ]
    );
    assert_eq!(rec.severity, Severity::Success);
    assert_eq!(rec.retired_unix_ms, Some(9_000));
    assert!(
        !c.fix_record(gone, "Login back", stamp(9_500), now),
        "resolved"
    );
    let read = c.post(warn("n.s-1.si"), stamp(10_000), now).id;
    c.commit_rows(now, 3);
    assert!(c.mark_seen(read, now + ms(3)));
    assert_eq!(
        c.post(warn("n.s-1.si"), stamp(11_000), now).outcome,
        PostOutcome::Suppressed,
        "NEGATIVE CONTROL: the read keeps it quiet"
    );
    assert!(c.fix_record(read, "Login back", stamp(12_000), now + ms(4)));
    assert_eq!(
        c.post(warn("n.s-1.si"), stamp(13_000), now).outcome,
        PostOutcome::New,
        "the next loss is news"
    );
    let live = c.live_by_key("n.s-1.si").unwrap().id;
    assert!(
        !c.fix_record(live, "Login back", stamp(14_000), now),
        "live"
    );
    let told = c.post(warn("n.s-1.ca"), stamp(15_000), now).id;
    assert!(c.supersede_record(live, told, now));
    assert!(
        !c.fix_record(live, "Login back", stamp(16_000), now),
        "replaced"
    );
    assert!(c.withdraw(told, now + ms(5)));
    assert!(!c.fix_record(told, "", stamp(17_000), now), "no words");
    assert!(!c.fix_record(c.log().next_id(), "Login back", stamp(17_000), now));
    let lines = c.log.drain_pending();
    let mut replayed = MessageLog::empty();
    replayed.replay_all(lines.iter().map(|l| LogLine::decode(&l.encode()).unwrap()));
    let rec = replayed.get(gone).expect("the record");
    assert_eq!(rec.state, LogState::Retired(Retired::Resolved(Outcome::Ok)));
    assert_eq!(rec.title, "Login back");
    assert_eq!(rec.severity, Severity::Success);
}

#[test]
fn a_live_row_becomes_its_own_record_in_place() {
    use crate::UpgradeWord::{NotToday, Now, Skip};
    let now = t0();
    let mut c = fresh(now);
    let warn = |age: &str| {
        Message::new(tags::HARNESS, Severity::Warn, "Couldn't upgrade Claude yet")
            .key("harness.upgrade.s-a")
            .line(format!("behind for {age}: a draft waits in its prompt"))
            .hold(Hold::STANDING)
            .action(upgrade_word(NotToday))
            .action(upgrade_word(Skip))
    };
    let asks = |age: &str, words: [crate::UpgradeWord; 2]| {
        words.into_iter().fold(
            info("Couldn't upgrade Claude in its tab yet")
                .key("harness.upgrade.s-a")
                .line(format!("behind for {age}: its own work runs"))
                .hold(Hold::LogOnly),
            |m, w| m.action(upgrade_word(w)),
        )
    };
    let id = c.post(warn("8 h"), stamp(1), now).id;
    c.commit_rows(now, 3);
    assert!(c.keep_as_record(id, &asks("8 h", [NotToday, Skip]), now + ms(1)));
    assert!(c.live(id).is_none(), "off the glass");
    let rec = c.log().get(id).unwrap();
    assert_eq!(rec.state, LogState::Retired(Retired::Recorded));
    assert_eq!(rec.title, "Couldn't upgrade Claude in its tab yet");
    assert_eq!(rec.detail, vec!["behind for 8 h: its own work runs"]);
    assert_eq!(rec.severity, Severity::Info, "the record's mark");
    assert!(!c.keep_as_record(id, &asks("9 h", [NotToday, Skip]), now + ms(2)));

    let other = c
        .post(warn("8 h").key("harness.upgrade.s-b"), stamp(2), now)
        .id;
    assert!(c.keep_as_record(other, &asks("8 h", [Now, NotToday]), now + ms(3)));
    let rec = c.log().get(other).unwrap();
    assert_eq!(rec.state, LogState::Retired(Retired::Recorded));
    assert_eq!(
        rec.detail,
        vec!["behind for 8 h: a draft waits in its prompt"]
    );
    assert_eq!(
        rec.severity,
        Severity::Info,
        "the record's mark all the same"
    );
}

#[test]
fn a_retired_live_rows_record_names_only_the_outcome_it_claimed() {
    let now = t0();
    let mut c = fresh(now);
    let live = || {
        Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab tools").hold(Hold::Live {
            stale_after: crate::STALE_TAILED,
        })
    };
    let post =
        |c: &mut MessageCenter, n: u64| c.post(live().key(&format!("pass.{n}")), stamp(n), now).id;
    let plain = post(&mut c, 1);
    let complete = post(&mut c, 2);
    let fault = post(&mut c, 3);
    let warned = post(&mut c, 4);
    let delivered = post(&mut c, 5);
    c.commit_rows(now, 3);
    assert!(c.withdraw(plain, now + ms(1)));
    assert!(c.withdraw_with(complete, EchoKind::Complete, now + ms(2)));
    assert!(c.withdraw_with(fault, EchoKind::Fault, now + ms(3)));
    assert!(c.resolve(warned, Outcome::Warn, now + ms(4)));
    assert!(c.resolve(delivered, Outcome::Ok, now + ms(5)));
    let stale = post(&mut c, 6);
    let settled = c.settle(now + crate::STALE_TAILED + ms(1), true);
    assert_eq!(settled.retired, vec![(stale, Retired::Stale)]);
    let warning = c
        .post(
            Message::new(tags::CONFIG, Severity::Warn, "Misspelled setting")
                .line("scrollbak_lines \u{2192} scrollback_lines")
                .finished_as("Misspelled setting fixed")
                .key("config.aterm"),
            stamp(7),
            now,
        )
        .id;
    assert!(c.resolve(warning, Outcome::Ok, now + crate::STALE_TAILED + ms(2)));
    let ok = (Severity::Success, '\u{2713}');
    let warn = (Severity::Warn, '\u{26a0}');
    let ended = ("Installing ended", Some("Installing ALab tools"));
    let finished = ("Installed ALab tools", None);
    let stopped = ("Installing stopped", Some("Installing ALab tools"));
    let fixed = ("Misspelled setting fixed", Some("Misspelled setting"));
    let lines = c.log.drain_pending();
    let mut file = MessageLog::empty();
    file.replay_all(lines.iter().map(|l| LogLine::decode(&l.encode()).unwrap()));
    for (case, id, want_words, want_mark) in [
        ("plain", plain, ended, (Severity::Info, '\u{2139}')),
        ("complete", complete, finished, ok),
        ("fault", fault, stopped, warn),
        ("warned", warned, stopped, warn),
        ("delivered", delivered, finished, ok),
        ("stale", stale, stopped, warn),
        ("fixed", warning, fixed, ok),
    ] {
        for (log, side) in [(c.log(), "ring"), (&file, "file")] {
            let rec = log.get(id).unwrap();
            let words = (rec.title.as_str(), rec.detail.first().map(String::as_str));
            assert_eq!(words, want_words, "{case} ({side})");
            assert_eq!((rec.severity, rec.glyph.ch()), want_mark, "{case} ({side})");
        }
    }
}

#[test]
fn a_row_pushed_off_glass_by_a_warn_does_not_burn_its_hold() {
    let now = t0();
    let mut c = fresh(now);
    let r: Vec<MessageId> = (0..3)
        .map(|i| c.post(info(&format!("r{i}")), stamp(i), now + ms(i)).id)
        .collect();
    c.commit_rows(now + ms(3), 3);
    assert_eq!(glass_ids(&c), r);
    let warn = c
        .post(
            Message::new(tags::UPDATE, Severity::Warn, "Update failed"),
            stamp(9),
            now + ms(10),
        )
        .id;
    c.commit_rows(now + ms(10), 3);
    assert_eq!(
        glass_ids(&c),
        vec![warn, r[0]],
        "two slots plus the overflow row"
    );
    assert_eq!(c.queued(), 2);
    for id in [r[1], r[2]] {
        let row = c.live(id).unwrap();
        assert!(!glass_ids(&c).contains(&id));
        assert!(
            row.is_queued() || row.fold_at.is_none(),
            "{id}: off glass yet is_queued() is {} and its hold still runs (fold_at {:?})",
            row.is_queued(),
            row.fold_at
        );
    }
    let settled = c.settle(now + ms(3) + HOLD_INFO, true);
    for (id, how) in &settled.retired {
        assert!(
            *id == r[0] || *how != Retired::Folded,
            "{id} folded as {how:?} after spending {:?} of its {HOLD_INFO:?} hold off glass",
            HOLD_INFO - ms(7)
        );
    }
}

#[test]
fn settle_reports_the_plus_n_capsule_change() {
    let now = t0();
    let mut c = fresh(now);
    let a = c.post(info("a").hold(Hold::STANDING), stamp(1), now).id;
    c.post(
        Message::new(tags::PACKAGES, Severity::Success, "b"),
        stamp(2),
        now,
    );
    c.post(
        Message::new(tags::PACKAGES, Severity::Success, "c"),
        stamp(3),
        now,
    );
    c.commit_rows(now, 1);
    assert_eq!(glass_ids(&c), vec![a]);
    let before = c.presentation(80, &char_width, Links::Painted);
    assert_eq!(before.rows[0].capsules.last().unwrap().text, "+2 \u{203a}");
    let settled = c.settle(now + OVERFLOW_PATIENCE_MIN, true);
    assert_eq!(settled.retired.len(), 2);
    let after = c.presentation(80, &char_width, Links::Painted);
    assert_ne!(before, after, "the capsule now reads Details ›");
    assert!(
        settled.glass_changed,
        "the visible capsule changed ({:?} → {:?}) but glass_changed is false",
        before.rows[0].capsules.last().unwrap().text,
        after.rows[0].capsules.last().unwrap().text
    );
}
