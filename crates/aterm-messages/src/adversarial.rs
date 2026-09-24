// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Adversarial probes (semantics lens): each test asserts the DESIGNED
//! behaviour (docs/DESIGN-unified-messages-2026-09-21.md §1, §2.4) so a
//! failure is the evidence of a defect. Test-only; nothing here ships.

use crate::center::{MessageCenter, Outcome};
use crate::glass::{
    CapsuleSpec, Hit, Links, Presentation, RowKind, RowSpec, layout_row, overflow_spec,
};
use crate::log::{LogLine, MessageLog, Retired};
use crate::model::{
    Decision, Hold, Intent, Message, MessageId, Restatement, Severity, WallStamp, tags,
};
use crate::{
    Duration, HOLD_ASK, HOLD_INFO, HOLD_WARN, Instant, LOG_CAP, MARGIN, MAX_LIVE, STALE_HANDOFF,
};

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

/// §1.4: "Only rows in the first `committed` glass slots get
/// `on_glass_since` … so a clamped third row never burns its hold
/// unpainted"; §2.4: "Holds anchor at FIRST glass". A fourth message turns
/// the third slot into the overflow row: the third row is no longer painted,
/// yet it keeps burning the hold it anchored, folds `Folded` while hidden,
/// and — if it returns before that — comes back with a burned anchor.
#[test]
fn a_row_displaced_by_the_overflow_row_does_not_burn_its_hold_hidden() {
    let now = t0();
    let mut c = fresh(now);
    let a = c.post(info("a"), stamp(1), now).id;
    let b = c.post(info("b"), stamp(2), now).id;
    let third = c.post(info("c"), stamp(3), now).id;
    assert_eq!(c.commit_rows(now, 3), Some(3));
    assert_eq!(glass_ids(&c), vec![a, b, third]);
    assert_eq!(c.live(third).unwrap().fold_at, Some(now + HOLD_INFO));

    // A fourth Info arrives one second later: the band becomes 2 rows + overflow.
    let d = c.post(info("d"), stamp(4), now + secs(1)).id;
    assert_eq!(
        c.commit_rows(now + secs(1), 3),
        None,
        "3 wanted, 3 committed"
    );
    assert_eq!(
        glass_ids(&c),
        vec![a, b],
        "the third slot is the overflow row"
    );
    let p = c.presentation(120, &chars, None, Links::Painted);
    assert_eq!(p.rows[2].kind, RowKind::Overflow { hidden: 2 });
    assert_eq!(c.queued(), 2, "c and d are both off glass");
    assert!(c.live(d).unwrap().is_queued());

    // The displaced row is off glass but is neither queued (no patience) nor
    // released from its hold.
    let displaced = c.live(third).unwrap();
    assert!(
        displaced.is_queued() || displaced.fold_at.is_none(),
        "a row that is not painted must not burn its hold: on_glass_since={:?} fold_at={:?} is_queued={}",
        displaced.on_glass_since,
        displaced.fold_at,
        displaced.is_queued()
    );
}

/// The second half of the same defect: the displaced row folds `Folded`
/// (read) at its ORIGINAL anchor while hidden behind the overflow row.
#[test]
fn a_row_hidden_behind_the_overflow_row_does_not_fold_as_read() {
    let now = t0();
    let mut c = fresh(now);
    let _a = c.post(info("a"), stamp(1), now).id;
    let _b = c.post(info("b"), stamp(2), now).id;
    let third = c.post(info("c"), stamp(3), now).id;
    c.commit_rows(now, 3);
    let _d = c.post(info("d"), stamp(4), now + secs(1)).id;
    c.commit_rows(now + secs(1), 3);
    assert!(!glass_ids(&c).contains(&third), "c is hidden");
    let settled = c.settle(now + HOLD_INFO, true);
    let how = settled
        .retired
        .iter()
        .find(|(id, _)| *id == third)
        .map(|(_, h)| h.clone());
    assert_ne!(
        how,
        Some(Retired::Folded),
        "c was painted for one second and then hidden; it must not retire as Folded (read) — \
         a hidden row either waits for the glass or folds Unseen by patience"
    );
}

