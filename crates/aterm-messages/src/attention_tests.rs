// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for attention and motion invariants.

use crate::animate::{Anim, Look, Pace, Tone};
use crate::center::{EchoKind, MessageCenter, Outcome};
use crate::glass::{Links, Presentation, RowKind};
use crate::log::{LogLine, LogState, MessageLog, Retired};
use crate::model::{
    Amount, Hold, Intent, Load, Loads, Message, MessageId, Meter, Restatement, Severity, Unit,
    WallStamp, tags,
};
use crate::text::char_width;
use crate::{
    ANIM_FRAME, COMET_PERIOD, Duration, HOLD_WARN, Instant, LOAD_AFTER, PROGRESS_GRACE,
    SHRINK_QUIET, STALE_TAILED, STALE_UPDATE,
};

fn t0() -> Instant {
    Instant::now()
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn stamp() -> WallStamp {
    WallStamp { unix_ms: 1 }
}

fn fresh(now: Instant) -> MessageCenter {
    MessageCenter::new(MessageLog::empty(), now)
}

fn warn(title: &str) -> Message {
    Message::new(tags::CONFIG, Severity::Warn, title)
}

fn download(done: u64) -> Message {
    let total = 74_000_000;
    Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.92.0")
        .meter(Meter {
            fill_permille: None,
            stats: format!("{} MB / 74 MB", done / 1_000_000),
            amount: Some(Amount {
                series: Amount::series_of("aterm 0.92.0"),
                done,
                total,
                unit: Unit::Bytes,
            }),
            load: None,
            busy: false,
        })
        .hold(Hold::Live {
            stale_after: STALE_UPDATE,
        })
        .key("update.progress")
}

fn busy(title: &str) -> Message {
    Message::new(tags::PACKAGES, Severity::Info, title)
        .in_flight()
        .hold(Hold::Live {
            stale_after: STALE_TAILED,
        })
}

fn present(c: &MessageCenter, cols: usize) -> Presentation {
    c.presentation(cols, &char_width, None, Links::Painted)
}

fn kinds(p: &Presentation) -> Vec<RowKind> {
    p.rows.iter().map(|r| r.kind).collect()
}

#[test]
fn a_row_with_reveal_after_is_not_eligible_until_its_grace() {
    let now = t0();
    let mut c = fresh(now);
    let id = c
        .post(download(1).reveal_after(PROGRESS_GRACE), stamp(), now)
        .id;
    assert!(c.live(id).is_some(), "live, restated and resolved as any");
    assert!(!c.live(id).unwrap().revealed);
    assert_eq!(c.eligible(), 0);
    assert_eq!(c.wanted_rows(), 0);
    assert_eq!(c.commit_rows(now, 3), None, "nothing to show yet");
    assert_eq!(c.fingerprint(80), 0);
    let settled = c.settle(now + PROGRESS_GRACE - ms(1), true);
    assert!(!settled.glass_changed);
    assert_eq!(c.eligible(), 0);
    let settled = c.settle(now + PROGRESS_GRACE, true);
    assert!(settled.glass_changed, "revealed");
    assert_eq!(c.eligible(), 1);
    assert_eq!(c.commit_rows(now + PROGRESS_GRACE, 3), Some(1));
    assert_eq!(c.on_glass().next().map(|l| l.id), Some(id));
}

#[test]
fn a_supersede_keeps_the_earlier_reveal() {
    let now = t0();
    let mut c = fresh(now);
    let first = c
        .post(download(1).reveal_after(PROGRESS_GRACE), stamp(), now)
        .id;
    let verifying = Message::new(tags::UPDATE, Severity::Info, "Verifying aterm v0.92.0")
        .in_flight()
        .hold(Hold::Live {
            stale_after: STALE_UPDATE,
        })
        .key("update.progress")
        .reveal_after(PROGRESS_GRACE);
    let second = c.post(verifying, stamp(), now + ms(1500)).id;
    assert!(c.live(first).is_none());
    assert_eq!(
        c.live(second).unwrap().reveal_at,
        Some(now + PROGRESS_GRACE),
        "the first clock, not a fresh grace"
    );
    c.settle(now + PROGRESS_GRACE, true);
    assert!(c.live(second).unwrap().revealed);
    let third = c
        .post(
            download(2).reveal_after(PROGRESS_GRACE),
            stamp(),
            now + ms(2500),
        )
        .id;
    assert!(c.live(third).unwrap().revealed);
}

#[test]
fn a_row_resolved_inside_its_grace_never_regrids_and_leaves_no_echo() {
    let now = t0();
    let mut c = fresh(now);
    let id = c
        .post(download(1).reveal_after(PROGRESS_GRACE), stamp(), now)
        .id;
    assert_eq!(c.commit_rows(now, 3), None);
    assert!(c.resolve(id, Outcome::Ok, now + ms(900)));
    assert_eq!(c.commit_rows(now + ms(900), 3), None, "never a re-grid");
    assert!(c.echoes().is_empty(), "no echo for a row nobody saw");
    assert_eq!(
        c.log().get(id).unwrap().state,
        LogState::Retired(Retired::Resolved(Outcome::Ok)),
        "the record says it happened"
    );
    assert_eq!(c.deadline(true), None);
}

#[test]
fn an_unrevealed_row_moves_neither_the_overflow_row_nor_the_plus_count() {
    let now = t0();
    let mut c = fresh(now);
    let ids: Vec<MessageId> = (0..3)
        .map(|i| {
            c.post(warn(&format!("w{i}")).hold(Hold::STANDING), stamp(), now)
                .id
        })
        .collect();
    c.commit_rows(now, 3);
    let before = present(&c, 80);
    let fp = c.fingerprint(80);
    c.post(download(1).reveal_after(PROGRESS_GRACE), stamp(), now);
    assert_eq!(present(&c, 80), before, "no overflow row, nothing moved");
    assert_eq!(c.fingerprint(80), fp);
    assert_eq!(c.queued(), 0);
    assert_eq!(c.on_glass().map(|l| l.id).collect::<Vec<_>>(), ids);
    let mut one = fresh(now);
    one.post(warn("w").hold(Hold::STANDING), stamp(), now);
    one.commit_rows(now, 1);
    let before = present(&one, 80);
    one.post(download(1).reveal_after(PROGRESS_GRACE), stamp(), now);
    assert_eq!(
        present(&one, 80),
        before,
        "the Details capsule reads Details"
    );
    assert_eq!(
        before.rows[0].capsules.last().unwrap().text,
        "Details \u{203a}"
    );
    one.settle(now + PROGRESS_GRACE, true);
    assert_eq!(
        present(&one, 80).rows[0].capsules.last().unwrap().text,
        "+1 \u{203a}"
    );
}

#[test]
fn deadline_names_the_reveal_instant() {
    let now = t0();
    let mut c = fresh(now);
    c.post(download(1).reveal_after(PROGRESS_GRACE), stamp(), now);
    assert_eq!(c.deadline(true), Some(now + PROGRESS_GRACE));
    assert_eq!(
        c.deadline(false),
        Some(now + PROGRESS_GRACE),
        "a reveal is not a hold: the freeze keeps it"
    );
}

#[test]
fn mark_seen_folds_a_standing_row() {
    let now = t0();
    let mut c = fresh(now);
    let standing = c
        .post(
            warn("Auto-update failing").hold(Hold::STANDING),
            stamp(),
            now,
        )
        .id;
    let live = c.post(busy("Installing Homebrew"), stamp(), now).id;
    let ask = c
        .post(
            warn("asked")
                .action(Intent::NotNow {
                    decision: crate::model::Decision::FileAccess,
                })
                .hold(Hold::asking(crate::HOLD_ASK)),
            stamp(),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    assert!(c.mark_seen(standing, now + ms(1)));
    assert!(c.live(standing).is_none(), "read: folded");
    assert_eq!(
        c.log().get(standing).unwrap().state,
        LogState::Retired(Retired::Folded)
    );
    assert!(c.mark_seen(live, now + ms(2)) && c.live(live).is_some());
    assert!(c.mark_seen(ask, now + ms(3)) && c.live(ask).is_some());
}

#[test]
fn a_record_retires_recorded() {
    let now = t0();
    let mut c = fresh(now);
    let id = c
        .post(warn("a record").hold(Hold::LogOnly), stamp(), now)
        .id;
    assert_eq!(
        c.log().get(id).unwrap().state,
        LogState::Retired(Retired::Recorded)
    );
    assert_eq!(Retired::Recorded.as_word(), "recorded");
    let lines = c.drain_new_for_persist();
    let retired = lines
        .iter()
        .find(|l| matches!(l, LogLine::Retired { .. }))
        .expect("the record's line");
    let enc = retired.encode();
    assert!(
        enc.contains("\thow=folded\trec=1\t"),
        "spelled as the fold an older build wrote: {enc}"
    );
    assert_eq!(&LogLine::decode(&enc).unwrap(), retired, "round trip");
    let older = enc.replace("\trec=1", "");
    match LogLine::decode(&older).unwrap() {
        LogLine::Retired { how, .. } => assert_eq!(how, Retired::Folded),
        other => panic!("{other:?}"),
    }
    let mut log = MessageLog::empty();
    for line in &lines {
        log.replay(line.clone());
    }
    assert_eq!(
        log.get(id).unwrap().state,
        LogState::Retired(Retired::Recorded)
    );
    let row = c.post(warn("on glass"), stamp(), now).id;
    c.commit_rows(now, 3);
    c.restate(
        row,
        Restatement {
            hold: Some(Hold::LogOnly),
            ..Restatement::default()
        },
        now,
    );
    assert_eq!(
        c.log().get(row).unwrap().state,
        LogState::Retired(Retired::Folded)
    );
}

#[test]
fn no_excerpt_rows_lay_out_and_speak_without_detail0() {
    let now = t0();
    let mut c = fresh(now);
    let id = c
        .post(
            warn("aterm crashed last time")
                .line("crash log at /x/crash.log")
                .no_excerpt(),
            stamp(),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    let p = present(&c, 160);
    assert_eq!(p.rows[0].detail, None, "the title alone");
    assert_eq!(p.rows[0].spoken(""), "aterm crashed last time");
    assert_eq!(
        c.live(id).unwrap().msg.detail,
        vec!["crash log at /x/crash.log".to_string()],
        "the line still rides Details and the log"
    );
    let before = c.fingerprint(160);
    c.restate(
        id,
        Restatement {
            excerpt: Some(true),
            ..Restatement::default()
        },
        now,
    );
    assert!(present(&c, 160).rows[0].detail.is_some());
    assert_ne!(c.fingerprint(160), before);
}

#[test]
fn a_carried_row_keeps_its_excerpt_flag() {
    let now = t0();
    let mut parent = fresh(now);
    let id = parent
        .post(warn("quiet").line("hidden").no_excerpt(), stamp(), now)
        .id;
    parent.commit_rows(now, 3);
    let carry = parent.carried();
    assert!(!carry.live[0].excerpt);
    let mut child = fresh(now);
    child.seed_carried(&carry, stamp(), now);
    assert!(!child.live(id).unwrap().msg.excerpt);
    assert_eq!(present(&child, 160).rows[0].detail, None);
    let mut parent = fresh(now);
    parent.post(download(1).reveal_after(PROGRESS_GRACE), stamp(), now);
    assert!(parent.carried().live.is_empty());
}

#[test]
fn the_indicator_is_the_meters_state_never_the_holds() {
    let now = t0();
    let mut c = fresh(now);
    let blocked = c
        .post(
            Message::new(tags::UPDATE, Severity::Info, "Close the editor to finish").hold(
                Hold::Live {
                    stale_after: STALE_UPDATE,
                },
            ),
            stamp(),
            now,
        )
        .id;
    assert_eq!(c.live(blocked).unwrap().msg.meter, None, "nothing invented");
    assert!(!c.live(blocked).unwrap().is_animated());
    let id = c.post(busy("Installing aterm v0.92.0"), stamp(), now).id;
    assert_eq!(c.live(id).unwrap().msg.meter, Some(Meter::busy("")));
    c.commit_rows(now, 3);
    let p = present(&c, 120);
    let row_of = |p: &Presentation, id| {
        p.rows
            .iter()
            .position(|r| r.kind == RowKind::Message(id))
            .unwrap()
    };
    let still = &p.rows[row_of(&p, blocked)];
    assert!(still.meter.is_none() && still.track.is_none() && !still.busy);
    let moving = &p.rows[row_of(&p, id)];
    assert_eq!(moving.track, Some((0, 120)));
    let m = c.motion(&p, now + ms(500), Look::MOVING);
    assert_eq!(
        m.rows[row_of(&p, blocked)],
        crate::animate::RowMotion::default()
    );
    assert!(matches!(m.rows[row_of(&p, id)].anim, Anim::Comet { .. }));
    let other = c.post(busy("other"), stamp(), now).id;
    c.restate(
        other,
        Restatement {
            meter: Some(None),
            ..Restatement::default()
        },
        now,
    );
    assert!(!c.live(other).unwrap().is_animated());
    let carry = c.carried();
    let mut child = fresh(now);
    child.seed_carried(&carry, stamp(), now);
    assert!(child.live(id).unwrap().is_busy());
    assert_eq!(child.live(blocked).unwrap().msg.meter, None);
    child.commit_rows(now, 3);
    let p = present(&child, 120);
    assert_eq!(p.rows[row_of(&p, id)].track, Some((0, 120)));
}

#[test]
fn load_words_show_at_post_and_their_slot_never_moves() {
    let now = t0();
    let mut c = fresh(now);
    let heavy = |fill, load: Option<Load>| {
        Message::new(tags::TOOLCHAIN, Severity::Info, "Installing ALab tools")
            .meter(Meter {
                fill_permille: Some(fill),
                stats: "3 of 10 programs".into(),
                load,
                ..Meter::default()
            })
            .hold(Hold::Live {
                stale_after: STALE_TAILED,
            })
            .key("toolchain.pass")
    };
    let id = c.post(heavy(100, Some(Load::Disk)), stamp(), now).id;
    c.commit_rows(now, 3);
    let first = present(&c, 160).rows[0].clone();
    assert_eq!(
        first.load.map(|(_, w)| w),
        Some("disk busy"),
        "from the first frame"
    );
    assert_eq!(
        c.deadline(true),
        Some(now + STALE_TAILED),
        "no load wake: the words are already up"
    );
    let unmoved = |c: &MessageCenter, what: &str| {
        let row = present(c, 160).rows[0].clone();
        assert_eq!(row.load_slot, first.load_slot, "{what}: the slot");
        assert_eq!(
            row.meter.map(|m| (m.0, m.1)),
            Some((0, 160)),
            "{what}: the meter"
        );
        assert_eq!(
            row.stats.as_ref().map(|s| s.0),
            first.stats.as_ref().map(|s| s.0),
            "{what}: the stats"
        );
        row.load.map(|(_, w)| w)
    };
    let restate = |c: &mut MessageCenter, load: Option<Load>, at: Duration| {
        c.restate(
            id,
            Restatement {
                meter: Some(heavy(200, load).meter),
                ..Restatement::default()
            },
            now + at,
        );
    };
    restate(&mut c, Some(Load::Disk), ms(500));
    assert_eq!(unmoved(&c, "the same load"), Some("disk busy"));
    restate(&mut c, Some(Load::Network), ms(1000));
    assert_eq!(
        unmoved(&c, "a resource change"),
        Some("network busy"),
        "the words swap at once"
    );
    restate(&mut c, None, ms(2000));
    assert_eq!(
        unmoved(&c, "the load ending"),
        None,
        "the words leave with it"
    );
    restate(&mut c, Some(Load::Disk), ms(3000));
    assert_eq!(
        unmoved(&c, "a load after a lull"),
        None,
        "a load declared after none waits"
    );
    assert_eq!(c.deadline(true), Some(now + ms(3000) + LOAD_AFTER));
    let settled = c.settle(now + ms(3000) + LOAD_AFTER, true);
    assert!(settled.glass_changed);
    assert_eq!(unmoved(&c, "the latch"), Some("disk busy"));
}

#[test]
fn the_load_slot_is_reserved_for_the_declared_loads() {
    let now = t0();
    let mut c = fresh(now);
    let gauge = |load: Option<Load>| Meter {
        load,
        ..Meter::busy("3 of 10 targets")
    };
    let row = |declared: &[Load]| {
        Message::new(tags::SYSTEM, Severity::Info, "Building aterm")
            .meter(gauge(Some(Load::Cpu)))
            .loads(declared.iter().copied())
            .hold(Hold::Live {
                stale_after: STALE_TAILED,
            })
    };
    let id = c.post(row(&[Load::Cpu]), stamp(), now).id;
    c.commit_rows(now, 3);
    let slot = |c: &MessageCenter| present(c, 80).rows[0].load_slot.map(|(_, w)| w);
    let stats = |c: &MessageCenter| present(c, 80).rows[0].stats.clone().map(|s| s.1);
    assert_eq!(c.live(id).unwrap().load_slot, Loads::NONE.with(Load::Cpu));
    assert_eq!(slot(&c), Some("CPU busy".chars().count()));
    assert_eq!(stats(&c).as_deref(), Some("3 of 10 targets"), "stats at 80");
    let restate = |c: &mut MessageCenter, load: Option<Load>, at: Duration| {
        c.restate(
            id,
            Restatement {
                meter: Some(Some(gauge(load))),
                ..Restatement::default()
            },
            now + at,
        );
    };
    restate(&mut c, None, ms(500));
    assert_eq!(
        slot(&c),
        Some("CPU busy".chars().count()),
        "the words leave"
    );
    restate(&mut c, Some(Load::Cpu), ms(1000));
    assert_eq!(slot(&c), Some("CPU busy".chars().count()), "and come back");
    restate(&mut c, Some(Load::Network), ms(1500));
    assert_eq!(
        c.live(id).unwrap().load_slot,
        [Load::Cpu, Load::Network].into_iter().collect::<Loads>(),
        "a load outside the declaration widens it once"
    );
    assert_eq!(slot(&c), Some("network busy".chars().count()));
    let mut u = fresh(now);
    let other = u.post(row(&[]), stamp(), now).id;
    u.commit_rows(now, 3);
    assert_eq!(u.live(other).unwrap().load_slot, Loads::ALL);
    assert_eq!(
        present(&u, 80).rows[0].load_slot.map(|(_, w)| w),
        Some("network busy".chars().count())
    );
    assert_eq!(
        present(&u, 80).rows[0]
            .stats
            .clone()
            .map(|s| s.1)
            .as_deref(),
        Some("3 of 10 targets"),
        "80: the count, whole"
    );
}

#[test]
fn a_resolved_live_row_echoes_in_its_slot_then_frees_it() {
    let now = t0();
    let mut c = fresh(now);
    let a = c.post(busy("a"), stamp(), now).id;
    let b = c.post(busy("b"), stamp(), now + ms(1)).id;
    let z = c.post(busy("z"), stamp(), now + ms(2)).id;
    c.commit_rows(now + ms(2), 3);
    assert_eq!(
        kinds(&present(&c, 80)),
        [
            RowKind::Message(a),
            RowKind::Message(b),
            RowKind::Message(z)
        ]
    );
    let at = now + ms(100);
    assert!(c.resolve(b, Outcome::Ok, at));
    assert!(c.live(b).is_none(), "gone at once for every watcher");
    assert_eq!(
        c.log().get(b).unwrap().state,
        LogState::Retired(Retired::Resolved(Outcome::Ok))
    );
    assert_eq!(
        kinds(&present(&c, 80)),
        [RowKind::Message(a), RowKind::Echo(b), RowKind::Message(z)],
        "the survivor holds its place"
    );
    assert_eq!(c.glass_position(z), Some(2));
    let echo = &c.echoes()[0];
    assert_eq!(echo.kind, EchoKind::Complete);
    assert_eq!(echo.until, at + EchoKind::Complete.span(echo.from_permille));
    assert_eq!(c.deadline(true), Some(echo.until));
    let until = echo.until;
    assert!(!c.settle(until - ms(1), true).glass_changed);
    let settled = c.settle(until, true);
    assert!(settled.glass_changed, "the echo ended");
    assert_eq!(
        kinds(&present(&c, 80)),
        [RowKind::Message(a), RowKind::Message(z)],
        "then it moves up"
    );
    assert_eq!(c.glass_position(z), Some(1));
    assert!(c.resolve(z, Outcome::Warn, until));
    assert_eq!(c.echoes()[0].kind, EchoKind::Fault);
    assert!(c.withdraw(a, until));
    assert!(c.echoes().iter().any(|e| e.kind == EchoKind::Vanish));
}

#[test]
fn a_fault_echo_wears_the_warn_glyph_and_says_failed() {
    for (look, name) in [(Look::MOVING, "moving"), (Look::STILL, "still")] {
        let now = t0();
        let mut c = fresh(now);
        let dl = c.post(download(30_000_000), stamp(), now).id;
        let comet = c.post(busy("Installing Homebrew"), stamp(), now).id;
        c.commit_rows(now, 3);
        let before = present(&c, 120);
        let at = now + ms(20_000);
        assert!(c.resolve(dl, Outcome::Warn, at));
        assert!(c.withdraw_with(comet, EchoKind::Fault, at));
        let p = present(&c, 120);
        assert_eq!(
            p.rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
            [RowKind::Echo(dl), RowKind::Echo(comet)]
        );
        for (row, was) in p.rows.iter().zip(&before.rows) {
            assert_eq!(
                (row.meter, row.eta, row.elapsed),
                (was.meter, was.eta, was.elapsed),
                "{name}: laid out as the row was"
            );
        }
        for k in [0u64, 100, 400, 550] {
            let m = c.motion(&p, at + ms(k), look);
            for row in &m.rows {
                assert_eq!(row.glyph, Some('\u{26a0}'), "{name}@{k}: the glyph");
            }
            assert_eq!(m.rows[0].eta.as_deref(), Some("failed"), "{name}@{k}");
            assert_eq!(m.rows[1].readout.as_deref(), Some("failed"), "{name}@{k}");
        }
    }
    for look in [Look::MOVING, Look::STILL] {
        let now = t0();
        let mut c = fresh(now);
        let dl = c.post(download(30_000_000), stamp(), now).id;
        let comet = c.post(busy("Installing Homebrew"), stamp(), now).id;
        c.commit_rows(now, 3);
        let at = now + ms(20_000);
        assert!(c.resolve(dl, Outcome::Ok, at));
        assert!(c.resolve(comet, Outcome::Ok, at));
        let p = present(&c, 120);
        assert!(p.rows[0].eta.is_some() && p.rows[1].elapsed.is_some());
        for k in [0u64, 100, 400, 800] {
            let m = c.motion(&p, at + ms(k), look);
            for row in &m.rows {
                assert_eq!(row.glyph, Some('\u{2713}'), "{look:?}@{k}: the glyph");
            }
            assert_eq!(m.rows[0].eta, None, "@{k}");
            assert_eq!(m.rows[1].readout, None, "@{k}");
        }
        assert_eq!(p.rows[0].pct, None, "no `100%` on a Complete echo");
        assert!(
            p.rows
                .iter()
                .flat_map(|r| &r.capsules)
                .all(|cap| cap.full_label.is_empty() && cap.text.trim().is_empty()),
            "an echo's capsules draw nothing"
        );
    }
    let now = t0();
    let mut c = fresh(now);
    let dl = c.post(download(30_000_000), stamp(), now).id;
    let comet = c.post(busy("Installing Homebrew"), stamp(), now).id;
    c.commit_rows(now, 3);
    let at = now + ms(20_000);
    assert!(c.withdraw(dl, at));
    assert!(c.withdraw(comet, at));
    let p = present(&c, 120);
    let m = c.motion(&p, at + ms(100), Look::MOVING);
    assert_eq!((m.rows[0].glyph, m.rows[0].eta.as_deref()), (None, None));
    assert_eq!(m.rows[1].readout.as_deref(), Some("for 20 s"));
}

#[test]
fn echoes_never_change_the_row_count() {
    let now = t0();
    let run = |row: Message, echoes: usize| -> Vec<Option<u16>> {
        let mut c = fresh(now);
        let id = c.post(row, stamp(), now).id;
        let other = c.post(warn("other").hold(Hold::STANDING), stamp(), now).id;
        let mut answers = vec![c.commit_rows(now, 3)];
        c.resolve(id, Outcome::Ok, now + ms(10));
        assert_eq!(c.echoes().len(), echoes);
        for step in 0..40u64 {
            let at = now + ms(10 + step * 50);
            c.settle(at, true);
            answers.push(c.commit_rows(at, 3));
            assert_eq!(c.wanted_rows(), 1, "the echo is never wanted");
        }
        c.resolve(other, Outcome::Ok, now + ms(3000));
        answers.push(c.commit_rows(now + ms(3000), 3));
        answers.push(c.commit_rows(now + ms(3000) + SHRINK_QUIET, 3));
        answers
    };
    let held = run(warn("held"), 0);
    assert_eq!(held[31], Some(1), "the held run shrinks at step 30 (+1510)");
    assert_eq!(run(download(51_800_000), 1), held);
    let mut late = held.clone();
    late.swap(31, 34);
    assert_eq!(run(busy("work"), 1), late);
    let mut c = fresh(now);
    c.post(warn("top").hold(Hold::STANDING), stamp(), now);
    let id = c.post(busy("work"), stamp(), now).id;
    c.commit_rows(now, 3);
    c.resolve(id, Outcome::Ok, now + ms(1));
    assert_eq!(c.echoes().len(), 1);
    assert_eq!(c.commit_rows(now + ms(2), 1), Some(1));
    assert!(c.echoes().is_empty(), "the echo's slot is gone");
}

#[test]
fn withdraw_with_keeps_the_withdrawn_record_and_shows_the_asked_echo() {
    let now = t0();
    let mut c = fresh(now);
    let id = c.post(busy("Installing ALab tools"), stamp(), now).id;
    c.commit_rows(now, 3);
    assert!(c.withdraw_with(id, EchoKind::Complete, now + ms(1)));
    assert_eq!(
        c.log().get(id).unwrap().state,
        LogState::Retired(Retired::Withdrawn),
        "no outcome on appstatus (ruling 13)"
    );
    assert_eq!(c.echoes()[0].kind, EchoKind::Complete);
    assert!(!c.withdraw_with(id, EchoKind::Fault, now + ms(2)), "gone");
}

#[test]
fn only_live_revealed_rows_on_glass_echo() {
    let now = t0();
    let mut c = fresh(now);
    let held = c
        .post(warn("held").hold(Hold::For(HOLD_WARN)), stamp(), now)
        .id;
    let standing = c
        .post(warn("standing").hold(Hold::STANDING), stamp(), now)
        .id;
    let ask = c
        .post(
            warn("ask").hold(Hold::asking(crate::HOLD_ASK)),
            stamp(),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    for id in [held, standing, ask] {
        assert!(c.resolve(id, Outcome::Ok, now + ms(5)));
        assert!(c.echoes().is_empty(), "{id}: no echo");
    }
    let mut c = fresh(now);
    let on_glass = c
        .post(warn("held").hold(Hold::For(HOLD_WARN)), stamp(), now)
        .id;
    c.commit_rows(now, 1);
    let queued = c.post(busy("queued"), stamp(), now).id;
    let hidden = c
        .post(busy("hidden").reveal_after(PROGRESS_GRACE), stamp(), now)
        .id;
    assert!(c.live(queued).unwrap().is_queued());
    for id in [queued, hidden, on_glass] {
        assert!(c.resolve(id, Outcome::Ok, now + ms(5)));
        assert!(c.echoes().is_empty(), "{id}: no echo");
    }
}

#[test]
fn a_change_of_activity_restarts_the_motion_epoch() {
    let now = t0();
    let mut c = fresh(now);
    let staged = warn("aterm v0.92.0 is ready").key("update.progress");
    let id = c.post(staged, stamp(), now).id;
    c.commit_rows(now, 3);
    assert_eq!(c.live(id).unwrap().motion_since, Some(now));
    let later = now + ms(5000);
    c.restate(
        id,
        Restatement {
            hold: Some(Hold::Live {
                stale_after: STALE_UPDATE,
            }),
            meter: Some(Some(Meter::busy(""))),
            ..Restatement::default()
        },
        later,
    );
    assert_eq!(c.live(id).unwrap().activity(), 2);
    assert_eq!(c.live(id).unwrap().motion_since, Some(later));
    c.restate(
        id,
        Restatement {
            title: Some("Installing aterm v0.92.0".into()),
            ..Restatement::default()
        },
        later + ms(100),
    );
    assert_eq!(c.live(id).unwrap().motion_since, Some(later));
    let dl = c.post(download(1), stamp(), later + ms(200)).id;
    let epoch = c.live(dl).unwrap().motion_since;
    let verifying = busy("Verifying aterm v0.92.0").key("update.progress");
    let v = c.post(verifying, stamp(), later + ms(900)).id;
    assert_ne!(c.live(v).unwrap().motion_since, epoch);
    assert_eq!(c.live(v).unwrap().motion_since, Some(later + ms(900)));
}

#[test]
fn a_row_the_band_grows_past_keeps_its_motion_epoch() {
    let now = t0();
    let mut c = fresh(now);
    let comet = c.post(busy("Installing ALab tools"), stamp(), now).id;
    let held = c.post(warn("Font not found"), stamp(), now).id;
    c.commit_rows(now, 3);
    assert!(c.glass_position(comet).is_some() && c.glass_position(held).is_some());
    let epoch = c.live(comet).unwrap().motion_since;
    assert_eq!(epoch, Some(now));
    let later = now + ms(28_500);
    let dl = c
        .post(
            download(1).reveal_after(PROGRESS_GRACE),
            stamp(),
            now + ms(100),
        )
        .id;
    c.settle(later, true);
    c.commit_rows(later, 3);
    assert!(c.glass_position(dl).is_some(), "the third row is up");
    assert_eq!(
        c.live(comet).unwrap().motion_since,
        epoch,
        "the comet keeps its phase"
    );
}

fn scene(now: Instant) -> (MessageCenter, Presentation) {
    let mut c = fresh(now);
    c.post(busy("Installing Homebrew"), stamp(), now);
    let dl = c.post(download(30_000_000), stamp(), now).id;
    let done = c.post(busy("done soon"), stamp(), now).id;
    c.commit_rows(now, 3);
    c.restate(
        dl,
        Restatement {
            meter: Some(download(40_000_000).meter),
            ..Restatement::default()
        },
        now + ms(1000),
    );
    c.resolve(done, Outcome::Ok, now + ms(1500));
    let p = present(&c, 120);
    (c, p)
}

#[test]
fn motion_is_a_pure_function_of_the_frame_instant() {
    let now = t0();
    let (c, p) = scene(now);
    let (c2, p2) = scene(now);
    for k in 0..200u64 {
        let t = now + ms(k * 17);
        let a = c.motion(&p, t, Look::MOVING);
        assert_eq!(a, c2.motion(&p2, t, Look::MOVING), "same state, same frame");
        let q = c.frame_instant(t);
        let fine = c.fine_instant(t).max(q);
        assert!(a.at == q || a.at == fine, "{k}");
        assert_eq!(
            c.motion(&p, a.at, Look::MOVING).rows,
            a.rows,
            "the frame is its instant's"
        );
        let step = (q + ANIM_FRAME).min(c.fine_instant(t) + c.refresh());
        if t + ms(10) < step {
            assert_eq!(
                c.motion(&p, t + ms(10), Look::MOVING).rows,
                a.rows,
                "t and t + 10 ms in one step give one frame"
            );
        }
    }
}

#[test]
fn the_comet_head_moves_forward_calmly_on_every_frame() {
    let mut prev: Option<i64> = None;
    let mut t = Duration::ZERO;
    while t < COMET_PERIOD {
        let x = crate::animate::comet_head(t);
        if let Some(p) = prev {
            assert!(x > p, "backwards or still at {t:?}");
            assert!(
                x - p <= i64::from(crate::ROW) / 60,
                "{} of the row in a frame at {t:?}",
                x - p
            );
        }
        prev = Some(x);
        t += ANIM_FRAME;
    }
    for cols in [80usize, 120, 200] {
        let mut prev_head: Option<usize> = None;
        let mut t = ANIM_FRAME;
        while t < COMET_PERIOD - ANIM_FRAME {
            let cells = crate::animate::comet(t, true).cells(cols);
            let head = cells.iter().rposition(|c| c.fill > 0);
            if let (Some(p), Some(h)) = (prev_head, head) {
                assert!(h >= p, "{cols}: backwards at {t:?}");
                assert!(
                    h - p <= 1 + cols / 60,
                    "{cols}: jumped {} cells at {t:?}",
                    h - p
                );
            }
            if head.is_some() {
                prev_head = head;
            }
            t += ANIM_FRAME;
        }
    }
}

#[test]
fn the_comet_never_leaves_the_row_empty_and_never_rests() {
    for cols in [40usize, 80, 120, 200] {
        let mut t = ANIM_FRAME;
        while t < COMET_PERIOD * 4 {
            let into = t.as_millis() % COMET_PERIOD.as_millis();
            let handover = into < ANIM_FRAME.as_millis()
                || into > COMET_PERIOD.as_millis() - ANIM_FRAME.as_millis();
            let cells = crate::animate::comet(t, true).cells(cols);
            assert!(
                handover || cells.iter().any(|c| c.fill > 0),
                "{cols}: an empty row at {t:?}"
            );
            t += ANIM_FRAME;
        }
    }
    let now = t0();
    let mut c = fresh(now);
    c.post(busy("Installing Homebrew"), stamp(), now);
    c.commit_rows(now, 3);
    let p = present(&c, 120);
    for k in [100u64, 1000, 2990, 3100, 3700, 9000] {
        let at = now + ms(k);
        let m = c.motion(&p, at, Look::MOVING);
        assert!(
            matches!(m.rows[0].anim, Anim::Comet { .. }),
            "{k}: {:?}",
            m.rows[0].anim
        );
        assert_eq!(
            m.rows[0].glyph, None,
            "{k}: the row's own glyph, no spinner"
        );
        let d = c.motion_deadline(&p, at, Look::MOVING).unwrap();
        assert_eq!(d, c.frame_instant(at) + ANIM_FRAME, "{k}: the next frame");
    }
    let m = c.motion(&p, now + ms(500), Look::STILL);
    assert_eq!((m.rows[0].anim, m.rows[0].glyph), (Anim::Track, None));
    assert!(
        m.rows[0]
            .surface
            .stops
            .iter()
            .all(|s| s.tone == Tone::TRACK)
    );
}

#[test]
fn the_glint_never_lifts_the_track() {
    for shown in [100u16, 380, 620, 999] {
        for k in 0..60u64 {
            let s = crate::animate::bar(shown, Some(ANIM_FRAME * u32::try_from(k).unwrap()), true);
            for stop in &s.stops {
                assert!(
                    stop.tone.fill == 255 || stop.tone.lift == 0,
                    "{shown}/{k}: the glint lifted the track: {stop:?}"
                );
            }
        }
    }
    let lifted = (0..48u64).any(|k| {
        crate::animate::bar(620, Some(ANIM_FRAME * u32::try_from(k).unwrap()), true)
            .stops
            .iter()
            .any(|s| s.tone.lift > 0)
    });
    assert!(lifted);
}

#[test]
fn still_forms_have_no_time_dependence() {
    let now = t0();
    let (c, p) = scene(now);
    let a = c.motion(&p, now + ms(2000), Look::STILL);
    let b = c.motion(&p, now + ms(9000), Look::STILL);
    for (x, y) in a.rows.iter().zip(&b.rows) {
        if !matches!(x.anim, Anim::Echo { .. }) {
            assert_eq!(x.surface, y.surface, "a still surface never moves");
        }
    }
    let stalled = c.motion(&p, now + Duration::from_secs(3600), Look::STILL);
    assert!(
        stalled.rows[1]
            .surface
            .stops
            .iter()
            .any(|s| s.tone == Tone::STALLED),
        "an hour without bytes: the still bar wears the stalled tone"
    );
    assert_eq!(a.rows[0].anim, Anim::Track);
    let at = now + ms(2500);
    let slot = |t: Instant| c.motion(&p, t, Look::STILL).rows[1].eta.clone();
    assert_eq!(slot(at), None);
    if let Some(d) = c.motion_deadline(&p, at, Look::STILL) {
        assert!(d >= now + ms(9_900), "{:?}", d.duration_since(now));
    }
}

#[test]
fn a_busy_echo_runs_the_comet_on_from_its_last_live_frame() {
    for kind in [EchoKind::Fault, EchoKind::Vanish] {
        let now = t0();
        let mut c = fresh(now);
        let id = c.post(busy("Installing Homebrew"), stamp(), now).id;
        c.commit_rows(now, 3);
        let p = present(&c, 120);
        let q = c.frame_instant(now + ms(1234));
        let live = c.motion(&p, q, Look::MOVING).rows[0].surface.clone();
        assert!(
            live.stops.iter().any(|s| s.tone.fill > 0),
            "PRECONDITION: the comet is on the row"
        );
        let epoch = c.live(id).unwrap().motion_since.expect("a moving row");
        if kind == EchoKind::Vanish {
            assert!(c.withdraw(id, q));
        } else {
            assert!(c.resolve(id, Outcome::Warn, q));
        }
        let p = present(&c, 120);
        let first = c.motion(&p, q, Look::MOVING);
        assert!(matches!(first.rows[0].anim, Anim::Echo { .. }), "{kind:?}");
        assert_eq!(c.echoes()[0].kind, kind);
        assert_eq!(
            first.rows[0].surface, live,
            "{kind:?}: the first echo frame"
        );
        assert_eq!(first.rows[0].fade, 0);
        let next = c.motion(&p, q + ANIM_FRAME, Look::MOVING).rows[0]
            .surface
            .clone();
        if kind == EchoKind::Fault {
            assert_eq!(next.stops.len(), live.stops.len());
            for (n, l) in next.stops.iter().zip(&live.stops) {
                assert_eq!(n.at, l.at, "frozen: {n:?} {l:?}");
                assert!(n.tone.fill <= l.tone.fill && n.tone.lift <= l.tone.lift);
                assert_eq!(n.tone.warn, 0, "no warn on a busy row's surface");
            }
            assert!(next != live, "draining");
        } else {
            let at = c.fine_instant(q + ANIM_FRAME);
            let moved = crate::animate::comet(at - epoch, true);
            assert_eq!(next, moved, "{kind:?}: the comet runs on");
        }
        let still = c.motion(&p, q, Look::STILL).rows[0].surface.clone();
        assert!(
            still.stops.iter().all(|s| s.tone.fill == 0),
            "{kind:?}: a still echo keeps the track"
        );
    }
}

#[test]
fn flat_look_uses_only_track_and_full_fill() {
    let flat = Look {
        pace: Pace::Moving,
        graded: false,
    };
    let now = t0();
    let (c, p) = scene(now);
    for k in 0..150u64 {
        let m = c.motion(&p, now + ms(k * 33), flat);
        for row in &m.rows {
            if matches!(row.anim, Anim::Echo { .. }) {
                assert_eq!(row.fade, 0, "no fade under High Contrast");
            }
            if row.surface.is_empty() {
                continue;
            }
            assert!(row.surface.flat, "{k}: a flat surface");
            for t in row.surface.cells(p.cols) {
                assert!(
                    t == Tone::TRACK || t == Tone::FULL,
                    "{k}: a graded tone {t:?} under a flat look"
                );
            }
        }
    }
}

#[test]
fn no_motion_deadline_when_idle() {
    let now = t0();
    let empty = fresh(now);
    let p = present(&empty, 80);
    assert_eq!(empty.motion_deadline(&p, now, Look::MOVING), None);
    assert_eq!(empty.motion(&p, now, Look::MOVING).fingerprint(), 0);
    let mut c = fresh(now);
    c.post(warn("held"), stamp(), now);
    c.post(warn("standing").hold(Hold::STANDING), stamp(), now);
    c.post(
        warn("ask").hold(Hold::asking(crate::HOLD_ASK)),
        stamp(),
        now,
    );
    c.commit_rows(now, 3);
    let p = present(&c, 80);
    assert_eq!(c.motion_deadline(&p, now, Look::MOVING), None);
    assert_eq!(c.motion(&p, now, Look::MOVING).fingerprint(), 0);
    let mut c = fresh(now);
    c.post(warn("fourth").hold(Hold::STANDING), stamp(), now);
    c.post(busy("queued"), stamp(), now);
    c.commit_rows(now, 1);
    let p = present(&c, 80);
    assert!(p.rows.iter().all(
        |r| !matches!(r.kind, RowKind::Message(id) if c.live(id).is_some_and(|l| l.is_animated()))
    ));
    assert_eq!(c.motion_deadline(&p, now, Look::MOVING), None);
}

#[test]
fn motion_deadlines_land_on_the_frame_grid() {
    let now = t0();
    let (c, p) = scene(now);
    for look in [Look::MOVING, Look::STILL] {
        for k in 0..400u64 {
            let t = now + ms(k * 23 + 7);
            let Some(d) = c.motion_deadline(&p, t, look) else {
                continue;
            };
            assert!(d > t, "{k}: a deadline in the past");
            let since = d.duration_since(now).as_micros();
            assert!(
                since % ANIM_FRAME.as_micros() == 0 || since % c.refresh().as_micros() == 0,
                "{k}: {since} µs is off both grids"
            );
        }
    }
}

fn first_run(permille: u16) -> Message {
    Message::new(tags::PACKAGES, Severity::Info, "Installing ALab tools")
        .meter(Meter {
            fill_permille: Some(permille),
            stats: "3 of 10 programs".into(),
            amount: Some(Amount {
                series: Amount::series_of("first run"),
                done: u64::from(permille),
                total: 1000,
                unit: Unit::Steps,
            }),
            load: Some(Load::Disk),
            busy: false,
        })
        .hold(Hold::Live {
            stale_after: STALE_TAILED,
        })
        .key("toolchain.pass")
}

#[test]
fn a_hidden_estimate_leaves_the_eta_slot_blank() {
    for (name, cols) in [
        ("download", 80usize),
        ("download", 120),
        ("first-run", 80),
        ("first-run", 120),
    ] {
        let now = t0();
        let mut c = fresh(now);
        let msg = if name == "download" {
            download(31_080_000)
        } else {
            first_run(420)
        };
        let id = c.post(msg, stamp(), now).id;
        c.commit_rows(now, 3);
        let p = present(&c, cols);
        let at = now + ms(12_400);
        assert!(
            matches!(
                c.live(id).unwrap().track.eta(at),
                crate::progress::Eta::Hidden
            ),
            "one read: nothing to estimate from"
        );
        assert!(p.rows[0].eta.is_some(), "the slot is reserved");
        for look in [Look::MOVING, Look::STILL] {
            assert_eq!(c.motion(&p, at, look).rows[0].eta, None, "{name}/{cols}");
        }
        if let Some(d) = c.motion_deadline(&p, at, Look::STILL) {
            assert!(d >= now + ms(13_400), "{name}/{cols}: {:?}", d - now);
        }
    }
    let now = t0();
    let mut c = fresh(now);
    let id = c.post(download(0), stamp(), now).id;
    c.commit_rows(now, 3);
    let mut t = now;
    for k in 1..=16u64 {
        t = now + ms(500 * k);
        c.restate(
            id,
            Restatement {
                meter: Some(download(2_000_000 * k).meter),
                ..Restatement::default()
            },
            t,
        );
    }
    let p = present(&c, 120);
    let eta = c.motion(&p, t, Look::MOVING).rows[0].eta.clone();
    assert!(
        eta.as_deref().is_some_and(|w| w.ends_with(" left")),
        "latched: {eta:?}"
    );
}

fn all_but_title(row: &crate::glass::RowLayout) -> crate::glass::RowLayout {
    let mut row = row.clone();
    row.title.1.clear();
    row.full_title.clear();
    row
}

fn next_piece_col(row: &crate::glass::RowLayout, cols: usize) -> usize {
    let mut next = cols - crate::MARGIN;
    if let Some((c, _)) = &row.detail {
        next = next.min(c - 2);
    }
    if let Some((c, _)) = &row.pct {
        next = next.min(*c);
    }
    for c in row.elapsed.into_iter().chain(row.eta) {
        next = next.min(c);
    }
    if let Some((c, _)) = row.load_slot {
        next = next.min(c - 2);
    }
    if let Some((c, _)) = &row.stats {
        next = next.min(*c);
    }
    if let Some(cap) = row.capsules.first() {
        next = next.min(cap.col);
    }
    next
}

#[test]
fn the_complete_echo_says_the_finished_form_without_a_reflow() {
    let full = |m: Message| {
        m.meter(Meter {
            fill_permille: Some(1000),
            stats: "74 MB / 74 MB".into(),
            ..Meter::default()
        })
    };
    let rows: Vec<(Message, &str)> = vec![
        (full(download(74_000_000)), "Downloaded aterm v0.92.0"),
        (busy("Installing Homebrew"), "Installed Homebrew"),
        (
            busy("Finishing aterm v0.92.0").finished_as("Installed aterm v0.92.0"),
            "Installed aterm v0.92.0",
        ),
        (
            busy("Finishing v2").finished_as("Installed and verified v2"),
            "Installed and verified v2",
        ),
        (
            busy("Installing ALab tools")
                .action(Intent::NewWindow)
                .line("about 3 GB"),
            "Installed ALab tools",
        ),
        (busy("Working"), "Working"),
    ];
    for (msg, finished) in rows {
        assert_eq!(msg.finished_title(), finished);
        let title = msg.title.clone();
        for cols in [60usize, 80, 120, 160] {
            let now = t0();
            let mut c = fresh(now);
            let id = c.post(msg.clone(), stamp(), now).id;
            c.commit_rows(now, 3);
            let at = now + ms(20_000);
            let live = present(&c, cols).rows[0].clone();
            assert!(c.resolve(id, Outcome::Ok, at));
            let echo = present(&c, cols).rows[0].clone();
            assert!(matches!(echo.kind, RowKind::Echo(_)));
            let mut was = all_but_title(&live);
            was.kind = echo.kind;
            was.stats = None;
            was.pct = None;
            let mut got = all_but_title(&echo);
            assert_eq!(got.capsules.len(), was.capsules.len());
            for (e, l) in got.capsules.iter().zip(&was.capsules) {
                assert_eq!((e.col, e.width), (l.col, l.width), "{title}@{cols}");
                assert!(e.text.trim().is_empty(), "{title}@{cols}");
            }
            got.capsules.clone_from(&was.capsules);
            assert_eq!(got, was, "{title}@{cols}: nothing moved");
            let laid = char_width(&live.title.1);
            let painted = char_width(&echo.title.1);
            assert!(painted >= laid, "{title}@{cols}: padded to the laid title");
            assert!(
                echo.title.0 + painted < next_piece_col(&echo, cols),
                "{title}@{cols}: a blank before the next piece: {echo:?}"
            );
            let words = echo.title.1.trim_end();
            let room = next_piece_col(&echo, cols) - echo.title.0 - 1;
            if char_width(finished) <= room.max(laid) {
                assert_eq!(words, finished, "{title}@{cols}: whole where it fits");
            } else {
                let stem = words.trim_end_matches('\u{2026}');
                assert!(words.ends_with('\u{2026}'), "{title}@{cols}: {words:?}");
                assert!(finished.starts_with(stem), "{title}@{cols}: {words:?}");
            }
            if cols >= 120 && finished.len() <= title.len() {
                assert_eq!(words, finished, "{title}@{cols}: whole on a wide row");
            }
            assert_eq!(echo.full_title, finished, "{title}@{cols}: spoken whole");
            let rec = c.log().get(id).unwrap();
            let logged = crate::words::done_form(finished).unwrap_or_else(|| finished.to_string());
            assert_eq!(rec.title, logged, "the record states the outcome");
            assert!(rec.detail.is_empty(), "{title}: {:?}", rec.detail);
            assert_eq!(
                (rec.severity, rec.glyph.ch()),
                (Severity::Success, '\u{2713}')
            );
        }
    }
    let now = t0();
    let mut c = fresh(now);
    let id = c.post(busy("Installing Homebrew"), stamp(), now).id;
    c.commit_rows(now, 3);
    assert!(c.resolve(id, Outcome::Warn, now + ms(5000)));
    assert_eq!(present(&c, 120).rows[0].title.1, "Installing Homebrew");
    let mut c = fresh(now);
    let id = c
        .post(
            busy("Checking aterm v0.92.0").finished_as("Verified aterm v0.92.0"),
            stamp(),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    let retitle = |title: &str, finished: Option<Option<String>>| Restatement {
        title: Some(title.into()),
        finished,
        ..Restatement::default()
    };
    assert!(c.restate(id, retitle("Checking aterm v0.92.0", None), now + ms(1)));
    assert_eq!(
        c.live(id).unwrap().msg.finished.as_deref(),
        Some("Verified aterm v0.92.0"),
        "the same title keeps its words"
    );
    assert!(c.restate(id, retitle("Installing aterm v0.92.0", None), now + ms(2)));
    assert_eq!(
        c.live(id).unwrap().msg.finished_title(),
        "Installed aterm v0.92.0"
    );
    let declared = Some(Some("Updated to aterm v0.92.0".to_string()));
    assert!(c.restate(
        id,
        retitle("Installing aterm v0.92.0", declared),
        now + ms(3)
    ));
    assert!(c.resolve(id, Outcome::Ok, now + ms(4)));
    assert_eq!(
        present(&c, 120).rows[0].title.1.trim_end(),
        "Updated to aterm v0.92.0"
    );
}

#[test]
fn a_restatement_takes_the_short_form_it_carries() {
    let now = t0();
    let mut c = fresh(now);
    let id = c
        .post(
            busy("Claude's login expired in tab 1").short_title("Login expired"),
            stamp(),
            now,
        )
        .id;
    let short = |c: &MessageCenter| c.live(id).and_then(|l| l.msg.short_title.clone());
    let retitle = |title: &str, short_title: Option<Option<String>>| Restatement {
        title: Some(title.into()),
        short_title,
        ..Restatement::default()
    };
    assert!(c.restate(
        id,
        retitle("Claude's login expired in tab 1", None),
        now + ms(1)
    ));
    assert_eq!(
        short(&c).as_deref(),
        Some("Login expired"),
        "the same title"
    );
    let usage = Some(Some("Usage limit".to_string()));
    assert!(c.restate(
        id,
        retitle("Claude usage limit reached in tab 1", usage.clone()),
        now + ms(2)
    ));
    assert_eq!(short(&c).as_deref(), Some("Usage limit"));
    assert!(c.restate(
        id,
        Restatement {
            short_title: Some(None),
            ..Restatement::default()
        },
        now + ms(3)
    ));
    assert_eq!(short(&c), None, "cleared");
    assert!(c.restate(
        id,
        retitle("Claude usage limit reached in tab 1", usage),
        now + ms(4)
    ));
    assert!(c.restate(
        id,
        retitle("Claude sign-in cancelled in tab 1", None),
        now + ms(5)
    ));
    assert_eq!(short(&c), None, "the control");
}

#[test]
fn an_echo_keeps_the_load_slot_but_draws_no_load_words() {
    let filled = |m: Message| {
        m.meter(Meter {
            fill_permille: Some(660),
            load: Some(Load::Network),
            stats: "1 of 2 programs".into(),
            ..Meter::default()
        })
    };
    let busy_loaded = |m: Message| {
        m.meter(Meter {
            busy: true,
            load: Some(Load::Disk),
            ..Meter::default()
        })
    };
    let declared = [Load::Network, Load::Disk, Load::Cpu];
    let rows = [
        filled(busy("Updating ALab tools").loads(declared)),
        busy_loaded(busy("Removing ALab tools").loads(declared)),
        busy_loaded(busy("Removing ALab tools")),
    ];
    for msg in rows {
        for (kind, outcome) in [
            (EchoKind::Complete, Some(Outcome::Ok)),
            (EchoKind::Fault, Some(Outcome::Warn)),
            (EchoKind::Vanish, None),
        ] {
            for cols in [60usize, 80, 120] {
                let now = t0();
                let mut c = fresh(now);
                let id = c.post(msg.clone(), stamp(), now).id;
                c.commit_rows(now, 3);
                let at = now + ms(20_000);
                let live = present(&c, cols).rows[0].clone();
                assert!(
                    live.load.is_some(),
                    "{}@{cols}: the live row says it",
                    msg.title
                );
                match outcome {
                    Some(o) => assert!(c.resolve(id, o, at)),
                    None => assert!(c.withdraw_with(id, kind, at)),
                }
                let echo = present(&c, cols).rows[0].clone();
                assert!(matches!(echo.kind, RowKind::Echo(_)), "{kind:?}");
                assert_eq!(
                    echo.load, None,
                    "{}@{cols} {kind:?}: no load words",
                    msg.title
                );
                assert_eq!(
                    echo.load_slot, live.load_slot,
                    "{}@{cols} {kind:?}: the slot stays",
                    msg.title
                );
                assert_eq!(echo.elapsed, live.elapsed);
                assert_eq!(echo.eta, live.eta);
                assert_eq!(echo.capsules.len(), live.capsules.len());
                for (e, l) in echo.capsules.iter().zip(&live.capsules) {
                    assert_eq!((e.col, e.width), (l.col, l.width));
                    assert!(e.text.trim().is_empty() && e.full_label.is_empty());
                }
            }
        }
    }
}

#[test]
fn an_echo_keeps_the_stops_cells_and_draws_no_stop() {
    use crate::glass::CapsuleRole;
    let msg = crate::waits::paste_row(3, 1, 1_100_000, 4_200_000, Duration::ZERO);
    for kind in [EchoKind::Complete, EchoKind::Fault, EchoKind::Vanish] {
        for cols in [60usize, 80, 120] {
            let now = t0();
            let mut c = fresh(now);
            let id = c.post(msg.clone(), stamp(), now).id;
            let _ = c.settle(now, true);
            c.commit_rows(now, 3);
            let live = present(&c, cols).rows[0].clone();
            let stop = live
                .capsules
                .iter()
                .find(|cap| cap.full_label == "Stop paste")
                .cloned()
                .expect("the live row offers the stop");
            assert_eq!(stop.role, CapsuleRole::Primary);
            assert!(c.withdraw_with(id, kind, now + ms(5_000)));
            let echo = present(&c, cols).rows[0].clone();
            assert!(matches!(echo.kind, RowKind::Echo(_)));
            assert_eq!(echo.capsules.len(), live.capsules.len(), "{kind:?}@{cols}");
            for (e, l) in echo.capsules.iter().zip(&live.capsules) {
                assert_eq!(
                    (e.col, e.width),
                    (l.col, l.width),
                    "{kind:?}@{cols}: no reflow"
                );
                assert!(e.text.trim().is_empty(), "{kind:?}@{cols}: {:?}", e.text);
                assert_eq!(e.role, CapsuleRole::Details);
                assert_eq!(e.full_label, "");
            }
        }
    }
}
