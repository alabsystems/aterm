// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Fleet Field's laws (`docs/DESIGN-fleet-field-2026-11-06.md` B2), each with a negative
//! control.

use super::*;
use crate::fleet_glance::{
    AgentTally, BookFacts, COUNT_FRESH, FleetNeed, FleetSession, FleetSnapshot,
};
use aterm_types::network_state::HistoryState;

thread_local! {
    /// How many fields this thread projected: the Fleet Field's idle tax.
    pub(crate) static PROJECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const NOW: u64 = 1_800_000_000_000;
const FRESH_MS: u64 = COUNT_FRESH.as_millis() as u64;

fn mac(n: usize, build: &str) -> FleetMac {
    FleetMac {
        name: format!("m{n}"),
        dial: format!("m{n}.local"),
        node: Some(format!("{n:064x}")),
        read_at_ms: NOW,
        build: Some(build.to_owned()),
        activity: Some(AgentTally::default()),
        instances_read: 1,
        ..FleetMac::default()
    }
}

fn silent(n: usize, reason: Reach, why: &str) -> Unanswered {
    Unanswered {
        name: format!("m{n}"),
        dial: format!("m{n}.local"),
        why: why.to_owned(),
        reason,
        ..Unanswered::default()
    }
}

fn need(sid: &str, word: NeedWord) -> FleetNeed {
    FleetNeed {
        sid: sid.to_owned(),
        word,
        who: "claude".to_owned(),
        worker_of: None,
        age_ms: Some(240_000),
        what: None,
        claim: None,
        carries_on: false,
    }
}

fn tally(busy: u16, waiting: u16, walled: u16) -> Option<AgentTally> {
    Some(AgentTally {
        agents: busy + waiting + walled + 1,
        busy,
        idle: 1,
        waiting,
        walled,
        unknown: 0,
    })
}

/// `n` seats shaped like the live lab: every 9th silent, every 13th carried (and so listed
/// silent too, as [`crate::fleet_glance::carry`] lists it), every 7th busy, every 11th walled,
/// every 17th waiting, every 19th an older aterm, every 23rd on another build.
fn fleet(n: usize) -> FleetSnapshot {
    let mut snapshot = FleetSnapshot {
        read_at_ms: NOW,
        ..FleetSnapshot::default()
    };
    for i in 1..=n {
        if i % 9 == 0 {
            snapshot.unanswered.push(silent(
                i,
                Reach::NoAnswer,
                &format!("m{i} at 10.0.0.{i}: no answer"),
            ));
            continue;
        }
        let mut m = mac(i, if i % 23 == 0 { "0.156.0" } else { "0.160.0" });
        if i % 7 == 0 {
            m.activity = tally(2, 0, 0);
        }
        if i % 11 == 0 {
            m.activity = tally(1, 0, 1);
        }
        if i % 17 == 0 {
            m.activity = tally(0, 1, 0);
        }
        if i % 19 == 0 {
            m.older = true;
            m.activity = None;
        }
        if i % 13 == 0 {
            m.carried = true;
            m.read_at_ms = NOW - 60_000;
            snapshot
                .unanswered
                .push(silent(i, Reach::NoReply, "no reply"));
        }
        snapshot.macs.push(m);
    }
    snapshot
}

fn slot_of(snapshot: FleetSnapshot) -> FleetSlot {
    FleetSlot {
        generation: 3,
        snapshot: Some(Arc::new(snapshot)),
        book: BookFacts {
            in_fleet: true,
            ..BookFacts::default()
        },
        ..FleetSlot::default()
    }
}

fn active() -> NetworkObservation {
    NetworkObservation {
        visibility: Visibility::Active,
        history: HistoryState::Ready,
        ..NetworkObservation::default()
    }
}

fn field_of(snapshot: FleetSnapshot, now_ms: u64) -> Field {
    project(Some(&slot_of(snapshot)), &active(), now_ms)
}

fn inks() -> aterm_vitals::ink::VitalsInks {
    crate::stats_deck::cached_inks([20, 20, 20], [230, 230, 230], [100, 180, 200])
}

/// Paint `field` alone on a band `w` pt wide at `scale`, its top at 0.
fn painted(field: &Field, w: f32, scale: f32) -> (Vec<DrawPrim>, Vec<Hit>) {
    let inks = inks();
    let mut p = Painter::new(&inks, scale, 14.0 * scale, false);
    let mut hits = Vec::new();
    paint(&mut p, field, &active(), None, 0.0, w * scale, &mut hits);
    (p.prims, hits)
}

fn seat_hits(hits: &[Hit]) -> Vec<(SeatKey, crate::stats_paint::Rect)> {
    hits.iter()
        .filter_map(|hit| match hit.kind {
            HitKind::Network(Target::Seat(key)) => Some((key, hit.rect)),
            _ => None,
        })
        .collect()
}

fn texts(prims: &[DrawPrim]) -> Vec<(f32, String)> {
    prims
        .iter()
        .filter_map(|prim| match prim {
            DrawPrim::Text { x, s, .. } => Some((*x, s.clone())),
            _ => None,
        })
        .collect()
}

fn inside(r: crate::stats_paint::Rect, x: f32, y: f32) -> bool {
    x >= r.0 && x < r.0 + r.2 && y >= r.1 && y < r.1 + r.3
}

/// The seats a painted field accounts for: one a seat hit, plus the presence groups' figures.
fn accounted(prims: &[DrawPrim], hits: &[Hit]) -> usize {
    let mut n = seat_hits(hits).len();
    for hit in hits {
        let HitKind::Network(Target::Group(class)) = hit.kind else {
            continue;
        };
        if class.presence().is_none() {
            continue;
        }
        let figure = prims
            .iter()
            .find_map(|prim| match prim {
                DrawPrim::Text { x, baseline, s, .. } if inside(hit.rect, *x, *baseline - 1.0) => {
                    s.parse::<usize>().ok()
                }
                _ => None,
            })
            .expect("a group's figure");
        n += figure;
    }
    n
}

/// B2 #1. Negative control: a projection that skips the carried Macs accounts for fewer.
#[test]
fn every_seat_drawn_or_counted_no_plus_n() {
    for n in [0, 1, 5, 63, 250] {
        let field = field_of(fleet(n), NOW);
        assert_eq!(field.seats.len(), n, "one seat a Mac, a carried one once");
        for scale in [1.0, 2.0] {
            for w in (300..=1600).step_by(50) {
                let (prims, hits) = painted(&field, w as f32, scale);
                assert_eq!(accounted(&prims, &hits), n, "n {n} at {w} pt ×{scale}");
                for (_, s) in texts(&prims) {
                    assert!(!s.starts_with('+'), "no +N: {s}");
                }
            }
        }
    }
    // Negative controls, through the projection: a Mac gone from the read is one seat fewer,
    // and a carried Mac whose silent row names another dial is no longer one seat but two.
    let mut gone = fleet(63);
    gone.macs.remove(0);
    let (prims, hits) = painted(&field_of(gone, NOW), 900.0, 1.0);
    assert_eq!(accounted(&prims, &hits), 62);
    let mut twice = fleet(63);
    let carried = twice
        .macs
        .iter()
        .find(|m| m.carried)
        .expect("a carried Mac");
    let row = twice
        .unanswered
        .iter_mut()
        .find(|u| u.dial == carried.dial)
        .expect("its silent row");
    row.dial = format!("{}.lan", row.dial);
    let (prims, hits) = painted(&field_of(twice, NOW), 900.0, 1.0);
    assert_eq!(accounted(&prims, &hits), 64);
}

/// B2 #2. Negative control: renaming m5 to m500 moves that seat to the end.
#[test]
fn seat_columns_hold_still_across_order_and_state() {
    let base = fleet(63);
    let rects = |snapshot: FleetSnapshot| {
        let mut map = seat_hits(&painted(&field_of(snapshot, NOW), 900.0, 2.0).1);
        map.sort_by_key(|(key, _)| format!("{key:?}"));
        map
    };
    let before = rects(base.clone());
    let mut moved = base.clone();
    moved.macs.reverse();
    moved.unanswered.rotate_left(3);
    for m in &mut moved.macs {
        m.activity = if m.activity.is_some() {
            None
        } else {
            tally(3, 1, 1)
        };
        m.unproven = !m.unproven;
        m.build = Some("0.161.0".into());
    }
    // A Mac that answered goes silent, and a silent one answers: the same seats.
    let gone = moved.macs.remove(0);
    moved.unanswered.push(silent(
        gone.name[1..].parse().unwrap(),
        Reach::Reset,
        "reset",
    ));
    let at = moved
        .unanswered
        .iter()
        .position(|u| !moved.macs.iter().any(|m| m.dial == u.dial))
        .unwrap();
    let back = moved.unanswered.remove(at);
    moved.macs.push(FleetMac {
        name: back.name.clone(),
        dial: back.dial.clone(),
        ..mac(9, "0.160.0")
    });
    assert_eq!(rects(moved), before, "position is identity");

    let mut renamed = base;
    renamed.macs[4].name = "m500".into();
    let key = seat_key(&renamed.macs[4].dial);
    let after = rects(renamed);
    let at = |map: &[(SeatKey, crate::stats_paint::Rect)], k: SeatKey| {
        map.iter().find(|(key, _)| *key == k).map(|(_, r)| *r)
    };
    assert_ne!(at(&after, key), at(&before, key));
    for i in 1..5 {
        let k = seat_key(&format!("m{i}.local"));
        assert_eq!(at(&after, k), at(&before, k), "the seats before it hold");
    }
}

/// B2 #3: the boundaries come from [`cols`], not literals.
#[test]
fn tiers_from_tier_fn() {
    let inks = inks();
    for scale in [1.0_f32, 2.0] {
        let p = Painter::new(&inks, scale, 14.0 * scale, false);
        for w in [400.0_f32, 700.0, 1100.0] {
            let probe = field_of(fleet(10), NOW);
            let min = p.ui_text_w(probe.words.last().unwrap(), false);
            let c = cols(w * scale, scale, min);
            assert!(c >= COLS_MIN, "{w}");
            assert_eq!(tier(c, c), Tier::A);
            assert_eq!(tier(c + 1, c), Tier::B);
            assert_eq!(tier(2 * c, c), Tier::B);
            assert_eq!(tier(2 * c + 1, c), Tier::C);
        }
    }
    assert_eq!(tier(3, 5), Tier::A, "a few seats are never a tally");
    assert_eq!(
        tier(6, 5),
        Tier::C,
        "below 8 columns an overflow is a tally"
    );
    // The painter follows the function: a full-height hit per seat in A, a half one in B, a
    // group hit and no seat hit in C, over widths that reach every tier.
    let snapshot = fleet(20);
    let field = field_of(snapshot, NOW);
    let p = Painter::new(&inks, 1.0, 14.0, false);
    let min = p.ui_text_w(field.words.last().unwrap(), false);
    let mut seen = Vec::new();
    for w in 200..=500 {
        let w = w as f32;
        let t = tier(20, cols(w, 1.0, min));
        assert_eq!(layout(&field, &active(), w, 1.0).tier, t, "{w}");
        let (_, hits) = painted(&field, w, 1.0);
        let heights: Vec<f32> = seat_hits(&hits).iter().map(|(_, r)| r.3).collect();
        match t {
            Tier::A => assert!(heights.len() == 20 && heights.iter().all(|h| *h == 14.0)),
            Tier::B => assert!(heights.len() == 20 && heights.iter().all(|h| *h == 7.0)),
            Tier::C => assert!(heights.is_empty()),
        }
        if !seen.contains(&t) {
            seen.push(t);
        }
    }
    assert_eq!(seen, [Tier::C, Tier::B, Tier::A], "every arm ran");
}

/// The words a painted field drew: the last text, which follows every tally figure.
fn words_drawn(prims: &[DrawPrim]) -> (f32, String) {
    texts(prims).pop().expect("the field's words")
}

/// B2 #4: from the window's minimum width up, the words drawn are one of the field's forms and,
/// wherever the band holds the shortest past the seats' start, end inside the right margin.
/// Negative control: the same layout with no words reserved runs the shortest form past the
/// margin near 400 pt.
#[test]
fn up_count_never_vanishes() {
    for (n, scale) in [(63, 1.0_f32), (63, 2.0), (250, 1.0), (5, 1.0)] {
        let field = field_of(fleet(n), NOW);
        let shortest = field.words.last().unwrap().clone();
        assert_eq!(shortest, format!("{}/{n}", field.tallies.answered));
        let min = text_w(&shortest, scale);
        for pt in (160..=1600).step_by(5) {
            let w = pt as f32 * scale;
            let (prims, hits) = painted(&field, pt as f32, scale);
            let (x, text) = words_drawn(&prims);
            assert!(field.words.contains(&text), "{w}: {text}");
            let right = w - GAP_PT * scale;
            if right - SEATS_X_PT * scale >= min {
                assert!(
                    x + text_w(&text, scale) <= right,
                    "{n} at {w}: {text} from {x}"
                );
                assert!(
                    hits.iter()
                        .any(|h| matches!(h.kind, HitKind::Network(Target::Fleet))),
                    "{w}: the words keep their hit"
                );
            } else {
                assert_eq!((x, text.as_str()), (SEATS_X_PT * scale, shortest.as_str()));
            }
        }
    }
    let field = field_of(fleet(63), NOW);
    let shortest = field.words.last().unwrap().clone();
    let min = text_w(&shortest, 1.0);
    let starved = (380..=420).any(|w| {
        let w = w as f32;
        layout_with(&field, w, 1.0, 0.0).words_x + min > w - GAP_PT
    });
    assert!(starved, "the reservation is what keeps the count");
}

/// A tally that cannot hold an activity group beside the reserved words draws its presence
/// groups alone; one that cannot hold every presence group draws the ones that fit, in order,
/// and the words' `a/n` counts every seat.
#[test]
fn tally_gives_way_to_the_words() {
    let field = field_of(fleet(250), NOW);
    let group_hits = |w: f32| {
        painted(&field, w, 1.0)
            .1
            .iter()
            .filter_map(|h| match h.kind {
                HitKind::Network(Target::Group(class)) => Some(class),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let presence_only = (250..=700)
        .map(|w| w as f32)
        .find(|w| {
            let classes = group_hits(*w);
            classes.len() == 4 && classes.iter().all(|c| c.presence().is_some())
        })
        .expect("a width that holds the presence groups and no activity group");
    assert_eq!(layout(&field, &active(), presence_only, 1.0).tier, Tier::C);
    assert_eq!(layout(&field, &active(), 1000.0, 1.0).tier, Tier::C);
    let wide = group_hits(1000.0);
    assert!(
        wide.iter().any(|c| c.presence().is_none()),
        "control: a wide tally draws its activity groups: {wide:?}"
    );
    let narrow = group_hits(200.0);
    assert!(narrow.len() < 4, "{narrow:?}");
    assert!(narrow.iter().all(|c| c.presence().is_some()));
    let (_, text) = words_drawn(&painted(&field, 200.0, 1.0).0);
    assert_eq!(text, format!("{}/250", field.tallies.answered));
}

fn band_prims(prims: &[DrawPrim], y0: f32, y1: f32) -> Vec<DrawPrim> {
    prims
        .iter()
        .filter(|prim| match prim {
            DrawPrim::Panel { y, h, .. } | DrawPrim::Stroke { y, h, .. } => *y >= y0 && y + h <= y1,
            DrawPrim::Line { y1: a, y2: b, .. } => a.min(*b) >= y0 && a.max(*b) <= y1,
            _ => false,
        })
        .cloned()
        .collect()
}

/// B2 #5: a seat whose activity was not read draws nothing below its body, and one read with
/// no agents draws the baseline.
#[test]
fn absent_is_not_zero() {
    let mut copy = FleetSnapshot {
        read_at_ms: NOW,
        seats: Some(aterm_netplace::words::Seats::default()),
        ..FleetSnapshot::default()
    };
    copy.macs.push(FleetMac {
        activity: None,
        ..mac(1, "0.160.0")
    });
    let copy = field_of(copy, NOW);
    assert!(copy.copy_path);
    assert!(
        copy.words.iter().all(|w| !w.contains("busy")),
        "{:?}",
        copy.words
    );
    let (copy_prims, _) = painted(&copy, 700.0, 1.0);
    assert!(band_prims(&copy_prims, 7.5, 14.0).is_empty());

    let mut survey = FleetSnapshot {
        read_at_ms: NOW,
        ..FleetSnapshot::default()
    };
    survey.macs.push(FleetMac {
        activity: Some(AgentTally::default()),
        ..mac(1, "0.160.0")
    });
    let survey = field_of(survey, NOW);
    assert!(survey.words[0].contains("0 busy"), "{:?}", survey.words);
    let (survey_prims, _) = painted(&survey, 700.0, 1.0);
    assert_eq!(
        band_prims(&survey_prims, 7.5, 14.0).len(),
        1,
        "the baseline"
    );
}

/// A prim's geometry without its colour, relative to `(x, y)`.
fn geometry(prims: &[DrawPrim], x: f32, y: f32) -> Vec<String> {
    prims
        .iter()
        .map(|prim| match prim {
            DrawPrim::Panel {
                x: px, y: py, w, h, ..
            } => {
                format!("panel {} {} {w} {h}", px - x, py - y)
            }
            DrawPrim::Stroke {
                x: px,
                y: py,
                w,
                h,
                width,
                ..
            } => {
                format!("stroke {} {} {w} {h} {width}", px - x, py - y)
            }
            DrawPrim::Line {
                x1,
                y1,
                x2,
                y2,
                width,
                ..
            } => {
                format!("line {} {} {} {} {width}", x1 - x, y1 - y, x2 - x, y2 - y)
            }
            other => format!("{other:?}"),
        })
        .collect()
}

/// B2 #6: every body, overlay and activity mark differs by geometry once every ink is one.
#[test]
fn shape_not_colour() {
    let mut inks = inks();
    let one = inks.machine;
    inks.hollow = one;
    inks.over = one;
    inks.grid = one;
    inks.aterm = one;
    inks.words2 = one;
    for scale in [1.0_f32, 2.0] {
        let mut p = Painter::new(&inks, scale, 14.0 * scale, false);
        let g = Glyph {
            x: 10.0,
            y: 10.0,
            side: (MARK_PT * scale).round(),
            s: scale,
            hair: p.hair(),
        };
        let mut shapes: Vec<Vec<String>> = Vec::new();
        let mut take = |p: &mut Painter<'_>| {
            shapes.push(geometry(&std::mem::take(&mut p.prims), g.x, g.y));
        };
        for presence in [
            Presence::Answered,
            Presence::Carried,
            Presence::Silent,
            Presence::NotCounted,
        ] {
            body(&mut p, &g, presence);
            take(&mut p);
        }
        corner(&mut p, &g);
        take(&mut p);
        build_hair(&mut p, &g);
        take(&mut p);
        for mark in [
            Activity::Quiet,
            Activity::Busy(1),
            Activity::Busy(2),
            Activity::Waiting,
            Activity::Walled,
            Activity::Stuck,
        ] {
            activity(&mut p, &g, mark);
            take(&mut p);
        }
        assert_eq!(shapes.len(), 12);
        for (i, a) in shapes.iter().enumerate() {
            assert!(!a.is_empty());
            for b in &shapes[i + 1..] {
                assert_ne!(a, b, "×{scale}");
            }
        }
    }
}

/// B2 #8. Negative control: the same Mac without its wall draws the busy bar.
#[test]
fn wall_outranks_busy() {
    let mut snapshot = FleetSnapshot {
        read_at_ms: NOW,
        ..FleetSnapshot::default()
    };
    snapshot.macs.push(FleetMac {
        activity: tally(2, 0, 1),
        ..mac(1, "0.160.0")
    });
    let walled = field_of(snapshot.clone(), NOW);
    assert_eq!(walled.seats[0].activity, Some(Activity::Walled));
    assert!(walled.words[0].contains("2 busy"), "{:?}", walled.words);
    assert!(walled.words[0].contains("1 walled"));
    assert!(walled.seats[0].attention);
    snapshot.macs[0].activity = tally(2, 0, 0);
    let busy = field_of(snapshot, NOW);
    assert_eq!(busy.seats[0].activity, Some(Activity::Busy(2)));
    assert!(!busy.seats[0].attention, "busy alone asks for nobody");
}

fn corner_drawn(prims: &[DrawPrim], scale: f32, over: aterm_vitals::ink::Rgb) -> bool {
    prims.iter().any(|prim| {
        matches!(prim, DrawPrim::Panel { w, h, fill, .. }
            if *w == 2.0 * scale && *h == 2.0 * scale && *fill == rgba(over, 255))
    })
}

/// B2 #9: the corner comes from `SeatRow.needs_update` (the copy path) alone, never from an old
/// build. Negative control: the same copy seat whose row says no update has no corner.
#[test]
fn needs_update_only_from_seat_row() {
    let inks = inks();
    let mut survey = FleetSnapshot {
        read_at_ms: NOW,
        ..FleetSnapshot::default()
    };
    survey.macs.push(mac(1, "0.147.0"));
    survey.macs.push(mac(2, "0.160.0"));
    survey.macs.push(mac(3, "0.160.0"));
    let (prims, _) = painted(&field_of(survey, NOW), 700.0, 1.0);
    assert!(
        !corner_drawn(&prims, 1.0, inks.over),
        "a build alone is no update"
    );

    let mut copy = FleetSnapshot {
        read_at_ms: NOW,
        seats: Some(aterm_netplace::words::Seats::default()),
        ..FleetSnapshot::default()
    };
    copy.macs.push(FleetMac {
        needs_update: true,
        activity: None,
        ..mac(1, "0.147.0")
    });
    let field = field_of(copy.clone(), NOW);
    assert!(field.seats[0].needs_update && field.seats[0].attention);
    assert!(
        field.words[0].contains("1 needs update"),
        "{:?}",
        field.words
    );
    assert!(corner_drawn(&painted(&field, 700.0, 1.0).0, 1.0, inks.over));
    copy.macs[0].needs_update = false;
    assert!(!corner_drawn(
        &painted(&field_of(copy.clone(), NOW), 700.0, 1.0).0,
        1.0,
        inks.over
    ));
    // A silent seat of the copy whose row says it needs an update is counted and drawn so, as
    // `aterm fleet` lists it. Negative control: its row says no update.
    copy.macs.push(FleetMac {
        activity: None,
        ..mac(2, "0.160.0")
    });
    copy.macs.push(FleetMac {
        activity: None,
        ..mac(3, "0.160.0")
    });
    copy.unanswered.push(Unanswered {
        needs_update: true,
        build: Some("0.147.0".into()),
        ..silent(
            106,
            Reach::Silent {
                since_ms: NOW - 60_000,
            },
            "silent for 1 min",
        )
    });
    let field = field_of(copy.clone(), NOW);
    let m106 = field.seats.iter().find(|s| s.name == "m106").expect("m106");
    assert!(m106.needs_update && m106.presence == Presence::Silent);
    assert!(
        field.words[0].contains("1 needs update"),
        "{:?}",
        field.words
    );
    let builds = details(&field, Target::Fleet)
        .into_iter()
        .find(|(n, _)| n == "Builds")
        .expect("builds")
        .1;
    assert!(builds.contains("m106 (needs update)"), "{builds}");
    assert!(corner_drawn(&painted(&field, 700.0, 1.0).0, 1.0, inks.over));
    copy.unanswered[0].needs_update = false;
    let field = field_of(copy, NOW);
    assert!(field.words.iter().all(|w| !w.contains("needs update")));
    assert!(!corner_drawn(
        &painted(&field, 700.0, 1.0).0,
        1.0,
        inks.over
    ));
}

/// B2 #10. Negative control: on one build no seat has the hair.
#[test]
fn prevailing_build() {
    let mut snapshot = FleetSnapshot {
        read_at_ms: NOW,
        ..FleetSnapshot::default()
    };
    for i in 1..=5 {
        snapshot
            .macs
            .push(mac(i, if i == 4 { "0.156.0" } else { "0.160.0" }));
    }
    let field = field_of(snapshot.clone(), NOW);
    assert_eq!(field.prevailing.as_deref(), Some("0.160.0"));
    let odd: Vec<&str> = field
        .seats
        .iter()
        .filter(|s| s.other_build)
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(odd, ["m4"]);
    assert!(
        field.words[0].contains("1 other build"),
        "{:?}",
        field.words
    );
    snapshot.macs[3].build = Some("0.160.0".into());
    let one = field_of(snapshot, NOW);
    assert!(one.seats.iter().all(|s| !s.other_build));
    assert_eq!(
        prevailing(
            [
                "0.155.0",
                "0.99.0",
                "0.160.0+gabc",
                "0.155.0",
                "0.160.0+gabc"
            ]
            .into_iter()
        ),
        Some("0.160.0+gabc".into()),
        "a tie goes to the highest release, numerically"
    );
    assert_eq!(
        prevailing(["0.99.0", "0.100.0"].into_iter()),
        Some("0.100.0".into())
    );
}

/// B2 #11, the GUI half: every producer token round-trips, an unknown one is `Unread` with its
/// words kept, and two `no-answer` Macs at different addresses are one lens group.
#[test]
fn reasons_typed_by_producer() {
    // The survey that produces these tokens dials from unix only.
    #[cfg(unix)]
    for token in aterm_net::fleet_member::REASONS {
        assert_eq!(Reach::parse(token).token(), Some(*token));
    }
    assert_eq!(Reach::parse("zzz"), Reach::Unread);
    let mut snapshot = FleetSnapshot {
        read_at_ms: NOW,
        ..FleetSnapshot::default()
    };
    snapshot.unanswered.push(silent(
        1,
        Reach::parse("no-answer"),
        "m1 at 10.0.0.1:7414: did not answer in time",
    ));
    snapshot.unanswered.push(silent(
        2,
        Reach::parse("no-answer"),
        "m2 at 10.0.0.2:7414: did not answer in time",
    ));
    snapshot
        .unanswered
        .push(silent(3, Reach::parse("zzz"), "a future failure"));
    let field = field_of(snapshot, NOW);
    let groups = silence_groups(&field);
    assert_eq!(groups.len(), 2, "{groups:?}");
    assert_eq!(groups[0], (Reach::NoAnswer, vec!["m1".into(), "m2".into()]));
    let key = field.seats[2].key;
    let rows = details(&field, Target::Seat(key));
    let presence = &rows.iter().find(|(n, _)| n == "Presence").unwrap().1;
    assert!(presence.contains("reason not read") && presence.contains("a future failure"));
    let lens = details(&field, Target::Fleet);
    let not = &lens.iter().find(|(n, _)| n == "Not answering").unwrap().1;
    assert!(not.starts_with("did not answer in time — m1, m2"), "{not}");
}

/// B2 #14. Negative control: a fresh read draws its seats.
#[test]
fn stale_or_future_read_draws_no_seats() {
    for now in [NOW + FRESH_MS + 1, NOW - 1] {
        let field = field_of(fleet(10), now);
        assert!(matches!(field.source, Source::Stale { .. }));
        let (prims, hits) = painted(&field, 700.0, 1.0);
        assert!(seat_hits(&hits).is_empty());
        assert!(
            prims
                .iter()
                .all(|prim| matches!(prim, DrawPrim::Text { .. })),
            "words only"
        );
        let words = texts(&prims);
        assert!(words[0].1.starts_with("fleet read "), "{words:?}");
        assert!(targets(&field, &layout(&field, &active(), 700.0, 1.0)) == vec![Target::Fleet]);
    }
    let stale = field_of(fleet(10), NOW + 12 * 60_000);
    assert_eq!(stale.words[0], "fleet read 12 min ago");
    let (_, hits) = painted(&field_of(fleet(10), NOW), 700.0, 1.0);
    assert_eq!(seat_hits(&hits).len(), 10);
}

/// B2 #15. Negative control: with the clock still the key holds.
#[test]
fn freshness_edge_rekeys_without_a_read() {
    let mut snapshot = fleet(5);
    snapshot.macs[1].carried = true;
    snapshot.macs[1].read_at_ms = NOW - FRESH_MS;
    let slot = slot_of(snapshot);
    let at = project(Some(&slot), &active(), NOW);
    assert_eq!(at.seats[1].presence, Presence::Carried);
    let again = project(Some(&slot), &active(), NOW);
    assert_eq!(again.key, at.key, "the clock still: the key holds");
    let past = project(Some(&slot), &active(), NOW + 1);
    assert_eq!(past.seats[1].presence, Presence::Silent);
    assert_ne!(past.key, at.key);
    assert_eq!(
        slot.snapshot.as_ref().unwrap().next_freshness_change(NOW),
        Some(NOW + 1),
        "the service turn's due is this edge"
    );
}

/// B2 #16. Negative control: nothing left out, no `+`.
#[test]
fn lower_bound_plus() {
    let mut snapshot = FleetSnapshot {
        read_at_ms: NOW,
        ..FleetSnapshot::default()
    };
    snapshot.macs.push(FleetMac {
        activity: tally(42, 0, 0),
        left_out: 3,
        ..mac(1, "0.160.0")
    });
    let field = field_of(snapshot.clone(), NOW);
    assert!(field.lower_bound);
    assert!(field.words[0].contains("42+ busy"), "{:?}", field.words);
    let lens = details(&field, Target::Fleet);
    assert!(
        lens.iter()
            .any(|(_, v)| v.contains("at least 42 agents busy"))
    );
    snapshot.macs[0].left_out = 0;
    let exact = field_of(snapshot, NOW);
    assert!(!exact.lower_bound);
    assert!(exact.words.iter().all(|w| !w.contains('+')));
}

/// B2 #17: no seat count moves the band's height, and every hit stays inside it; the Fleet
/// lens opened with the bars off moves no terminal geometry.
#[test]
fn render_integrity() {
    let g = |w: f32| crate::stats_deck::DeckGeom {
        w,
        h: 200.0,
        pad: 0.0,
        scale: 2.0,
        font_px: 28.0,
    };
    let height = super::super::height_px(2.0);
    for n in [0, 63, 256] {
        let field = field_of(fleet(n), NOW);
        assert_eq!(super::super::height_px(2.0), height);
        for w in [800.0, 1400.0, 2200.0] {
            let (_, hits) = super::super::paint_with_inks(
                &g(w),
                300.0,
                &aterm_types::network_state::NetworkStateSnapshot::default(),
                &crate::lab_deck::LabDeck::default(),
                Some(&field),
                None,
                &inks(),
                false,
            );
            for hit in hits {
                assert!(hit.rect.1 >= 300.0 && hit.rect.1 + hit.rect.3 <= 300.0 + height as f32);
                assert!(
                    hit.rect.0 >= 0.0 && hit.rect.0 + hit.rect.2 <= w + 0.5,
                    "{hit:?}"
                );
            }
        }
    }
    let mut app = crate::App::headless_for_test();
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    let wid = *app.windows.keys().next().expect("window");
    let before = (
        app.windows[&wid].rows,
        app.windows[&wid].cols,
        app.windows[&wid].metrics.head,
    );
    app.fleet_field = Some(Arc::new(field_of(fleet(63), NOW)));
    assert!(app.open_network_lens());
    app.windows.get_mut(&wid).unwrap().stats_lens.network_target = Target::Fleet;
    assert!(app.lens_frame(wid).is_some());
    assert_ne!(app.stats_lens_fp(wid), 0);
    assert_eq!(
        (
            app.windows[&wid].rows,
            app.windows[&wid].cols,
            app.windows[&wid].metrics.head
        ),
        before
    );
}

/// B2 #18: undrawn, the field reads nothing and projects nothing; drawn with an unchanged
/// generation it projects once.
#[test]
fn zero_idle_tax() {
    let mut app = crate::App::headless_for_test();
    let wid = *app.windows.keys().next().expect("window");
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    app.network_source = None;
    app.fleet_glance = Some(crate::fleet_glance::FleetGlance::over(slot_of(fleet(5))));
    let reads = || crate::fleet_glance::GENERATION_READS.with(std::cell::Cell::get);
    let projections = || PROJECTIONS.with(std::cell::Cell::get);
    let (r0, p0) = (reads(), projections());
    assert_eq!(app.network_state_deadline(), None, "nothing draws the band");
    let now = std::time::Instant::now();
    app.service_network_state(now);
    assert_eq!(
        (reads(), projections()),
        (r0, p0),
        "an undrawn band reads nothing"
    );
    assert!(app.fleet_field.is_none());

    assert!(app.open_network_lens());
    assert!(
        app.network_state_deadline().is_some(),
        "a drawn field arms the band"
    );
    app.service_network_state(now);
    assert_eq!(projections(), p0 + 1);
    let field = app.fleet_field.clone().expect("projected");
    assert_eq!(field.seats.len(), 5);
    app.service_network_state(now + std::time::Duration::from_secs(1));
    assert_eq!(projections(), p0 + 1, "the same generation projects once");
    assert!(reads() > r0, "a drawn band reads the generation");
    assert!(Arc::ptr_eq(&field, app.fleet_field.as_ref().unwrap()));
    app.close_stats_lens(wid);
    assert_eq!(app.network_state_deadline(), None);
}

/// B2 #19. Negative control: a quiet seat gains a stop once it has a need.
#[test]
fn navigation_visits_attention_only() {
    let snapshot = fleet(63);
    let field = field_of(snapshot.clone(), NOW);
    let network = aterm_types::network_state::NetworkStateSnapshot::default();
    let lab = crate::lab_deck::LabDeck::default();
    let stops = |field: &Field| {
        super::super::targets_with(&network, &lab, Some(field), (900.0, 1.0))
            .into_iter()
            .filter_map(|t| match t {
                Target::Seat(key) => Some(key),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let attention: Vec<SeatKey> = field
        .seats
        .iter()
        .filter(|s| s.attention)
        .map(|s| s.key)
        .collect();
    assert!(!attention.is_empty() && attention.len() < field.seats.len());
    assert_eq!(stops(&field), attention, "in strip order");
    let all = super::super::targets_with(&network, &lab, Some(&field), (900.0, 1.0));
    assert_eq!(&all[..3], [Target::Overview, Target::Local, Target::Fleet]);
    let meters = super::super::meters(&network, &lab, Some(&field), (900.0, 1.0));
    let fleet_meters = meters.len() - 4;
    assert_eq!(
        fleet_meters,
        2 + attention.len(),
        "Fleet, the strip, the attention seats"
    );

    let quiet = field
        .seats
        .iter()
        .find(|s| !s.attention && s.presence == Presence::Answered)
        .expect("a quiet seat");
    let SeatSrc::Mac(i) = quiet.src else {
        panic!("a quiet seat answered")
    };
    let key = quiet.key;
    let mut asked = snapshot;
    asked.macs[i].needs.push(need("s-1a2b", NeedWord::Question));
    let asked = field_of(asked, NOW);
    assert!(stops(&asked).contains(&key));
    assert!(!stops(&field).contains(&key));
    let lens = details(&asked, Target::Fleet);
    let waiting = &lens
        .iter()
        .find(|(n, _)| n == "Waiting on a person")
        .unwrap()
        .1;
    assert!(
        waiting.contains("s-1a2b · claude · question · 4 min"),
        "{waiting}"
    );
}

/// B2 #20: with no fleet the strip draws the observer's peers by slot, every one a stop, in
/// today's words; the observed seat's lens is today's peer rows.
#[test]
fn observer_fallback() {
    let mut network = aterm_types::network_state::NetworkStateSnapshot::default();
    network.network.visibility = Visibility::Active;
    network.network.history = HistoryState::Ready;
    for (slot, reach) in [
        (7, Reachability::Reachable),
        (2, Reachability::LastSeen),
        (4, Reachability::Unknown),
    ] {
        network
            .network
            .peers
            .push(aterm_types::network_state::PeerObservation {
                slot,
                identity: aterm_types::network_state::MachineIdentity {
                    machine_id: [slot as u8; 16],
                    incarnation: [1; 16],
                    label: Some(format!("peer{slot}")),
                },
                first_seen_ms: 1,
                last_seen_ms: 1000,
                last_checked_ms: Some(2000),
                last_repair: None,
                last_authenticated_endpoint: None,
                reachability: reach,
                instances: None,
                samples: Vec::new(),
            });
    }
    let g = crate::stats_deck::DeckGeom {
        w: 700.0,
        h: 200.0,
        pad: 0.0,
        scale: 1.0,
        font_px: 14.0,
    };
    let inks = inks();
    let (prims, hits) = super::super::paint_with_inks(
        &g,
        100.0,
        &network,
        &crate::lab_deck::LabDeck::default(),
        None,
        None,
        &inks,
        false,
    );
    let seats: Vec<SeatKey> = seat_hits(&hits).into_iter().rev().map(|(k, _)| k).collect();
    assert_eq!(
        seats,
        [
            SeatKey::Observed(2),
            SeatKey::Observed(4),
            SeatKey::Observed(7)
        ]
    );
    assert!(texts(&prims).iter().any(|(_, s)| s == "1 reachable"));
    assert!(
        corner_drawn(&prims, 1.0, inks.over),
        "the unknown peer's tick"
    );
    let lab = crate::lab_deck::LabDeck::default();
    let targets = super::super::targets_with(&network, &lab, None, (700.0, 1.0));
    assert!(!targets.contains(&Target::Fleet));
    for slot in [2, 4, 7] {
        assert!(targets.contains(&Target::Seat(SeatKey::Observed(slot))));
    }
    let rows = super::super::details(&network, Target::Seat(SeatKey::Observed(2)));
    assert_eq!(rows[0], ("Machine".into(), "peer2".into()));
    for name in [
        "Observation",
        "Last outbound",
        "Machine UUID",
        "Incarnation",
        "History",
    ] {
        assert!(rows.iter().any(|(n, _)| n == name), "{name}");
    }
    let local = super::super::details(&network, Target::Local);
    let observed = &local
        .iter()
        .find(|(n, _)| n == "Observed machines")
        .unwrap()
        .1;
    assert!(
        observed.starts_with("3 known · 1 reachable · 1 last seen"),
        "{observed}"
    );
}

/// B2 #21: the field's words are the band's 9.5 pt UI face, 12 pt clear of the last column.
#[test]
fn faces() {
    for scale in [1.0_f32, 2.0] {
        for n in [5, 63] {
            let field = field_of(fleet(n), NOW);
            let (prims, hits) = painted(&field, 900.0, scale);
            let last = seat_hits(&hits)
                .iter()
                .map(|(_, r)| r.0 + r.2)
                .fold(0.0_f32, f32::max);
            let mut words = 0;
            for prim in &prims {
                if let DrawPrim::Text { x, px, face, .. } = prim {
                    assert_ne!(*face, crate::widget::TextFace::Mono);
                    assert_eq!(*px, 9.5 * scale);
                    assert!(*x >= last + GAP_PT * scale - 0.5, "{x} after {last}");
                    words += 1;
                }
            }
            assert_eq!(words, 1);
        }
    }
}

/// The lens's seat rows: each read row, and a not-read one said as such, never as 0.
#[test]
fn seat_lens_says_what_was_read() {
    let mut snapshot = fleet(3);
    snapshot.macs[0].agent_sessions.push(FleetSession {
        sid: "s-1a2b".into(),
        program: Some("claude".into()),
        agent: AgentWord::Walled("usage-weekly".into()),
        task: Some(TaskWord::Working),
        ctx_left_pct: Some(40),
        usage_5h_pct: None,
        usage_7d_pct: Some(98),
    });
    snapshot.macs[0].activity = tally(0, 0, 1);
    snapshot.macs[1].older = true;
    snapshot.macs[1].activity = None;
    let field = field_of(snapshot, NOW);
    let rows = details(&field, Target::Seat(field.seats[0].key));
    let row = |rows: &[(String, String)], name: &str| {
        rows.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
    };
    assert!(
        row(&rows, "Sessions")
            .unwrap()
            .contains("s-1a2b · claude · wall:usage-weekly · working · ctx 40 % · 5h/7d –/98 %")
    );
    let older = details(&field, Target::Seat(field.seats[1].key));
    assert_eq!(row(&older, "Agents").unwrap(), "not read: older aterm");
    assert_eq!(field.seats[1].presence, Presence::NotCounted);
    let lens = details(&field, Target::Fleet);
    assert!(
        row(&lens, "At a limit")
            .unwrap()
            .starts_with("wall:usage-weekly — m1 · s-1a2b · 7d 98 %")
    );
    assert!(row(&lens, "Tasks").unwrap().starts_with("1 working"));
    let group = details(&field, Target::Group(SeatClass::NotCounted));
    assert_eq!(group[0].1, "1 not counted");
}

/// 63 seats shaped like the live lab: 7 silent Macs with their reasons (three that did not answer
/// in time, one where nothing listens, one whose name did not resolve, one with no handshake, one
/// gone off the link), 5 stragglers on older builds, 27 walled agents on 13 Macs, 3 waiting, 1
/// stuck, one carried Mac and one older aterm.
fn live_lab() -> FleetSnapshot {
    let mut snapshot = FleetSnapshot {
        read_at_ms: NOW,
        ..FleetSnapshot::default()
    };
    let silent_at = [
        (9, Reach::NoAnswer),
        (22, Reach::NoAnswer),
        (31, Reach::NoAnswer),
        (40, Reach::NothingListens),
        (47, Reach::NoName),
        (52, Reach::NoHandshake),
        (58, Reach::Gone),
    ];
    let stragglers = [
        (4, "0.156.0"),
        (12, "0.155.0"),
        (17, "0.154.0"),
        (25, "0.154.0"),
        (33, "0.147.0"),
    ];
    let walled = [2, 3, 5, 7, 11, 13, 19, 23, 29, 37, 41, 43, 53];
    let waiting = [6, 14, 27];
    let mut walls_left = 27u16;
    for i in 1..=63 {
        if let Some((_, reason)) = silent_at.iter().find(|(n, _)| *n == i) {
            snapshot.unanswered.push(silent(
                i,
                *reason,
                &format!("m{i} at 10.0.0.{i}: {}", reach_words(*reason)),
            ));
            continue;
        }
        let build = stragglers
            .iter()
            .find(|(n, _)| *n == i)
            .map_or("0.160.0", |(_, b)| b);
        let mut m = mac(i, build);
        let busy = u16::try_from(i % 3).expect("small");
        let walls = if walled.contains(&i) {
            let here = if walls_left > 3 { 2 } else { walls_left };
            walls_left -= here;
            here
        } else {
            0
        };
        let wait = u16::from(waiting.contains(&i));
        m.activity = tally(busy, wait, walls);
        if i == 34 {
            m.needs.push(need("s-34aa", NeedWord::Stalled));
        }
        if i == 61 {
            m.carried = true;
            m.read_at_ms = NOW - 60_000;
            snapshot
                .unanswered
                .push(silent(i, Reach::NoReply, "no reply"));
        }
        if i == 62 {
            m.older = true;
            m.activity = None;
        }
        snapshot.macs.push(m);
    }
    assert_eq!(walls_left, 0, "27 walls on 13 Macs");
    snapshot
}

/// B2 #23: the band's line 1 at 0, 5, 63 and 250 seats, 700 and 1100 pt, 2×, in dark, light,
/// Increase Contrast and Differentiate Without Color, as PNGs in `ATERM_DECK_GALLERY` (a scratch
/// folder without it), with each frame's line-1 words in `field_text.tsv`. The 63-seat frame is
/// [`live_lab`]; the others are [`fleet`].
#[test]
fn network_field_gallery() {
    let (dir, _gallery) = crate::scripted_redraw_fixture::gallery_directory("ATERM_DECK_GALLERY");
    crate::tray_raster::prepare_ui_fonts_for_direct_view_test();
    let dark =
        aterm_vitals::ink::vitals_inks([0x30, 0x31, 0x35], [0xD0, 0xD0, 0xD0], [0x50, 0xFA, 0x7B]);
    let light = crate::stats_deck::paint_tests::light_inks();
    // Increase Contrast's swaps, as the deck makes them for the band.
    let mut contrast = dark;
    contrast.hollow = contrast.words2;
    contrast.machine = contrast.machine_ic;
    contrast.aterm = contrast.aterm_ic;
    contrast.heat = contrast.heat_ic;
    let looks = [
        ("dark", dark, false),
        ("light", light, false),
        ("contrast", contrast, false),
        ("dwc", dark, true),
    ];
    let scale = 2.0f32;
    let height = super::super::height_px(f64::from(scale));
    let network = aterm_types::network_state::NetworkStateSnapshot {
        network: active(),
        ..Default::default()
    };
    let mut tsv = String::from("seats\tpt\tlook\ttext\n");
    let mut written = 0;
    let mut scenes: Vec<(String, Field)> = [0, 5, 63, 250]
        .into_iter()
        .map(|n| {
            let field = field_of(if n == 63 { live_lab() } else { fleet(n) }, NOW);
            assert_eq!(field.seats.len(), n);
            (format!("{n}seats"), field)
        })
        .collect();
    // A captured `aterm ctl survey --fleet --json` (`ATERM_FIELD_SURVEY=<file>`) is drawn as the
    // glance would parse it, so the field can be judged on what the fleet's builds really write.
    // The capture holds session ids and titles: it stays outside the checkout.
    if let Some(path) = std::env::var_os("ATERM_FIELD_SURVEY") {
        let json = std::fs::read_to_string(&path).expect("the captured survey reads");
        let snapshot = crate::fleet_glance::parse(&json, None, NOW).expect("the capture parses");
        let field = field_of(snapshot, NOW);
        tsv.push_str(&format!(
            "# capture: {} seats, {} answered\n",
            field.seats.len(),
            field.tallies.answered
        ));
        scenes.push(("capture".into(), field));
    }
    for (scene, field) in &scenes {
        let field = field.clone();
        let n = scene;
        for w in [700.0f32, 1100.0] {
            let g = crate::stats_deck::DeckGeom {
                w: w * scale,
                h: height as f32,
                pad: 12.0 * scale,
                scale,
                font_px: 13.0 * scale,
            };
            for (look, inks, dwc) in looks {
                let (prims, _) = super::super::paint_with_inks(
                    &g,
                    0.0,
                    &network,
                    &crate::lab_deck::LabDeck::default(),
                    Some(&field),
                    None,
                    &inks,
                    dwc,
                );
                let line1: Vec<String> = prims
                    .iter()
                    .filter_map(|prim| match prim {
                        DrawPrim::Text { baseline, s, .. }
                            if *baseline < 15.0 * scale && s != "Network State" =>
                        {
                            Some(s.clone())
                        }
                        _ => None,
                    })
                    .collect();
                tsv.push_str(&format!("{n}\t{w}\t{look}\t{}\n", line1.join(" | ")));
                let (rgba, pw, ph) = crate::tray_raster::rasterize_tray(
                    &prims,
                    g.w as u32,
                    height as u32,
                    1.0,
                    [0, 0, 0, 0],
                );
                let png = crate::app_introspect::encode_rgba8_png(&rgba, pw, ph)
                    .expect("the frame encodes");
                std::fs::write(dir.join(format!("field_{n}_{w}pt_2x_{look}.png")), png)
                    .expect("the PNG writes");
                written += 1;
            }
        }
    }
    std::fs::write(dir.join("field_text.tsv"), tsv).expect("the text table writes");
    assert_eq!(written, scenes.len() * 2 * looks.len());
}

/// The App re-projects the field with no new read where a freshness edge passes, a stale read's
/// age moves a minute on, or the observer's status changes; and it does not before then.
#[test]
fn reprojection_follows_edges_without_a_read() {
    let mut app = crate::App::headless_for_test();
    let mut snapshot = fleet(5);
    snapshot.macs[1].carried = true;
    snapshot.macs[1].read_at_ms = NOW - FRESH_MS;
    let glance = crate::fleet_glance::FleetGlance::over(slot_of(snapshot));
    app.fleet_glance = Some(glance);
    let mut network = (*app.network_snapshot).clone();
    network.network = active();
    app.network_snapshot = Arc::new(network);
    let carried = |app: &crate::App| app.fleet_field.as_ref().expect("projected").seats[1].presence;
    assert!(app.reproject_fleet_field(NOW), "the first projection");
    assert_eq!(carried(&app), Presence::Carried);
    assert_eq!(app.fleet_field_due, Some(NOW + 1), "the carried Mac's edge");
    let key = app.fleet_field.as_ref().unwrap().key;
    // Negative control: before the edge nothing re-projects.
    let projections = PROJECTIONS.with(std::cell::Cell::get);
    assert!(!app.reproject_fleet_field(NOW));
    assert_eq!(PROJECTIONS.with(std::cell::Cell::get), projections);
    assert!(app.reproject_fleet_field(NOW + 1), "the edge passed");
    assert_eq!(carried(&app), Presence::Silent);
    assert_ne!(app.fleet_field.as_ref().unwrap().key, key);

    // A stale read's words move a minute on, and not before.
    let stale = NOW + FRESH_MS + 1;
    app.reproject_fleet_field(stale);
    let words = app.fleet_field.as_ref().unwrap().words[0].clone();
    assert!(words.starts_with("fleet read "), "{words}");
    assert_eq!(app.fleet_field_due, Some(stale + 60_000));
    assert!(!app.reproject_fleet_field(stale + 59_999));
    assert!(app.reproject_fleet_field(stale + 60_000));
    assert_ne!(app.fleet_field.as_ref().unwrap().words[0], words);

    // The observer going offline leads the words, at once, with the clock still.
    let fresh = NOW;
    app.fleet_field_due = None;
    app.reproject_fleet_field(fresh);
    assert!(!app.reproject_fleet_field(fresh), "nothing moved");
    let mut offline = (*app.network_snapshot).clone();
    offline.network.visibility = Visibility::Offline;
    app.network_snapshot = Arc::new(offline);
    assert!(app.reproject_fleet_field(fresh));
    let words = &app.fleet_field.as_ref().unwrap().words;
    assert!(words[0].starts_with("Offline · "), "{words:?}");
}

/// With no network source and no lab feed, a drawn fleet field polls on its own edges and a
/// minute's fallback, never each second.
#[test]
fn a_drawn_field_does_not_tick_each_second() {
    let mut app = crate::App::headless_for_test();
    app.config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    app.network_source = None;
    app.fleet_glance = Some(crate::fleet_glance::FleetGlance::over(FleetSlot {
        book: BookFacts {
            in_fleet: true,
            ..BookFacts::default()
        },
        ..FleetSlot::default()
    }));
    assert!(app.open_network_lens());
    let now = std::time::Instant::now();
    app.service_network_state(now);
    assert_eq!(
        app.fleet_field.as_ref().map(|f| f.source),
        Some(Source::Reading)
    );
    assert_eq!(
        app.network_next_poll,
        Some(now + crate::lab_deck::Feed::EVERY),
        "no edge to wait for: the fallback, and a new read clears it"
    );
}

/// The words' failure suffix and observer lead stay off the reserved shortest form; before a
/// read the words say whether one is under way or failed.
#[test]
fn fleet_words_say_the_read_and_the_observer() {
    let mut slot = slot_of(fleet(10));
    let plain = project(Some(&slot), &active(), NOW);
    slot.error = Some("the fleet client failed".into());
    let failed = project(Some(&slot), &active(), NOW);
    assert_eq!(failed.words.len(), plain.words.len());
    let last = failed.words.len() - 1;
    for (form, plain) in failed.words[..last].iter().zip(&plain.words) {
        assert_eq!(*form, format!("{plain} · last read failed"));
    }
    assert_eq!(failed.words[last], plain.words[last], "the reserved form");
    let offline = NetworkObservation {
        visibility: Visibility::Offline,
        ..active()
    };
    let led = project(Some(&slot), &offline, NOW);
    for (form, failed) in led.words[..last].iter().zip(&failed.words) {
        assert_eq!(*form, format!("Offline · {failed}"));
    }
    assert_eq!(led.words[last], plain.words[last]);

    slot.snapshot = None;
    assert_eq!(
        project(Some(&slot), &active(), NOW).words,
        ["fleet not read"]
    );
    assert_eq!(project(Some(&slot), &active(), NOW).source, Source::NotRead);
    slot.error = None;
    let reading = project(Some(&slot), &active(), NOW);
    assert_eq!(
        (reading.source, reading.words.as_slice()),
        (Source::Reading, &["reading the fleet".to_owned()][..])
    );
}

/// `Activity spread` puts every seat in one bucket, from its Mac's tally: a walled Mac with busy
/// agents is busy, a walled one with none is apart from quiet.
#[test]
fn activity_spread_is_a_partition() {
    let snapshot = live_lab();
    let field = field_of(snapshot, NOW);
    let line = details(&field, Target::Fleet)
        .into_iter()
        .find(|(n, _)| n == "Activity spread")
        .expect("read on the survey")
        .1;
    let sum: usize = line
        .split(" · ")
        .map(|term| term.split(' ').next().unwrap().parse::<usize>().unwrap())
        .sum();
    assert_eq!(sum, field.seats.len(), "{line}");
    let mut one = FleetSnapshot {
        read_at_ms: NOW,
        ..FleetSnapshot::default()
    };
    one.macs.push(FleetMac {
        activity: tally(5, 0, 1),
        ..mac(1, "0.160.0")
    });
    one.macs.push(FleetMac {
        activity: tally(0, 0, 2),
        ..mac(2, "0.160.0")
    });
    one.macs.push(mac(3, "0.160.0"));
    let field = field_of(one, NOW);
    let line = details(&field, Target::Fleet)
        .into_iter()
        .find(|(n, _)| n == "Activity spread")
        .unwrap()
        .1;
    assert_eq!(
        line,
        "1 Mac busy · 1 waiting, at a limit or stuck · 0 quiet · 1 no agents · 0 not read"
    );
}

/// `Not answering` groups by reason, never by an age or address: two copy-path silent Macs
/// with different ages are one group, and a carried Mac past counting stands in its silent
/// row's group.
#[test]
fn silence_groups_by_reason_alone() {
    let mut snapshot = FleetSnapshot {
        read_at_ms: NOW,
        seats: Some(aterm_netplace::words::Seats::default()),
        ..FleetSnapshot::default()
    };
    for (n, ago, why) in [
        (1, 60_000, "silent for 1 min"),
        (2, 7_200_000, "silent for 2 h"),
    ] {
        snapshot.unanswered.push(Unanswered {
            silent_since_ms: Some(NOW - ago),
            ..silent(
                n,
                Reach::Silent {
                    since_ms: NOW - ago,
                },
                why,
            )
        });
    }
    let mut gone = mac(3, "0.160.0");
    gone.carried = true;
    gone.read_at_ms = NOW - FRESH_MS - 1;
    snapshot.macs.push(gone);
    snapshot
        .unanswered
        .push(silent(3, Reach::NoHandshake, "m3: no handshake"));
    let field = field_of(snapshot, NOW);
    let groups = silence_groups(&field);
    assert_eq!(groups.len(), 2, "{groups:?}");
    assert_eq!(groups[0].0, Reach::Silent { since_ms: 0 });
    assert_eq!(groups[0].1.len(), 2);
    assert!(groups[0].1[0].starts_with("m1 (1 min") && groups[0].1[1].starts_with("m2 (2 h"));
    assert_eq!(groups[1].0, Reach::NoHandshake);
    assert!(groups[1].1[0].starts_with("m3 (0.160.0"), "{groups:?}");
}

/// The Local lens's observed rows say reachability and a repair's outcome in words.
#[test]
fn observed_rows_are_words() {
    use aterm_types::network_state::{RepairObservation, RepairOutcome};
    let mut network = active();
    network
        .peers
        .push(aterm_types::network_state::PeerObservation {
            slot: 3,
            identity: aterm_types::network_state::MachineIdentity {
                machine_id: [3; 16],
                incarnation: [1; 16],
                label: Some("peer3".into()),
            },
            first_seen_ms: 1,
            last_seen_ms: 1000,
            last_checked_ms: None,
            last_repair: Some(RepairObservation {
                completed_at_ms: 900,
                outbound: true,
                stage: None,
                outcome: RepairOutcome::Deferred(None),
                table: None,
                sent: None,
                received: None,
                deferred: None,
            }),
            last_authenticated_endpoint: None,
            reachability: Reachability::Unknown,
            instances: None,
            samples: Vec::new(),
        });
    let rows = observed_rows(&network);
    let (_, words) = rows.iter().find(|(n, _)| n == "peer3").expect("its row");
    assert!(words.starts_with("not checked now · last seen "), "{words}");
    assert!(words.ends_with(" · repair: Work deferred"), "{words}");
    for (_, words) in &rows {
        assert!(
            !words.contains("Deferred(") && !words.contains("Unknown"),
            "{words}"
        );
    }
}

/// In a tally the keys and the meters reach the drawn groups after the Fleet words, and no
/// undrawn seat. Negative control: the same field on a wide band visits its attention seats.
#[test]
fn a_tally_is_navigated_by_its_groups() {
    let field = field_of(fleet(63), NOW);
    let network = aterm_types::network_state::NetworkStateSnapshot {
        network: active(),
        ..Default::default()
    };
    let lab = crate::lab_deck::LabDeck::default();
    let w = 330.0;
    let drawn = layout(&field, &active(), w, 1.0);
    assert_eq!(drawn.tier, Tier::C);
    assert!(!drawn.groups.is_empty());
    let all = super::super::targets_with(&network, &lab, Some(&field), (w, 1.0));
    let groups: Vec<Target> = drawn
        .groups
        .iter()
        .map(|g| Target::Group(g.class))
        .collect();
    assert_eq!(
        &all[2..3 + groups.len()],
        [&[Target::Fleet][..], &groups].concat()
    );
    assert!(!all.iter().any(|t| matches!(t, Target::Seat(_))));
    let hit_groups: Vec<Target> = painted(&field, w, 1.0)
        .1
        .iter()
        .rev()
        .filter_map(|h| match h.kind {
            HitKind::Network(t @ Target::Group(_)) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(hit_groups, groups, "every group a key reaches is drawn");
    let meters = super::super::meters(&network, &lab, Some(&field), (w, 1.0));
    assert_eq!(meters.len() - 4, 2 + groups.len());
    let wide = super::super::targets_with(&network, &lab, Some(&field), (900.0, 1.0));
    assert!(wide.iter().any(|t| matches!(t, Target::Seat(_))));
    assert!(!wide.iter().any(|t| matches!(t, Target::Group(_))));
}

/// A tally group too wide for the room left leaves it to a narrower group after it, rather
/// than ending the tally: 150 answered Macs need three digits, 5 silent ones one, so some width
/// holds the silent group and not the answered one. Every drawn group still ends inside the
/// room the reserved words leave.
#[test]
fn a_wide_tally_group_leaves_room_to_a_narrower_one() {
    let mut snapshot = FleetSnapshot {
        read_at_ms: NOW,
        ..FleetSnapshot::default()
    };
    for i in 1..=150 {
        snapshot.macs.push(FleetMac {
            activity: None,
            ..mac(i, "0.160.0")
        });
    }
    for i in 151..=155 {
        snapshot
            .unanswered
            .push(silent(i, Reach::NoAnswer, "no answer"));
    }
    let field = field_of(snapshot, NOW);
    let words_min = 40.0;
    let skipped = (60..=400).map(|w| w as f32).find(|w| {
        let classes: Vec<SeatClass> = layout_with(&field, *w, 1.0, words_min)
            .groups
            .iter()
            .map(|g| g.class)
            .collect();
        classes.contains(&SeatClass::Silent) && !classes.contains(&SeatClass::Answered)
    });
    assert!(
        skipped.is_some(),
        "some width draws the narrow silent group without the wide answered one"
    );
    for w in (60..=400).map(|w| w as f32) {
        let drawn = layout_with(&field, w, 1.0, words_min);
        if drawn.tier == Tier::C && !drawn.groups.is_empty() {
            assert!(
                drawn.words_x <= w - words_min,
                "the groups leave the words their room at {w}"
            );
        }
    }
}

/// The Builds line counts the prevailing build, yet still names a seat on it that needs an
/// update; with none needing one, the count stands alone.
#[test]
fn builds_line_names_a_prevailing_seat_that_needs_an_update() {
    let mut snapshot = FleetSnapshot {
        read_at_ms: NOW,
        ..FleetSnapshot::default()
    };
    for i in 1..=3 {
        snapshot.macs.push(mac(i, "0.160.0"));
    }
    snapshot.macs.push(mac(4, "0.156.0"));
    snapshot.macs[1].needs_update = true;
    let builds = |snapshot: FleetSnapshot| {
        details(&field_of(snapshot, NOW), Target::Fleet)
            .into_iter()
            .find(|(n, _)| n == "Builds")
            .expect("builds")
            .1
    };
    assert_eq!(
        builds(snapshot.clone()),
        "0.160.0 ×3 (m2 needs update) · 0.156.0 m4"
    );
    snapshot.macs[1].needs_update = false;
    assert_eq!(builds(snapshot), "0.160.0 ×3 · 0.156.0 m4", "control");
}

/// The words count every session whose seat draws a waiting diamond: an agent the tally counts
/// waiting once, and a shell's need a person decides beside it, but an agent's need never twice.
#[test]
fn waiting_words_count_what_the_diamonds_draw() {
    let waiting_agent = FleetSession {
        sid: "s-agent".into(),
        program: Some("claude".into()),
        agent: AgentWord::Waiting,
        task: None,
        ctx_left_pct: None,
        usage_5h_pct: None,
        usage_7d_pct: None,
    };
    let mut snapshot = fleet(3);
    snapshot.macs[0].activity = tally(0, 1, 0);
    snapshot.macs[0].agent_sessions.push(waiting_agent);
    snapshot.macs[0].needs.push(need("s-agent", NeedWord::Ask));
    snapshot.macs[1].needs.push(need("s-shell", NeedWord::Ask));
    // A stuck need is no decision: it adds nothing to waiting (on its own Mac, which it marks
    // stuck rather than waiting).
    snapshot.macs[2]
        .needs
        .push(need("s-stuck", NeedWord::Stalled));
    let field = field_of(snapshot.clone(), NOW);
    assert_eq!(field.tallies.waiting, 2, "the agent once, the shell once");
    let diamonds = field
        .seats
        .iter()
        .filter(|seat| seat.activity == Some(Activity::Waiting))
        .count();
    assert_eq!(diamonds, 2);
    // Control: a read that left agents out cannot tell a shell's need from an unlisted agent's.
    snapshot.macs[1].sessions_more = 1;
    assert_eq!(field_of(snapshot, NOW).tallies.waiting, 1);
}

/// An older node counts an idle agent that ended its turn on a question by its word alone: it
/// lists the agent idle and its tally holds no waiting. The field still counts it once, from its
/// need, as its seat's diamond draws it.
#[test]
fn an_older_nodes_idle_asking_agent_still_counts_waiting() {
    let mut snapshot = fleet(2);
    snapshot.macs[0].activity = tally(0, 0, 0);
    snapshot.macs[0].agent_sessions.push(FleetSession {
        sid: "s-idle".into(),
        program: Some("claude".into()),
        agent: AgentWord::Idle,
        task: None,
        ctx_left_pct: None,
        usage_5h_pct: None,
        usage_7d_pct: None,
    });
    snapshot.macs[0].needs.push(need("s-idle", NeedWord::Ask));
    assert_eq!(field_of(snapshot.clone(), NOW).tallies.waiting, 1);
    // Control: a newer node lists it waiting and counts it in its tally; still once.
    snapshot.macs[0].agent_sessions[0].agent = AgentWord::Waiting;
    snapshot.macs[0].activity = tally(0, 1, 0);
    assert_eq!(field_of(snapshot, NOW).tallies.waiting, 1);
}

/// A needs row read before the next activity count names an agent's ask while the tally still
/// counts it busy: the agent waits, once, and leaves busy. A Mac whose tally was not read still
/// counts the decided needs its diamond draws.
#[test]
fn a_decided_need_moves_its_agent_to_waiting_once() {
    let mut snapshot = fleet(3);
    snapshot.macs[0].activity = tally(1, 0, 0);
    snapshot.macs[0].agent_sessions.push(FleetSession {
        sid: "s-busy".into(),
        program: Some("claude".into()),
        agent: AgentWord::Busy,
        task: None,
        ctx_left_pct: None,
        usage_5h_pct: None,
        usage_7d_pct: None,
    });
    snapshot.macs[0].needs.push(need("s-busy", NeedWord::Ask));
    // Two decided needs of one session are one session waiting.
    snapshot.macs[0]
        .needs
        .push(need("s-busy", NeedWord::Approval));
    let field = field_of(snapshot.clone(), NOW);
    assert_eq!(field.tallies.waiting, 1);
    assert_eq!(field.tallies.busy, Some(0));
    // A Mac with no tally read and an ask: counted from its need.
    snapshot.macs[1].activity = None;
    snapshot.macs[1]
        .needs
        .push(need("s-shell", NeedWord::Question));
    let field = field_of(snapshot.clone(), NOW);
    assert_eq!(field.tallies.waiting, 2);
    let diamonds = field
        .seats
        .iter()
        .filter(|seat| seat.activity == Some(Activity::Waiting))
        .count();
    assert_eq!(diamonds, 2, "the words count what the diamonds draw");
    // Control: without the needs nothing waits and the agent is busy.
    snapshot.macs[0].needs.clear();
    snapshot.macs[1].needs.clear();
    let field = field_of(snapshot, NOW);
    assert_eq!((field.tallies.waiting, field.tallies.busy), (0, Some(1)));
}

fn observed(n: u64) -> aterm_types::network_state::NetworkStateSnapshot {
    let mut network = aterm_types::network_state::NetworkStateSnapshot::default();
    network.network.visibility = Visibility::Active;
    network.network.history = HistoryState::Ready;
    for slot in 0..n {
        network
            .network
            .peers
            .push(aterm_types::network_state::PeerObservation {
                slot,
                identity: aterm_types::network_state::MachineIdentity {
                    machine_id: [slot as u8; 16],
                    incarnation: [1; 16],
                    label: Some(format!("peer{slot}")),
                },
                first_seen_ms: 1,
                last_seen_ms: 1000,
                last_checked_ms: Some(2000),
                last_repair: None,
                last_authenticated_endpoint: None,
                reachability: Reachability::Reachable,
                instances: None,
                samples: Vec::new(),
            });
    }
    network
}

/// An observer field too wide for the band is a tally: the keys visit its drawn groups, each a
/// hit, and a group's lens names the observed peers rather than an unread fleet.
#[test]
fn an_observer_tally_keys_reach_what_it_draws() {
    let network = observed(63);
    let lab = crate::lab_deck::LabDeck::default();
    let (w, scale) = (400.0, 1.0);
    let obs_field = observer(&network.network);
    assert_eq!(layout(&obs_field, &network.network, w, scale).tier, Tier::C);
    let targets = super::super::targets_with(&network, &lab, None, (w, scale));
    let field_targets: Vec<Target> = targets
        .iter()
        .copied()
        .filter(|t| matches!(t, Target::Seat(_) | Target::Group(_) | Target::Fleet))
        .collect();
    assert!(!field_targets.is_empty());
    assert!(
        field_targets.iter().all(|t| matches!(t, Target::Group(_))),
        "{field_targets:?}"
    );
    let g = crate::stats_deck::DeckGeom {
        w,
        h: 200.0,
        pad: 0.0,
        scale,
        font_px: 14.0,
    };
    let (_, hits) =
        super::super::paint_with_inks(&g, 100.0, &network, &lab, None, None, &inks(), false);
    for target in &field_targets {
        assert!(
            hits.iter().any(|hit| hit.kind == HitKind::Network(*target)),
            "{target:?} has no hit"
        );
        let rows = super::super::details_lab_with_caption(&network, &lab, None, *target, None);
        assert_eq!(rows[0].0, "Group", "{rows:?}");
    }
    // Control: a few peers are drawn seats, and the keys visit them.
    let few = observed(3);
    let targets = super::super::targets_with(&few, &lab, None, (700.0, 1.0));
    assert!(targets.contains(&Target::Seat(SeatKey::Observed(0))));
    assert!(!targets.iter().any(|t| matches!(t, Target::Group(_))));
}

/// A place caption wider than the room the status reserved leaves the status, never words past
/// the band's margin.
#[test]
fn a_place_caption_that_does_not_fit_leaves_the_status() {
    let obs = active();
    let caption = PlaceCaption {
        long: "Remote · cellular · far from the lab".into(),
        short: "Remote · cellular".into(),
        offline: false,
    };
    if !super::super::place_only(super::super::network_status_of(&obs), &caption) {
        panic!("the test's caption must replace the status");
    }
    let room = text_w("0 reachable", 1.0);
    assert_eq!(
        observer_words(1.0, &obs, Some(&caption), false, room),
        "0 reachable"
    );
    // Control: with room the caption is drawn.
    let wide = observer_words(1.0, &obs, Some(&caption), false, 1000.0);
    assert_eq!(wide, caption.long);
}