/// …and when a displaced row RETURNS to the glass it is re-anchored (the
/// promotion rule: "the freed bottom slot is filled by the highest-ranked
/// queued row, whose hold anchors THEN").
#[test]
fn a_displaced_row_re_anchors_when_it_returns_to_the_glass() {
    let now = t0();
    let mut c = fresh(now);
    let a = c.post(info("a"), stamp(1), now).id;
    let b = c.post(info("b"), stamp(2), now).id;
    let third = c.post(info("c"), stamp(3), now).id;
    c.commit_rows(now, 3);
    let d = c.post(info("d"), stamp(4), now + secs(1)).id;
    c.commit_rows(now + secs(1), 3);
    let back = now + secs(20);
    assert!(c.resolve(a, Outcome::Ok, back));
    assert_eq!(
        glass_ids(&c),
        vec![b, third, d],
        "three live rows: no overflow row; c returns"
    );
    assert_eq!(
        c.live(third).unwrap().fold_at,
        Some(back + HOLD_INFO),
        "a row promoted to the glass anchors its hold THEN; it came back with a burned anchor instead"
    );
}

/// The same family through the D1 clamp: an afford shrink pushes rows off
/// the glass; they keep burning.
#[test]
fn a_row_pushed_off_by_an_afford_shrink_does_not_burn_hidden() {
    let now = t0();
    let mut c = fresh(now);
    let a = c.post(info("a"), stamp(1), now).id;
    let b = c.post(info("b"), stamp(2), now).id;
    c.commit_rows(now, 3);
    assert_eq!(glass_ids(&c), vec![a, b]);
    // The window shrinks: afford 1, committed at once (the quiet is for a
    // burst of folding rows, not for a window that lost its terminal row).
    assert_eq!(c.commit_rows(now + secs(1), 1), Some(1));
    assert_eq!(glass_ids(&c), vec![a]);
    let hidden = c.live(b).unwrap();
    assert!(
        hidden.fold_at.is_none() || hidden.is_queued(),
        "b is not painted at afford 1 but keeps fold_at={:?}",
        hidden.fold_at
    );
}

/// §1.8 / the builder's own rule: "a restate or supersede clears the
/// carried flag". A carried HELD row restated by the successor before the
/// handoff commit (the config reporter re-stating its count, the update
/// reporter its words) loses the handoff cap's replacement: the flag is
/// cleared, the hold is never armed by `after_handoff_commit`, and the row
/// leaves as `Stale` at `seed + STALE_HANDOFF` — a held row on glass whose
/// hold never anchors and whose retire reason is a lie.
#[test]
fn a_carried_held_row_restated_before_commit_still_folds_on_its_hold() {
    let now = t0();
    let mut parent = fresh(now);
    let id = parent
        .post(
            Message::new(
                tags::CONFIG,
                Severity::Warn,
                "3 keybindings in aterm.toml were skipped",
            )
            .key("config.keybindings"),
            stamp(1),
            now,
        )
        .id;
    parent.commit_rows(now, 3);
    let carry = parent.carried();

    let later = now + secs(5);
    let mut child = fresh(later);
    child.seed_carried(&carry, later);
    assert_eq!(
        child.live(id).unwrap().stale_at,
        Some(later + STALE_HANDOFF)
    );
    // The successor restates the words (no hold change) before the commit.
    assert!(child.restate(
        id,
        Restatement {
            title: Some("2 keybindings in aterm.toml were skipped".into()),
            ..Restatement::default()
        },
        later + secs(1)
    ));
    let commit = later + secs(2);
    child.after_handoff_commit(commit);
    let row = child.live(id).unwrap();
    assert!(
        row.fold_at.is_some(),
        "a Warn Default row on glass after the commit must have an anchored hold; fold_at={:?} stale_at={:?}",
        row.fold_at,
        row.stale_at
    );
}

/// The observable consequence of the previous test: the row overstays its
/// 45 s hold and leaves as `Stale` after 125 s.
#[test]
fn a_carried_held_row_restated_before_commit_leaves_on_time_and_as_folded() {
    let now = t0();
    let mut parent = fresh(now);
    let id = parent
        .post(
            Message::new(tags::CONFIG, Severity::Warn, "3 keybindings were skipped")
                .key("config.keybindings"),
            stamp(1),
            now,
        )
        .id;
    parent.commit_rows(now, 3);
    let carry = parent.carried();
    let later = now + secs(5);
    let mut child = fresh(later);
    child.seed_carried(&carry, later);
    child.restate(
        id,
        Restatement {
            title: Some("2 keybindings were skipped".into()),
            ..Restatement::default()
        },
        later + secs(1),
    );
    let commit = later + secs(2);
    child.after_handoff_commit(commit);
    let on_time = child.settle(commit + HOLD_WARN, true);
    assert_eq!(
        on_time.retired,
        vec![(id, Retired::Folded)],
        "the Warn row folds after HOLD_WARN; instead it lingers (and later retires as {:?})",
        child.settle(later + STALE_HANDOFF, true).retired
    );
}

