// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the attention and motion invariants of design §10.12 — the
//! reveal (E2), the record's retirement (E4), the excerpt flag (E1), the
//! live-indicator invariant, the echoes, the load words, the motion epoch,
//! and the motion layer's purity, bounds, forms and deadlines.

use crate::animate::{Anim, Look, Pace, Tone};
use crate::center::{EchoKind, MessageCenter, Outcome};
use crate::glass::{Links, Presentation, RowKind};
use crate::log::{LogLine, LogState, MessageLog, Retired};
use crate::model::{
    Amount, Hold, Intent, Load, Message, MessageId, Meter, Restatement, Severity, Unit, WallStamp,
    tags,
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

/// A live download row: determinate, with an amount.
fn download(done: u64) -> Message {
    let total = 74_000_000;
    Message::new(tags::UPDATE, Severity::Info, "Downloading aterm v0.92.0")
        .meter(Meter {
            fill_permille: None,
            stats: "45 MB / 74 MB".into(),
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

/// A live indeterminate row (the comet).
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

// ---- attention (E1–E5) --------------------------------------------------------

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
    // A revealed row stays revealed through a supersede with a grace.
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
    // Three rows fill a three-row band; an unrevealed fourth must not call
    // the overflow row up.
    let mut c = fresh(now);
    let ids: Vec<MessageId> = (0..3)
        .map(|i| {
            c.post(warn(&format!("w{i}")).hold(Hold::Standing), stamp(), now)
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
    // At one row, no `+1 ›` for a row nobody may see yet.
    let mut one = fresh(now);
    one.post(warn("w").hold(Hold::Standing), stamp(), now);
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
    // Once revealed, it counts.
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
            warn("Auto-update failing").hold(Hold::Standing),
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
                .hold(Hold::Ask {
                    for_: crate::HOLD_ASK,
                }),
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
    // An OLDER reader ignores the key it does not know: without `rec=1`
    // the line reads as the fold it always wrote.
    let older = enc.replace("\trec=1", "");
    match LogLine::decode(&older).unwrap() {
        LogLine::Retired { how, .. } => assert_eq!(how, Retired::Folded),
        other => panic!("{other:?}"),
    }
    // The compacted file keeps it a record.
    let mut log = MessageLog::empty();
    for line in &lines {
        log.replay(line.clone());
    }
    assert_eq!(
        log.get(id).unwrap().state,
        LogState::Retired(Retired::Recorded)
    );
    let again: Vec<String> = log.compact_lines().iter().map(LogLine::encode).collect();
    assert!(again.iter().any(|l| l.contains("rec=1")), "{again:?}");
    // A restate to LogOnly of a row that WAS on the glass stays a fold.
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
    // The flag restates like any word, and the fingerprint follows.
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
    child.seed_carried(&carry, now);
    assert!(!child.live(id).unwrap().msg.excerpt);
    assert_eq!(present(&child, 160).rows[0].detail, None);
    // An unrevealed row is not carried.
    let mut parent = fresh(now);
    parent.post(download(1).reveal_after(PROGRESS_GRACE), stamp(), now);
    assert!(parent.carried().live.is_empty());
}

/// THE INDICATOR IS THE METER'S STATE (design ruling 139, the merge's M4 —
/// it replaces the pill's invariant `Hold::Live ⇒ meter.is_some()`): the
/// engine never invents an indicator from the hold. A Live row that declared
/// neither a fill nor `busy` — blocked on the person, like the update flow's
/// editor block — is STILL: no meter, no track, no motion, no echo. A row
/// that declares `busy` (the reporter's `in_flight()`) sweeps the comet; a
/// restate that clears the meter stills it; the carry keeps `busy`, so a
/// carried busy row with no fill and no stats keeps its track.
#[test]
fn the_indicator_is_the_meters_state_never_the_holds() {
    let now = t0();
    let mut c = fresh(now);
    let blocked = c
        .post(
            Message::new(tags::UPDATE, Severity::Warn, "Close the editor to finish").hold(
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
    // Restate: clearing the meter stills the row.
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
    // Seed: a carried busy row with no fill and no stats keeps its track.
    let carry = c.carried();
    let mut child = fresh(now);
    child.seed_carried(&carry, now);
    assert!(child.live(id).unwrap().is_busy());
    assert_eq!(child.live(blocked).unwrap().msg.meter, None);
    child.commit_rows(now, 3);
    let p = present(&child, 120);
    assert_eq!(p.rows[row_of(&p, id)].track, Some((0, 120)));
}

// ---- the center's motion state ---------------------------------------------

/// A row POSTED heavy shows its load words from the first frame — it is on
/// the glass because it is heavy (review round 2, 2026-09-23: the latch at
/// +5 s laid the live row out again mid-animation) — and its layout reserves
/// ONE slot for them for the row's life: a resource change swaps the words
/// at once (the machine stayed busy), the load ending takes them, a load
/// declared after none waits [`LOAD_AFTER`], and none of it moves the meter
/// or anything after it.
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
    assert_eq!(echo.until, at + EchoKind::Complete.span());
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
    // Resolved Warn is a Fault; withdrawn is a fade.
    assert!(c.resolve(z, Outcome::Warn, until));
    assert_eq!(c.echoes()[0].kind, EchoKind::Fault);
    assert!(c.withdraw(a, until));
    assert!(c.echoes().iter().any(|e| e.kind == EchoKind::Vanish));
}

/// A Fault echo SAYS it failed for its whole life — ⚠ in the glyph cell and
/// `failed` in the row's time slot (the ETA slot of a determinate row, the
/// elapsed slot of a comet) — and a Complete echo says it is DONE there
/// under its ✓ (review round 2, 2026-09-23: a bar that turned yellow and
/// faded read as a render glitch; review round 3, 2026-09-24: `✓
/// Downloading aterm … 100%` said done with its glyph and working with its
/// words). A Vanish claims no outcome and says neither. The slots are the
/// row's own: nothing re-grids.
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
    // A Complete echo wears ✓ and says `done`, in the same slots, for its
    // whole life and in every look.
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
            assert_eq!(m.rows[0].eta.as_deref(), Some(crate::DONE_WORD), "@{k}");
            assert_eq!(m.rows[1].readout.as_deref(), Some(crate::DONE_WORD), "@{k}");
        }
    }
    // A Vanish says neither word: the comet's slot keeps its frozen clock.
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
    assert_eq!(m.rows[1].readout.as_deref(), Some("0:20"));
}

#[test]
fn echoes_never_change_the_row_count() {
    let now = t0();
    // The same script with a LIVE row (it echoes) and a HELD one (no echo).
    let run = |live: bool| -> Vec<Option<u16>> {
        let mut c = fresh(now);
        let row = if live { busy("work") } else { warn("held") };
        let id = c.post(row, stamp(), now).id;
        let other = c.post(warn("other").hold(Hold::Standing), stamp(), now).id;
        let mut answers = vec![c.commit_rows(now, 3)];
        c.resolve(id, Outcome::Ok, now + ms(10));
        assert_eq!(c.echoes().len(), usize::from(live));
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
    assert_eq!(run(true), run(false));
    // The window's own shrink drops an echo past the new count.
    let mut c = fresh(now);
    c.post(warn("top").hold(Hold::Standing), stamp(), now);
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
        .post(warn("standing").hold(Hold::Standing), stamp(), now)
        .id;
    let ask = c
        .post(
            warn("ask").hold(Hold::Ask {
                for_: crate::HOLD_ASK,
            }),
            stamp(),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    let queued = c.post(busy("queued"), stamp(), now).id;
    let hidden = c
        .post(busy("hidden").reveal_after(PROGRESS_GRACE), stamp(), now)
        .id;
    assert!(c.live(queued).unwrap().is_queued());
    // The queued and unrevealed rows first: a resolve above would promote
    // the queued one onto the glass.
    for id in [queued, hidden, held, standing, ask] {
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
    // The held staged row restated live in place: a new epoch, so the
    // comet enters at the left, not mid-sweep.
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
    // A restate that keeps the activity keeps the epoch.
    c.restate(
        id,
        Restatement {
            title: Some("Installing aterm v0.92.0".into()),
            ..Restatement::default()
        },
        later + ms(100),
    );
    assert_eq!(c.live(id).unwrap().motion_since, Some(later));
    // A supersede from determinate to indeterminate restarts it too.
    let dl = c.post(download(1), stamp(), later + ms(200)).id;
    let epoch = c.live(dl).unwrap().motion_since;
    let verifying = busy("Verifying aterm v0.92.0").key("update.progress");
    let v = c.post(verifying, stamp(), later + ms(900)).id;
    assert_ne!(c.live(v).unwrap().motion_since, epoch);
    assert_eq!(c.live(v).unwrap().motion_since, Some(later + ms(900)));
}

/// A row released and re-placed while the band grows — the overflow row's
/// transient when a third row is revealed into a two-row band — keeps its
/// motion epoch: the comet on the row already up never restarts because
/// another row arrived.
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

// ---- the motion layer -----------------------------------------------------------

/// A center with a comet row, a determinate row mid-glide and an echo, on
/// a 120-column band.
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
        assert_eq!(a.at, q);
        assert_eq!(
            c.motion(&p, q, Look::MOVING).rows,
            a.rows,
            "the frame is its instant's"
        );
        if t + ms(10) < q + ANIM_FRAME {
            assert_eq!(
                c.motion(&p, t + ms(10), Look::MOVING).rows,
                a.rows,
                "t and t + 10 ms in one step give one frame"
            );
        }
    }
}

#[test]
fn frame_sequence_is_bit_stable() {
    use crate::animate::Surface;
    use crate::glass::Fnv;
    // 120 frames of a comet, a gliding-glinting bar and an echo, hashed: the
    // integer math is the same on every target.
    let mut h = Fnv::new();
    let e = crate::center::Echo {
        id: MessageId::FIRST,
        msg: busy("x"),
        kind: EchoKind::Complete,
        from_permille: 420,
        indeterminate: false,
        comet_since: None,
        elapsed: ms(0),
        started: t0(),
        until: t0(),
        slot: 0,
        load: None,
        load_slot: false,
    };
    let mut fold = |s: &Surface| {
        h.byte(u8::from(s.flat));
        for stop in &s.stops {
            h.num(u64::from(stop.at));
            h.byte(stop.tone.fill);
            h.byte(stop.tone.lift);
            h.byte(stop.tone.warn);
        }
        for t in s.cells(97) {
            h.byte(t.fill);
            h.byte(t.lift);
            h.byte(t.warn);
        }
    };
    for k in 0..120u64 {
        let t = ANIM_FRAME * u32::try_from(k).unwrap();
        fold(&crate::animate::comet(t * 3, true));
        fold(&crate::animate::bar(620, Some(t), true));
        let (echo, fade) = crate::animate::echo(&e, t / 4, Look::MOVING);
        fold(&echo);
        fold(&Surface::uniform(
            Tone {
                fill: fade,
                lift: 0,
                warn: 0,
            },
            false,
        ));
    }
    assert_eq!(h.finish(), FRAME_SEQUENCE_FNV, "{:#x}", h.finish());
}

/// The pinned hash of [`frame_sequence_is_bit_stable`]'s 120 frames
/// (re-pinned 2026-09-24 on the merge with main's full-width meter, design
/// rulings 136–141: the surfaces are fractions of the whole row now; and
/// again the same day when the comet's lead widened to 4 %,
/// [`crate::COMET_LEAD_PERMILLE`]; and again when the comet's tail took 32
/// chords in place of 8, its faint end rounded up — design ruling 157).
const FRAME_SEQUENCE_FNV: u64 = 0xec8d_c966_6000_f3dd;

/// The comet's head moves forward on every frame of a crossing and never
/// back, by at most a sixtieth of the row per frame (a cell and a third at
/// 80 columns, a calm 0.4 rows a second on average: design ruling 138) —
/// and the lit extent follows it at cell resolution: at 80, 120 and 200
/// columns the rightmost lit cell never jumps back and never skips more than
/// the head moved.
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

/// The comet never leaves the row empty: it enters through the window's
/// left edge as the last one leaves through the right, so only the frame at
/// a crossing's hand-over can be dark (review 2026-09-23 — an empty grey
/// track read as 0 % or stalled) — and it asks a frame on every grid step,
/// with the spinner turning in the glyph cell every [`crate::SPIN_FRAMES`]
/// frames.
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
    let mut spins = Vec::new();
    for k in [100u64, 1000, 2990, 3100, 3700, 9000] {
        let at = now + ms(k);
        let m = c.motion(&p, at, Look::MOVING);
        let Anim::Comet { spin, .. } = m.rows[0].anim else {
            panic!("{k}: {:?}", m.rows[0].anim);
        };
        assert_eq!(m.rows[0].glyph, Some(crate::SPINNER[usize::from(spin)]));
        spins.push(spin);
        let d = c.motion_deadline(&p, at, Look::MOVING).unwrap();
        assert_eq!(d, c.frame_instant(at) + ANIM_FRAME, "{k}: the next frame");
    }
    assert!(spins.windows(2).any(|w| w[0] != w[1]), "the spinner turns");
    // One spinner step per SPIN_FRAMES frames, about main's 125 ms.
    let steps: Vec<u8> = (0..16u32)
        .map(|f| crate::animate::spin_at(now + ANIM_FRAME * f, now))
        .collect();
    assert_eq!(steps, [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3]);
    // Still: the unlit track and the row's own glyph.
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

/// THE COMET IS ONE SOFT GRADIENT (design ruling 138): no two stops share a
/// position — no hard edge anywhere on it — its fill rises to the head and
/// falls after it, and inside the row it lights about a fifth of the row
/// (main's `COMET_PERMILLE`); the window's own edges are the only ends it
/// has.
#[test]
fn the_comet_is_one_soft_gradient_about_a_fifth_of_the_row() {
    let mut inside = 0;
    let mut t = Duration::ZERO;
    while t < COMET_PERIOD {
        let s = crate::animate::comet(t, true);
        assert!(
            s.stops.windows(2).all(|w| w[0].at < w[1].at),
            "a hard edge at {t:?}: {s:?}"
        );
        let peak = s
            .stops
            .iter()
            .enumerate()
            .max_by_key(|(_, p)| p.tone.fill)
            .map_or(0, |(i, _)| i);
        for (i, w) in s.stops.windows(2).enumerate() {
            if i < peak {
                assert!(w[0].tone.fill <= w[1].tone.fill, "{t:?}: the tail dips");
            } else {
                assert!(w[0].tone.fill >= w[1].tone.fill, "{t:?}: the lead rises");
            }
        }
        let x = crate::animate::comet_head(t);
        let lead = i64::from(crate::ROW) * i64::from(crate::COMET_LEAD_PERMILLE) / 1000;
        let len = i64::from(crate::ROW) * i64::from(crate::COMET_PERMILLE) / 1000;
        if x - len + lead > 0 && x + lead < i64::from(crate::ROW) {
            inside += 1;
            let lit = s.cells(1000).iter().filter(|c| c.fill > 0).count();
            assert!((190..=210).contains(&lit), "{lit}‰ lit at {t:?}");
        }
        t += ANIM_FRAME;
    }
    assert!(
        inside > 40,
        "the comet crosses wholly inside the row: {inside}"
    );
    // Flat: a solid segment in the full ink.
    let flat = crate::animate::comet(COMET_PERIOD / 2, false);
    assert!(flat.flat);
    assert!(
        flat.cells(120)
            .iter()
            .all(|c| *c == Tone::TRACK || *c == Tone::FULL)
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
    // It does lift the fill somewhere mid-travel.
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
    // Seven seconds apart, both before the download's bytes stall: the
    // still surfaces are identical (a stall is information, not motion — it
    // changes the bar's tone once, below).
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
    // The deadline is text only: the download's ETA slot shows its clock
    // while the estimate is hidden, so the next change is its next second.
    let at = now + ms(2500);
    let clock = |t: Instant| c.motion(&p, t, Look::STILL).rows[1].eta.clone();
    assert_eq!(clock(at).as_deref(), Some("0:02"));
    let d = c.motion_deadline(&p, at, Look::STILL).expect("a text tick");
    assert!(
        d >= now + ms(3000) && d <= now + ms(3000) + ANIM_FRAME,
        "{:?}",
        d.duration_since(now)
    );
    assert_eq!(clock(d).as_deref(), Some("0:03"));
}

/// A BUSY ROW'S ECHO PICKS UP THE COMET WHERE IT WAS (design ruling 162):
/// a Fault or a Vanish on a moving busy row draws, on its first frame, the
/// very surface the last live frame drew — the comet runs on and warms (or
/// fades); it never cuts to a bare track and warms from nothing. The still
/// and flat looks keep the track.
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
        // …and it keeps moving: the next frame's comet is the next live one's.
        let next = c.motion(&p, q + ANIM_FRAME, Look::MOVING).rows[0]
            .surface
            .clone();
        let mut moved = crate::animate::comet(q + ANIM_FRAME - epoch, true);
        if kind == EchoKind::Fault {
            let w = next.stops[0].tone.warn;
            for s in &mut moved.stops {
                s.tone.warn = w;
            }
        }
        assert_eq!(next, moved, "{kind:?}: the comet runs on");
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
    // Held, ask and standing rows: no motion.
    let mut c = fresh(now);
    c.post(warn("held"), stamp(), now);
    c.post(warn("standing").hold(Hold::Standing), stamp(), now);
    c.post(
        warn("ask").hold(Hold::Ask {
            for_: crate::HOLD_ASK,
        }),
        stamp(),
        now,
    );
    c.commit_rows(now, 3);
    let p = present(&c, 80);
    assert_eq!(c.motion_deadline(&p, now, Look::MOVING), None);
    assert_eq!(c.motion(&p, now, Look::MOVING).fingerprint(), 0);
    // A queued live row (behind the overflow row) arms nothing either.
    c.post(warn("fourth").hold(Hold::Standing), stamp(), now);
    c.post(busy("queued"), stamp(), now);
    c.commit_rows(now, 3);
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
            let since = d.duration_since(now).as_millis();
            assert_eq!(
                since % ANIM_FRAME.as_millis(),
                0,
                "{k}: {since} ms is off the grid"
            );
        }
    }
}

/// A layout painted as `glass`'s pinned strings are, with one frame's time
/// words in their slots: the readout in the elapsed slot, the ETA words in
/// the ETA slot.
fn render_with_motion(
    row: &crate::glass::RowLayout,
    rm: &crate::animate::RowMotion,
    cols: usize,
) -> String {
    let mut cells: Vec<char> = crate::glass::tests::render(row, cols).chars().collect();
    let mut put = |col: usize, words: &str| {
        for (i, ch) in words.chars().enumerate() {
            if let Some(cell) = cells.get_mut(col + i) {
                *cell = ch;
            }
        }
    };
    if let (Some(col), Some(words)) = (row.elapsed, rm.readout.as_deref()) {
        put(col, words);
    }
    if let (Some(col), Some(words)) = (row.eta, rm.eta.as_deref()) {
        put(col, words);
    }
    cells.into_iter().collect()
}

/// The first run's live row: a step count behind its fill, disk busy.
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

/// THE ETA SLOT ALWAYS SAYS HOW LONG (review round 3, 2026-09-24): until
/// the estimate latches — and whenever it goes hidden again — a determinate
/// row's reserved ETA slot shows the work's elapsed CLOCK in the label ink
/// (`0:12`), where it painted ten blank cells mid-row between `35%` and
/// `· disk busy`. A clock is never mistakable for the `… left` it gives way
/// to (ruling 129), it fits the short slot (`ELAPSED_W ≤ ETA_SHORT_W`), and
/// nothing re-lays: the slot was reserved for the row's life (ruling 100).
/// Pinned string for string at 80 and 120 columns, then latched.
#[test]
fn a_hidden_estimate_leaves_the_eta_slot_the_elapsed_clock() {
    let pinned: &[(&str, usize, &str)] = &[
        (
            "download",
            80,
            " ℹ Downloading aterm v0.92.0  42% 0:12          45 MB / 74 MB        Details ›  ",
        ),
        (
            "download",
            120,
            " ℹ Downloading aterm v0.92.0  42% 0:12          45 MB / 74 MB                                                Details ›  ",
        ),
        (
            "first-run",
            80,
            " ℹ Installing ALab tools  42% 0:12         · disk busy               Details ›  ",
        ),
        (
            "first-run",
            120,
            " ℹ Installing ALab tools  42% 0:12         · disk busy     3 of 10 programs                                  Details ›  ",
        ),
    ];
    let mut failures = Vec::new();
    for (name, cols, want) in pinned {
        let now = t0();
        let mut c = fresh(now);
        let msg = if *name == "download" {
            download(31_080_000)
        } else {
            first_run(420)
        };
        let id = c.post(msg, stamp(), now).id;
        c.commit_rows(now, 3);
        let p = present(&c, *cols);
        let at = now + ms(12_400);
        assert!(
            matches!(
                c.live(id).unwrap().track.eta(at),
                crate::progress::Eta::Hidden
            ),
            "one read: nothing to estimate from"
        );
        let m = c.motion(&p, at, Look::MOVING);
        let got = render_with_motion(&p.rows[0], &m.rows[0], *cols);
        if got != *want {
            failures.push(format!("(\"{name}\", {cols}, {got:?}),"));
        }
        // The same words still, and the next second is asked for.
        assert_eq!(
            c.motion(&p, at, Look::STILL).rows[0].eta.as_deref(),
            Some("0:12")
        );
        let d = c.motion_deadline(&p, at, Look::STILL).expect("a tick");
        assert!(d <= now + ms(13_000) + ANIM_FRAME, "{name}/{cols}");
        assert_eq!(
            c.motion(&p, d, Look::STILL).rows[0].eta.as_deref(),
            Some("0:13")
        );
    }
    assert!(failures.is_empty(), "re-pin:\n{}", failures.join("\n"));
    // Latched, the slot says what is LEFT, and the clock is gone.
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

/// Every piece of `row` but its title — what a Complete echo's finished
/// title must leave exactly where the row had it (ruling 154).
fn all_but_title(row: &crate::glass::RowLayout) -> crate::glass::RowLayout {
    let mut row = row.clone();
    row.title.1.clear();
    row.full_title.clear();
    row
}

/// The first cell another piece of `row` paints after the title (a joint
/// counts), else the right margin.
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

/// THE COMPLETE ECHO SAYS THE FINISHED FORM (ruling 154): `✓ Downloaded
/// aterm v0.92.0  100% done`, never `✓ Downloading … done`. The title's
/// leading participle reads finished through the closed table, or the
/// reporter's declared words win; a title the table does not know keeps its
/// words. Only the Complete echo: a Fault keeps the title (`Downloading …
/// failed` is honest), and the RECORD keeps what the reporter resolved with.
/// Nothing else on the row moves — pct, time, load, stats and capsules keep
/// their columns at 60, 80, 120 and 160 — and the words never run into the
/// next piece.
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
            // Declared words longer than the title's cells elide in place.
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
            assert_eq!(all_but_title(&echo), was, "{title}@{cols}: nothing moved");
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
                // Longer than the cells it may take: elided, never reflowed.
                let stem = words.trim_end_matches('\u{2026}');
                assert!(words.ends_with('\u{2026}'), "{title}@{cols}: {words:?}");
                assert!(finished.starts_with(stem), "{title}@{cols}: {words:?}");
            }
            if cols >= 120 && finished.len() <= title.len() {
                assert_eq!(words, finished, "{title}@{cols}: whole on a wide row");
            }
            assert_eq!(echo.full_title, finished, "{title}@{cols}: spoken whole");
            assert_eq!(
                c.log().get(id).unwrap().title,
                title,
                "the record keeps the reporter's words"
            );
        }
    }
    // A Fault keeps the title.
    let now = t0();
    let mut c = fresh(now);
    let id = c.post(busy("Installing Homebrew"), stamp(), now).id;
    c.commit_rows(now, 3);
    assert!(c.resolve(id, Outcome::Warn, now + ms(5000)));
    assert_eq!(present(&c, 120).rows[0].title.1, "Installing Homebrew");
    // A restatement that re-titles the row drops the words declared for the
    // old title; a declared restatement sets new ones.
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
