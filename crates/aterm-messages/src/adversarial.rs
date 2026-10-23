// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Adversarial message-center and glass-layout tests.

use crate::center::MessageCenter;
use crate::glass::{
    CapsuleSpec, Hit, Links, Presentation, RowKind, RowSpec, layout_row, overflow_spec,
    overflow_words,
};
use crate::log::{LogLine, MessageLog, Retired};
use crate::model::{
    Decision, Hold, Intent, Message, MessageId, Restatement, Severity, WallStamp, tags,
};
use crate::{Duration, HOLD_ASK, HOLD_INFO, HOLD_WARN, Instant, LOG_CAP, MARGIN};

fn t0() -> Instant {
    Instant::now()
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
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

fn glass_ids(c: &MessageCenter) -> Vec<MessageId> {
    c.on_glass().map(|l| l.id).collect()
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

fn caps(intents: &[Intent]) -> Vec<CapsuleSpec> {
    let mut v: Vec<CapsuleSpec> = intents
        .iter()
        .enumerate()
        .map(|(i, it)| CapsuleSpec::authored(it, i as u8))
        .collect();
    v.push(CapsuleSpec::details(0));
    v
}

#[test]
fn a_queued_carried_held_row_is_not_folded_as_read_after_commit() {
    let now = t0();
    let mut parent = fresh(now);
    let ids: Vec<MessageId> = (0..4)
        .map(|i| {
            parent
                .post(
                    info(&format!("r{i}")),
                    stamp(i),
                    now + Duration::from_millis(i),
                )
                .id
        })
        .collect();
    parent.commit_rows(now + secs(1), 3);
    assert_eq!(glass_ids(&parent), vec![ids[0], ids[1]]);
    let carry = parent.carried();
    let later = now + secs(5);
    let mut child = fresh(later);
    child.seed_carried(&carry, stamp(5_000), later);
    assert_eq!(glass_ids(&child), vec![ids[0], ids[1]]);
    assert!(child.live(ids[2]).unwrap().is_queued());
    let commit = later + secs(1);
    child.after_handoff_commit(commit);
    let queued = child.live(ids[2]).unwrap();
    assert!(queued.is_queued(), "still queued after the commit");
    assert_eq!(
        queued.fold_at, None,
        "a queued row has no anchor (`fold_at: None until first glass`)"
    );
}

#[test]
fn the_excerpt_never_comes_back_while_narrowing() {
    let spec = RowSpec {
        kind: RowKind::Message(MessageId::from_raw(2).unwrap()),
        severity: Severity::Warn,
        live: false,
        glyph: '\u{26a0}',
        title: "aterm.toml has a problem",
        title_short: None,
        detail0: Some("skipping \"cmd+shift+k\": unknown action \"foo\""),
        step: false,
        step_short: None,
        meter: None,
        animated: false,
        busy: false,
        eta: false,
        load: None,
        load_slot: crate::model::Loads::NONE,
        capsules: caps(&[
            Intent::OpenConfigEditor { line: None },
            Intent::OpenSystemPane {
                pane: "full-disk-access".into(),
            },
        ]),
    };
    let mut gone_at: Option<usize> = None;
    let mut comebacks = Vec::new();
    for cols in (8..=200).rev() {
        let row = layout_row(&spec, cols, &chars);
        match (gone_at, &row.detail) {
            (None, None) => gone_at = Some(cols),
            (Some(g), Some((_, d))) => comebacks.push((g, cols, d.clone())),
            _ => {}
        }
    }
    assert!(
        comebacks.is_empty(),
        "the excerpt was dropped at {:?} and came back: {:?}",
        comebacks.first().map(|c| c.0),
        comebacks.first()
    );
}

#[test]
fn a_live_row_keeps_its_log_record() {
    let now = t0();
    let mut c = fresh(now);
    let standing = c
        .post(
            Message::new(tags::RENDER, Severity::Warn, "GPU lost").hold(Hold::STANDING),
            stamp(1),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    for i in 0..LOG_CAP {
        c.post(
            info(&format!("quiet {i}")).hold(Hold::LogOnly),
            stamp(2 + i as u64),
            now,
        );
    }
    assert!(c.live(standing).is_some(), "the row is live");
    assert_eq!(glass_ids(&c), vec![standing], "…and on glass");
    assert!(
        c.log().get(standing).is_some(),
        "a live row's record is gone from the log while the row is on glass"
    );
}

#[test]
fn a_single_empty_detail_line_round_trips() {
    let now = t0();
    let mut c = fresh(now);
    let id = c
        .post(info("t").line("").line("kept").line(""), stamp(1), now)
        .id;
    assert_eq!(c.live(id).unwrap().msg.detail, vec!["kept".to_string()]);
    let line = c.drain_new_for_persist().into_iter().next().unwrap();
    let back = LogLine::decode(&line.encode()).unwrap();
    assert_eq!(back, line, "{}", line.encode());
    assert!(c.restate(
        id,
        Restatement {
            detail: Some(vec![String::new(), "re".into(), String::new()]),
            ..Restatement::default()
        },
        now
    ));
    assert_eq!(c.live(id).unwrap().msg.detail, vec!["re".to_string()]);
}

#[test]
fn layout_never_panics_below_eight_cols() {
    let fixtures: Vec<_> = crate::glass::tests::fixtures()
        .into_iter()
        .chain(crate::glass::tests::motion_fixtures())
        .collect();
    for (name, spec) in &fixtures {
        for cols in 0..8 {
            let row = layout_row(spec, cols, &chars);
            let painted = crate::glass::tests::render(&row, cols);
            assert_eq!(chars(&painted), cols, "{name}@{cols}: {painted:?}");
            let p = Presentation {
                cols,
                rows: vec![row],
            };
            for col in 0..cols {
                let _ = p.hit(0, col);
            }
            assert_eq!(p.hit(0, cols), Hit::Nothing);
        }
        for links in [Links::Painted, Links::Withheld] {
            for progress in [None, Some("Downloading aterm v0.91.0 45%")] {
                let words = overflow_words(99, links, progress);
                let _ = layout_row(&overflow_spec(99, &words), 0, &chars);
            }
        }
    }
}

#[test]
fn the_width_law_holds_under_a_two_cell_measure() {
    let wide = |s: &str| -> usize {
        s.chars()
            .map(|c| if c.is_ascii_uppercase() { 2 } else { 1 })
            .sum()
    };
    let fixtures: Vec<_> = crate::glass::tests::fixtures()
        .into_iter()
        .chain(crate::glass::tests::motion_fixtures())
        .collect();
    for (name, spec) in &fixtures {
        for cols in 8..=200usize {
            let row = layout_row(spec, cols, &wide);
            let Some(first) = row.capsules.first() else {
                continue;
            };
            if first.col == 0 || row.title.1.is_empty() {
                continue; // the documented degenerate clip
            }
            let mut end = row.title.0 + wide(&row.title.1);
            if let Some((c, d)) = &row.detail {
                assert!(*c >= end + 3, "{name}@{cols}: excerpt overlaps");
                end = c + wide(d);
            }
            if let Some(meter) = row.meter {
                assert_eq!(
                    (meter.0, meter.1),
                    (0, cols),
                    "{name}@{cols}: the meter is the row"
                );
            }
            if let Some((c, p)) = &row.pct {
                assert!(*c >= end + 1, "{name}@{cols}: pct overlaps");
                end = c + wide(p);
            }
            if let Some(c) = row.elapsed {
                assert!(c >= end + 1, "{name}@{cols}: elapsed overlaps");
                end = c + row.elapsed_width();
            }
            if let Some(c) = row.eta {
                assert!(c >= end + 1, "{name}@{cols}: eta overlaps");
                end = c + row.eta_width();
            }
            if let Some((c, s)) = &row.stats {
                assert!(*c >= end + 2, "{name}@{cols}: stats overlap");
                end = c + wide(s);
            }
            if let Some((c, w)) = row.load_slot {
                assert!(c >= end + 2, "{name}@{cols}: load slot overlaps");
                end = c + w;
            }
            if let (Some((c, w)), Some((slot, sw))) = (row.load, row.load_slot) {
                assert_eq!(c + wide(w), slot + sw, "{name}@{cols}: right-aligned words");
            }
            assert!(
                first.col >= end + 2
                    || first.col == end + 1
                        && crate::glass::tests::spent_clear_cell(spec, &row, &wide),
                "{name}@{cols}: capsules overlap the flow"
            );
            let last = row.capsules.last().unwrap();
            assert_eq!(
                last.col + last.width,
                cols - MARGIN,
                "{name}@{cols}: right-aligned"
            );
            for c in &row.capsules {
                assert_eq!(c.width, wide(&c.text) + 2, "{name}@{cols}: capsule width");
            }
        }
    }
}

#[test]
fn a_carried_ask_row_keeps_its_own_hold_after_the_commit() {
    let now = t0();
    let mut parent = fresh(now);
    let id = parent
        .post(
            Message::new(tags::PRIVACY, Severity::Info, "File access not confirmed")
                .action(Intent::OpenSystemPane {
                    pane: "full-disk-access".into(),
                })
                .action(Intent::NotNow {
                    decision: Decision::FileAccess,
                })
                .hold(Hold::asking(HOLD_ASK)),
            stamp(1),
            now,
        )
        .id;
    parent.commit_rows(now, 3);
    let carry = parent.carried();
    let later = now + secs(5);
    let mut child = fresh(later);
    child.seed_carried(&carry, stamp(5_000), later);
    child.after_handoff_commit(later + secs(1));
    let row = child.live(id).unwrap();
    assert_eq!(
        row.msg.hold,
        Hold::asking(HOLD_ASK),
        "the hold word is kept"
    );
    assert_eq!(
        row.fold_at,
        Some(later + secs(1) + HOLD_ASK),
        "…and the anchor is its own span"
    );
    assert!(HOLD_ASK > HOLD_INFO);
}

#[test]
fn a_duplicate_post_keeps_the_live_severity() {
    let now = t0();
    let mut c = fresh(now);
    let a = c.post(info("Update failed"), stamp(1), now);
    let again = c.post(
        Message::new(tags::SYSTEM, Severity::Error, "Update failed"),
        stamp(2),
        now,
    );
    assert_eq!(again.outcome, crate::center::PostOutcome::Duplicate);
    assert_eq!(c.live(a.id).unwrap().msg.severity, Severity::Info);
    assert_eq!(c.live(a.id).unwrap().repeats, 2);
}

#[test]
fn a_carried_row_the_parent_ended_ends_though_another_writer_took_its_number() {
    let now = t0();
    let mut parent = fresh(now);
    let resolved = parent
        .post(
            Message::new(tags::CONFIG, Severity::Warn, "resolved there").key("resolved there"),
            stamp(10),
            now,
        )
        .id;
    parent.commit_rows(now, 1);
    let carry = parent.carried();
    assert!(parent.resolve(resolved, crate::Outcome::Ok, now + secs(1)));
    let mut other = fresh(now);
    let theirs = other.post(info("another aterm's note"), stamp(5), now).id;
    assert_eq!(
        theirs, resolved,
        "PRECONDITION: both minted the same number"
    );
    let mut lines = other.drain_new_for_persist();
    lines.extend(parent.drain_new_for_persist());
    let mut disk = MessageLog::empty();
    disk.replay_all(lines);
    assert_ne!(
        disk.get(resolved).map(|rec| rec.stamp),
        Some(stamp(10)),
        "PRECONDITION: the parent's record moved off its number"
    );

    let later = now + secs(5);
    let mut child = fresh(later);
    child.seed_carried(&carry, stamp(5_000), later);
    child.merge_reloaded_log(&disk);
    let ended = child.end_carried_rows_the_parent_ended(&disk, later);
    assert_eq!(
        ended
            .iter()
            .map(|(msg, how)| (msg.title.as_str(), how.clone()))
            .collect::<Vec<_>>(),
        vec![("resolved there", Retired::Resolved(crate::Outcome::Ok))],
        "the parent's ending is found under the fresh id"
    );
    assert!(child.live(resolved).is_none());
}

#[test]
fn a_carried_row_the_parent_ended_after_the_capture_ends_in_the_successor() {
    let now = t0();
    let mut parent = fresh(now);
    let warn = |title: &str| Message::new(tags::CONFIG, Severity::Warn, title).key(title);
    let resolved = parent.post(warn("resolved there"), stamp(1), now).id;
    let folded = parent.post(warn("folded there"), stamp(2), now).id;
    let restated = parent.post(warn("restated here"), stamp(3), now).id;
    let open = parent
        .post(warn("open").hold(Hold::For(secs(110))), stamp(4), now)
        .id;
    parent.commit_rows(now, 5);
    let carry = parent.carried();
    assert_eq!(carry.live.len(), 4, "PRECONDITION: all four cross");
    assert!(parent.resolve(resolved, crate::Outcome::Ok, now + secs(1)));
    assert!(parent.withdraw(restated, now + secs(1)));
    let _ = parent.settle(now + HOLD_WARN + secs(1), true);
    assert!(
        parent.live(folded).is_none() && parent.live(open).is_some(),
        "PRECONDITION: one hold ran out on the parent's glass"
    );
    let disk = parent.log().clone();

    let later = now + secs(5);
    let mut child = fresh(later);
    child.seed_carried(&carry, stamp(5_000), later);
    child.restate(
        restated,
        Restatement {
            title: Some("restated by the successor".into()),
            ..Restatement::default()
        },
        later,
    );
    child.merge_reloaded_log(&disk);
    let pending_before = child.log().pending_len();
    let ended = child.end_carried_rows_the_parent_ended(&disk, later);
    assert_eq!(
        ended
            .iter()
            .map(|(msg, how)| (msg.title.as_str(), how.clone()))
            .collect::<Vec<_>>(),
        vec![("resolved there", Retired::Resolved(crate::Outcome::Ok))],
        "each ended row comes back with the parent's ending"
    );
    assert!(child.live(resolved).is_none());
    assert!(matches!(
        child.log().get(resolved).and_then(|rec| rec.retired()),
        Some(Retired::Resolved(_))
    ));
    assert_eq!(
        child.log().pending_len(),
        pending_before,
        "the parent's line is in the file: no second one"
    );
    for (id, why) in [
        (folded, "a fold is about the parent's glass"),
        (restated, "a restated row is the successor's"),
        (open, "nobody ended it"),
    ] {
        assert!(child.live(id).is_some(), "{why}");
    }
}