/// After the handoff commit a carried held row that is QUEUED (never on
/// the successor's glass) gets `fold_at` armed anyway and folds `Folded`
/// without ever being painted.
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
    child.seed_carried(&carry, later);
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

/// §1.4: "Ask and Standing rows are never evicted by a post"; the glass is
/// STABLE ORDER — "a row on glass keeps its position while it lives". A
/// flood of queued Warn posts past MAX_LIVE evicts the OLDEST lowest-severity
/// held row — which is the row at the TOP of the glass, under the pointer —
/// while 61 never-painted queued rows survive.
#[test]
fn a_flood_past_max_live_does_not_evict_a_row_on_glass() {
    let now = t0();
    let mut c = fresh(now);
    let a = c.post(info("a"), stamp(1), now).id;
    let b = c.post(info("b"), stamp(2), now).id;
    let third = c.post(info("c"), stamp(3), now).id;
    c.commit_rows(now, 3);
    assert_eq!(glass_ids(&c), vec![a, b, third]);
    for i in 0..(MAX_LIVE - 3 + 1) {
        c.post(
            Message::new(tags::PACKAGES, Severity::Warn, &format!("warn {i}")),
            stamp(10 + i as u64),
            now + Duration::from_millis(1 + i as u64),
        );
    }
    assert_eq!(c.live_rows().count(), MAX_LIVE);
    assert!(
        c.live(a).is_some(),
        "the top glass row was evicted by a flood of queued rows: glass now {:?}, evicted {:?}",
        glass_ids(&c),
        c.log()
            .pending_lines()
            .filter_map(|l| match l {
                LogLine::Retired {
                    id,
                    how: Retired::Evicted,
                    title,
                    ..
                } => Some((*id, title.clone())),
                _ => None,
            })
            .collect::<Vec<_>>()
    );
}

/// THE METER IS THE ROW (ruling 55, 2026-09-23). It used to be sacrificed
/// after the excerpt, and this test caught it coming BACK at a capsule
/// long→short transition. It is no longer sacrificed at all: at every width
/// from 200 cols down to 8 a metered row's meter is `(0, cols, fill)` — it
/// can neither leave nor come back.
#[test]
fn the_meter_spans_the_row_at_every_width() {
    let spec = RowSpec {
        kind: RowKind::Message(MessageId::from_raw(1).unwrap()),
        severity: Severity::Info,
        live: true,
        glyph: '\u{21bb}',
        title: "Downloading aterm v0.91.0",
        detail0: None,
        meter: Some((Some(500), "45 MB / 90 MB")),
        animated: false,
        busy: false,
        eta: false,
        load: None,
        load_slot: false,
        capsules: caps(&[Intent::OpenSettings {
            route: "/updates".into(),
        }]),
    };
    let off: Vec<(usize, Option<(usize, usize, u16)>)> = (8..=200)
        .rev()
        .map(|cols| (cols, layout_row(&spec, cols, &chars).meter))
        .filter(|&(cols, meter)| meter != Some((0, cols, 500)))
        .collect();
    assert!(off.is_empty(), "(cols, meter) off the whole row: {off:?}");
}

/// The same non-monotonicity for the excerpt on an UNMETERED row: two
/// capsules with a combined long→short delta ≥ 3 + DETAIL_FLOOR bring the
/// excerpt back after it was dropped.
#[test]
fn the_excerpt_never_comes_back_while_narrowing() {
    let spec = RowSpec {
        kind: RowKind::Message(MessageId::from_raw(2).unwrap()),
        severity: Severity::Warn,
        live: false,
        glyph: '\u{26a0}',
        title: "aterm.toml has a problem",
        detail0: Some("skipping \"cmd+shift+k\": unknown action \"foo\""),
        meter: None,
        animated: false,
        busy: false,
        eta: false,
        load: None,
        load_slot: false,
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

/// §1.6: "the log keeps it". A LIVE row's record can fall out of the ring
/// while the row is still on glass — LOG_CAP LogOnly posts push it out —
/// after which `log().get(id)` is `None` for a row `on_glass()` still yields.
#[test]
fn a_live_row_keeps_its_log_record() {
    let now = t0();
    let mut c = fresh(now);
    let standing = c
        .post(
            Message::new(tags::RENDER, Severity::Warn, "GPU lost").hold(Hold::Standing),
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

/// Codec identity: a message with ONE empty detail line used to encode to
/// `detail=` and decode to no lines at all. An empty line is dropped at
/// ingress now (the builder, `normalized`, `restate`), so the live row, its
/// Posted line and the decoded record all agree.
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

/// Never a panic, rows exactly `cols` cells, at every width below the
/// design's own test floor (0..8).
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
        let _ = layout_row(&overflow_spec(99, Links::Painted), 0, &chars);
        let _ = layout_row(&overflow_spec(99, Links::Withheld), 0, &chars);
    }
}

/// The hit test agrees with the layout in the degenerate clip too
/// (cols 8..40, where capsules may stand from column 0).
#[test]
fn hit_test_agrees_with_layout_in_the_degenerate_clip() {
    let fixtures = crate::glass::tests::fixtures();
    for cols in 8..40usize {
        let rows: Vec<_> = fixtures
            .iter()
            .map(|(_, s)| layout_row(s, cols, &chars))
            .collect();
        let p = Presentation { cols, rows };
        for (r, row) in p.rows.iter().enumerate() {
            let RowKind::Message(id) = row.kind else {
                unreachable!()
            };
            for col in 0..cols {
                let want = match row
                    .capsules
                    .iter()
                    .find(|c| col >= c.col && col < c.col + c.width)
                {
                    Some(c) if c.action.is_details() => Hit::Details(id),
                    Some(c) => Hit::Capsule(id, c.action),
                    None => Hit::Body(id),
                };
                assert_eq!(p.hit(r, col), want, "row {r} col {col} @ {cols}");
            }
        }
    }
}

/// Under a grapheme-aware measure that is NOT `chars().count()` (every
/// uppercase letter two cells here), no placed element reaches past `cols`
/// and the capsules stay right-aligned, at every width where the row is not
/// degenerate.
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
            // The meter is the row's surface, under every piece: never a span.
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
                end = c + crate::ELAPSED_W;
            }
            if let Some(c) = row.eta {
                assert!(c >= end + 1, "{name}@{cols}: eta overlaps");
                end = c + row.eta_width();
            }
            if let Some((c, w)) = row.load {
                assert!(c >= end + 3, "{name}@{cols}: load overlaps");
                end = c + wide(w);
            }
            if let Some((c, s)) = &row.stats {
                assert!(*c >= end + 2, "{name}@{cols}: stats overlap");
                end = c + wide(s);
            }
            assert!(
                first.col >= end + 2,
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

// ---- design-literal behaviour, pinned (review nits, not defects) ----

/// §1.8 says "`after_handoff_commit` folds every carried non-Live/Standing
/// row after its severity hold" — wording inherited from the status bars,
/// which had no Ask or For rows. Decision taken here: the row's OWN hold
/// (`arm`'s span — the severity's for `Default`, the named span for `For` /
/// `Ask`), so a question that had ten minutes in the parent keeps them in
/// the successor instead of dropping to 30 s because a handoff happened.
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
                .hold(Hold::Ask { for_: HOLD_ASK }),
            stamp(1),
            now,
        )
        .id;
    parent.commit_rows(now, 3);
    let carry = parent.carried();
    let later = now + secs(5);
    let mut child = fresh(later);
    child.seed_carried(&carry, later);
    child.after_handoff_commit(later + secs(1));
    let row = child.live(id).unwrap();
    assert_eq!(
        row.msg.hold,
        Hold::Ask { for_: HOLD_ASK },
        "the hold word is kept"
    );
    assert_eq!(
        row.fold_at,
        Some(later + secs(1) + HOLD_ASK),
        "…and the anchor is its own span"
    );
    assert!(HOLD_ASK > HOLD_INFO);
}

/// §1.4 literally: a post with "no key on either side and equal (tag,
/// title, detail)" is a Duplicate — severity, hold, meter and actions are
/// not compared, so an Error re-post of a live Info row is swallowed into
/// `repeats` and the row stays Info.
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
