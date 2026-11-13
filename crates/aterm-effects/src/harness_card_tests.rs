// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::oracle::{QuadSink, Sink, kitty_quads, lay_star, rim_quads};
use super::*;
use crate::cursor_glow::Geom;
use crate::effect_util::{STAR_GLINT, push_twinkle_star, star_core_px, star_waist_px};
use aterm_render::GlowQuad;

pub(super) const fn scene() -> CardScene {
    CardScene {
        origin_x: 10,
        origin_y: 46,
        cw: 9,
        ch: 18,
        row_base: 2,
        pad_left: 10,
        pad_above: 0,
        pad_below: 0,
        top: 3,
        rows: 4,
        cols: 60,
        trail: Some((32, 40)),
        kitty_col: Some(40),
        kitty_rows: 2,
        clip: WHOLE_STREAM,
        palette: CardPalette::Cruise,
        dark: true,
        ground: NIGHT,
        serious: false,
        coat: 0x00E8_A060,
        eye: 0x0040_C080,
        wand: Some((10, 3)),
    }
}

const NIGHT: u32 = 0x0012_1721;
const DAY: u32 = 0x00F8_FAFD;
/// Silver Aerogel's ground, a mid grey [`dark_ground`] calls dark.
pub(super) const AEROGEL: u32 = 0x0092_9292;
/// Unikitty's ground, a mid pink [`dark_ground`] calls dark, on which no added light reads.
pub(super) const UNIKITTY: u32 = 0x00FF_8CD9;
/// A raspberry mid ground on which the glints' rest gold misses the floor added though white
/// light reads, so a glint laid frame by frame on its flare would flip from ink to light.
const RASPBERRY: u32 = 0x00FF_375F;
/// A dim scarlet on which white reads better than black yet no added light reads the floor.
const SCARLET: u32 = 0x00EB_0000;

fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

const WHOLE_STREAM: (i32, i32, i32, i32) = (i32::MIN, i32::MIN, i32::MAX, i32::MAX);

pub(super) const fn widest() -> CardScene {
    CardScene {
        cols: 120,
        trail: Some((8, 100)),
        kitty_col: Some(100),
        ..scene()
    }
}

pub(super) const fn clipped() -> CardScene {
    let s = scene();
    let (cw, ch) = (s.cw as i32, s.ch as i32);
    CardScene {
        clip: (
            s.origin_x,
            0,
            s.origin_x + 50 * cw,
            s.origin_y + (s.top + 3) as i32 * ch + ch / 2,
        ),
        ..s
    }
}

pub(super) fn scenes() -> Vec<CardScene> {
    let two = scenes_two_rows();
    let tall = two.iter().map(|s| CardScene {
        kitty_rows: 3,
        ..*s
    });
    two.iter().copied().chain(tall).collect()
}

fn scenes_two_rows() -> Vec<CardScene> {
    let base = scene();
    let mut v = vec![base];
    v.push(CardScene {
        dark: false,
        ..base
    });
    v.push(CardScene {
        palette: CardPalette::Land,
        ..base
    });
    v.push(CardScene {
        palette: CardPalette::Land,
        dark: false,
        ..base
    });
    v.push(CardScene {
        palette: CardPalette::Off,
        ..base
    });
    v.push(CardScene {
        palette: CardPalette::Off,
        dark: false,
        ..base
    });
    v.push(CardScene {
        cw: 8,
        ch: 16,
        pad_left: 0,
        pad_above: 16,
        origin_x: 0,
        origin_y: 0,
        row_base: 0,
        top: 0,
        wand: Some((10, 0)),
        ..base
    });
    v.push(CardScene {
        cw: 16,
        ch: 33,
        kitty_col: Some(44),
        trail: Some((32, 44)),
        ..base
    });
    v.push(CardScene {
        origin_x: -3,
        pad_left: 2,
        rows: 3,
        ..base
    });
    v.push(CardScene {
        cw: 7,
        ch: 13,
        rows: 3,
        dark: false,
        trail: None,
        ..base
    });
    v.push(CardScene {
        cw: 17,
        ch: 35,
        kitty_col: Some(44),
        trail: Some((32, 44)),
        ..base
    });
    v.push(clipped());
    v.push(CardScene {
        dark: false,
        palette: CardPalette::Land,
        ..clipped()
    });
    v.push(aired(base));
    v.push(aired(CardScene {
        dark: false,
        palette: CardPalette::Land,
        ..base
    }));
    v.push(aired(CardScene {
        dark: false,
        palette: CardPalette::Off,
        ..base
    }));
    v.push(CardScene {
        cw: 16,
        ch: 33,
        pad_above: 33,
        rows: 3,
        kitty_col: Some(44),
        trail: Some((32, 44)),
        ..base
    });
    v
}

pub(super) const fn aired(s: CardScene) -> CardScene {
    CardScene {
        pad_above: s.ch,
        pad_below: s.ch,
        ..s
    }
}

fn frame(
    card: &mut HarnessCard,
    now: Instant,
    s: &CardScene,
    animate: bool,
) -> (Vec<GlowQuad>, Vec<GlowQuad>, u64) {
    let (mut under, mut over) = (Vec::new(), Vec::new());
    let fp = card.tick(now, s, animate, &mut under, &mut over);
    (under, over, fp)
}

const fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn kitty_left(over: &[GlowQuad]) -> Option<i64> {
    over.iter()
        .filter(|q| q.alpha == 255)
        .map(|q| i64::from(q.x))
        .min()
}

fn ride_from(s: &CardScene) -> i64 {
    let g = Geo::new(s).expect("a scene");
    let seat = kitty_rect(&g, s).expect("a kitty");
    ride_span(&g, s, &seat).expect("a ride").0
}

fn shown_at_rest(s: &CardScene, at: Instant) -> HarnessCard {
    let mut card = HarnessCard::default();
    frame(&mut card, at, s, false);
    card
}

fn settled(s: &CardScene) -> (Vec<GlowQuad>, Vec<GlowQuad>, u64) {
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    card.arm(t0, 7);
    frame(&mut card, t0 + ms(EPISODE_MS + 1), s, false)
}

#[derive(Debug, Default)]
struct Parts {
    border: Vec<GlowQuad>,
    trail: Vec<GlowQuad>,
    kitty: Vec<GlowQuad>,
    rim: Vec<GlowQuad>,
    stars: Vec<GlowQuad>,
    sparkles: Vec<GlowQuad>,
    burst: Vec<GlowQuad>,
    glints: Vec<GlowQuad>,
}

fn parts(s: &CardScene, clock: Clock) -> Parts {
    parts_seeded(s, clock, 7)
}

fn lay_calls(cx: &Emit<'_>, out: &mut Vec<GlowQuad>) -> impl FnMut(StarCall) {
    let sink = Sink { geo: cx.geo };
    let trail = cx.trail_ink();
    let ground = cx.scene.ground;
    move |st: StarCall| {
        let st = st.laid_on(ground);
        lay_star(
            out,
            &sink,
            st.clip,
            st.centre,
            st.arm,
            st.cov,
            st.gold,
            st.color,
            trail.covers(),
            st.light,
        );
    }
}

fn parts_seeded(s: &CardScene, clock: Clock, seed: u64) -> Parts {
    let geo = Geo::new(s).expect("a scene");
    let cx = Emit::new(s, geo, clock, seed);
    let mut out = Parts::default();
    let sink = Sink { geo };
    cx.border(&mut QuadSink {
        sink,
        out: &mut out.border,
    });
    cx.trail(&mut QuadSink {
        sink,
        out: &mut out.trail,
    });
    kitty_quads(&cx, &sink, &mut out.kitty);
    rim_quads(&cx, &sink, &mut out.rim);
    cx.field(&mut lay_calls(&cx, &mut out.stars));
    cx.shed(&mut lay_calls(&cx, &mut out.sparkles));
    cx.burst_stars(&mut lay_calls(&cx, &mut out.burst));
    cx.laid_glints(&mut lay_calls(&cx, &mut out.glints));
    out
}

fn assert_in_band_and_box(q: &GlowQuad, s: &CardScene, what: &str) {
    let (x0, y0) = (i64::from(q.x), i64::from(q.y));
    let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
    assert!(q.w > 0 && q.h > 0, "{what}: an empty quad {q:?}");
    let r = i64::from(q.row) - i64::from(s.row_base);
    let band0 = i64::from(s.origin_y) + r * i64::from(s.ch);
    assert!(
        band0 <= y0 && y1 <= band0 + i64::from(s.ch),
        "{what}: {q:?} leaves its band [{band0}, {})",
        band0 + i64::from(s.ch)
    );
    let cw = i64::from(s.cw);
    let bx0 = (i64::from(s.origin_x) - i64::from(s.pad_left).min(cw / 2)).max(0);
    let bx1 = i64::from(s.origin_x) + (i64::from(s.cols) + 1) * cw;
    let ch = i64::from(s.ch);
    let air = |pad: u16| i64::from(pad).min(ch / 2);
    let top = i64::from(s.origin_y) + i64::from(s.top) * ch;
    let by0 = top - if s.top > 0 { air(s.pad_above) } else { 0 };
    let by1 = top + i64::from(s.rows) * ch + air(s.pad_below);
    assert!(
        bx0 <= x0 && x1 <= bx1 && by0 <= y0 && y1 <= by1,
        "{what}: {q:?} leaves the box x {bx0}..{bx1} y {by0}..{by1}"
    );
    let (cx0, cy0, cx1, cy1) = s.clip;
    assert!(
        i64::from(cx0) <= x0
            && x1 <= i64::from(cx1)
            && i64::from(cy0) <= y0
            && y1 <= i64::from(cy1),
        "{what}: {q:?} leaves the clip {:?}",
        s.clip
    );
}

#[test]
fn kitty_frames_are_rectangular_and_drawn_in_the_palette() {
    for (art, h) in [(KITTY_SHORT, KITTY_H), (KITTY_TALL, KITTY_TALL_H)] {
        for f in [art.open, art.blink, art.flick] {
            assert_eq!(f.len(), h);
            for row in f.iter() {
                assert_eq!(row.len(), KITTY_W, "a ragged row {row:?}");
                assert!(
                    row.bytes().all(|b| KITTY_PALETTE.contains(&b)),
                    "an unknown pixel in {row:?}"
                );
            }
        }
        for (r, (open, blink)) in art.open.iter().zip(art.blink.iter()).enumerate() {
            assert_eq!(
                open == blink,
                !(art.eyes.0..art.eyes.1).contains(&r),
                "the blink is the eyes alone (row {r})"
            );
            if open != blink {
                assert!(
                    !blink.contains(['e', 'k', 'w']),
                    "the eyes closed (row {r})"
                );
            }
        }
        for (open, flick) in art.open.iter().zip(art.flick.iter()) {
            assert_eq!(
                open[KITTY_TAIL_COLS..],
                flick[KITTY_TAIL_COLS..],
                "the flick is the tail alone"
            );
        }
        assert!(
            two_eyes_of_iris_and_pupil(art.open, art.eyes),
            "the cat has two eyes, each an iris and a pupil"
        );
        for (what, pupil_row, catch_row) in [
            ("a right eye without its pupil", "ekccccee", "ewccccew"),
            ("one eye", "ekcccccc", "ewcccccc"),
            ("the eyes run together", "ekkkkkek", "ewkkkkew"),
        ] {
            let rows: Vec<String> = art
                .open
                .iter()
                .map(|r| {
                    r.replace("ekccccek", pupil_row)
                        .replace("ewccccew", catch_row)
                })
                .collect();
            let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
            assert_ne!(rows, art.open, "CONTROL: {what} is drawn");
            assert!(
                !two_eyes_of_iris_and_pupil(&rows, art.eyes),
                "CONTROL: {what} passes the eye check"
            );
        }
        let rear = art.rear();
        for f in [art.open, art.blink, art.flick] {
            for row in &f[art.body.0..art.body.0 + art.body.1] {
                let first = row.find(|c| c != '.').expect("a body row");
                assert!(
                    first <= rear && !row[first..rear].contains('.'),
                    "{row:?} meets the trail flush at column {rear}"
                );
            }
        }
    }
    let cols_of = |f: &[&str], ch: char| -> std::collections::BTreeSet<usize> {
        f.iter()
            .flat_map(|r| {
                r.char_indices()
                    .filter(move |&(_, c)| c == ch)
                    .map(|(i, _)| i)
            })
            .collect()
    };
    for ch in ['e', 'k', 'n'] {
        assert_eq!(
            cols_of(KITTY_SHORT.open, ch),
            cols_of(KITTY_TALL.open, ch),
            "{ch:?} on the same columns"
        );
    }
    assert_eq!(KITTY_SHORT.open[0], KITTY_TALL.open[0], "the ears' tips");
    let (short, tall) = (KITTY_SHORT.open, KITTY_TALL.open);
    assert_eq!(
        [short[11], short[12], short[12], short[13]],
        [tall[17], tall[18], tall[19], tall[20]],
        "the legs, a row longer, and the paws"
    );
    let mut stray: Vec<&str> = KITTY_TALL.blink.to_vec();
    stray[0] = "..............o......oo.";
    assert!(!differ_only_in(
        KITTY_TALL.open,
        &stray,
        KITTY_TALL.eyes,
        (0, KITTY_W)
    ));
    let mut wag: Vec<&str> = KITTY_TALL.flick.to_vec();
    wag[1] = "....o........ono....ono.";
    assert!(!differ_only_in(
        KITTY_TALL.open,
        &wag,
        (0, KITTY_TALL_H),
        (0, KITTY_TAIL_COLS)
    ));
    let mut ragged: Vec<&str> = KITTY_TALL.open.to_vec();
    ragged[3] = "oco.........onnoooooonn";
    assert!(!frame_is_well_formed(&ragged, KITTY_TALL_H));
    let rear = KITTY_TALL.rear();
    assert!(trail_meets_flush(KITTY_TALL.open, KITTY_TALL.body, rear));
    let mut holed: Vec<&str> = KITTY_TALL.open.to_vec();
    holed[KITTY_TALL.body.0 + 1] = "o.occcccccccocewccccewco";
    assert!(!trail_meets_flush(&holed, KITTY_TALL.body, rear));
    let open_rear = |art: KittyArt| {
        art.open[art.body.0..art.body.0 + art.body.1]
            .iter()
            .map(|r| r.find(|c| c != '.').expect("a body row"))
            .max()
            .expect("body rows")
    };
    for art in [KITTY_SHORT, KITTY_TALL] {
        assert!(trail_meets_flush(art.open, art.body, open_rear(art)));
        assert!(
            !trail_meets_flush(art.flick, art.body, open_rear(art)),
            "CONTROL: the flick's back past the open frame's rear"
        );
    }
    assert!(frame_is_well_formed(KITTY_TALL.open, KITTY_TALL_H));
    assert!(
        !frame_is_well_formed(KITTY_TALL.open, KITTY_H),
        "the wrong height"
    );
}

#[test]
fn kitty_cols_rounds_the_art_up_to_whole_cells() {
    assert_eq!(kitty_cols(8, 16), 6, "2 px pixels: 48 px in 8 px cells");
    assert_eq!(kitty_cols(9, 18), 6, "2 px pixels: 48 px in 9 px cells");
    assert_eq!(kitty_cols(16, 33), 6, "4 px pixels: 96 px in 16 px cells");
    assert_eq!(kitty_cols(7, 21), 7, "2 px pixels: 48 px in 7 px cells");
    assert_eq!(kitty_cols(17, 35), 6, "4 px pixels: 96 px in 17 px cells");
    assert_eq!(kitty_cols(8, 14), 3, "1 px pixels: 24 px in 8 px cells");
    assert_eq!(kitty_cols(8, 8), 3, "7 pixel rows under a 1 px stroke");
    assert_eq!(
        kitty_cols(8, 7),
        0,
        "7 px less the stroke cannot hold 7 rows"
    );
    assert_eq!(kitty_cols(8, 6), 0, "a 6 px cell cannot hold 7 pixel rows");
    assert_eq!(kitty_cols(0, 16), 0);
    assert_eq!(kitty_cols(8, 0), 0);
}

#[test]
fn the_kitty_clears_the_border_at_every_cell_height() {
    let mut drawn = [0; 2];
    for (k, kitty_rows) in [2u8, 3].into_iter().enumerate() {
        for ch in 1..=96u16 {
            let cw = (ch / 2).max(1);
            let cols = kitty_cols(cw, ch);
            let s = CardScene {
                cw,
                ch,
                origin_x: 4,
                origin_y: 4,
                top: 0,
                row_base: 0,
                rows: 3,
                trail: Some((32, 40)),
                kitty_col: Some(40),
                kitty_rows,
                cols: 40 + cols + 1,
                ..scene()
            };
            let g = Geo::new(&s).expect("a scene");
            let Some(at) = kitty_rect(&g, &s) else {
                assert_eq!(
                    cols, 0,
                    "{ch} px, {kitty_rows} rows: the kitty fits its columns but was dropped"
                );
                continue;
            };
            let (r, p) = (at.rect, at.p);
            let inner = g.inner();
            assert_eq!(
                r.meet(inner),
                r,
                "{ch} px: the kitty {r:?} meets the stroke {inner:?}"
            );
            assert_eq!(at.art.rows(), usize::from(kitty_rows));
            assert!(
                r.y0 >= g.band(0) && r.y1 <= g.band(i64::from(kitty_rows)),
                "{ch} px: {r:?}"
            );
            if kitty_rows == 2 {
                assert_eq!(
                    r.y0 + KITTY_HALF as i64 * p,
                    g.band(1),
                    "{ch} px: split at the boundary"
                );
            }
            assert!(
                r.x1 - r.x0 <= i64::from(cols) * i64::from(cw),
                "{ch} px: wider than its {cols} columns"
            );
            assert_eq!(p, i64::from(kitty_px(ch)));
            let settled = parts(&s, Clock::SETTLED);
            for q in settled.kitty.iter().chain(&settled.rim) {
                let (x0, y0) = (i64::from(q.x), i64::from(q.y));
                let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
                assert!(
                    inner.x0 <= x0 && x1 <= inner.x1 && inner.y0 <= y0 && y1 <= inner.y1,
                    "{ch} px: a kitty or rim pixel {q:?} on the stroke"
                );
            }
            drawn[k] += 1;
        }
    }
    assert!(
        drawn.iter().all(|&n| n >= 80),
        "the sweep must draw the kitty: {drawn:?}"
    );
}

fn played(s: &CardScene, t0: Instant) -> HarnessCard {
    let mut card = HarnessCard::default();
    card.arm(t0, 7);
    frame(&mut card, t0, s, true);
    frame(&mut card, t0 + ms(EPISODE_MS - 5), s, true);
    card
}

#[test]
fn a_held_card_is_static_and_wants_no_frames() {
    for s in scenes() {
        let t0 = Instant::now();
        let rest = settled(&s);
        assert_ne!(rest.2, 0, "a settled card still draws");
        let mut card = played(&s, t0);
        frame(&mut card, t0 + ms(EPISODE_MS + 500), &s, true);
        assert!(!card.at_rest(), "CONTROL: it flows");
        for k in 1..6 {
            let now = t0 + ms(EPISODE_MS + 500 + k * 977);
            assert_eq!(frame(&mut card, now, &s, false), rest, "held {k}");
            assert!(card.at_rest(), "held {k}: at rest");
            assert_eq!(card.deadline(now), None, "a held card wants no frames");
        }
        let mut unarmed = HarnessCard::default();
        let first = frame(&mut unarmed, t0, &s, true);
        for k in 1..4 {
            let now = t0 + ms(k * 977);
            assert_eq!(frame(&mut unarmed, now, &s, true), first, "unarmed {k}");
            assert_eq!(unarmed.deadline(now), None, "an unarmed card idles");
        }
    }
    assert_eq!(
        HarnessCard::default().deadline(Instant::now()),
        None,
        "an unarmed card idles"
    );
}

#[test]
fn after_the_intro_the_card_flows_on_the_step_grid() {
    for s in scenes() {
        let t0 = Instant::now();
        let mut card = played(&s, t0);
        let end = t0 + ms(EPISODE_MS);
        assert_eq!(
            card.deadline(end),
            Some(end),
            "the flow's first frame is owed"
        );
        let first = EPISODE_MS / FLOW_STEP_MS + 1;
        let mut prev: Option<(Vec<GlowQuad>, Vec<GlowQuad>, u64)> = None;
        for k in first..first + 2 * GLINT_PERIOD_MS / FLOW_STEP_MS {
            let at = t0 + ms(k * FLOW_STEP_MS + 3);
            let f = frame(&mut card, at, &s, true);
            assert!(card.flowing(at) && !card.intro_live(), "step {k}: flowing");
            assert_eq!(card.flow_step(at), Some(k), "step {k}: the grid");
            let due = t0 + ms((k + 1) * FLOW_STEP_MS + FLOW_SLACK_MS);
            assert_eq!(card.deadline(at), Some(due), "step {k}: the next step");
            assert_eq!(card.deadline(at + ms(90)), Some(due), "step {k}: fixed");
            let again = frame(&mut card, at + ms(FLOW_STEP_MS - 10), &s, true);
            assert_eq!(again, f, "step {k}: a step's frame is the step's alone");
            if let Some(p) = &prev {
                assert_ne!(p.2, f.2, "step {k}: the fingerprint held");
                assert!(p.0 != f.0 || p.1 != f.1, "step {k}: the quads held");
            }
            prev = Some(f);
        }
    }
}

#[test]
fn the_flow_settles_when_a_gate_closes_and_resumes_from_the_settled_colours() {
    for s in scenes() {
        let t0 = Instant::now();
        let rest = settled(&s);
        let border = parts(&s, Clock::SETTLED).border.len();
        let mut card = played(&s, t0);
        frame(&mut card, t0 + ms(EPISODE_MS + 200), &s, true);
        let shut = t0 + ms(EPISODE_MS + 300);
        assert_eq!(frame(&mut card, shut, &s, false), rest, "settled at once");
        assert_eq!(card.deadline(shut), None, "held: no wake");
        let back = t0 + ms(EPISODE_MS + 5300);
        let (under, over, fp) = frame(&mut card, back, &s, true);
        assert!(card.flowing(back), "it flows again");
        assert_ne!(fp, rest.2, "a flow frame");
        assert_eq!(
            under[..border],
            rest.0[..border],
            "the border resumes settled"
        );
        assert_eq!(
            over, rest.1,
            "the kitty, its rim and the stars resume settled"
        );
        let up = back + ms(GLINT_FADE_MS + 2 * FLOW_STEP_MS);
        let (later_under, _, _) = frame(&mut card, up, &s, true);
        assert_ne!(
            later_under[..border],
            rest.0[..border],
            "the border flows on"
        );
    }
}

#[test]
fn an_adopted_ripple_flows_on_the_first_cards_steps() {
    let s = scene();
    let t0 = Instant::now();
    let mut first = HarnessCard::default();
    first.arm(t0, 7);
    let origin = first.ripple_origin().expect("armed");
    assert_eq!(origin, t0);
    let t1 = t0 + ms(50);
    let mut second = HarnessCard::default();
    second.adopt_ripple(origin);
    second.arm(t1, 9);
    assert_eq!(second.ripple_origin(), Some(origin), "the arm keeps it");
    second.adopt_ripple(t1);
    assert_eq!(
        second.ripple_origin(),
        Some(origin),
        "a started clock stays"
    );
    let mut alone = HarnessCard::default();
    alone.arm(t1, 9);
    let flow = t1 + ms(EPISODE_MS + 1000);
    for card in [&mut first, &mut second, &mut alone] {
        frame(card, t1, &s, true);
        frame(card, flow, &s, true);
        assert!(card.flowing(flow), "CONTROL: it flows");
    }
    let (a, b, c) = (
        first.deadline(flow).expect("a step"),
        second.deadline(flow).expect("a step"),
        alone.deadline(flow).expect("a step"),
    );
    assert_eq!(a, b, "one step for both");
    assert_eq!(first.flow_step(flow), second.flow_step(flow));
    let off = c.max(a).duration_since(c.min(a)).as_millis() % u128::from(FLOW_STEP_MS);
    assert_eq!(
        off.min(u128::from(FLOW_STEP_MS) - off),
        50,
        "CONTROL: 50 ms off"
    );
}

#[test]
fn a_lock_is_whole_while_unseen_and_slews_a_step_at_most_once_seen() {
    let s = scene();
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    card.arm(t0, 7);
    frame(&mut card, t0, &s, true);
    let seen = t0 + ms(1000);
    assert!(!card.glints_shown(seen), "CONTROL: no glint shows yet");
    card.lock_glint(seen);
    let g0 = t0 + ms(1000 - FLARE_STEP_LEAD_MS);
    assert_eq!(card.glint0, Some(g0), "adopted, unseen");
    frame(&mut card, t0 + ms(EPISODE_MS - 5), &s, true);
    let mut kept = CardFrames::default();
    let at = t0 + ms(EPISODE_MS + 200);
    let _ = card.draw(at, &s, true, &mut kept);
    assert!(card.glints_shown(at), "CONTROL: the flow's glints show");
    assert!(
        card.replay_step(&s, true, &kept, at + ms(1)).is_some(),
        "CONTROL: kept"
    );
    let flare = |birth_after_g0: u64, off: i64| {
        let birth = later(g0, birth_after_g0);
        let start = if off >= 0 {
            later(birth, off.unsigned_abs())
        } else {
            birth - ms(off.unsigned_abs())
        };
        later(start, FLARE_STEP_LEAD_MS)
    };
    let grid = card.grid_gen;
    card.lock_glint(flare(3 * GLINT_PERIOD_MS, 3));
    assert_eq!(
        (card.glint0, card.grid_gen),
        (Some(g0), grid),
        "a 3 ms error is noise"
    );
    assert!(
        card.replay_step(&s, true, &kept, at + ms(1)).is_some(),
        "still kept"
    );
    card.lock_glint(flare(3 * GLINT_PERIOD_MS, 40));
    assert_eq!(card.glint0, Some(g0 + ms(10)), "slewed by +10");
    assert_eq!(card.fade0, None, "the glints stay up");
    assert!(
        !card.replay_step(&s, true, &kept, at + ms(1)).is_some(),
        "a frame of the old grid is drawn anew"
    );
    let g1 = g0 + ms(10);
    card.lock_glint(later(
        later(g1, 10 * GLINT_PERIOD_MS) - ms(30),
        FLARE_STEP_LEAD_MS,
    ));
    assert_eq!(card.glint0, Some(g1 - ms(7)), "slewed by −7");
    let g2 = g1 - ms(7);
    card.lock_glint(later(g2, 11 * GLINT_PERIOD_MS + 3040 + FLARE_STEP_LEAD_MS));
    let g3 = g2 - ms(8);
    assert_eq!(card.glint0, Some(g3), "folded, slewed by −8");
    card.lock_glint(later(g3, 13 * GLINT_PERIOD_MS + 1000 + FLARE_STEP_LEAD_MS));
    let g4 = later(g3, FLOW_STEP_MS);
    assert_eq!(card.glint0, Some(g4), "a step toward it");
    card.lock_glint(later(
        later(g4, 15 * GLINT_PERIOD_MS) - ms(1000),
        FLARE_STEP_LEAD_MS,
    ));
    assert_eq!(card.glint0, Some(g4 - ms(FLOW_STEP_MS)), "a step back");
    assert_eq!(card.fade0, None, "no lock fades the glints");
    let mut fresh = HarnessCard::default();
    let first = t0 + ms(20 * GLINT_PERIOD_MS);
    fresh.lock_glint(first);
    fresh.arm(first + ms(500), 7);
    assert_eq!(
        fresh.glint0,
        Some(first - ms(FLARE_STEP_LEAD_MS)),
        "the first arm keeps the flare's grid"
    );
}

#[test]
fn the_lock_averages_claudes_repaint_jitter_out() {
    let s = scene();
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    card.arm(t0, 7);
    frame(&mut card, t0, &s, true);
    let claude0 = t0 + ms(500);
    let late = |k: u64| {
        if k % 8 == 5 {
            250
        } else {
            (k * 53) % (crate::ripple::CLAUDE_REPAINT_P90_MS + 1)
        }
    };
    let seen = |k: u64| later(claude0, k * GLINT_PERIOD_MS + late(k));
    let p = GLINT_PERIOD_MS as i64;
    let fold = |e: i64| {
        let e = e.rem_euclid(p);
        if e > p / 2 { e - p } else { e }
    };
    let offset = |card: &HarnessCard| {
        let g0 = card.glint0.expect("a grid");
        fold(
            i64::try_from(ms_between(claude0, g0)).unwrap()
                - i64::try_from(ms_between(g0, claude0)).unwrap(),
        )
    };
    let lead = i64::try_from(FLARE_STEP_LEAD_MS).unwrap();
    for k in 0..2 {
        assert!(
            !card.glints_shown(seen(k)),
            "flare {k}: in the intro, unseen"
        );
        card.lock_glint(seen(k));
        assert_eq!(
            offset(&card),
            i64::try_from(late(k)).unwrap() - lead,
            "flare {k}: taken whole"
        );
    }
    frame(&mut card, t0 + ms(EPISODE_MS - 5), &s, true);
    frame(&mut card, t0 + ms(EPISODE_MS + 200), &s, true);
    let (mut worst, mut max_move) = (0i64, 0i64);
    let mean = (2..302)
        .map(|k| i64::try_from(late(k)).unwrap())
        .sum::<i64>()
        / 300;
    for k in 2..302 {
        let before = offset(&card);
        assert!(card.glints_shown(seen(k)), "flare {k}: the glints show");
        card.lock_glint(seen(k));
        let moved = (offset(&card) - before).abs();
        let bound = if late(k) == 250 { 64 } else { 40 };
        assert!(moved <= bound, "flare {k}: moved {moved} ms");
        max_move = max_move.max(moved);
        assert_eq!(card.fade0, None, "flare {k}: no fade");
        if k >= 12 {
            worst = worst.max((offset(&card) - (mean - lead)).abs());
        }
    }
    let half_cell = (500.0 / crate::ripple::GLINT_CELLS_PER_S) as i64;
    assert!(worst <= half_cell, "locked within {worst} ms");
    assert!(max_move > 0, "CONTROL: the lock does move the grid");
    let (mut off, mut refades, mut chased) = (0i64, 0, 0i64);
    for k in 2..302 {
        let e = fold(i64::try_from(late(k)).unwrap() - off);
        if e.abs() <= 64 {
            chased = chased.max(e.abs());
            off += e;
        } else {
            refades += 1;
            off = i64::try_from(late(k)).unwrap();
        }
    }
    assert!(
        refades > 30 && chased > 40,
        "CONTROL: the old rule re-fades ({refades}) and chases the jitter ({chased} ms)"
    );
}

#[test]
fn a_step_lock_folds_into_half_a_step() {
    let s = scene();
    let mut fresh = HarnessCard::default();
    fresh.lock_step(Instant::now());
    assert_eq!(fresh.glint0, None, "CONTROL: no grid, none made");
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    card.arm(t0, 7);
    frame(&mut card, t0, &s, true);
    let g0 = card.glint0.expect("armed");
    let seen = later(g0, 5 * FLOW_STEP_MS + 50 + FLARE_STEP_LEAD_MS);
    assert!(!card.glints_shown(seen), "CONTROL: no glint shows yet");
    card.lock_step(seen);
    let g1 = later(g0, 50);
    assert_eq!(card.glint0, Some(g1), "taken whole, unseen");
    frame(&mut card, t0 + ms(EPISODE_MS - 5), &s, true);
    let mut kept = CardFrames::default();
    let at = t0 + ms(EPISODE_MS + 200);
    let _ = card.draw(at, &s, true, &mut kept);
    assert!(card.glints_shown(at), "CONTROL: the flow's glints show");
    let seen_at =
        |g: Instant, step: u64, off: u64| later(g, step * FLOW_STEP_MS + off + FLARE_STEP_LEAD_MS);
    card.lock_step(seen_at(g1, 60, 3));
    assert_eq!(card.glint0, Some(g1), "a 3 ms error is noise");
    assert!(
        card.replay_step(&s, true, &kept, at + ms(1)).is_some(),
        "still kept"
    );
    card.lock_step(seen_at(g1, 61, 40));
    let g2 = later(g1, 10);
    assert_eq!(card.glint0, Some(g2), "slewed by +10");
    card.lock_step(seen_at(g2, 62, 100));
    assert_eq!(card.glint0, Some(g2 - ms(7)), "folded, slewed by −7");
}

#[test]
fn claudes_repaints_carry_the_flowing_cards_steps() {
    let s = scene();
    let run = |period: u64, extra: u64, steps_lock: bool| {
        let t0 = Instant::now();
        let us = |n: u64| t0 + Duration::from_micros(n);
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        let mut kept = CardFrames::default();
        let mut draw = |card: &mut HarnessCard, now: Instant| {
            if card.replay_step(&s, true, &kept, now).is_none() {
                let _ = card.draw(now, &s, true, &mut kept);
            }
        };
        draw(&mut card, t0);
        let claude0 = 500_000u64;
        let repaint = |n: u64| claude0 + n * period + ((n * 37) % 13) * 1000;
        let flaring = |at: u64| (at.saturating_sub(claude0) % 3_072_000) < 2 * 128_000;
        let (mut n, mut lit) = (0u64, false);
        let (mut fires, mut steps) = (0u64, 0u64);
        let settle = 20_000_000u64;
        let end = settle + 30_000_000;
        let mut last_step = None;
        loop {
            let at = repaint(n);
            if at > end {
                break;
            }
            while let Some(wake) = card.deadline(us(at)).filter(|&w| w < us(at)) {
                let w = u64::try_from(wake.duration_since(t0).as_micros()).unwrap();
                draw(&mut card, wake);
                if w >= settle {
                    fires += 1;
                }
                if card.deadline(wake) == Some(wake) {
                    break;
                }
            }
            let flare = flaring(at);
            if flare && !lit {
                card.lock_glint(us(at) - ms(extra));
            } else if steps_lock {
                card.lock_step(us(at) - ms(extra));
            }
            lit = flare;
            draw(&mut card, us(at));
            if at >= settle {
                let step = card.flow_step(us(at));
                if step != last_step {
                    steps += 1;
                    last_step = step;
                }
            }
            n += 1;
        }
        (fires, steps)
    };
    let (fires, steps) = run(127_700, 0, true);
    assert!(steps > 200, "CONTROL: 30 s of flow ({steps} steps)");
    assert!(
        fires * 20 < steps,
        "rides Claude's repaints: {fires} wakes in {steps} steps"
    );
    for period in [128_000, 128_600, 129_000] {
        let (fires, steps) = run(period, 0, true);
        assert!(
            fires * 5 < steps,
            "{period} µs: still rides them: {fires} wakes in {steps} steps"
        );
    }
    let (fires, steps) = run(127_700, 0, false);
    assert!(
        fires * 2 > steps,
        "NEGATIVE CONTROL: the flare's lock alone falls behind: {fires} of {steps}"
    );
    let (fires, steps) = run(127_700, 83 - FLARE_STEP_LEAD_MS, false);
    assert!(
        fires * 10 >= steps * 9,
        "NEGATIVE CONTROL: the old lock's wakes: {fires} of {steps} steps"
    );
}

#[test]
fn episode_frames_move_and_keep_a_deadline() {
    for s in scenes() {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 11);
        let mut prev: Option<(Vec<GlowQuad>, Vec<GlowQuad>, u64)> = None;
        let mut at = 0;
        while at < EPISODE_MS {
            let now = t0 + ms(at);
            let f = frame(&mut card, now, &s, true);
            assert_eq!(card.deadline(now), Some(now + ms(FRAME_MS)), "{at} ms");
            if let Some(p) = &prev {
                assert_ne!(p.2, f.2, "the fingerprint held at {at} ms");
                if at < 5000 {
                    assert!(p.0 != f.0 || p.1 != f.1, "the quads held at {at} ms");
                }
            }
            prev = Some(f);
            at += FRAME_MS;
        }
        let end = t0 + ms(EPISODE_MS);
        assert_eq!(
            card.deadline(end),
            Some(end),
            "the flow's first frame is owed"
        );
        frame(&mut card, end, &s, true);
        let step = EPISODE_MS / FLOW_STEP_MS;
        assert_eq!(
            card.deadline(end),
            Some(t0 + ms((step + 1) * FLOW_STEP_MS + FLOW_SLACK_MS)),
            "then the step grid"
        );
    }
}

#[test]
fn every_quad_lies_in_one_band_inside_the_box() {
    for s in scenes() {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 3);
        let mut at = 0;
        while at <= EPISODE_MS + 2 * GLINT_PERIOD_MS + 100 {
            let (under, over, _) = frame(&mut card, t0 + ms(at), &s, true);
            for q in &under {
                assert_in_band_and_box(q, &s, &format!("under at {at} ms"));
            }
            for q in &over {
                assert_in_band_and_box(q, &s, &format!("over at {at} ms"));
            }
            at += 17;
        }
    }
}

#[test]
fn air_moves_the_border_and_nothing_else() {
    let extent = |quads: &[GlowQuad]| {
        let y0 = quads.iter().map(|q| i64::from(q.y)).min();
        let y1 = quads.iter().map(|q| i64::from(q.y) + i64::from(q.h)).max();
        let x0 = quads.iter().map(|q| i64::from(q.x)).min();
        let x1 = quads.iter().map(|q| i64::from(q.x) + i64::from(q.w)).max();
        (y0.unwrap_or(0), y1.unwrap_or(0), x0, x1)
    };
    let tagged = |quads: &[GlowQuad], row: u16| quads.iter().filter(|q| q.row == row).count();
    for bare in [
        scene(),
        CardScene {
            kitty_rows: 3,
            dark: false,
            palette: CardPalette::Land,
            ..scene()
        },
    ] {
        let g = Geo::new(&bare).expect("a scene");
        let (top, bottom) = (g.band(g.top), g.band(g.top + g.rows));
        let half = g.ch / 2;
        let (over, under) = (
            bare.row_base + bare.top - 1,
            bare.row_base + bare.top + bare.rows,
        );
        for clock in [
            Clock::SETTLED,
            Clock::riding(150),
            Clock::riding(1300),
            Clock::seated(150),
            Clock::flowing(EPISODE_MS + 500),
            Clock::flowing(EPISODE_MS + 1900),
        ] {
            let (a, b) = (parts(&bare, clock), parts(&aired(bare), clock));
            let what = format!("{} rows, {clock:?}", bare.kitty_rows);
            assert!(!a.kitty.is_empty() && !a.trail.is_empty(), "{what}");
            assert_eq!(a.trail, b.trail, "{what}: the trail");
            assert_eq!(a.kitty, b.kitty, "{what}: the kitty");
            assert_eq!(a.rim, b.rim, "{what}: the rim");
            assert_eq!(a.stars, b.stars, "{what}: the field");
            assert_eq!(a.sparkles, b.sparkles, "{what}: the sparkles");
            assert_eq!(a.burst, b.burst, "{what}: the burst");
            let (ya0, ya1, xa0, xa1) = extent(&a.border);
            assert_eq!(
                (ya0, ya1, tagged(&a.border, over), tagged(&a.border, under)),
                (top, bottom, 0, 0),
                "{what}: CONTROL, no air: the border closes on the boxed rows"
            );
            let (yb0, yb1, xb0, xb1) = extent(&b.border);
            assert_eq!((yb0, yb1), (top - half, bottom + half), "{what}: the air");
            assert_eq!((xb0, xb1), (xa0, xa1), "{what}: the sides stay");
            let n = tagged(&a.border, bare.row_base + bare.top);
            assert!(n > 2, "{what}: the top edge");
            assert_eq!(tagged(&b.border, over), n, "{what}: over the box");
            assert_eq!(tagged(&b.border, under), n, "{what}: under the box");
            assert_eq!(b.border.len(), a.border.len() + 4, "{what}: four slices");
            for q in &b.border {
                assert_in_band_and_box(q, &aired(bare), &what);
            }
        }
    }
    let first = CardScene { top: 0, ..scene() };
    let lifted = CardScene {
        pad_above: first.ch,
        ..first
    };
    let border = parts(&lifted, Clock::SETTLED).border;
    assert_eq!(border, parts(&first, Clock::SETTLED).border);
    assert_eq!(
        extent(&border).0,
        i64::from(first.origin_y),
        "nothing over band 0"
    );
}

fn runs_of(frame: &[&str]) -> usize {
    frame
        .iter()
        .map(|r| {
            let b = r.as_bytes();
            (0..b.len())
                .filter(|&i| b[i] != b'.' && (i == 0 || b[i - 1] != b[i]))
                .count()
        })
        .sum()
}

#[test]
fn the_kitty_is_opaque_square_pixels_and_dark_marks_are_light() {
    for kitty_rows in [2u8, 3] {
        let s = CardScene {
            kitty_rows,
            ..scene()
        };
        let g = Geo::new(&s).expect("a scene");
        let at = kitty_rect(&g, &s).expect("a kitty");
        let inks = KittyInk::new(s.coat, s.eye, s.dark);
        let (under, over, _) = settled(&s);
        assert!(
            under.iter().all(|q| q.alpha == 0 && q.alpha2 == 0),
            "dark border and trail are light"
        );
        assert!(
            over.iter().all(|q| q.alpha == 0 || q.alpha == 255),
            "the rim and the stars are light on dark"
        );
        let kitty: Vec<GlowQuad> = over.iter().filter(|q| q.alpha == 255).copied().collect();
        let art = |f: &[&str]| f.iter().map(|r| (*r).to_string()).collect::<Vec<_>>();
        assert_eq!(
            drawn_kitty(&kitty, &at, inks),
            art(at.art.open),
            "{kitty_rows} rows"
        );
        let area: i64 = kitty.iter().map(|q| i64::from(q.w) * i64::from(q.h)).sum();
        let solid = at
            .art
            .open
            .iter()
            .map(|r| r.bytes().filter(|&b| b != b'.').count())
            .sum::<usize>() as i64;
        assert_eq!(
            area,
            solid * at.p * at.p,
            "{kitty_rows} rows: no overlap, no spill"
        );
        let runs = runs_of(at.art.open);
        assert!(
            kitty.len() * 6 < runs * 5,
            "{kitty_rows} rows: {} quads for {runs} runs",
            kitty.len()
        );
        if kitty_rows == 2 {
            let mid = s.origin_y + i32::from(s.top + 1) * i32::from(s.ch);
            assert!(
                kitty.iter().all(|q| {
                    let (y0, y1) = (i32::from(q.y), i32::from(q.y) + i32::from(q.h));
                    y1 <= mid || y0 >= mid
                }) && kitty.iter().any(|q| i32::from(q.y) + i32::from(q.h) == mid),
                "the halves meet at the boundary"
            );
        }
        let light = CardScene { dark: false, ..s };
        let lit = parts(&light, Clock::SETTLED);
        assert_eq!(lit.kitty.len(), kitty.len(), "{kitty_rows} rows");
        assert!(
            lit.kitty
                .iter()
                .chain(&lit.rim)
                .all(|q| (q.alpha, q.alpha2) == (255, 255)),
            "the kitty and its rim are opaque on light too (print art)"
        );
        let mut nudged = kitty.clone();
        nudged[0].x += u16::try_from(at.p).expect("a pixel");
        assert_ne!(drawn_kitty(&nudged, &at, inks), art(at.art.open));
    }
}

fn mark_inks(palette: CardPalette) -> Vec<u32> {
    (0..TRAIL_STOPS.len())
        .map(|k| palette.star_ink(k))
        .chain([WHITE])
        .collect()
}

#[test]
fn a_light_theme_is_source_over_ink_throughout() {
    for s in scenes().into_iter().filter(|s| !s.dark) {
        let inks = mark_inks(s.palette);
        let printed = |c: u32, a: u8| inks.iter().any(|&k| premul_rgb(k, a) == c);
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 5);
        let flow = (EPISODE_MS..=EPISODE_MS + 2 * GLINT_PERIOD_MS).step_by(FLOW_STEP_MS as usize);
        for at in (0..EPISODE_MS).step_by(30).chain(flow) {
            let (under, over, _) = frame(&mut card, t0 + ms(at), &s, true);
            for q in under.iter().chain(&over) {
                assert!(
                    q.alpha > 0 && q.alpha2 > 0,
                    "additive on white at {at} ms: {q:?}"
                );
                if q.alpha < 255 || q.alpha2 < 255 {
                    assert!(
                        printed(q.color, q.alpha) && printed(q.color2, q.alpha2),
                        "a translucent mark off the print at {at} ms: {q:?}"
                    );
                }
            }
        }
        let law = InkRole::OverText.ink(spectrum_stop(0));
        assert!(!printed(premul_rgb(law, 176), 176), "CONTROL");
    }
}

fn on_any(quads: &[GlowQuad], x: i64, y: i64) -> bool {
    quads.iter().any(|q| {
        let (qx, qy) = (i64::from(q.x), i64::from(q.y));
        (qx..qx + i64::from(q.w)).contains(&x) && (qy..qy + i64::from(q.h)).contains(&y)
    })
}

fn print_breaches(marks: &[GlowQuad], trail: &[GlowQuad], palette: CardPalette) -> Vec<String> {
    let inks: Vec<u32> = (0..TRAIL_STOPS.len())
        .map(|k| palette.star_ink(k))
        .collect();
    let mut out = Vec::new();
    for q in marks {
        let white = q.color == premul_rgb(WHITE, q.alpha);
        if white && inks.iter().any(|&k| premul_rgb(k, q.alpha) == q.color) {
            continue;
        }
        for y in i64::from(q.y)..i64::from(q.y) + i64::from(q.h) {
            for x in i64::from(q.x)..i64::from(q.x) + i64::from(q.w) {
                if white != on_any(trail, x, y) {
                    out.push(format!("({x}, {y}) white {white}: {q:?}"));
                }
            }
        }
    }
    out
}

#[test]
fn light_marks_print_on_the_paper_and_glint_on_the_trail() {
    let (mut glints, mut inked) = (0usize, 0usize);
    let longest = CardScene {
        dark: false,
        ..widest()
    };
    for s in scenes()
        .into_iter()
        .filter(|s| !s.dark && s.trail.is_some())
        .chain([longest])
    {
        for t in (0..=EPISODE_MS).step_by(30).map(Some).chain([None]) {
            for ride_ms in [t, None] {
                let clock = Clock {
                    t_ms: t,
                    ride_ms,
                    glint: None,
                    flow: None,
                };
                let p = parts(&s, clock);
                let marks: Vec<GlowQuad> = p
                    .stars
                    .iter()
                    .chain(&p.sparkles)
                    .chain(&p.burst)
                    .copied()
                    .collect();
                let bad = print_breaches(&marks, &p.trail, s.palette);
                assert!(
                    bad.is_empty(),
                    "{t:?}/{ride_ms:?}: {:?}",
                    &bad[..bad.len().min(4)]
                );
                for q in &marks {
                    if q.color == premul_rgb(WHITE, q.alpha) {
                        glints += 1;
                    } else {
                        inked += 1;
                    }
                }
            }
        }
    }
    assert!(glints > 0 && inked > 0, "glints {glints}, ink {inked}");
    let s = CardScene {
        dark: false,
        ..scene()
    };
    let geo = Geo::new(&s).expect("a scene");
    let region = star_region(&geo, &s).expect("a region");
    let p = parts(&s, Clock::SETTLED);
    let (ya, yb) = trail_band(&geo, &s);
    let centre = (geo.col(34), (ya + yb) / 2);
    let mut old = Vec::new();
    let sink = Sink { geo };
    lay_star(
        &mut old,
        &sink,
        region,
        centre,
        4,
        STAR_COV,
        false,
        s.palette.star_ink(0),
        &[],
        false,
    );
    assert!(
        !print_breaches(&old, &p.trail, s.palette).is_empty(),
        "CONTROL"
    );
}

#[test]
fn light_marks_lie_only_on_cells_read_blank() {
    let (mut marks_seen, mut dark_past) = (0usize, 0usize);
    for s in scenes().into_iter().filter(|s| !s.dark) {
        let g = Geo::new(&s).expect("a scene");
        let start = match (s.trail, s.kitty_col) {
            (Some((t0, _)), Some(k)) => t0.min(k),
            (Some((t0, _)), None) => t0,
            (None, Some(k)) => k,
            (None, None) => continue,
        };
        let blank = Rect {
            x0: g.col(start),
            y0: g.band(g.top),
            x1: g.col(s.cols),
            y1: g.band(g.top + 2),
        };
        for t in (0..=EPISODE_MS).step_by(30).map(Some).chain([None]) {
            for ride_ms in [t, None] {
                let p = parts(
                    &s,
                    Clock {
                        t_ms: t,
                        ride_ms,
                        glint: None,
                        flow: None,
                    },
                );
                for q in p.stars.iter().chain(&p.sparkles).chain(&p.burst) {
                    marks_seen += 1;
                    let (x, y) = (i64::from(q.x), i64::from(q.y));
                    let (w, h) = (i64::from(q.w), i64::from(q.h));
                    assert!(
                        x >= blank.x0 && y >= blank.y0 && x + w <= blank.x1 && y + h <= blank.y1,
                        "{t:?}/{ride_ms:?}: {q:?} outside {blank:?}"
                    );
                }
            }
        }
        let dark = CardScene { dark: true, ..s };
        let region = star_region(&g, &dark).expect("a region");
        dark_past += usize::from(region.x1 > blank.x1);
    }
    assert!(marks_seen > 0);
    assert!(dark_past > 0, "CONTROL");
}

#[test]
fn the_light_field_keeps_off_the_trails_edge() {
    let mut across_dark = 0;
    for s in scenes().into_iter().filter(|s| s.trail.is_some()) {
        let g = Geo::new(&s).expect("a scene");
        let Some(t) = trail_rect(&g, &s, kitty_rect(&g, &s)) else {
            continue;
        };
        for seed in 0..64 {
            let (spots, n) = field_spots(&g, &s, seed, !s.dark);
            for sp in &spots[..n] {
                let arm = i64::from(star_arm_px(star_arm(g.ch as f32, sp.ratio)));
                let m = arm + 1;
                let reach = Rect {
                    x0: sp.x - m,
                    y0: sp.y - m,
                    x1: sp.x + m + 1,
                    y1: sp.y + m + 1,
                };
                if s.dark {
                    across_dark += usize::from(across_trail(reach, t));
                } else {
                    assert!(!across_trail(reach, t), "seed {seed}: {sp:?}");
                }
            }
        }
    }
    assert!(across_dark > 0, "CONTROL");
}

#[test]
fn the_kept_line_inks_are_the_law() {
    let cream = 0x00FB_F1C7;
    let step = |i: usize| i as f32 / LINE_INK_STEPS as f32;
    for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
        let mut inks = LineInks::default();
        inks.solve(palette, WHITE);
        for i in 0..=LINE_INK_STEPS {
            for ground in [WHITE, cream] {
                assert_eq!(
                    inks.at(palette, ground, step(i)),
                    palette.text_at(step(i), ground),
                    "{palette:?} step {i} on {ground:06x}"
                );
            }
        }
        let half = 0.4 / LINE_INK_STEPS as f32;
        assert_eq!(
            inks.at(palette, WHITE, step(7) + half),
            inks.at(palette, WHITE, step(7))
        );
        assert_eq!(inks.at(palette, WHITE, -1.0), inks.at(palette, WHITE, 0.0));
        assert_eq!(inks.at(palette, WHITE, 2.0), inks.at(palette, WHITE, 1.0));
        assert_eq!(
            inks.at(palette, WHITE, f32::NAN),
            inks.at(palette, WHITE, 0.0)
        );
        assert!(
            (0..=LINE_INK_STEPS)
                .any(|i| palette.text_at(step(i), WHITE) != palette.text_at(step(i), cream)),
            "{palette:?}: CONTROL, the grounds' inks differ"
        );
    }
    let t0 = Instant::now();
    let mut light = HarnessCard::default();
    light.arm(t0, 7);
    let page = CardScene {
        dark: false,
        ground: cream,
        palette: CardPalette::Land,
        ..scene()
    };
    light.tick(t0, &page, true, &mut Vec::new(), &mut Vec::new());
    assert_eq!(light.inks.key, Some((CardPalette::Land, cream)));
    let white = CardScene {
        ground: WHITE,
        palette: CardPalette::Cruise,
        ..page
    };
    light.tick(
        t0 + ms(FRAME_MS),
        &white,
        true,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    assert_eq!(light.inks.key, Some((CardPalette::Cruise, WHITE)));
    let mut dark = HarnessCard::default();
    dark.arm(t0, 7);
    dark.tick(t0, &scene(), true, &mut Vec::new(), &mut Vec::new());
    assert_eq!(dark.inks.key, None);

    let mut masked = LineInks::default();
    masked.solve(CardPalette::Land, 0xFF00_0000 | cream);
    assert_eq!(masked.key, Some((CardPalette::Land, cream)));
    let _ = take_line_ink_solves();
    assert_eq!(
        masked.at(CardPalette::Land, 0xFF00_0000 | cream, step(7)),
        CardPalette::Land.text_at(step(7), cream)
    );
    assert_eq!(take_line_ink_solves(), 0);

    let mut frames = CardFrames::default();
    let mut drawn = HarnessCard::default();
    let _ = drawn.draw(t0, &page, false, &mut frames);
    let mut replayed = HarnessCard::default();
    assert!(replayed.replay(&page, false, &frames).is_some());
    assert_eq!(replayed.inks.key, Some((CardPalette::Land, cream)));
    let _ = take_line_ink_solves();
    let _ = replayed.line_ink(t0, 4, 2, 9, cream, false);
    assert_eq!(take_line_ink_solves(), 0);

    let flow_at = t0 + ms(EPISODE_MS + 1);
    let mut flow_frames = CardFrames::default();
    let mut flow_drawn = HarnessCard::default();
    flow_drawn.arm(t0, 7);
    let _ = flow_drawn.draw(flow_at, &page, true, &mut flow_frames);
    let mut flow_replayed = HarnessCard::default();
    flow_replayed.arm(t0, 7);
    let _ = flow_replayed.draw(flow_at, &scene(), true, &mut CardFrames::default());
    assert_eq!(flow_replayed.inks.key, None);
    assert!(
        flow_replayed
            .replay_step(&page, true, &flow_frames, flow_at + ms(10))
            .is_some()
    );
    assert_eq!(flow_replayed.inks.key, Some((CardPalette::Land, cream)));
    let _ = take_line_ink_solves();
    let _ = flow_replayed.line_ink(flow_at + ms(10), 4, 2, 9, cream, true);
    assert_eq!(take_line_ink_solves(), 0);
}

fn star_boxes(quads: &[GlowQuad]) -> Vec<Rect> {
    let rect = |q: &GlowQuad| Rect {
        x0: i64::from(q.x),
        y0: i64::from(q.y),
        x1: i64::from(q.x) + i64::from(q.w),
        y1: i64::from(q.y) + i64::from(q.h),
    };
    let mut boxes: Vec<Rect> = Vec::new();
    for r in quads.iter().map(rect) {
        let grown = Rect {
            x0: r.x0 - 1,
            y0: r.y0 - 1,
            x1: r.x1 + 1,
            y1: r.y1 + 1,
        };
        let mut merged = r;
        boxes.retain(|b| {
            if b.meet(grown).is_empty() {
                true
            } else {
                merged = Rect {
                    x0: merged.x0.min(b.x0),
                    y0: merged.y0.min(b.y0),
                    x1: merged.x1.max(b.x1),
                    y1: merged.y1.max(b.y1),
                };
                false
            }
        });
        boxes.push(merged);
    }
    boxes
}

#[test]
fn light_sparkles_lie_whole_on_the_trails_ink() {
    let mut seen = 0usize;
    for s in scenes()
        .into_iter()
        .filter(|s| s.trail.is_some())
        .map(|s| CardScene { dark: false, ..s })
    {
        for t in (0..RIDE_MS).step_by(20) {
            let p = parts(
                &s,
                Clock {
                    t_ms: Some(t),
                    ride_ms: Some(t),
                    glint: None,
                    flow: None,
                },
            );
            for b in star_boxes(&p.sparkles) {
                seen += 1;
                assert_eq!(
                    b.x1 - b.x0,
                    b.y1 - b.y0,
                    "{}x{} at {t} ms: a cut sparkle {b:?}",
                    s.cw,
                    s.ch
                );
            }
            assert!(
                p.sparkles
                    .iter()
                    .all(|q| q.color == premul_rgb(WHITE, q.alpha)),
                "{}x{} at {t} ms: a sparkle off the trail's ink",
                s.cw,
                s.ch
            );
        }
    }
    assert!(seen > 0);
    let s = CardScene {
        dark: false,
        ..scene()
    };
    let geo = Geo::new(&s).expect("a scene");
    let clock = Clock {
        t_ms: Some(300),
        ride_ms: Some(300),
        glint: None,
        flow: None,
    };
    let cx = Emit::new(&s, geo, clock, 7);
    let region = star_region(&geo, &s).expect("a region");
    let lay = cx.trail_lay().expect("a trail");
    let x = (lay.tx0 + lay.drawn) / 2;
    let trail = cx.trail_ink();
    let edge = trail
        .covers()
        .iter()
        .find(|c| (c.x0..c.x1).contains(&x))
        .expect("a segment");
    let centre = (x, edge.y0);
    let mut two = Vec::new();
    let sink = Sink { geo };
    let arm = i64::from(star_arm_px(star_arm(
        geo.ch as f32,
        STAR_ARM_FINE * STAR_ARM_INK,
    )));
    lay_star(
        &mut two,
        &sink,
        region,
        centre,
        arm,
        SHED_COV,
        false,
        s.palette.star_ink(0),
        trail.covers(),
        false,
    );
    assert!(
        two.iter().any(|q| q.color == premul_rgb(WHITE, q.alpha))
            && two.iter().any(|q| q.color != premul_rgb(WHITE, q.alpha)),
        "CONTROL: two tones {two:?}"
    );
}

#[test]
fn a_light_stars_nucleus_and_glints_print_at_its_strength() {
    let s = CardScene {
        dark: false,
        ..scene()
    };
    let geo = Geo::new(&s).expect("a scene");
    let region = star_region(&geo, &s).expect("a region");
    let centre = (region.x0 + 3 * geo.cw, region.y0 + geo.ch);
    let arm = 8;
    let d = ((arm as f32 * STAR_GLINT).round() as i64).max(1);
    let cov = STAR_COV * STAR_STEADY_ENV;
    for k in 0..TRAIL_STOPS.len() {
        let ink_rgb = s.palette.star_ink(k);
        let mut out = Vec::new();
        let sink = Sink { geo };
        lay_star(
            &mut out,
            &sink,
            region,
            centre,
            arm,
            cov,
            true,
            ink_rgb,
            &[],
            false,
        );
        let arm32 = i32::try_from(arm).expect("a small arm");
        let (c, t) = (
            i64::from(star_core_px(arm32)),
            i64::from(star_waist_px(arm32)),
        );
        assert!(c > t, "the nucleus stands out past the waist");
        let cc = (c - 1) / 2;
        let core = Rect {
            x0: centre.0 - cc,
            y0: centre.1 - cc,
            x1: centre.0 - cc + c,
            y1: centre.1 - cc + c,
        };
        let touching: Vec<&GlowQuad> = out
            .iter()
            .filter(|q| {
                !core
                    .meet(Rect {
                        x0: i64::from(q.x),
                        y0: i64::from(q.y),
                        x1: i64::from(q.x) + i64::from(q.w),
                        y1: i64::from(q.y) + i64::from(q.h),
                    })
                    .is_empty()
            })
            .collect();
        assert!(!touching.is_empty());
        for q in touching {
            assert_eq!(
                (q.color, q.alpha),
                (premul_rgb(ink_rgb, 255), 255),
                "ink {k}: the nucleus {q:?}"
            );
        }
        for (gx, gy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
            let (x, y) = (centre.0 + gx, centre.1 + gy);
            let dot = out
                .iter()
                .find(|q| (i64::from(q.x), i64::from(q.y), q.w, q.h) == (x, y, 1, 1))
                .unwrap_or_else(|| panic!("ink {k}: no glint at ({x}, {y})"));
            assert_eq!(
                (dot.color, dot.alpha),
                (premul_rgb(ink_rgb, 255), 255),
                "ink {k}: {dot:?}"
            );
        }
        let before = ink(
            ink_rgb,
            (cov * STAR_GLINT_COV).floor() * 255.0 / STAR_PRINT_FULL,
        );
        assert!(before.alpha < 128, "CONTROL {before:?}");
        let additive = ink(
            ink_rgb,
            (cov * STAR_CORE_ADD).floor() * 255.0 / STAR_PRINT_FULL,
        );
        assert!(additive.alpha < 128, "CONTROL {additive:?}");
    }
}

fn pixels(quads: &[GlowQuad]) -> std::collections::BTreeSet<(i64, i64)> {
    let mut px = std::collections::BTreeSet::new();
    for q in quads {
        for y in i64::from(q.y)..i64::from(q.y) + i64::from(q.h) {
            for x in i64::from(q.x)..i64::from(q.x) + i64::from(q.w) {
                px.insert((x, y));
            }
        }
    }
    px
}

fn one_sided(cx: &Emit, region: Rect, (x, y): (i64, i64)) -> Rect {
    let Some(lay) = cx.trail_lay() else {
        return region;
    };
    let w = cx.wave(&lay, 0).abs();
    let t = Rect {
        x0: lay.tx0,
        y0: lay.ya - w,
        x1: lay.drawn,
        y1: lay.yb + w,
    };
    if t.contains(x, y) {
        return region.meet(Rect {
            y0: lay.ya + w,
            y1: lay.yb - w,
            ..t
        });
    }
    [
        (t.x0 - 1 - x, Rect { x1: t.x0, ..region }),
        (x - t.x1, Rect { x0: t.x1, ..region }),
        (t.y0 - 1 - y, Rect { y1: t.y0, ..region }),
        (y - t.y1, Rect { y0: t.y1, ..region }),
    ]
    .into_iter()
    .filter(|(d, _)| *d >= 0)
    .max_by_key(|(d, _)| *d)
    .map_or(region, |(_, r)| region.meet(r))
}

#[test]
fn light_burst_stars_lie_whole() {
    let (mut frames, mut crossed, mut cut_before) = (0usize, 0usize, 0usize);
    for s in scenes()
        .into_iter()
        .filter(|s| !s.dark && s.trail.is_some())
    {
        let geo = Geo::new(&s).expect("a scene");
        for seed in [3u64, 7, 11, 19, 23] {
            for ride in [true, false] {
                for t in (0..RIDE_MS + BURST_MS).step_by(20) {
                    let clock = Clock {
                        t_ms: Some(t),
                        ride_ms: ride.then_some(t),
                        glint: None,
                        flow: None,
                    };
                    let cx = Emit::new(&s, geo, clock, seed);
                    let Some(ring) = cx.burst_ring() else {
                        continue;
                    };
                    frames += 1;
                    let sink = Sink { geo };
                    let mut burst = Vec::new();
                    cx.burst_stars(&mut lay_calls(&cx, &mut burst));
                    let (mut whole, mut before) = (Vec::new(), Vec::new());
                    for (k, &c) in ring.centres.iter().enumerate() {
                        let rgb = s.palette.star_ink(k);
                        for (out, clip) in [
                            (&mut whole, ring.region),
                            (&mut before, one_sided(&cx, ring.region, c)),
                        ] {
                            lay_star(
                                out,
                                &sink,
                                clip,
                                c,
                                ring.arm,
                                ring.cov,
                                true,
                                rgb,
                                &[],
                                false,
                            );
                        }
                    }
                    let want = pixels(&whole);
                    let what = format!("{}x{} seed {seed} ride {ride} {t} ms", s.cw, s.ch);
                    assert_eq!(pixels(&burst), want, "{what}");
                    let tones = burst
                        .iter()
                        .map(|q| q.color == premul_rgb(WHITE, q.alpha))
                        .collect::<std::collections::BTreeSet<bool>>();
                    crossed += usize::from(tones.len() == 2);
                    cut_before += usize::from(pixels(&before) != want);
                }
            }
        }
    }
    assert!(
        frames > 0 && crossed > 0,
        "{frames} frames, {crossed} in two tones"
    );
    assert!(cut_before * 2 > frames, "CONTROL: {cut_before} of {frames}");
}

#[test]
fn the_light_sweep_only_deepens_a_shade() {
    let grounds = light_builtins();
    let lum = |g: &&(&str, u32)| relative_luminance(g.1);
    let brightest = grounds.iter().max_by(|a, b| lum(a).total_cmp(&lum(b)));
    let dimmest = grounds.iter().min_by(|a, b| lum(a).total_cmp(&lum(b)));
    let value = |rgb: u32| (rgb >> 16).max((rgb >> 8) & 0xff).max(rgb & 0xff) as f32;
    for &(name, g) in brightest.into_iter().chain(dimmest) {
        for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            let page = CardScene {
                palette,
                dark: false,
                ground: g,
                ..scene()
            };
            card.tick(t0, &page, false, &mut Vec::new(), &mut Vec::new());
            card.arm(t0, 9);
            let len = 80u16;
            let mut least = f32::MAX;
            for at in (0..=EPISODE_MS).step_by(10) {
                let now = t0 + ms(at);
                let Some(run) = card.running(now) else {
                    continue;
                };
                let clock = Clock {
                    t_ms: Some(run),
                    ride_ms: None,
                    glint: None,
                    flow: None,
                };
                let phi = clock.line_drift(LINE_DRIFT);
                for (i, c) in card.line_ink(now, 0, 0, len, g, true).iter().enumerate() {
                    let rgb = (u32::from(c.color[0]) << 16)
                        | (u32::from(c.color[1]) << 8)
                        | u32::from(c.color[2]);
                    let x = i as f32 / f32::from(len - 1);
                    let plain = card.inks.at(palette, g, tri(x + phi));
                    least = least.min(value(rgb) / value(plain));
                }
            }
            assert!(
                (0.69..0.8).contains(&least),
                "{name} {palette:?}: the darkest swept letter keeps {least:.3}"
            );
        }
    }
    let red = PRINT_CRUISE[0];
    let mud = mix_rgb(red, 0, LINE_SWEEP_GAIN);
    assert!(value(mud) / value(red) < 0.69, "CONTROL");
}

#[test]
fn the_kept_line_inks_step_unseen() {
    let grounds = light_builtins();
    let lum = |g: &&(&str, u32)| relative_luminance(g.1);
    let brightest = grounds.iter().max_by(|a, b| lum(a).total_cmp(&lum(b)));
    let dimmest = grounds.iter().min_by(|a, b| lum(a).total_cmp(&lum(b)));
    let gap = |palette: CardPalette, g: u32, steps: usize| {
        (0..steps)
            .map(|i| {
                let at = |i: usize| palette.text_at(i as f32 / steps as f32, g);
                crate::color_math::oklab_distance(at(i), at(i + 1))
            })
            .fold(0.0f32, f32::max)
    };
    for &(name, g) in brightest.into_iter().chain(dimmest) {
        for palette in [CardPalette::Cruise, CardPalette::Land] {
            let mut inks = LineInks::default();
            inks.solve(palette, g);
            let widest = (0..LINE_INK_STEPS)
                .map(|i| crate::color_math::oklab_distance(inks.ink[i], inks.ink[i + 1]))
                .fold(0.0f32, f32::max);
            assert!(widest <= 0.02, "{name} {palette:?}: a step of {widest:.4}");
            assert!(
                gap(palette, g, LINE_INK_STEPS / 16) > 0.02,
                "{name} {palette:?}: CONTROL"
            );
        }
    }
}

fn light_builtins() -> Vec<(&'static str, u32)> {
    use aterm_types::scheme::{Appearance, builtin, builtin_names};
    builtin_names()
        .into_iter()
        .filter_map(|n| Some((n, builtin(n)?)))
        .filter(|(_, s)| s.appearance == Appearance::Light)
        .map(|(n, s)| {
            let c = s.background;
            (
                n,
                (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b),
            )
        })
        .collect()
}

/// Ground polarity belongs to the product, including midtones between ordinary night and
/// paper themes. The leading star uses the same polarity when choosing light or print ink.
#[test]
fn card_and_ink_agree_on_every_builtin_ground() {
    use aterm_types::scheme::{builtin, builtin_names};
    let rgb = |c: aterm_types::Rgb| [c.r, c.g, c.b];
    let grounds = builtin_names()
        .into_iter()
        .map(|name| rgb(builtin(name).unwrap().background))
        .chain(aterm_theme_pack::themes().iter().map(|theme| {
            rgb(aterm_types::scheme::parse_theme_str(theme.text)
                .unwrap()
                .scheme
                .background)
        }))
        .chain((0..=255).map(|gray| [gray; 3]))
        .chain([[112, 128, 144], [160, 100, 80], [60, 140, 100]]);
    for bg in grounds {
        let ground = (u32::from(bg[0]) << 16) | (u32::from(bg[1]) << 8) | u32::from(bg[2]);
        let dark = !aterm_messages::ink::bg_is_light(bg);
        assert_eq!(
            dark,
            aterm_messages::ink::contrast([255; 3], bg)
                >= aterm_messages::ink::contrast([0; 3], bg),
            "ground {ground:06x} chooses the ink with greater contrast"
        );
        assert_eq!(dark_ground(ground), dark, "ground {ground:06x}");
        assert_eq!(
            line_star(WHITE, ground).1,
            if dark {
                DecoBlend::Add
            } else {
                DecoBlend::Over
            },
            "the leading star's blend on {ground:06x}"
        );
    }
}

fn light_letters(palette: CardPalette, ground: u32) -> Vec<u32> {
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    card.tick(
        t0,
        &CardScene {
            palette,
            dark: false,
            ground,
            ..scene()
        },
        false,
        &mut Vec::new(),
        &mut Vec::new(),
    );
    card.line_ink(t0, 0, 0, 80, ground, false)
        .iter()
        .map(|c| {
            (u32::from(c.color[0]) << 16) | (u32::from(c.color[1]) << 8) | u32::from(c.color[2])
        })
        .collect()
}

#[test]
fn the_light_line_is_the_print_at_text_weight() {
    let grounds = light_builtins();
    assert!(grounds.len() >= 7, "{grounds:?}");
    for &(name, g) in &grounds {
        for palette in [CardPalette::Cruise, CardPalette::Land] {
            for rgb in light_letters(palette, g) {
                let ch = [rgb >> 16, (rgb >> 8) & 0xff, rgb & 0xff];
                let c = contrast(rgb, g);
                assert!(
                    ch.contains(&0),
                    "{name} {palette:?}: {rgb:06x} is not a pure hue"
                );
                assert!(c >= 4.5, "{name} {palette:?}: {rgb:06x} at {c:.2}:1");
                assert!(
                    ch.contains(&0xff) || c < 4.6,
                    "{name} {palette:?}: {rgb:06x} is deeper than the text needs, {c:.2}:1"
                );
            }
        }
    }
    let white = 0x00FF_FFFF;
    for &print in &PRINT_CRUISE[..4] {
        let law = InkRole::OverText.ink(print);
        assert!(contrast(law, white) > 6.0, "CONTROL {law:06x}");
    }
}

#[test]
fn the_light_line_reads_on_every_light_ground() {
    let dim = 0x00C8_C8C8;
    assert!(!dark_ground(dim));
    let mut grounds: Vec<u32> = light_builtins().into_iter().map(|(_, g)| g).collect();
    grounds.push(dim);
    for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
        for &g in &grounds {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.tick(
                t0,
                &CardScene {
                    palette,
                    dark: false,
                    ground: g,
                    ..scene()
                },
                false,
                &mut Vec::new(),
                &mut Vec::new(),
            );
            card.arm(t0, 9);
            for at in (0..=EPISODE_MS + 100).step_by(50) {
                for c in card.line_ink(t0 + ms(at), 0, 0, 80, g, true) {
                    let rgb = (u32::from(c.color[0]) << 16)
                        | (u32::from(c.color[1]) << 8)
                        | u32::from(c.color[2]);
                    assert!(
                        contrast(rgb, g) >= 4.5,
                        "{palette:?} {rgb:06x} on {g:06x} at {at} ms: {:.2}:1",
                        contrast(rgb, g)
                    );
                }
            }
        }
        let worst = light_letters(palette, 0x00FF_FFFF)
            .into_iter()
            .map(|rgb| contrast(rgb, dim))
            .fold(f32::MAX, f32::min);
        assert!(worst < 4.5, "CONTROL {palette:?}: {worst:.2}");
    }
}

/// The line reads 4.5:1 on every ground [`dark_ground`] calls dark, mid grounds included (the
/// dark built-ins, a grey ramp to the cut, Silver Aerogel, Unikitty): the resting letters
/// ([`CardPalette::letter_on`]) and every frame of the episode. Negative control: the lifted
/// letter alone fails on Silver Aerogel's ground.
#[test]
fn the_dark_line_reads_on_every_dark_ground() {
    use aterm_types::scheme::{Appearance, builtin, builtin_names};
    let mut grounds: Vec<u32> = builtin_names()
        .into_iter()
        .filter_map(builtin)
        .filter(|s| s.appearance == Appearance::Dark)
        .map(|s| {
            let c = s.background;
            (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
        })
        .collect();
    assert!(grounds.len() >= 7, "{grounds:?}");
    grounds.extend((0..=255u32).step_by(6).map(|v| v * 0x0001_0101));
    grounds.extend([0x0092_9292, 0x00FF_8CD9]);
    grounds.retain(|&g| dark_ground(g));
    let line = LineAt {
        row: 0,
        col: 0,
        len: 60,
        hero: 20..26,
        wand: None,
    };
    let mut ink = Vec::new();
    let mut lows = Vec::new();
    for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
        for &g in &grounds {
            // The ground's least: `(contrast, what)`.
            let mut least = (f32::MAX, String::new());
            let mut note = |rgb: u32, what: &dyn Fn() -> String| {
                let c = contrast(rgb, g);
                if c < least.0 {
                    least = (c, format!("{rgb:06x} {}", what()));
                }
            };
            for k in 0..=255u8 {
                let t = f32::from(k) / 255.0;
                note(palette.letter_on(t, g), &|| format!("resting at {t:.3}"));
            }
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.tick(
                t0,
                &CardScene {
                    palette,
                    ground: g,
                    ..scene()
                },
                false,
                &mut Vec::new(),
                &mut Vec::new(),
            );
            card.arm(t0, 9);
            for at in (0..=EPISODE_MS + 100).step_by(50) {
                ink.clear();
                card.line_ink_into(Some(t0 + ms(at)), &line, g, true, &mut ink);
                for c in &ink {
                    let rgb = (u32::from(c.color[0]) << 16)
                        | (u32::from(c.color[1]) << 8)
                        | u32::from(c.color[2]);
                    note(rgb, &|| format!("cell {} at {at} ms", c.col));
                }
            }
            if least.0 < 4.5 {
                lows.push(format!(
                    "{palette:?} on {g:06x}: {} at {:.2}:1",
                    least.1, least.0
                ));
            }
        }
    }
    assert!(
        lows.is_empty(),
        "{} grounds under 4.5:1: {lows:#?}",
        lows.len()
    );
    let aerogel = 0x0092_9292;
    let lifted = (0..=255u8)
        .map(|k| {
            contrast(
                CardPalette::Cruise.letter(f32::from(k) / 255.0, true),
                aerogel,
            )
        })
        .fold(f32::MAX, f32::min);
    assert!(
        lifted < 1.5,
        "CONTROL: the lifted letter alone, {lifted:.2}:1"
    );
}

/// The ground of every built-in and pack theme that [`dark_ground`] calls dark, by name.
fn dark_theme_grounds() -> Vec<(String, u32)> {
    use aterm_types::scheme::{builtin, builtin_names, parse_theme_str};
    let rgb = |c: aterm_types::Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
    builtin_names()
        .into_iter()
        .filter_map(|n| Some((n.to_owned(), rgb(builtin(n)?.background))))
        .chain(aterm_theme_pack::themes().iter().map(|t| {
            let parsed = parse_theme_str(t.text).expect("a pack theme parses");
            (t.id.to_owned(), rgb(parsed.scheme.background))
        }))
        .filter(|&(_, g)| dark_ground(g))
        .collect()
}

/// Adversarial grounds for the low-level light-to-print fallback: black reads at least as
/// well as white. These tests deliberately request light-emitting stars on those grounds,
/// independently of the product's polarity decision, to exercise the fallback itself.
fn print_fallback_grounds() -> Vec<u32> {
    let mut grounds: Vec<u32> = (0..=255u32).step_by(3).map(|v| v * 0x0001_0101).collect();
    grounds.extend((0..12u8).map(|k| crate::color_math::hsv2rgb(f32::from(k) * 30.0, 0.6, 0.8)));
    grounds.extend([AEROGEL, UNIKITTY, RASPBERRY]);
    grounds.retain(|&g| contrast(0, g) >= contrast(WHITE, g));
    grounds
}

/// The instants a card's stars run through: the settled frame, the ride with its sparkles and
/// burst every 50 ms, the seated intro's twinkle every 100 ms, and the flow's glints every
/// step for two periods.
fn star_clocks() -> Vec<Clock> {
    let mut v = vec![Clock::SETTLED];
    v.extend((0..=RIDE_MS + BURST_MS).step_by(50).map(Clock::riding));
    v.extend((0..EPISODE_MS).step_by(100).map(Clock::seated));
    v.extend(
        (0..2 * GLINT_PERIOD_MS)
            .step_by(FLOW_STEP_MS as usize)
            .map(Clock::flowing),
    );
    v
}

/// The stars `s` lays at `clock`: as the frame lays them ([`Emit::each_star`]) when `laid`,
/// else as its four kinds give them, a dark scene's light unchecked against its ground.
fn star_calls(s: &CardScene, clock: Clock, laid: bool) -> Vec<StarCall> {
    let geo = Geo::new(s).expect("a scene");
    let cx = Emit::new(s, geo, clock, 7);
    let mut calls = Vec::new();
    let mut lay = |c: StarCall| calls.push(c);
    if laid {
        cx.each_star(&mut lay);
    } else {
        cx.burst_stars(&mut lay);
        cx.glints(&mut lay);
        cx.field(&mut lay);
        cx.shed(&mut lay);
    }
    calls
}

/// The dark law scenes, all three palettes and both kitties, on `ground`.
fn dark_scenes_on(ground: u32) -> Vec<CardScene> {
    scenes()
        .into_iter()
        .filter(|s| s.dark)
        .map(|s| CardScene { ground, ..s })
        .collect()
}

/// The contrast a dark scene's star reads on `ground` at a settled star's strength: its light
/// added at [`STAR_PRINT_FULL`], or its printed ink.
fn star_reads(c: &StarCall, ground: u32) -> f32 {
    if c.light {
        contrast(
            add_sat(ground, premul_rgb(c.color, STAR_PRINT_FULL as u8)),
            ground,
        )
    } else {
        contrast(c.color, ground)
    }
}

/// `l` lightened from `r` along its hue: no channel of `l` under `r`'s, and `r`'s channel
/// order kept (a channel above another in `r` is not below it in `l`).
fn lightened_from(l: u32, r: u32) -> bool {
    let ch = |c: u32| [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff];
    let (l, r) = (ch(l), ch(r));
    (0..3).all(|i| l[i] >= r[i] && (0..3).all(|j| r[i] <= r[j] || l[i] >= l[j]))
}

/// On every dark built-in and pack theme, each star the frame lays is the star its kind gives
/// wherever that light reads [`MARK_CONTRAST`] at a settled star's strength, and otherwise
/// (only Cruise's deep hues) that light lifted along its hue, still added. Negative control:
/// some stars start under the floor, and Silver Aerogel's stars are floored.
#[test]
fn every_dark_builtin_and_pack_theme_keeps_its_stars_or_lifts_them() {
    let grounds = dark_theme_grounds();
    assert!(grounds.len() >= 30, "{} dark themes", grounds.len());
    let clocks = star_clocks();
    let shape = |c: &StarCall| (c.clip, c.centre, c.arm, c.cov, c.gold);
    let (mut stars, mut lifted, mut wrong) = (0usize, 0usize, Vec::new());
    for (name, g) in &grounds {
        for s in dark_scenes_on(*g) {
            for &clock in &clocks {
                let (laid, raw) = (star_calls(&s, clock, true), star_calls(&s, clock, false));
                assert_eq!(laid.len(), raw.len(), "{name} at {clock:?}");
                for (l, r) in laid.iter().zip(&raw) {
                    let right = if star_reads(r, *g) >= MARK_CONTRAST {
                        l == r
                    } else {
                        lifted += 1;
                        s.palette == CardPalette::Cruise
                            && l.light
                            && shape(l) == shape(r)
                            && lightened_from(l.color, r.color)
                            && star_reads(l, *g) >= MARK_CONTRAST
                    };
                    if !right {
                        wrong.push(format!(
                            "{name} {g:06x} {:?} at {clock:?}: {:06x} laid {:06x}",
                            s.palette, r.color, l.color
                        ));
                    }
                }
                stars += laid.len();
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} stars laid wrong: {wrong:#?}",
        wrong.len()
    );
    assert!(stars > 100_000, "CONTROL: {stars} stars compared");
    assert!(
        lifted > 0,
        "NEGATIVE CONTROL: {lifted} stars start under the floor"
    );
    let floored = dark_scenes_on(AEROGEL)
        .iter()
        .flat_map(|s| clocks.iter().map(move |&c| (s, c)))
        .filter(|&(s, c)| star_calls(s, c, true) != star_calls(s, c, false))
        .count();
    assert!(
        floored > 0,
        "NEGATIVE CONTROL: Silver Aerogel's stars are floored"
    );
}

/// On every adversarial print ground ([`print_fallback_grounds`]) each star reads [`MARK_CONTRAST`] at
/// a settled star's strength, and some print; a printed field star still twinkles, its
/// coverage running under and up to [`STAR_PRINT_FULL`], which the settled star prints at.
/// Negative control: the unchecked light misses the floor on Silver Aerogel.
#[test]
fn light_emitting_stars_read_on_print_grounds() {
    let grounds = print_fallback_grounds();
    assert!(grounds.len() >= 20, "{grounds:06x?}");
    let clocks = star_clocks();
    let (mut lows, mut printed) = (Vec::new(), 0usize);
    for &g in &grounds {
        for s in dark_scenes_on(g) {
            for &clock in &clocks {
                for c in star_calls(&s, clock, true) {
                    let k = star_reads(&c, g);
                    if k < MARK_CONTRAST {
                        lows.push(format!(
                            "{:?} on {g:06x} at {clock:?}: {:06x} at {k:.2}:1",
                            s.palette, c.color
                        ));
                    }
                    printed += usize::from(!c.light);
                }
            }
        }
    }
    assert!(
        lows.is_empty(),
        "{} stars under the floor: {lows:#?}",
        lows.len()
    );
    assert!(printed > 0, "CONTROL: {printed} stars printed");
    for s in dark_scenes_on(UNIKITTY) {
        let geo = Geo::new(&s).expect("a scene");
        let field = |clock: Clock| {
            let mut calls = Vec::new();
            Emit::new(&s, geo, clock, 7).field(&mut |c: StarCall| {
                calls.push(c.laid_on(UNIKITTY));
            });
            calls
        };
        let twinkle: Vec<StarCall> = (0..EPISODE_MS)
            .step_by(50)
            .flat_map(|t| field(Clock::seated(t)))
            .collect();
        assert!(
            !twinkle.is_empty() && twinkle.iter().all(|c| !c.light),
            "{:?}: the field prints",
            s.palette
        );
        let (lo, hi) = twinkle.iter().fold((f32::MAX, 0.0f32), |(lo, hi), c| {
            (lo.min(c.cov), hi.max(c.cov))
        });
        assert!(
            lo < STAR_PRINT_FULL && hi >= STAR_PRINT_FULL,
            "{:?}: the printed field twinkles, coverage {lo}..{hi}",
            s.palette
        );
        assert!(
            field(Clock::SETTLED)
                .iter()
                .all(|c| c.cov >= STAR_PRINT_FULL),
            "{:?}: the settled field prints its whole ink",
            s.palette
        );
    }
    let unchecked = dark_scenes_on(AEROGEL)
        .iter()
        .flat_map(|s| clocks.iter().flat_map(|&c| star_calls(s, c, false)))
        .map(|c| star_reads(&c, AEROGEL))
        .fold(f32::MAX, f32::min);
    assert!(
        unchecked < MARK_CONTRAST,
        "NEGATIVE CONTROL: the unchecked light on Silver Aerogel, {unchecked:.2}:1"
    );
}

/// The line's `✦` ([`line_star`]) for every resting letter ink and palette: on every dark
/// built-in and pack theme its light added unchanged; on every mid ground reading
/// [`MARK_CONTRAST`], added or laid over; on paper laid over unchanged. Negative control:
/// added on Silver Aerogel, every letter misses the floor.
#[test]
fn the_lines_star_is_the_cards_mark_on_every_ground() {
    let letters = |g: u32| {
        [CardPalette::Cruise, CardPalette::Land, CardPalette::Off]
            .into_iter()
            .flat_map(move |p| (0..=255u8).map(move |k| p.letter_on(f32::from(k) / 255.0, g)))
    };
    let reads = |(c, blend): (u32, DecoBlend), g: u32| match blend {
        DecoBlend::Add => contrast(add_sat(g, c), g),
        DecoBlend::Over => contrast(c, g),
    };
    let (mut seen, mut changed, mut lows) = (0usize, Vec::new(), Vec::new());
    for (name, g) in dark_theme_grounds() {
        for c in letters(g) {
            let laid = line_star(c, g);
            if laid != (c, DecoBlend::Add) {
                changed.push(format!("{name} {g:06x}: {c:06x} laid {laid:?}"));
            }
            seen += 1;
        }
    }
    for g in print_fallback_grounds() {
        for c in letters(g) {
            let k = reads(line_star(c, g), g);
            if k < MARK_CONTRAST {
                lows.push(format!("{g:06x}: {c:06x} at {k:.2}:1"));
            }
        }
    }
    assert!(
        changed.is_empty(),
        "{} changed: {changed:#?}",
        changed.len()
    );
    assert!(seen >= 30 * 3 * 256, "CONTROL: {seen} dark-theme letters");
    assert!(lows.is_empty(), "{} under the floor: {lows:#?}", lows.len());
    assert!(
        letters(DAY).all(|c| line_star(c, DAY) == (c, DecoBlend::Over)),
        "on paper, laid over unchanged"
    );
    let added = letters(AEROGEL)
        .map(|c| reads((c, DecoBlend::Add), AEROGEL))
        .fold(0.0f32, f32::max);
    assert!(
        added < MARK_CONTRAST,
        "NEGATIVE CONTROL: added on Silver Aerogel, at best {added:.2}:1"
    );
}

/// On [`SCARLET`], a dim ground where white reads better than black but no added light reads
/// the floor, every star the frame lays is printed and reads [`MARK_CONTRAST`]. Negative
/// control: white light added there at a settled star's strength misses the floor.
#[test]
fn the_stars_print_where_no_light_reads() {
    let white = add_sat(SCARLET, premul_rgb(WHITE, STAR_PRINT_FULL as u8));
    assert!(
        contrast(WHITE, SCARLET) > contrast(0, SCARLET) && contrast(white, SCARLET) < MARK_CONTRAST,
        "NEGATIVE CONTROL: white light on scarlet reads {:.2}:1",
        contrast(white, SCARLET)
    );
    let clocks = star_clocks();
    let (mut stars, mut wrong) = (0usize, Vec::new());
    for s in dark_scenes_on(SCARLET) {
        for &clock in &clocks {
            for c in star_calls(&s, clock, true) {
                let k = star_reads(&c, SCARLET);
                if c.light || k < MARK_CONTRAST {
                    wrong.push(format!(
                        "{:?} at {clock:?}: {:06x} light {} at {k:.2}:1",
                        s.palette, c.color, c.light
                    ));
                }
                stars += 1;
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} stars laid wrong: {wrong:#?}",
        wrong.len()
    );
    assert!(stars > 1_000, "CONTROL: {stars} stars");
}

/// On every adversarial print ground the glints ([`Emit::laid_glints`]) are all added light
/// where their rest light is ([`dark_mark`]), else all print the rest's floored ink, both
/// seen. Negative control: on [`RASPBERRY`], glints laid frame by frame on their flared light
/// ([`StarCall::laid_on`]) take both polarities.
#[test]
fn a_glint_keeps_one_polarity_through_its_flare() {
    let clocks = star_clocks();
    let glints = |s: &CardScene, clock: Clock, laid: bool| {
        let cx = Emit::new(s, Geo::new(s).expect("a scene"), clock, 7);
        let mut calls = Vec::new();
        if laid {
            cx.laid_glints(&mut |c: StarCall| calls.push(c));
        } else {
            cx.glints(&mut |c: StarCall| calls.push(c.laid_on(s.ground)));
        }
        calls
    };
    let (mut seen, mut printed, mut wrong) = (0usize, 0usize, Vec::new());
    for g in print_fallback_grounds() {
        for s in dark_scenes_on(g) {
            let rest = dark_mark(s.palette.glint_rest(), g);
            for &clock in &clocks {
                for c in glints(&s, clock, true) {
                    let right = match rest {
                        DarkMark::Light(_) => c.light,
                        DarkMark::Print(ink) => !c.light && c.color == ink,
                    };
                    if !right {
                        wrong.push(format!("{:?} {g:06x} {clock:?}: {c:?}", s.palette));
                    }
                    seen += 1;
                    printed += usize::from(!c.light);
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} glints laid wrong: {wrong:#?}",
        wrong.len()
    );
    assert!(
        printed > 0 && printed < seen,
        "CONTROL: {printed} of {seen} glints printed"
    );
    let mut polarities = [false; 2];
    for s in dark_scenes_on(RASPBERRY) {
        for &clock in &clocks {
            for c in glints(&s, clock, false) {
                polarities[usize::from(c.light)] = true;
            }
        }
    }
    assert_eq!(
        polarities, [true; 2],
        "NEGATIVE CONTROL: on raspberry, glints laid frame by frame, printed and light"
    );
}

#[test]
fn serious_mode_is_a_static_border() {
    for s in scenes() {
        let s = CardScene { serious: true, ..s };
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 9);
        let first = frame(&mut card, t0 + ms(40), &s, true);
        assert_eq!(
            card.deadline(t0 + ms(40)),
            None,
            "serious mode ends the episode"
        );
        assert!(first.1.is_empty(), "no kitty, no stars");
        let t = i64::from((s.ch / 14).max(1));
        let cw = i64::from(s.cw);
        let bx0 = (i64::from(s.origin_x) - i64::from(s.pad_left).min(cw / 2)).max(0);
        let bx1 = i64::from(s.origin_x) + (i64::from(s.cols) + 1) * cw;
        let by0 = i64::from(s.origin_y) + i64::from(s.top) * i64::from(s.ch);
        let by1 = by0 + i64::from(s.rows) * i64::from(s.ch);
        for q in &first.0 {
            let (x0, y0) = (i64::from(q.x), i64::from(q.y));
            let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
            let on_rim = y1 <= by0 + t || y0 >= by1 - t || x1 <= bx0 + t || x0 >= bx1 - t;
            assert!(on_rim, "a serious quad off the rim: {q:?}");
        }
        assert!(!first.0.is_empty(), "the border stays");
        assert_eq!(frame(&mut card, t0 + ms(900), &s, true), first, "static");
    }
}

#[test]
fn reduced_motion_is_the_settled_frame_from_the_start() {
    for s in scenes() {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        assert!(
            card.deadline(t0).is_some(),
            "an armed card asks for its frame"
        );
        let still = frame(&mut card, t0 + ms(30), &s, false);
        assert_eq!(still, settled(&s), "animate=false is the settled frame");
        assert_eq!(card.deadline(t0 + ms(30)), None);
        for at in [400, 2000, EPISODE_MS + 500] {
            assert_eq!(frame(&mut card, t0 + ms(at), &s, false), still, "{at} ms");
            assert_eq!(card.deadline(t0 + ms(at)), None, "{at} ms");
        }
    }
}

#[test]
fn an_episode_held_back_before_it_played_is_owed() {
    for held in [2000, EPISODE_MS + 3000] {
        let s = scene();
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        let mut at = 0;
        while at <= held {
            let still = frame(&mut card, t0 + ms(at), &s, false);
            assert_eq!(still, settled(&s), "held {held}: {at} ms");
            at += 100;
        }
        let go = t0 + ms(held + 50);
        let (_, first, fp) = frame(&mut card, go, &s, true);
        assert_eq!(card.deadline(go), Some(go + ms(FRAME_MS)), "held {held}");
        let (_, rest, rest_fp) = settled(&s);
        assert_ne!(fp, rest_fp, "held {held}: the episode plays");
        assert_eq!(
            kitty_left(&first),
            kitty_left(&rest),
            "held {held}: the kitty stays in the seat the glass showed"
        );
        assert!(first.len() > rest.len(), "held {held}: the burst flies");
        let mut shown = shown_at_rest(&s, go);
        shown.arm(go, 7);
        assert_eq!(frame(&mut shown, go, &s, true).1, first, "held {held}");
        let late = go + ms(EPISODE_MS - FRAME_MS);
        frame(&mut card, late, &s, true);
        assert!(card.intro_live(), "held {held}: the intro still runs");
        let done = go + ms(EPISODE_MS);
        assert_ne!(frame(&mut card, done, &s, true).2, rest_fp, "held {held}");
        assert!(
            !card.intro_live() && card.flowing(done),
            "held {held}: then the flow"
        );
    }
}

#[test]
fn a_playing_episode_held_back_keeps_its_clock() {
    let s = scene();
    let t0 = Instant::now();
    let (_, rest, rest_fp) = settled(&s);
    let mut card = HarnessCard::default();
    card.arm(t0, 7);
    frame(&mut card, t0, &s, true);
    let held = frame(&mut card, t0 + ms(1000), &s, false);
    assert_eq!(held.2, rest_fp, "held back: the settled frame");
    assert_eq!(card.deadline(t0 + ms(1000)), None);
    let mut twin = shown_at_rest(&s, t0);
    twin.arm(t0, 7);
    assert_eq!(
        frame(&mut card, t0 + ms(2000), &s, true),
        frame(&mut twin, t0 + ms(2000), &s, true),
        "resumed on the arm's clock"
    );
    frame(&mut card, t0 + ms(2100), &s, false);
    let past = t0 + ms(EPISODE_MS + 100);
    let (_, after, fp) = frame(&mut card, past, &s, true);
    assert!(
        !card.intro_live() && card.flowing(past),
        "past its end it flows"
    );
    assert_ne!(fp, rest_fp, "a flow frame");
    assert_eq!(after, rest, "from the settled colours");
    let step = (EPISODE_MS + 100) / FLOW_STEP_MS;
    assert_eq!(
        card.deadline(past),
        Some(t0 + ms((step + 1) * FLOW_STEP_MS + FLOW_SLACK_MS))
    );
}

#[test]
fn the_flows_first_frame_is_asked_for_until_it_is_drawn() {
    let s = scene();
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    card.arm(t0, 7);
    let last = t0 + ms(EPISODE_MS - 10);
    frame(&mut card, last, &s, true);
    assert_eq!(card.deadline(last), Some(last + ms(FRAME_MS)));
    let past = t0 + ms(EPISODE_MS + 250);
    assert_eq!(
        card.deadline(past),
        Some(past),
        "the flow's first frame is owed"
    );
    assert_eq!(card.deadline(past), Some(past), "asked until drawn");
    let (_, _, fp) = frame(&mut card, past, &s, true);
    assert_ne!(fp, settled(&s).2, "a flow frame");
    let k = (EPISODE_MS + 250) / FLOW_STEP_MS;
    let next = t0 + ms((k + 1) * FLOW_STEP_MS + FLOW_SLACK_MS);
    assert_eq!(card.deadline(past), Some(next), "drawn: the next step");
    let mut held = HarnessCard::default();
    held.arm(t0, 7);
    frame(&mut held, t0, &s, true);
    frame(&mut held, t0 + ms(100), &s, false);
    assert_eq!(held.deadline(past), None);
}

#[test]
fn an_unticked_card_stops_asking_after_the_grace() {
    let s = scene();
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    card.arm(t0, 7);
    let mut at = 0;
    let mut last = 0;
    while at < EPISODE_MS {
        frame(&mut card, t0 + ms(at), &s, true);
        last = at;
        at += FRAME_MS;
    }
    let end = t0 + ms(EPISODE_MS);
    assert_eq!(
        card.deadline(end),
        Some(end),
        "the flow's first frame is owed"
    );
    let asked: Vec<u64> = (0..10_000)
        .filter(|&k| card.deadline(end + ms(k)).is_some())
        .collect();
    assert_eq!(
        asked,
        (0..last + SETTLE_GRACE_MS - EPISODE_MS).collect::<Vec<_>>(),
        "asked only inside the grace"
    );
    assert_eq!(card.deadline(t0 + ms(last + SETTLE_GRACE_MS)), None);
    let late = end + ms(60_000);
    assert_ne!(frame(&mut card, late, &s, true).2, settled(&s).2);
    assert!(card.flowing(late) && card.deadline(late).is_some_and(|d| d > late));

    let mut rearmed = HarnessCard::default();
    rearmed.arm(t0, 7);
    frame(&mut rearmed, t0, &s, true);
    let t1 = t0 + ms(1000);
    rearmed.arm(t1, 7);
    assert_eq!(rearmed.deadline(t1), Some(t1 + ms(FRAME_MS)));
    let gone = t1 + ms(EPISODE_MS);
    assert_eq!(rearmed.deadline(gone), None, "no frame of its own drawn");
}

#[test]
fn an_unticked_flowing_card_stops_asking_after_the_grace() {
    let s = scene();
    let t0 = Instant::now();
    let mut card = played(&s, t0);
    let drawn = t0 + ms(EPISODE_MS + 300);
    frame(&mut card, drawn, &s, true);
    let step = |at: u64| at / FLOW_STEP_MS;
    let next = |k: u64| t0 + ms((k + 1) * FLOW_STEP_MS + FLOW_SLACK_MS);
    let k = step(EPISODE_MS + 300);
    let asked: Vec<u64> = (0..10_000)
        .filter(|&j| card.deadline(drawn + ms(j)).is_some())
        .collect();
    assert_eq!(asked, (0..SETTLE_GRACE_MS).collect::<Vec<_>>(), "the grace");
    assert!(
        (0..SETTLE_GRACE_MS).all(|j| card.deadline(drawn + ms(j)) == Some(next(k))),
        "one fixed instant"
    );
    let late = drawn + ms(60_000);
    frame(&mut card, late, &s, true);
    assert_eq!(card.deadline(late), Some(next(step(EPISODE_MS + 60_300))));
}

#[test]
fn nothing_to_draw_holds_the_episode_back() {
    let s = scene();
    let (_, rest, _) = settled(&s);
    for (what, blank) in [
        (
            "an inverted clip",
            CardScene {
                clip: (4000, 0, 0, 4000),
                ..s
            },
        ),
        (
            "an empty clip",
            CardScene {
                clip: (0, 0, 0, 0),
                ..s
            },
        ),
        ("no columns", CardScene { cols: 0, ..s }),
        ("no rows", CardScene { rows: 0, ..s }),
        ("no cell size", CardScene { cw: 0, ..s }),
    ] {
        for later in [2000, EPISODE_MS + 1000] {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            for at in [0, 500, 1000] {
                let now = t0 + ms(at);
                assert_eq!(
                    frame(&mut card, now, &blank, true),
                    (Vec::new(), Vec::new(), 0),
                    "{what}: {at} ms"
                );
                assert_eq!(card.deadline(now), None, "{what}: owed, no wake");
            }
            let go = t0 + ms(later);
            let drawn = frame(&mut card, go, &s, true);
            let mut fresh = HarnessCard::default();
            fresh.arm(go, 7);
            assert_eq!(drawn, frame(&mut fresh, go, &s, true), "{what}: {later}");
            assert_eq!(
                kitty_left(&drawn.1),
                Some(ride_from(&s)),
                "{what}: {later}: the ride starts"
            );
            assert_ne!(kitty_left(&drawn.1), kitty_left(&rest), "{what}: {later}");
            assert_eq!(card.deadline(go), Some(go + ms(FRAME_MS)), "{what}");
        }
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, &s, true);
        assert_eq!(frame(&mut card, t0 + ms(1000), &blank, true).2, 0, "{what}");
        assert_eq!(card.deadline(t0 + ms(1000)), None, "{what}");
        let mut twin = HarnessCard::default();
        twin.arm(t0, 7);
        assert_eq!(
            frame(&mut card, t0 + ms(2000), &s, true),
            frame(&mut twin, t0 + ms(2000), &s, true),
            "{what}: resumed on the arm's clock"
        );
        let done = t0 + ms(EPISODE_MS + 100);
        assert_ne!(frame(&mut card, done, &s, true).2, 0, "{what}");
        assert!(card.flowing(done), "{what}: the flow takes over");
    }
}

#[test]
fn an_owed_episode_lapses_after_its_dwell() {
    let s = scene();
    let (_, rest_over, rest_fp) = settled(&s);
    let blank = CardScene { cols: 0, ..s };
    for (held_by, animate, by) in [("reduced", false, &s), ("nothing to draw", true, &blank)] {
        for (at, plays) in [
            (OWED_MAX_MS - 1, true),
            (OWED_MAX_MS, false),
            (OWED_MAX_MS + 3_600_000, false),
        ] {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            frame(&mut card, t0 + ms(10), by, animate);
            let now = t0 + ms(at);
            let (_, over, fp) = frame(&mut card, now, &s, true);
            let what = format!("{held_by}: paid at {at} ms");
            assert_ne!(fp, rest_fp, "{what}: it moves either way");
            assert_eq!(card.intro_live(), plays, "{what}");
            let want = if plays {
                now + ms(FRAME_MS)
            } else {
                t0 + ms((at / FLOW_STEP_MS + 1) * FLOW_STEP_MS + FLOW_SLACK_MS)
            };
            assert_eq!(card.deadline(now), Some(want), "{what}");
            if !plays {
                assert_eq!(over, rest_over, "{what}: no burst, the settled colours");
                let after = now + ms(1000);
                frame(&mut card, after, &s, true);
                assert!(!card.intro_live() && card.flowing(after), "{what}");
                card.arm(after, 7);
                frame(&mut card, after, &s, true);
                assert!(card.intro_live(), "{what}: a fresh episode");
            }
        }
    }
}

#[test]
fn an_episode_owed_through_the_latchs_longest_shed_still_plays() {
    const LATCH_DWELL_MAX_MS: u64 = 30_000;
    let s = scene();
    let (_, rest, rest_fp) = settled(&s);
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    card.arm(t0, 7);
    let mut at = 0;
    while at < LATCH_DWELL_MAX_MS {
        assert_eq!(frame(&mut card, t0 + ms(at), &s, false).2, rest_fp);
        at += 1000;
    }
    let restored = t0 + ms(LATCH_DWELL_MAX_MS + 3 * FRAME_MS);
    let (_, over, fp) = frame(&mut card, restored, &s, true);
    assert_ne!(fp, rest_fp, "the show plays");
    assert_eq!(
        kitty_left(&over),
        kitty_left(&rest),
        "in the seat it was shown in"
    );
    assert!(over.len() > rest.len(), "the burst flies");
    assert_eq!(card.deadline(restored), Some(restored + ms(FRAME_MS)));
}

fn trail_reach(under: &[GlowQuad], s: &CardScene) -> Option<(i64, i64)> {
    let inner = Geo::new(s).expect("a scene").inner();
    let inside = under.iter().filter(|q| {
        let (x, y) = (i64::from(q.x), i64::from(q.y));
        x >= inner.x0
            && y >= inner.y0
            && x + i64::from(q.w) <= inner.x1
            && y + i64::from(q.h) <= inner.y1
    });
    let x0 = inside.clone().map(|q| i64::from(q.x)).min()?;
    let x1 = inside.map(|q| i64::from(q.x) + i64::from(q.w)).max()?;
    Some((x0, x1))
}

fn whole(s: &CardScene, clock: Clock) -> (Vec<GlowQuad>, Vec<GlowQuad>) {
    let p = parts(s, clock);
    let under = [p.border, p.trail].concat();
    let over = [p.kitty, p.rim, p.burst, p.glints, p.stars, p.sparkles].concat();
    (under, over)
}

#[test]
fn a_kitty_shown_seated_rides_no_more() {
    for kitty_rows in [2u8, 3] {
        let s = CardScene {
            kitty_rows,
            ..scene()
        };
        let what = format!("{kitty_rows} rows");
        let (rest_under, rest, _) = settled(&s);
        let (seat, reach) = (kitty_left(&rest), trail_reach(&rest_under, &s));
        assert!(seat.is_some() && reach.is_some(), "{what}");
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        let (under, over, _) = frame(&mut card, t0, &s, true);
        assert_eq!(kitty_left(&over), Some(ride_from(&s)), "{what}: control");
        assert_ne!(trail_reach(&under, &s), reach, "{what}: control");
        frame(&mut card, t0 + ms(EPISODE_MS + 10), &s, true);
        let t1 = t0 + ms(EPISODE_MS + 5000);
        card.arm(t1, 7);
        let first = frame(&mut card, t1, &s, true);
        assert_eq!(
            (first.0.clone(), first.1.clone()),
            whole(&s, Clock::seated(0)),
            "{what}: the seated episode's first frame"
        );
        assert!(
            !parts(&s, Clock::seated(0)).burst.is_empty(),
            "{what}: burst"
        );
        for t in [FRAME_MS, RIDE_MS / 2, RIDE_MS, 3000] {
            let (under, over, _) = frame(&mut card, t1 + ms(t), &s, true);
            assert_eq!(kitty_left(&over), seat, "{what}: {t} ms, seated");
            assert_eq!(trail_reach(&under, &s), reach, "{what}: {t} ms, whole");
        }
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, &s, true);
        frame(&mut card, t0 + ms(300), &s, false);
        card.arm(t0 + ms(400), 7);
        for t in [400, 500, 900] {
            let (under, over, _) = frame(&mut card, t0 + ms(t), &s, true);
            assert_eq!(kitty_left(&over), seat, "{what}: resumed at {t} ms");
            assert_eq!(trail_reach(&under, &s), reach, "{what}: resumed at {t} ms");
        }
    }
}

#[test]
fn a_ride_armed_again_on_its_way_carries_on() {
    for kitty_rows in [2u8, 3] {
        let s = CardScene {
            kitty_rows,
            ..scene()
        };
        let what = format!("{kitty_rows} rows");
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, &s, true);
        let re = 500;
        frame(&mut card, t0 + ms(re), &s, true);
        card.arm(t0 + ms(re), 7);
        let mut last = i64::MIN;
        for t in (re..RIDE_MS + BURST_MS).step_by(FRAME_MS as usize) {
            let (under, over) = {
                let f = frame(&mut card, t0 + ms(t), &s, true);
                (f.0, f.1)
            };
            let clock = Clock::intro(t - re, Some(t));
            assert_eq!((under, over.clone()), whole(&s, clock), "{what}: {t} ms");
            let x0 = kitty_left(&over).expect("a kitty");
            assert!(x0 >= last, "{what}: {t} ms, back from {last} to {x0}");
            last = x0;
        }
        assert_ne!(
            whole(&s, Clock::riding(0)).1,
            whole(&s, Clock::riding(re)).1,
            "{what}: control"
        );
        assert!(
            !parts(&s, Clock::intro(RIDE_MS + 10 - re, Some(RIDE_MS + 10)))
                .burst
                .is_empty(),
            "{what}: the burst where the ride ends"
        );
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, &s, true);
        frame(&mut card, t0 + ms(2500), &s, true);
        let t1 = t0 + ms(2500);
        card.arm(t1, 7);
        let (under, over, _) = frame(&mut card, t1, &s, true);
        assert_eq!(
            (under, over),
            whole(&s, Clock::seated(0)),
            "{what}: run out"
        );
    }
}

#[test]
fn the_kitty_rides_in_along_the_trail() {
    for kitty_rows in [2u8, 3] {
        for dark in [true, false] {
            let s = CardScene {
                kitty_rows,
                dark,
                ..scene()
            };
            let what = format!("{kitty_rows} rows, dark {dark}");
            let g = Geo::new(&s).expect("a scene");
            let seat = kitty_rect(&g, &s).expect("a kitty");
            let (from, dist) = ride_span(&g, &s, &seat).expect("a ride");
            assert_eq!(from + dist, seat.rect.x0, "{what}");
            assert_eq!(s.trail.expect("a trail").1, s.kitty_col.expect("a kitty"));
            let trail_end = seat.rear();
            let inks = KittyInk::new(s.coat, s.eye, s.dark);
            let art = |f: &[&str]| f.iter().map(|r| (*r).to_string()).collect::<Vec<_>>();
            let mut last = i64::MIN;
            for t in (0..RIDE_MS + 4 * FRAME_MS).step_by(10) {
                let clock = Clock::riding(t);
                let p = parts(&s, clock);
                let x0 = kitty_left(&p.kitty).expect("a kitty drawn");
                assert!(x0 >= last, "{what}: {t} ms, back from {last} to {x0}");
                last = x0;
                let at = ridden(&g, &s, seat, clock);
                assert_eq!(at.rect.y0, seat.rect.y0, "{what}: on the seat's rows");
                assert_eq!(
                    drawn_kitty(&p.kitty, &at, inks),
                    art(at.art.frame(kitty_frame(clock))),
                    "{what}: {t} ms, whole where the ride has it"
                );
                for q in p.kitty.iter().chain(&p.rim).chain(&p.trail) {
                    assert_in_band_and_box(q, &s, &what);
                }
                let laid = p
                    .trail
                    .iter()
                    .map(|q| i64::from(q.x) + i64::from(q.w))
                    .max()
                    .unwrap_or(from);
                if t >= RIDE_MS {
                    assert_eq!(x0, seat.rect.x0, "{what}: {t} ms, seated");
                    assert_eq!(laid, trail_end, "{what}: {t} ms, the whole trail");
                } else {
                    assert_eq!(
                        laid,
                        trail_end.min(at.rear()),
                        "{what}: {t} ms, to its rear"
                    );
                }
            }
            let at = |t: u64| kitty_left(&parts(&s, Clock::riding(t)).kitty);
            assert_eq!(at(0), Some(from), "{what}: from the trail's start");
            let half = at(RIDE_MS / 2).expect("a kitty") - from;
            assert!(
                half * 8 >= dist * 7 - 8,
                "{what}: {half} of {dist} at half time"
            );
            assert!(at(RIDE_MS / 4).expect("a kitty") < seat.rect.x0, "{what}");
            assert_eq!(at(RIDE_MS), Some(seat.rect.x0), "{what}");
            assert_eq!(
                kitty_left(&parts(&s, Clock::SETTLED).kitty),
                Some(seat.rect.x0),
                "{what}: settled, seated"
            );
        }
    }
    let base = scene();
    let seat = kitty_rect(&Geo::new(&base).expect("a scene"), &base)
        .expect("a kitty")
        .rect
        .x0;
    for (name, s) in [
        (
            "no trail",
            CardScene {
                trail: None,
                ..base
            },
        ),
        (
            "a trail from the kitty",
            CardScene {
                trail: Some((40, 44)),
                ..base
            },
        ),
    ] {
        let g = Geo::new(&s).expect("a scene");
        assert!(
            ride_span(&g, &s, &kitty_rect(&g, &s).expect("a kitty")).is_none(),
            "{name}"
        );
        assert_eq!(
            kitty_left(&parts(&s, Clock::riding(0)).kitty),
            Some(seat),
            "{name}: seated at once"
        );
    }
}

#[test]
fn the_field_and_the_sparkles_come_up_behind_the_kitty() {
    for kitty_rows in [2u8, 3] {
        for dark in [true, false] {
            let s = CardScene {
                kitty_rows,
                dark,
                ..scene()
            };
            let what = format!("{kitty_rows} rows, dark {dark}");
            let g = Geo::new(&s).expect("a scene");
            let seat = kitty_rect(&g, &s).expect("a kitty");
            let region = star_region(&g, &s).expect("a star region");
            let reach = STAR_RATIOS
                .iter()
                .map(|r| i64::from(star_arm_px(star_arm(s.ch as f32, *r))))
                .max()
                .unwrap_or(0)
                + 1;
            let mut shed = 0;
            for t in (0..RIDE_MS + SHED_MS + 4 * FRAME_MS).step_by(10) {
                let clock = Clock::riding(t);
                let p = parts(&s, clock);
                let at = ridden(&g, &s, seat, clock).rect;
                for q in &p.stars {
                    let (x0, y0) = (i64::from(q.x), i64::from(q.y));
                    let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
                    let on = x0 < at.x1 && at.x0 < x1 && y0 < at.y1 && at.y0 < y1;
                    assert!(!on, "{what}: {t} ms, a star on the kitty {q:?}");
                    let ahead = x0 < seat.rect.x0 - reach && x1 > at.x0;
                    assert!(!ahead, "{what}: {t} ms, a star ahead of the kitty {q:?}");
                }
                let laid = trail_reach(&p.trail, &s).map_or(i64::MIN, |r| r.1);
                for q in &p.glints {
                    let x1 = i64::from(q.x) + i64::from(q.w);
                    assert!(
                        x1 <= at.x0,
                        "{what}: {t} ms, a glint ahead of the kitty {q:?}"
                    );
                    assert!(
                        x1 <= laid,
                        "{what}: {t} ms, a glint past the trail laid {q:?}"
                    );
                }
                if t >= RIDE_MS {
                    assert!(p.sparkles.is_empty(), "{what}: {t} ms, the sparkles gone");
                }
                shed += p.sparkles.len();
                for q in &p.sparkles {
                    assert_in_band_and_box(q, &s, &what);
                    let (x0, y0) = (i64::from(q.x), i64::from(q.y));
                    let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
                    assert!(x1 <= at.x0, "{what}: {t} ms, behind the kitty {q:?}");
                    assert!(
                        region.x0 <= x0 && x1 <= region.x1 && region.y0 <= y0 && y1 <= region.y1,
                        "{what}: {t} ms, in the star region {q:?}"
                    );
                }
            }
            assert!(shed > 0, "{what}: sparkles shed");
            let left_of_seat = |p: &Parts| {
                p.stars
                    .iter()
                    .filter(|q| i64::from(q.x) + i64::from(q.w) < seat.rect.x0 - reach)
                    .count()
            };
            let up = parts(&s, Clock::riding(RIDE_MS + STAR_REVEAL_MS as u64));
            let settled = parts(&s, Clock::SETTLED);
            assert!(
                left_of_seat(&settled) > 0,
                "{what}: a field left of the seat"
            );
            assert_eq!(left_of_seat(&parts(&s, Clock::riding(0))), 0, "{what}");
            assert!(left_of_seat(&up) > 0, "{what}: up after the ride");
            assert!(
                parts(&s, Clock::riding(0)).glints.is_empty(),
                "{what}: glints"
            );
            assert!(!up.glints.is_empty(), "{what}: glints up after the ride");
            assert!(settled.sparkles.is_empty(), "{what}: settled, none shed");
        }
    }
    let still = CardScene {
        trail: None,
        ..scene()
    };
    for t in (0..RIDE_MS + SHED_MS).step_by(10) {
        assert!(
            parts(&still, Clock::riding(t)).sparkles.is_empty(),
            "{t} ms"
        );
    }
}

#[test]
fn the_ride_ends_on_the_settled_frame() {
    for kitty_rows in [2u8, 3] {
        for dark in [true, false] {
            let s = CardScene {
                kitty_rows,
                dark,
                ..scene()
            };
            let what = format!("{kitty_rows} rows, dark {dark}");
            let rest = parts(&s, Clock::SETTLED);
            let rects = |qs: &[GlowQuad]| {
                let mut v: Vec<_> = qs.iter().map(|q| (q.x, q.y, q.w, q.h)).collect();
                v.sort_unstable();
                v
            };
            let span = |qs: &[GlowQuad]| {
                (
                    qs.iter().map(|q| q.x).min(),
                    qs.iter().map(|q| i64::from(q.x) + i64::from(q.w)).max(),
                )
            };
            let g = Geo::new(&s).expect("a scene");
            let seat = kitty_rect(&g, &s).expect("a kitty");
            let ring = rim_ring(
                &seat,
                seat.art.open,
                rim_px(seat.p, g.t),
                g.inner().meet(g.clip),
            );
            let lit = |qs: &[GlowQuad]| {
                coverage(qs)
                    .into_keys()
                    .collect::<std::collections::BTreeSet<_>>()
            };
            for t in (RIDE_MS..BLINK_AT_MS).step_by(10) {
                let p = parts(&s, Clock::riding(t));
                assert_eq!(p.kitty, rest.kitty, "{what}: {t} ms");
                let trail = lit(&p.trail);
                let off: std::collections::BTreeSet<_> = ring.difference(&trail).copied().collect();
                assert_eq!(lit(&p.rim), off, "{what}: {t} ms");
                assert_eq!(span(&p.trail), span(&rest.trail), "{what}: {t} ms");
            }
            let still = parts(&s, Clock::riding(EPISODE_MS - 100));
            assert_eq!(lit(&still.rim), lit(&rest.rim), "{what}: the wave still");
            assert_eq!(rects(&still.kitty), rects(&rest.kitty), "{what}");
            let start = parts(&s, Clock::riding(0));
            assert_ne!(start.kitty, rest.kitty, "{what}");
        }
    }
}

#[test]
fn the_burst_opens_where_the_ride_ends_and_ends() {
    for kitty_rows in [2u8, 3] {
        let s = CardScene {
            kitty_rows,
            ..scene()
        };
        let g = Geo::new(&s).expect("a scene");
        let seat = kitty_rect(&g, &s).expect("a kitty").rect;
        let burst = |t: u64| parts(&s, Clock::riding(t)).burst;
        for t in (0..RIDE_MS).step_by(FRAME_MS as usize) {
            assert!(
                burst(t).is_empty(),
                "{kitty_rows} rows: none on the ride, {t} ms"
            );
        }
        for t in (RIDE_MS..RIDE_MS + BURST_MS).step_by(FRAME_MS as usize) {
            let b = burst(t);
            if t < RIDE_MS + BURST_MS / 2 {
                assert!(b.len() >= BURST_STARS * 3, "{kitty_rows} rows: {t} ms");
            } else if b.is_empty() {
                continue;
            }
            let (x0, x1) = (
                b.iter().map(|q| i64::from(q.x)).min().unwrap_or(0),
                b.iter()
                    .map(|q| i64::from(q.x) + i64::from(q.w))
                    .max()
                    .unwrap_or(0),
            );
            let cx = (seat.x0 + seat.x1) / 2;
            assert!(
                x0 < cx && cx < x1,
                "{kitty_rows} rows: {t} ms round the seat"
            );
        }
        assert!(
            burst(RIDE_MS + BURST_MS).is_empty(),
            "{kitty_rows} rows: gone"
        );
        assert!(parts(&s, Clock::SETTLED).burst.is_empty(), "settled: none");
        let (_, after, _) = {
            let t0 = Instant::now();
            let mut card = HarnessCard::default();
            card.arm(t0, 7);
            frame(&mut card, t0 + ms(RIDE_MS + BURST_MS + SHED_MS), &s, true)
        };
        let (_, rest, _) = settled(&s);
        assert!(
            after.len() <= rest.len() + STARS_MAX * 4,
            "after the burst only the field is left"
        );
    }
    let still = CardScene {
        trail: None,
        ..scene()
    };
    assert!(!parts(&still, Clock::riding(0)).burst.is_empty());
    assert!(parts(&still, Clock::riding(BURST_MS)).burst.is_empty());
}

#[test]
fn the_field_is_five_to_nine_stars_clear_of_text_and_kitty() {
    let mut counts = std::collections::BTreeSet::new();
    for seed in 0..64 {
        for s in [
            scene(),
            CardScene {
                dark: false,
                ..scene()
            },
        ] {
            let g = Geo::new(&s).expect("a scene");
            let (spots, n) = field_spots(&g, &s, seed, !s.dark);
            counts.insert(n);
            let region = star_region(&g, &s).expect("a region");
            let kitty = kitty_rect(&g, &s).expect("a kitty").rect;
            let text_end = g.col(s.trail.expect("trail").0);
            for p in &spots[..n] {
                assert!(
                    region.contains(p.x, p.y),
                    "a star outside its region: {p:?}"
                );
                assert!(p.x >= text_end, "a star over the card's text: {p:?}");
                assert!(!kitty.contains(p.x, p.y), "a star on the kitty: {p:?}");
            }
        }
    }
    assert!(
        counts.iter().all(|n| (5..=9).contains(n)),
        "counts {counts:?}"
    );
    assert!(counts.len() > 1, "the seed varies the count: {counts:?}");
}

fn glint_scenes() -> Vec<CardScene> {
    let mut v = scenes();
    for (cw, ch) in [(9u16, 18u16), (16, 33), (17, 35)] {
        for kitty_rows in [2u8, 3] {
            for dark in [true, false] {
                v.push(CardScene {
                    cw,
                    ch,
                    kitty_rows,
                    dark,
                    trail: Some((32, 52)),
                    kitty_col: Some(52),
                    cols: 63,
                    ..scene()
                });
            }
        }
    }
    v
}

fn rect_of(q: &GlowQuad) -> Rect {
    Rect {
        x0: i64::from(q.x),
        y0: i64::from(q.y),
        x1: i64::from(q.x) + i64::from(q.w),
        y1: i64::from(q.y) + i64::from(q.h),
    }
}

#[test]
fn the_rainbows_glints_lie_on_its_stripes() {
    let mut counts = std::collections::BTreeSet::new();
    let mut laid = 0;
    for s in glint_scenes() {
        let what = format!("{}x{} {} rows dark {}", s.cw, s.ch, s.kitty_rows, s.dark);
        let g = Geo::new(&s).expect("a scene");
        let seat = kitty_rect(&g, &s);
        let glints = glint_spots(&g, &s, 7);
        let Some((t, amp)) = trail_rect(&g, &s, seat) else {
            assert_eq!(glints.n, 0, "{what}: no trail, no glints");
            continue;
        };
        let free = seat.map_or(t.x1, |at| at.rect.x0.min(t.x1));
        if glints.n > 0 {
            let cells = (free - t.x0) / g.cw;
            let want = usize::try_from(cells / GLINT_CELLS).expect("cells");
            assert_eq!(glints.n, want.clamp(1, GLINTS_MAX), "{what}: the count");
            assert!(glints.rest >= 1 && glints.rest <= glints.flare, "{what}");
        }
        counts.insert(glints.n);
        let band = Rect {
            x0: t.x0.max(g.col(s.trail.expect("a trail").0)),
            y0: t.y0 + amp,
            x1: free,
            y1: t.y1 - amp,
        };
        let clocks = std::iter::once(Clock::SETTLED).chain(
            (0..EPISODE_MS)
                .step_by(FRAME_MS as usize)
                .flat_map(|t| [Clock::riding(t), Clock::seated(t)]),
        );
        for clock in clocks {
            let p = parts(&s, clock);
            if clock.t_ms.is_none() {
                assert_eq!(
                    p.glints.is_empty(),
                    glints.n == 0,
                    "{what}: the settled frame's glints"
                );
            }
            for q in &p.glints {
                assert_in_band_and_box(q, &s, &what);
                let r = rect_of(q);
                assert_eq!(
                    r.meet(band),
                    r,
                    "{what} at {clock:?}: off the rainbow {q:?}"
                );
                if !s.dark {
                    let c = q.color;
                    assert!(
                        q.alpha > 0 && (c >> 16) & 0xff == c & 0xff && (c >> 8) & 0xff == c & 0xff,
                        "{what} at {clock:?}: not the paper's white {q:?}"
                    );
                }
            }
            laid += p.glints.len();
        }
    }
    assert!(laid > 0, "glints laid");
    assert!(
        counts.contains(&1) && counts.contains(&GLINTS_MAX),
        "one on a short trail, the most on a long one: {counts:?}"
    );
    let s = scene();
    let g = Geo::new(&s).expect("a scene");
    let (t, amp) = trail_rect(&g, &s, kitty_rect(&g, &s)).expect("a trail");
    let mut out = Vec::new();
    let sink = Sink { geo: g };
    let edge = (t.x0 + 2 * g.cw, t.y0 + amp);
    lay_star(
        &mut out,
        &sink,
        g.clip,
        edge,
        glint_spots(&g, &s, 7).rest,
        GLINT_COV,
        false,
        WHITE,
        &[],
        true,
    );
    let band = Rect {
        y0: t.y0 + amp,
        y1: t.y1 - amp,
        ..t
    };
    assert!(
        out.iter().any(|q| rect_of(q).meet(band) != rect_of(q)),
        "CONTROL: a glint on the band's edge leaves it"
    );
}

#[test]
fn a_glint_flares_as_the_sheen_passes() {
    for s in glint_scenes()
        .into_iter()
        .filter(|s| s.trail == Some((32, 52)))
    {
        let what = format!("{}x{} {} rows dark {}", s.cw, s.ch, s.kitty_rows, s.dark);
        let g = Geo::new(&s).expect("a scene");
        let glints = glint_spots(&g, &s, 7);
        assert_eq!(glints.n, GLINTS_MAX, "{what}");
        assert!(glints.flare > glints.rest, "{what}: room to flare");
        let reach = glints.flare + 1;
        let read = |p: &Parts, spot: &GlintSpot| {
            let mine: Vec<Rect> = p
                .glints
                .iter()
                .map(rect_of)
                .filter(|r| (r.x0 - spot.x).abs() <= reach && (r.y0 - spot.y).abs() <= reach)
                .collect();
            let half = mine
                .iter()
                .map(|r| (r.x1 - 1 - spot.x).max(spot.x - r.x0))
                .max();
            let dots = mine
                .iter()
                .any(|r| r.x1 - r.x0 == 1 && r.y1 - r.y0 == 1 && r.x0 != spot.x && r.y0 != spot.y);
            (half, dots)
        };
        let settled = parts(&s, Clock::SETTLED);
        for spot in glints.spots() {
            assert_eq!(
                read(&settled, spot),
                (Some(glints.rest), false),
                "{what}: at rest"
            );
            let mut flares = 0;
            let mut was_flaring = false;
            let mut rest_between = false;
            for t in (0..EPISODE_MS).step_by(FRAME_MS as usize / 3) {
                let (half, dots) = read(&parts(&s, Clock::seated(t)), spot);
                let half = half.expect("a glint");
                assert!(
                    (glints.rest..=glints.flare).contains(&half),
                    "{what}: {t} ms, arm {half}"
                );
                let flaring = half == glints.flare;
                if flaring && !was_flaring {
                    flares += 1;
                    assert!(dots, "{what}: {t} ms, a flare throws its glints");
                }
                if half == glints.rest && flares > 0 {
                    assert!(!dots, "{what}: CONTROL, {t} ms, at rest, no glints");
                    rest_between = true;
                }
                was_flaring = flaring;
            }
            let passes = (EPISODE_MS as f32 / SHIMMER_MS) as usize;
            assert!(
                flares >= passes - 1 && flares <= passes + 1,
                "{what}: {flares} flares for {passes} passes"
            );
            assert!(rest_between, "{what}: CONTROL, back at rest between passes");
        }
        let last = parts(&s, Clock::seated(EPISODE_MS - 1));
        let first = parts(&s, Clock::flowing(EPISODE_MS));
        for spot in glints.spots() {
            assert_eq!(
                read(&last, spot),
                read(&first, spot),
                "{what}: no jump into the flow"
            );
        }
    }
}

#[test]
fn a_flowing_glint_flares_as_the_ripple_passes() {
    let chroma = |rgb: u32| {
        let [_, r, g, b] = rgb.to_be_bytes();
        r.max(g).max(b) - r.min(g).min(b)
    };
    for s in glint_scenes()
        .into_iter()
        .filter(|s| s.trail == Some((32, 52)))
    {
        let what = format!("{}x{} {} rows dark {}", s.cw, s.ch, s.kitty_rows, s.dark);
        let g = Geo::new(&s).expect("a scene");
        let glints = glint_spots(&g, &s, 7);
        assert_eq!(glints.n, GLINTS_MAX, "{what}");
        let reach = glints.flare + 1;
        let read = |p: &Parts, spot: &GlintSpot| {
            let mine: Vec<Rect> = p
                .glints
                .iter()
                .map(rect_of)
                .filter(|r| (r.x0 - spot.x).abs() <= reach && (r.y0 - spot.y).abs() <= reach)
                .collect();
            let half = mine
                .iter()
                .map(|r| (r.x1 - 1 - spot.x).max(spot.x - r.x0))
                .max()
                .expect("a glint");
            let dots = mine
                .iter()
                .any(|r| r.x1 - r.x0 == 1 && r.y1 - r.y0 == 1 && r.x0 != spot.x && r.y0 != spot.y);
            (half, dots)
        };
        let held = parts(&s, Clock::SETTLED);
        let steps: Vec<Parts> = (0..GLINT_PERIOD_MS / FLOW_STEP_MS)
            .map(|k| parts(&s, Clock::flowing(EPISODE_MS + k * FLOW_STEP_MS)))
            .collect();
        for spot in glints.spots() {
            assert_eq!(
                read(&held, spot),
                (glints.rest, false),
                "{what}: CONTROL, the held card's glint at rest"
            );
            let seen: Vec<(i64, bool)> = steps.iter().map(|p| read(p, spot)).collect();
            assert!(
                seen.iter()
                    .all(|&(half, _)| (glints.rest..=glints.flare).contains(&half)),
                "{what}: {seen:?}"
            );
            assert!(
                seen.iter().any(|&(half, dots)| half > glints.rest && dots),
                "{what}: the crest flares the glint at ({}, {}): {seen:?}",
                spot.x,
                spot.y
            );
            assert!(
                seen.contains(&(glints.rest, false)),
                "{what}: and it is back at rest off the crest: {seen:?}"
            );
        }
        if s.dark {
            let off = CardScene {
                palette: CardPalette::Off,
                ..s
            };
            let starlight = parts(&off, Clock::SETTLED).glints;
            assert!(
                !starlight.is_empty() && starlight.iter().all(|q| chroma(q.color) < 0x20),
                "{what}: Off's glints are starlight"
            );
            assert!(
                held.glints.iter().any(|q| chroma(q.color) >= 0x20),
                "{what}: CONTROL, Cruise's are gold"
            );
        }
    }
}

#[test]
fn the_field_keeps_clear_of_the_glints() {
    let mut near_trail = 0;
    for seed in 0..200u64 {
        for s in glint_scenes()
            .into_iter()
            .filter(|s| s.cw == 9 && s.trail == Some((32, 52)))
        {
            let g = Geo::new(&s).expect("a scene");
            let glints = glint_spots(&g, &s, seed);
            let (field, n) = field_spots(&g, &s, seed, !s.dark);
            let gap = g.cw.max(g.ch / 2);
            for f in &field[..n] {
                for o in glints.spots() {
                    assert!(
                        (f.x - o.x).abs() >= gap || (f.y - o.y).abs() >= gap,
                        "seed {seed}: a field star {f:?} on a glint {o:?}"
                    );
                }
                let (t, _) = trail_rect(&g, &s, kitty_rect(&g, &s)).expect("a trail");
                near_trail += usize::from(t.contains(f.x, f.y));
            }
        }
    }
    assert!(
        near_trail > 0,
        "CONTROL: field stars on the trail, beside the glints"
    );
}

#[test]
fn the_dark_star_is_the_family_star() {
    let s = scene();
    let g = Geo::new(&s).expect("a scene");
    let geom = Geom {
        cw: 1,
        ch: usize::from(s.ch),
        rows: 64,
        cols: 4000,
        origin_x: 0,
        origin_y: s.origin_y as u16,
        win_w: 4000,
        win_h: 2000,
        head: 0,
    };
    for (arm, cov, gold) in [(2i64, 235.0f32, false), (3, 200.0, true), (6, 255.0, true)] {
        let (sx, sy) = (g.col(45), g.band(g.top + 1) + 1);
        let mut ours = Vec::new();
        let sink = Sink { geo: g };
        lay_star(
            &mut ours,
            &sink,
            g.clip,
            (sx, sy),
            arm,
            cov,
            gold,
            WHITE,
            &[],
            true,
        );
        let mut family = Vec::new();
        assert!(push_twinkle_star(
            &mut family,
            geom,
            sx as i32,
            sy as i32,
            arm as i32,
            cov as u8,
            gold,
            WHITE,
            4096
        ));
        for q in &mut family {
            q.row += s.row_base;
        }
        assert_eq!(ours, family, "arm {arm} cov {cov} gold {gold}");
    }
}

#[test]
fn the_card_is_deterministic() {
    for s in scenes() {
        let t0 = Instant::now();
        let (mut a, mut b) = (HarnessCard::default(), HarnessCard::default());
        a.arm(t0, 42);
        b.arm(t0, 42);
        let flow = [
            EPISODE_MS + 5,
            EPISODE_MS + 900,
            EPISODE_MS + 3100,
            EPISODE_MS + 6200,
        ];
        for at in [0, 33, 700, 2600].into_iter().chain(flow) {
            assert_eq!(
                frame(&mut a, t0 + ms(at), &s, true),
                frame(&mut b, t0 + ms(at), &s, true)
            );
        }
    }
    let s = scene();
    let t0 = Instant::now();
    let (mut a, mut b) = (HarnessCard::default(), HarnessCard::default());
    a.arm(t0, 1);
    b.arm(t0, 2);
    assert_ne!(
        frame(&mut a, t0 + ms(EPISODE_MS), &s, true),
        frame(&mut b, t0 + ms(EPISODE_MS), &s, true)
    );
}

fn observed(card: &HarnessCard, at: Instant) -> Vec<String> {
    let mut seen = Vec::new();
    for dt in [
        0,
        1,
        FRAME_MS,
        400,
        EPISODE_MS,
        EPISODE_MS + SETTLE_GRACE_MS + 1,
    ] {
        let t = at + ms(dt);
        seen.push(format!("deadline {dt}: {:?}", card.deadline(t)));
        seen.push(format!(
            "ink {dt}: {:?}",
            card.line_ink(t, 4, 2, 9, NIGHT, true)
        ));
    }
    seen.push(format!(
        "at_rest {} still {}",
        card.at_rest(),
        card.is_still()
    ));
    seen
}

fn frame_text(f: &CardFrame) -> String {
    format!(
        "under {:?} {:?} over {:?} {:?} at {:?}",
        f.under.items(),
        f.under.palette(),
        f.over.items(),
        f.over.palette(),
        f.origin
    )
}

#[test]
fn a_card_at_rest_is_replayed_bit_for_bit() {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Way {
        Draw,
        Held,
        Step,
    }
    use Way::{Draw, Held, Step};
    for s in scenes() {
        let land = CardScene {
            palette: match s.palette {
                CardPalette::Cruise => CardPalette::Land,
                CardPalette::Land => CardPalette::Off,
                CardPalette::Off => CardPalette::Cruise,
            },
            ..s
        };
        let serious = CardScene { serious: true, ..s };
        let t0 = Instant::now();
        let lapse = 3 * EPISODE_MS + OWED_MAX_MS;
        let steps: Vec<(u64, CardScene, bool, bool, Way)> = vec![
            (0, s, true, true, Draw),
            (33, s, true, false, Draw),
            (100, s, false, false, Draw),
            (140, s, false, false, Held),
            (180, s, false, false, Held),
            (220, s, true, false, Draw),
            (2000, s, true, false, Draw),
            (EPISODE_MS + 1, s, true, false, Draw),
            (EPISODE_MS + 10, s, true, false, Step),
            (EPISODE_MS + 40, s, true, false, Draw),
            (EPISODE_MS + 90, s, false, false, Draw),
            (EPISODE_MS + 92, s, false, false, Held),
            (EPISODE_MS + 95, land, true, false, Draw),
            (EPISODE_MS + 99, land, true, false, Step),
            (EPISODE_MS + 120, serious, true, false, Draw),
            (EPISODE_MS + 130, serious, true, false, Held),
            (EPISODE_MS + 140, s, true, false, Draw),
            (EPISODE_MS + 200, s, false, true, Draw),
            (EPISODE_MS + 300, s, false, false, Held),
            (EPISODE_MS + 400, s, false, false, Held),
            (EPISODE_MS + 500, s, true, false, Draw),
            (EPISODE_MS + 533, s, true, false, Draw),
            (3 * EPISODE_MS, s, false, true, Draw),
            (lapse + 10, s, false, false, Held),
            (lapse + 20, s, true, false, Draw),
            (lapse + 30, s, true, false, Step),
            (lapse + 200, s, true, false, Draw),
            (lapse + 210, s, true, false, Step),
        ];
        let (mut twin, mut twin_frames) = (HarnessCard::default(), CardFrames::default());
        let (mut host, mut frames) = (HarnessCard::default(), CardFrames::default());
        let mut last: Option<CardFrame> = None;
        for (at, scene, animate, arm, way) in steps {
            let now = t0 + ms(at);
            if arm {
                twin.arm(now, 9);
                host.arm(now, 9);
            }
            let want = twin.draw(now, &scene, animate, &mut twin_frames);
            let before = aterm_core::render::fx_counts_here();
            let mut clock_reads = 0;
            let replayed = match host.replay(&scene, animate, &frames) {
                Some(f) => Some((f, Held)),
                None => {
                    clock_reads += 1;
                    host.replay_step(&scene, animate, &frames, now)
                        .map(|f| (f, Step))
                }
            };
            let spent = aterm_core::render::fx_counts_here();
            assert_eq!(
                (spent.0 - before.0, spent.1 - before.1),
                (0, 0),
                "{at} ms: a replay builds nothing and copies nothing"
            );
            let (got, how) = match replayed {
                Some((f, how)) => {
                    let prev = last.as_ref().expect("a frame before a replay");
                    assert!(
                        Arc::ptr_eq(&f.under, &prev.under) && Arc::ptr_eq(&f.over, &prev.over),
                        "{at} ms: the kept Arcs themselves"
                    );
                    (f, how)
                }
                None => {
                    let f = host.draw(now, &scene, animate, &mut frames);
                    let built = aterm_core::render::fx_counts_here().0 - spent.0;
                    assert!(
                        built > 0 || f.is_empty(),
                        "{at} ms: CONTROL, a draw seals its contents"
                    );
                    (f, Draw)
                }
            };
            assert_eq!(how, way, "{at} ms: how it reached the glass");
            assert_eq!(
                clock_reads,
                usize::from(way != Held),
                "{at} ms: clock reads"
            );
            assert_eq!(frame_text(&got), frame_text(&want), "{at} ms: the frame");
            assert_eq!(observed(&host, now), observed(&twin, now), "{at} ms: host");
            last = Some(got);
        }
    }
}

fn lift_by_bisection(rgb: u32, floor: f32) -> u32 {
    if relative_luminance(rgb) >= floor {
        return rgb;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..14 {
        let mid = 0.5 * (lo + hi);
        if relative_luminance(mix_rgb(rgb, WHITE, mid)) >= floor {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    mix_rgb(rgb, WHITE, hi)
}

fn seams(quads: &[GlowQuad]) -> (Vec<String>, usize) {
    let mut ends = std::collections::HashMap::new();
    for q in quads {
        ends.insert((q.y, q.h, q.x + q.w), (q.color2, q.alpha2));
    }
    let (mut bad, mut checked) = (Vec::new(), 0);
    for q in quads {
        if let Some(&end) = ends.get(&(q.y, q.h, q.x)) {
            checked += 1;
            if end != (q.color, q.alpha) {
                bad.push(format!("{q:?} starts off {end:?}"));
            }
        }
    }
    (bad, checked)
}

#[test]
fn the_border_and_trail_segments_meet() {
    let mut checked = 0;
    for s in scenes() {
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        let flow =
            (EPISODE_MS + 1..EPISODE_MS + 2 * GLINT_PERIOD_MS).step_by(FLOW_STEP_MS as usize * 5);
        for at in [0, 40, 450, 1300, 2600, 4100].into_iter().chain(flow) {
            let (under, _, _) = frame(&mut card, t0 + ms(at), &s, true);
            let (bad, n) = seams(&under);
            assert!(bad.is_empty(), "at {at} ms: {bad:#?}");
            checked += n;
        }
    }
    assert!(checked > 100, "seams checked: {checked}");
    let (mut under, _, _) = settled(&scene());
    let target = (0..under.len())
        .find(|&i| {
            let q = under[i];
            under
                .iter()
                .any(|p| (p.y, p.h, p.x + p.w) == (q.y, q.h, q.x))
        })
        .expect("the settled frame has a seam");
    under[target].color ^= 0x0001_0101;
    assert_eq!(seams(&under).0.len(), 1, "a nudged start is caught");
}

#[test]
fn a_lift_finds_the_bisections_mix() {
    let coarse = |rgb: u32, floor: f32| {
        if relative_luminance(rgb) >= floor {
            return rgb;
        }
        let reaches = |t: f32| relative_luminance(mix_rgb(rgb, WHITE, t)) >= floor;
        let (mut short, mut step) = (0.0f32, 0.25f32);
        for _ in 0..6 {
            let n = [1.0, 2.0, 3.0].map(|j| !reaches(short + j * step));
            short += step * f32::from(n.iter().map(|&s| u8::from(s)).sum::<u8>());
            step *= 0.25;
        }
        mix_rgb(rgb, WHITE, short + 4.0 * step)
    };
    let mut seen_short = 0;
    let mut checked = 0;
    for (floor, every) in [
        (LINE_DARK_LUMA_FLOOR, 5),
        (0.0, 15),
        (0.02, 15),
        (0.18, 15),
        (0.5, 15),
        (0.9, 15),
        (1.0, 15),
        (1.2, 15),
    ] {
        for r in (0..=255u32).step_by(every) {
            for g in (0..=255u32).step_by(every) {
                for b in (0..=255u32).step_by(every) {
                    let rgb = r << 16 | g << 8 | b;
                    let want = lift_by_bisection(rgb, floor);
                    assert_eq!(lift_to_luminance(rgb, floor), want, "{rgb:06x} to {floor}");
                    seen_short += usize::from(coarse(rgb, floor) != want);
                    checked += 1;
                }
            }
        }
    }
    assert!(checked > 140_000, "{checked} colours");
    assert!(seen_short > 0, "CONTROL: a search a round short is seen");
}

#[test]
fn at_rest_means_no_deadline_at_any_instant() {
    let s = scene();
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    assert!(card.at_rest(), "unarmed");
    card.arm(t0, 5);
    assert!(!card.at_rest(), "CONTROL: armed");
    for (at, animate) in [
        (0, true),
        (500, true),
        (EPISODE_MS + 1, true),
        (EPISODE_MS + 2, true),
        (EPISODE_MS + 3, false),
        (EPISODE_MS + 900, true),
    ] {
        let now = t0 + ms(at);
        let _ = frame(&mut card, now, &s, animate);
        assert_eq!(card.at_rest(), !animate, "{at} ms");
        if card.at_rest() {
            for dt in [0, 1, FRAME_MS, EPISODE_MS, 10 * EPISODE_MS] {
                assert_eq!(card.deadline(now + ms(dt)), None, "{at} ms + {dt}");
            }
        } else {
            assert!(card.deadline(now).is_some(), "CONTROL: {at} ms moves");
        }
    }
    frame(&mut card, t0 + ms(EPISODE_MS + 1000), &s, false);
    assert!(card.at_rest(), "held");
}

#[test]
fn an_empty_scene_draws_nothing() {
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    card.arm(t0, 1);
    for s in [
        CardScene { rows: 0, ..scene() },
        CardScene { cols: 0, ..scene() },
        CardScene { ch: 0, ..scene() },
    ] {
        assert_eq!(frame(&mut card, t0, &s, true), (Vec::new(), Vec::new(), 0));
    }
}

#[test]
fn the_line_sweeps_then_settles() {
    let t0 = Instant::now();
    let (row, col, len) = (7u16, 11u16, 64u16);
    for (palette, ground) in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off]
        .into_iter()
        .flat_map(|p| [(p, NIGHT), (p, DAY)])
    {
        let dark = ground == NIGHT;
        let mut card = HarnessCard::default();
        card.tick(
            t0,
            &CardScene {
                palette,
                dark,
                ground,
                ..scene()
            },
            false,
            &mut Vec::new(),
            &mut Vec::new(),
        );
        card.arm(t0, 3);
        let rest = card.line_ink(t0 + ms(EPISODE_MS), row, col, len, ground, false);
        assert_eq!(rest.len(), usize::from(len));
        for (i, c) in rest.iter().enumerate() {
            assert_eq!(
                (c.row, c.col),
                (row, col + i as u16),
                "sorted, unique, contiguous"
            );
        }
        assert_eq!(
            rest,
            card.line_ink(t0 + ms(EPISODE_MS + 5000), row, col, len, ground, false),
            "settled is constant"
        );
        assert_eq!(
            rest,
            card.line_ink(t0 + ms(900), row, col, len, ground, false),
            "reduced motion is settled"
        );
        let mid = card.line_ink(t0 + ms(900), row, col, len, ground, true);
        let next = card.line_ink(t0 + ms(900 + FRAME_MS), row, col, len, ground, true);
        assert_ne!(mid, rest, "the episode moves the line");
        assert_ne!(mid, next, "frame to frame");
        let flow: Vec<Vec<InkCell>> = (0..GLINT_PERIOD_MS / FLOW_STEP_MS)
            .map(|k| {
                let at = t0 + ms(EPISODE_MS + 200 + k * FLOW_STEP_MS);
                card.line_ink(at, row, col, len, ground, true)
            })
            .collect();
        assert!(
            flow.iter().filter(|f| **f != rest).count() > flow.len() / 2,
            "{palette:?}: the flow's glints run along the line"
        );
        assert_ne!(
            rest[0].color,
            rest[rest.len() - 1].color,
            "the settled line is a gradient"
        );
        for c in rest.iter().chain(&mid).chain(flow.iter().flatten()) {
            let rgb = (u32::from(c.color[0]) << 16)
                | (u32::from(c.color[1]) << 8)
                | u32::from(c.color[2]);
            if dark {
                assert!(
                    relative_luminance(rgb) >= LINE_DARK_LUMA_FLOOR - 0.01,
                    "{palette:?}: {rgb:06x} too dark"
                );
            } else {
                assert!(
                    contrast(rgb, ground) >= 4.5,
                    "{palette:?}: {rgb:06x} under AA on {ground:06x}: {:.2}:1",
                    contrast(rgb, ground)
                );
            }
        }
    }
    let mut card = HarnessCard::default();
    card.arm(t0, 3);
    assert_eq!(card.line_ink(t0, 0, u16::MAX - 1, 5, NIGHT, true).len(), 2);
}

#[test]
fn the_lines_hero_word_wears_its_own_rainbow() {
    let t0 = Instant::now();
    let plain = LineAt {
        row: 5,
        col: 13,
        len: 55,
        hero: 0..0,
        wand: Some((10, 3)),
    };
    let at = LineAt {
        hero: 12..17,
        ..plain.clone()
    };
    let rgb = |c: &InkCell| {
        (u32::from(c.color[0]) << 16) | (u32::from(c.color[1]) << 8) | u32::from(c.color[2])
    };
    let ink = |card: &HarnessCard, now: Option<Instant>, at: &LineAt, ground, animate| {
        let mut out = Vec::new();
        card.line_ink_into(now, at, ground, animate, &mut out);
        out
    };
    for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
        for ground in [NIGHT, DAY] {
            let dark = ground == NIGHT;
            let what = format!("{palette:?} dark {dark}");
            let mut card = HarnessCard::default();
            card.tick(
                t0,
                &CardScene {
                    palette,
                    dark,
                    ground,
                    ..scene()
                },
                false,
                &mut Vec::new(),
                &mut Vec::new(),
            );
            let rest = ink(&card, None, &at, ground, false);
            let bare = ink(&card, None, &plain, ground, false);
            assert_eq!(rest.len(), 55, "{what}");
            for (i, c) in rest.iter().enumerate() {
                assert_eq!((c.row, c.col), (5, 13 + i as u16), "{what}: contiguous");
            }
            for i in (0..55).filter(|i| !(12..17).contains(i)) {
                assert_eq!(rest[i], bare[i], "{what}: cell {i} is the line's");
            }
            let hero: Vec<u32> = rest[12..17].iter().map(rgb).collect();
            let changed = (12..17).filter(|&i| rest[i] != bare[i]).count();
            assert!(
                changed >= 3,
                "{what}: the hero wears its own ink ({changed})"
            );
            let distinct = hero
                .iter()
                .enumerate()
                .all(|(i, a)| hero[..i].iter().all(|b| b != a));
            assert!(distinct, "{what}: five letters, five colours {hero:06x?}");
            let ch = |c: u32, sh: u32| (c >> sh) & 0xff;
            let red = hero
                .iter()
                .any(|&c| ch(c, 16) > ch(c, 8) + 40 && ch(c, 16) > ch(c, 0) + 40);
            let blue = hero
                .iter()
                .any(|&c| ch(c, 0) > ch(c, 16) + 30 && ch(c, 0) > ch(c, 8));
            match palette {
                CardPalette::Cruise => assert!(red && blue, "{what}: red to blue {hero:06x?}"),
                CardPalette::Land => assert!(!blue, "{what}: a sunset {hero:06x?}"),
                CardPalette::Off => {}
            }
            for &c in &hero {
                if dark {
                    assert!(
                        relative_luminance(c) >= LINE_HERO_DARK_LUMA_FLOOR - 0.01,
                        "{what}: {c:06x} under the hero's floor"
                    );
                } else {
                    assert!(
                        contrast(c, ground) >= 4.5,
                        "{what}: {c:06x} under AA on {ground:06x}"
                    );
                }
            }
            card.arm(t0, 3);
            assert_eq!(
                ink(&card, Some(t0 + ms(900)), &at, ground, false),
                rest,
                "{what}: reduced motion"
            );
            let mut seen: Vec<Vec<InkCell>> = Vec::new();
            for t in (0..EPISODE_MS / 2).step_by(FRAME_MS as usize) {
                let hero = ink(&card, Some(t0 + ms(t)), &at, ground, true)[12..17].to_vec();
                if !seen.contains(&hero) {
                    seen.push(hero);
                }
            }
            assert!(
                seen.len() > 20,
                "{what}: the intro moves it ({})",
                seen.len()
            );
            let flow = (0..GLINT_PERIOD_MS / FLOW_STEP_MS).any(|k| {
                let now = t0 + ms(EPISODE_MS + 200 + k * FLOW_STEP_MS);
                ink(&card, Some(now), &at, ground, true)[12..17] != rest[12..17]
            });
            assert!(flow, "{what}: the flow's glints reach the hero");
        }
    }
}

#[test]
fn the_cards_glints_run_outward_from_the_wand() {
    use crate::ripple::GLINT_CELLS_PER_S;
    let s = scene();
    let g = Geo::new(&s).expect("a scene");
    let (cw, ch) = (g.cw as f32, g.ch as f32);
    let (wx, wy) = (g.ox as f32 + 10.5 * cw, g.band(3) as f32 + 0.5 * ch);
    let at = |ms: u64| Clock {
        t_ms: None,
        ride_ms: None,
        glint: Some((ms, 1.0)),
        flow: None,
    };
    for k in 3..15u64 {
        let ms = k * FLOW_STEP_MS;
        let cx = Emit::new(&s, g, at(ms), 7);
        let front = GLINT_CELLS_PER_S * ms as f32 / 1000.0;
        assert_eq!(
            cx.ring_at(wx + front * cw, wy),
            1.0,
            "{ms} ms: the front, right"
        );
        assert_eq!(
            cx.ring_at(wx - front * cw, wy),
            1.0,
            "{ms} ms: the front, left"
        );
        for off in [-3.0, 3.0] {
            assert!(
                cx.ring_at(wx + (front + off) * cw, wy) < 0.5,
                "{ms} ms: {off} cells off the front"
            );
        }
    }
    let four = (4000.0 / GLINT_CELLS_PER_S).round() as u64;
    let cx = Emit::new(&s, g, at(four), 7);
    assert_eq!(
        cx.ring_at(wx, wy + 2.0 * ch),
        1.0,
        "two rows down: four cells"
    );
    assert!(
        cx.ring_at(wx, wy + 3.0 * ch) < 0.5,
        "three rows down: not yet"
    );
    let bare = CardScene { wand: None, ..s };
    let gb = Geo::new(&bare).expect("a scene");
    let inner = gb.inner();
    let ms = 10 * FLOW_STEP_MS;
    let cx = Emit::new(&bare, gb, at(ms), 7);
    let front = GLINT_CELLS_PER_S * ms as f32 / 1000.0;
    assert_eq!(
        cx.ring_at(inner.x0 as f32 + front * cw, inner.y0 as f32),
        1.0,
        "CONTROL: no wand, the inner box's corner"
    );
}

#[test]
fn the_flow_stays_under_the_photosensitivity_bound() {
    const SWING: f32 = 0.1;
    const MINUTE_MS: u64 = 60_000;
    let flashes = |series: &[f32]| {
        let (mut n, mut lo, mut hi, mut rising) = (0, f32::MAX, f32::MIN, true);
        for &v in series {
            if rising {
                lo = lo.min(v);
                if v - lo >= SWING {
                    n += 1;
                    rising = false;
                    hi = v;
                }
            } else {
                hi = hi.max(v);
                if hi - v >= SWING {
                    rising = true;
                    lo = v;
                }
            }
        }
        n
    };
    let lum = |premul: u32| relative_luminance(premul);
    for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
        let s = CardScene { palette, ..scene() };
        let g = Geo::new(&s).expect("a scene");
        let covering = |quads: &[GlowQuad], x: i64, y: i64| {
            quads
                .iter()
                .find(|q| {
                    let (qx, qy) = (i64::from(q.x), i64::from(q.y));
                    (qx..qx + i64::from(q.w)).contains(&x) && (qy..qy + i64::from(q.h)).contains(&y)
                })
                .map(|q| q.color)
        };
        let border = (g.col(26) + 2, g.outer.y0);
        let (_, yb) = trail_band(&g, &s);
        let trail = (g.col(36) + 2, yb - 2);
        let line = LineAt {
            row: 6,
            col: 13,
            len: 50,
            hero: 12..17,
            wand: Some((10, 3)),
        };
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        frame(&mut card, t0, &s, true);
        frame(&mut card, t0 + ms(EPISODE_MS - 5), &s, true);
        let (mut b, mut t, mut l) = (Vec::new(), Vec::new(), Vec::new());
        let mut ink = Vec::new();
        for at in (EPISODE_MS..EPISODE_MS + MINUTE_MS).step_by(FLOW_STEP_MS as usize) {
            let now = t0 + ms(at);
            let (under, _, _) = frame(&mut card, now, &s, true);
            b.push(lum(
                covering(&under, border.0, border.1).expect("the border")
            ));
            t.push(lum(covering(&under, trail.0, trail.1).expect("the trail")));
            ink.clear();
            card.line_ink_into(Some(now), &line, NIGHT, true, &mut ink);
            let c = ink[46].color;
            l.push(lum((u32::from(c[0]) << 16)
                | (u32::from(c[1]) << 8)
                | u32::from(c[2])));
        }
        for (what, series) in [("border", &b), ("trail", &t), ("line", &l)] {
            let n = flashes(series);
            let hz = n as f32 * 1000.0 / MINUTE_MS as f32;
            assert!(
                hz <= TWINKLE_FLASH_BOUND_HZ,
                "{palette:?} {what}: {n} flashes a minute, {hz} Hz"
            );
            assert!(
                n as u64 >= MINUTE_MS / (2 * GLINT_PERIOD_MS),
                "{palette:?} {what}: CONTROL, {n} flashes a minute"
            );
        }
    }
}

#[test]
fn the_line_wears_the_cards_palette() {
    let t0 = Instant::now();
    let mut card = HarnessCard::default();
    let cruise = card.line_ink(t0, 0, 0, 30, NIGHT, false);
    let mut under = Vec::new();
    let mut over = Vec::new();
    let mut wear = |card: &mut HarnessCard, palette| {
        card.tick(
            t0,
            &CardScene { palette, ..scene() },
            false,
            &mut under,
            &mut over,
        );
        card.line_ink(t0, 0, 0, 30, NIGHT, false)
    };
    let land = wear(&mut card, CardPalette::Land);
    assert_ne!(land, cruise);
    let off = wear(&mut card, CardPalette::Off);
    assert_ne!(off, cruise, "Off is not Cruise");
    assert_ne!(off, land, "Off is not Land");
    for ground in [NIGHT, DAY] {
        let line = card.line_ink(t0, 0, 0, 30, ground, false);
        let steps = LINE_INK_STEPS as f32;
        for (i, c) in line.iter().enumerate() {
            let t = tri(i as f32 / 29.0);
            let t = if dark_ground(ground) {
                t
            } else {
                (t * steps).round() / steps
            };
            let want = CardPalette::Off.letter_on(t, ground);
            assert_eq!(
                c.color,
                [(want >> 16) as u8, (want >> 8) as u8, want as u8],
                "on {ground:06x}: cell {i} is the palette's letter ink"
            );
        }
    }
}

#[test]
fn the_sunset_ramp_hits_its_stops() {
    assert_eq!(sunset(0.0), SUNSET[0]);
    assert_eq!(sunset(1.0), SUNSET[3]);
    assert_eq!(sunset(1.0 / 3.0), SUNSET[1]);
    assert_eq!(sunset(f32::NAN), SUNSET[0]);
    assert_eq!(sunset(7.0), SUNSET[3]);
}

#[test]
fn the_moonbow_ramp_hits_its_stops() {
    assert_eq!(moonbow(0.0), WHITE);
    assert_eq!(moonbow(0.0), MOONBOW[0]);
    assert_eq!(moonbow(1.0 / 3.0), MOONBOW[1]);
    assert_eq!(moonbow(2.0 / 3.0), MOONBOW[2]);
    assert_eq!(moonbow(1.0), MOONBOW[3]);
    assert_eq!(moonbow(f32::NAN), MOONBOW[0]);
    assert_eq!(moonbow(-3.0), MOONBOW[0]);
    assert_eq!(moonbow(7.0), MOONBOW[3]);
    assert_eq!(CardPalette::Off.art(0.0, true), WHITE);
    assert_eq!(CardPalette::Off.art(1.0, false), PRINT_OFF[5]);
    assert_eq!(CardPalette::Off.stripe(0, true), WHITE);
    assert_eq!(CardPalette::Off.stripe(5, true), MOONBOW[3]);
    assert_eq!(CardPalette::Off.stripe(5, false), PRINT_OFF[5]);
    for c in MOONBOW {
        assert!(relative_luminance(c) >= 0.41, "{c:06x}");
        assert_eq!(lift_to_luminance(c, LINE_DARK_LUMA_FLOOR), c);
    }
}

fn on_rim(q: &GlowQuad, s: &CardScene) -> bool {
    let g = Geo::new(s).expect("a scene");
    let inner = g.inner();
    let (x0, y0) = (i64::from(q.x), i64::from(q.y));
    let (x1, y1) = (x0 + i64::from(q.w), y0 + i64::from(q.h));
    y1 <= inner.y0 || y0 >= inner.y1 || x1 <= inner.x0 || x0 >= inner.x1
}

fn drawn_kitty(over: &[GlowQuad], at: &KittyAt, inks: KittyInk) -> Vec<String> {
    let (r0, p) = (at.rect, at.p);
    (0..at.art.h() as i64)
        .map(|r| {
            (0..KITTY_W as i64)
                .map(|c| {
                    let (x, y) = (r0.x0 + c * p + p / 2, r0.y0 + r * p + p / 2);
                    over.iter()
                        .filter(|q| q.alpha == 255)
                        .find(|q| {
                            let (qx, qy) = (i64::from(q.x), i64::from(q.y));
                            (qx..qx + i64::from(q.w)).contains(&x)
                                && (qy..qy + i64::from(q.h)).contains(&y)
                        })
                        .map_or('.', |q| {
                            char::from(
                                *KITTY_PALETTE
                                    .iter()
                                    .find(|&&b| inks.of(b) == Some(q.color))
                                    .expect("a palette colour"),
                            )
                        })
                })
                .collect()
        })
        .collect()
}

#[test]
fn the_blink_and_the_flick_come_in_their_windows() {
    for kitty_rows in [2u8, 3] {
        let s = CardScene {
            kitty_rows,
            ..scene()
        };
        let g = Geo::new(&s).expect("a scene");
        let at = kitty_rect(&g, &s).expect("a kitty");
        let inks = KittyInk::new(s.coat, s.eye, s.dark);
        let art = |f: &[&str]| f.iter().map(|r| (*r).to_string()).collect::<Vec<_>>();
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        let mut seen = [0usize; 3];
        for t in (0..EPISODE_MS).step_by(10) {
            let (_, over, _) = frame(&mut card, t0 + ms(t), &s, true);
            let at = ridden(&g, &s, at, Clock::riding(t));
            let in_window =
                |first: u64, period: u64, len: u64| t >= first && (t - first) % period < len;
            let (want, k) = if in_window(BLINK_AT_MS, BLINK_PERIOD_MS, BLINK_MS) {
                (at.art.blink, 1)
            } else if in_window(FLICK_AT_MS, FLICK_PERIOD_MS, FLICK_MS) {
                (at.art.flick, 2)
            } else {
                (at.art.open, 0)
            };
            assert_eq!(drawn_kitty(&over, &at, inks), art(want), "{t} ms");
            seen[k] += 1;
        }
        assert!(seen.iter().all(|&n| n > 0), "every frame drawn: {seen:?}");
        assert_eq!(seen[1], 2 * (BLINK_MS as usize / 10), "two blinks");
        assert_eq!(seen[2], 3 * (FLICK_MS as usize / 10), "three flicks");
        let (_, rest, _) = settled(&s);
        assert_eq!(drawn_kitty(&rest, &at, inks), art(at.art.open), "settled");
    }
}

#[test]
fn a_kitty_that_does_not_fit_is_dropped_and_the_rest_stays() {
    let fits = scene();
    let tight = CardScene { cols: 44, ..fits };
    let g = Geo::new(&tight).expect("a scene");
    assert!(kitty_rect(&g, &tight).is_none(), "the kitty is dropped");
    assert!(kitty_rect(&Geo::new(&fits).expect("a scene"), &fits).is_some());
    for dark in [true, false] {
        let s = CardScene { dark, ..tight };
        let t0 = Instant::now();
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        for at in [100, EPISODE_MS + 1] {
            let (under, over, _) = frame(&mut card, t0 + ms(at), &s, true);
            let marks = mark_inks(s.palette);
            assert!(
                !over
                    .iter()
                    .any(|q| q.alpha == 255 && (dark || !marks.contains(&q.color))),
                "dark {dark}, {at} ms: no kitty pixel"
            );
            assert!(
                under.iter().any(|q| on_rim(q, &s)),
                "dark {dark}: the border"
            );
            assert!(
                under.iter().any(|q| !on_rim(q, &s)),
                "dark {dark}: the trail"
            );
            assert!(!over.is_empty(), "dark {dark}, {at} ms: the stars");
            for q in under.iter().chain(&over) {
                assert_in_band_and_box(q, &s, &format!("dark {dark}, {at} ms"));
            }
        }
    }
}

fn gap_to_ramp(color: u32, ramp: impl Fn(f32) -> u32) -> u32 {
    (0..=2000)
        .map(|i| {
            let on = ramp(i as f32 / 2000.0);
            [16, 8, 0]
                .iter()
                .map(|sh| ((on >> sh) & 0xff).abs_diff((color >> sh) & 0xff))
                .max()
                .unwrap_or(0)
        })
        .min()
        .unwrap_or(u32::MAX)
}

#[test]
fn land_wears_the_sunset_ramp() {
    for dark in [true, false] {
        let land = CardScene {
            palette: CardPalette::Land,
            dark,
            ..scene()
        };
        let border = |t: f32| {
            if dark {
                light(sunset(t), f32::from(BORDER_COV)).color
            } else {
                ink(print_line(even_ramp(&PRINT_LAND, t)), PRINT_ALPHA).color
            }
        };
        let stripe = |rgb: u32| {
            if dark {
                light(rgb, f32::from(TRAIL_COV)).color
            } else {
                ink(rgb, PRINT_ALPHA).color
            }
        };
        let (under, _, _) = settled(&land);
        let (rim, trail): (Vec<&GlowQuad>, Vec<&GlowQuad>) =
            under.iter().partition(|q| on_rim(q, &land));
        assert!(!rim.is_empty() && !trail.is_empty(), "dark {dark}");
        for q in &rim {
            for c in [q.color, q.color2] {
                let gap = gap_to_ramp(c, border);
                assert!(
                    gap <= 2,
                    "dark {dark}: border {c:06x} off the sunset by {gap}"
                );
            }
        }
        let stops: Vec<u32> = (0..TRAIL_STOPS.len())
            .map(|k| {
                stripe(if dark {
                    sunset(k as f32 / (TRAIL_STOPS.len() - 1) as f32)
                } else {
                    PRINT_LAND[k]
                })
            })
            .collect();
        let worn: std::collections::BTreeSet<u32> =
            trail.iter().flat_map(|q| [q.color, q.color2]).collect();
        assert_eq!(
            worn,
            stops.iter().copied().collect(),
            "dark {dark}: the trail is the ramp's six stops"
        );
        let (red, rose) = if dark {
            (SUNSET[0], SUNSET[3])
        } else {
            (PRINT_LAND[0], PRINT_LAND[5])
        };
        assert_eq!(stops[0], stripe(red), "red on top");
        assert_eq!(stops[5], stripe(rose), "rose at the bottom");
        let cruise = CardScene {
            palette: CardPalette::Cruise,
            ..land
        };
        let (under, _, _) = settled(&cruise);
        let off = under
            .iter()
            .filter(|q| on_rim(q, &cruise))
            .map(|q| gap_to_ramp(q.color, border))
            .max()
            .unwrap_or(0);
        assert!(
            off > 40,
            "dark {dark}: Cruise's border is not a sunset ({off})"
        );
    }
}

#[test]
fn off_wears_the_moonbow() {
    for dark in [true, false] {
        for kitty_rows in [2u8, 3] {
            let off = CardScene {
                palette: CardPalette::Off,
                dark,
                kitty_rows,
                ..scene()
            };
            let border = |t: f32| {
                if dark {
                    light(moonbow(t), f32::from(BORDER_COV)).color
                } else {
                    ink(print_line(even_ramp(&PRINT_OFF, t)), PRINT_ALPHA).color
                }
            };
            let stripe = |rgb: u32| {
                if dark {
                    light(rgb, f32::from(TRAIL_COV)).color
                } else {
                    ink(rgb, PRINT_ALPHA).color
                }
            };
            let (under, _, _) = settled(&off);
            let (rim, trail): (Vec<&GlowQuad>, Vec<&GlowQuad>) =
                under.iter().partition(|q| on_rim(q, &off));
            assert!(!rim.is_empty() && !trail.is_empty(), "dark {dark}");
            for q in &rim {
                for c in [q.color, q.color2] {
                    let gap = gap_to_ramp(c, border);
                    assert!(
                        gap <= 2,
                        "dark {dark}, {kitty_rows} rows: border {c:06x} off the moonbow by {gap}"
                    );
                }
            }
            let stops: Vec<u32> = (0..TRAIL_STOPS.len())
                .map(|k| {
                    stripe(if dark {
                        moonbow(k as f32 / (TRAIL_STOPS.len() - 1) as f32)
                    } else {
                        PRINT_OFF[k]
                    })
                })
                .collect();
            let worn: std::collections::BTreeSet<u32> =
                trail.iter().flat_map(|q| [q.color, q.color2]).collect();
            assert_eq!(
                worn,
                stops.iter().copied().collect(),
                "dark {dark}, {kitty_rows} rows: the trail is the moonbow's six stops"
            );
            let top = if dark { WHITE } else { PRINT_OFF[0] };
            assert_eq!(stops[0], stripe(top), "white (pearl in print) on top");
            assert_eq!(
                stops[5],
                stripe(if dark { MOONBOW[3] } else { PRINT_OFF[5] }),
                "steel (graphite in print) at the bottom"
            );
            for other in [CardPalette::Cruise, CardPalette::Land] {
                let s = CardScene {
                    palette: other,
                    ..off
                };
                let (under, _, _) = settled(&s);
                let off_by = under
                    .iter()
                    .filter(|q| on_rim(q, &s))
                    .map(|q| gap_to_ramp(q.color, border))
                    .max()
                    .unwrap_or(0);
                assert!(
                    off_by > 40,
                    "dark {dark}: {other:?}'s border is not a moonbow ({off_by})"
                );
            }
        }
    }
}

const PRINT_OFF_MAX_CHROMA: f64 = 0.04;

fn pearl_on_paper(stripes: &[(u32, u8)]) -> Vec<String> {
    use aterm_types::Rgb;
    use aterm_types::scheme_signature::{chroma_hue, oklab, oklab_distance};
    if stripes.len() != TRAIL_STOPS.len() {
        return vec![format!("{} stripes", stripes.len())];
    }
    let mut breaches = Vec::new();
    let paper = |(c, a): (u32, u8)| {
        let ch = |sh: u32| (((c >> sh) & 0xff) + 255 - u32::from(a)).min(255) as u8;
        Rgb::new(ch(16), ch(8), ch(0))
    };
    let on: Vec<Rgb> = stripes.iter().map(|&s| paper(s)).collect();
    for (k, (&(c, a), rgb)) in stripes.iter().zip(&on).enumerate() {
        if a != 255 {
            breaches.push(format!("stripe {k} ({c:06x}) is not opaque: alpha {a}"));
        }
        let px = (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b);
        let (chroma, _) = chroma_hue(*rgb);
        if chroma > PRINT_OFF_MAX_CHROMA {
            breaches.push(format!(
                "stripe {k} ({px:06x}) is tinted: chroma {chroma:.3}"
            ));
        }
        let contrast = 1.05 / (relative_luminance(px) + 0.05);
        if contrast < PRINT_MIN_PAPER_CONTRAST {
            breaches.push(format!(
                "stripe {k} ({px:06x}) vanishes on the paper: {contrast:.2}:1"
            ));
        }
    }
    for i in 0..on.len() {
        for j in i + 1..on.len() {
            let d = oklab_distance(on[i], on[j]);
            if d < PRINT_MIN_STRIPE_DE {
                breaches.push(format!("stripes {i} and {j} are one colour (dE {d:.3})"));
            }
        }
    }
    for k in 1..on.len() {
        let (above, here) = (oklab(on[k - 1]).0, oklab(on[k]).0);
        if here >= above {
            breaches.push(format!(
                "stripe {k} is out of order: L {here:.3} under {above:.3}"
            ));
        }
    }
    breaches
}

#[test]
fn the_light_off_trail_is_pearl_print() {
    for kitty_rows in [2u8, 3] {
        let s = CardScene {
            dark: false,
            palette: CardPalette::Off,
            kitty_rows,
            ..scene()
        };
        let trail = parts(&s, Clock::SETTLED).trail;
        let mut ends: Vec<(u16, u32, u8)> = trail.iter().map(|q| (q.y, q.color, q.alpha)).collect();
        ends.sort_unstable();
        ends.dedup_by_key(|e| (e.1, e.2));
        let stripes: Vec<(u32, u8)> = ends.iter().map(|e| (e.1, e.2)).collect();
        assert_eq!(
            pearl_on_paper(&stripes),
            Vec::<String>::new(),
            "{kitty_rows} rows"
        );
    }
    let lay = |rgb: u32| {
        let p = ink(rgb, PRINT_ALPHA);
        (p.color, p.alpha)
    };
    let count =
        |breaches: &[String], what: &str| breaches.iter().filter(|b| b.contains(what)).count();
    let cruise: Vec<(u32, u8)> = PRINT_CRUISE.iter().map(|&c| lay(c)).collect();
    let cruise = pearl_on_paper(&cruise);
    assert_eq!(count(&cruise, "is tinted"), 6, "{cruise:?}");
    let white = pearl_on_paper(&[lay(WHITE); 6]);
    assert!(breached(&white, "vanishes"), "{white:?}");
    assert!(breached(&white, "one colour"), "{white:?}");
    let moon: Vec<(u32, u8)> = (0..TRAIL_STOPS.len())
        .map(|k| lay(CardPalette::Off.stripe(k, true)))
        .collect();
    let moon = pearl_on_paper(&moon);
    assert_eq!(count(&moon, "vanishes"), 3, "{moon:?}");
    for k in 0..3 {
        assert!(
            breached(&moon, &format!("stripe {k} (")),
            "stripe {k}: {moon:?}"
        );
    }
    let flipped: Vec<(u32, u8)> = PRINT_OFF.iter().rev().map(|&c| lay(c)).collect();
    let flipped = pearl_on_paper(&flipped);
    assert_eq!(count(&flipped, "out of order"), 5, "{flipped:?}");
    let vivid: Vec<(u32, u8)> = PRINT_OFF.iter().map(|&c| lay(c)).collect();
    let vivid = vivid_on_paper(CardPalette::Off, &vivid);
    assert!(breached(&vivid, "washed out"), "{vivid:?}");
}

#[test]
fn a_light_ground_prints_the_rainbow() {
    let mid = Clock::seated(1500 + 750);
    for kitty_rows in [2u8, 3] {
        let s = CardScene {
            dark: false,
            kitty_rows,
            ..scene()
        };
        let rest = parts(&s, Clock::SETTLED);
        assert_eq!(
            stripes_top_down(&rest.trail),
            PRINT_CRUISE.to_vec(),
            "{kitty_rows} rows: red on top"
        );
        for q in rest.trail.iter().chain(&rest.border).chain(&rest.rim) {
            assert_eq!((q.alpha, q.alpha2), (255, 255), "{kitty_rows} rows: {q:?}");
        }
        assert!(
            !PRINT_CRUISE.contains(&InkRole::Leading.ink(spectrum_stop(0))),
            "the light-ink law's red is not printed"
        );
        let gloss = parts(&s, mid).trail;
        let mut lifted = [0.0f32; 6];
        for q in &gloss {
            assert_eq!((q.alpha, q.alpha2), (255, 255), "{kitty_rows} rows: {q:?}");
            for c in [q.color, q.color2] {
                let (gap, k, t) = (0..PRINT_CRUISE.len())
                    .flat_map(|k| (0..=100).map(move |i| (k, i as f32 / 100.0)))
                    .map(|(k, t)| (channel_gap(c, mix_rgb(PRINT_CRUISE[k], WHITE, t)), k, t))
                    .min_by(|a, b| a.0.cmp(&b.0).then(a.2.total_cmp(&b.2)))
                    .expect("inks");
                assert!(gap <= 2, "{kitty_rows} rows: {c:06x} not an ink's gloss");
                lifted[k] = lifted[k].max(t);
            }
        }
        let middle = lifted[1..5].iter().copied().fold(0.0, f32::max);
        assert!(
            middle > 0.3,
            "{kitty_rows} rows: the gloss runs ({lifted:?})"
        );
        for edge in [lifted[0], lifted[5]] {
            assert!(
                edge <= SHIMMER_GAIN * LIGHT_GLINT_EDGE + 0.02 && edge > 0.0,
                "{kitty_rows} rows: an edge stripe glossed {edge} ({lifted:?})"
            );
        }
    }
}

fn channel_gap(a: u32, b: u32) -> u32 {
    [16, 8, 0]
        .iter()
        .map(|sh| ((a >> sh) & 0xff).abs_diff((b >> sh) & 0xff))
        .max()
        .unwrap_or(0)
}

fn stripes_top_down(trail: &[GlowQuad]) -> Vec<u32> {
    let mut trail = trail.to_vec();
    trail.sort_by_key(|q| (q.y, q.x));
    let mut worn: Vec<u32> = trail.iter().map(|q| q.color).collect();
    worn.dedup();
    worn
}

const PRINT_MIN_SATURATION: f64 = 0.6;

const PRINT_MIN_CHROMA: f64 = 0.16;

const PRINT_MIN_STRIPE_DE: f64 = 0.05;

const PRINT_MIN_PAPER_CONTRAST: f32 = 1.4;

fn vivid_on_paper(palette: CardPalette, stripes: &[(u32, u8)]) -> Vec<String> {
    use aterm_types::Rgb;
    use aterm_types::scheme_signature::{chroma_hue, oklab_distance};
    if stripes.len() != TRAIL_STOPS.len() {
        return vec![format!("{} stripes", stripes.len())];
    }
    let mut breaches = Vec::new();
    let paper = |(c, a): (u32, u8)| {
        let ch = |sh: u32| (((c >> sh) & 0xff) + 255 - u32::from(a)).min(255) as u8;
        Rgb::new(ch(16), ch(8), ch(0))
    };
    let on: Vec<Rgb> = stripes.iter().map(|&s| paper(s)).collect();
    let mut hues = Vec::new();
    for (k, (&(c, a), rgb)) in stripes.iter().zip(&on).enumerate() {
        if a != 255 {
            breaches.push(format!("stripe {k} ({c:06x}) is not opaque: alpha {a}"));
        }
        let px = (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b);
        let (_, sat, _) = crate::spectrum::spectrum_hsv(px);
        let (chroma, hue) = chroma_hue(*rgb);
        if sat < PRINT_MIN_SATURATION || chroma < PRINT_MIN_CHROMA {
            breaches.push(format!(
                "stripe {k} ({px:06x}) is washed out: saturation {sat:.2}, chroma {chroma:.3}"
            ));
        }
        let contrast = 1.05 / (relative_luminance(px) + 0.05);
        if contrast < PRINT_MIN_PAPER_CONTRAST {
            breaches.push(format!(
                "stripe {k} ({px:06x}) vanishes on the paper: {contrast:.2}:1"
            ));
        }
        hues.push(hue);
    }
    for i in 0..on.len() {
        for j in i + 1..on.len() {
            let d = oklab_distance(on[i], on[j]);
            if d < PRINT_MIN_STRIPE_DE {
                breaches.push(format!("stripes {i} and {j} are one colour (dE {d:.3})"));
            }
        }
    }
    let step = |a: f64, b: f64| (b - a + 540.0).rem_euclid(360.0) - 180.0;
    let peak = match palette {
        CardPalette::Cruise => hues.len() - 1,
        CardPalette::Land => 3,
        CardPalette::Off => hues.len() - 1,
    };
    for k in 1..hues.len() {
        if (k <= peak) != (step(hues[k - 1], hues[k]) > 0.0) {
            breaches.push(format!("stripe {k}'s hue is out of order: {hues:.1?}"));
        }
    }
    breaches
}

fn breached(breaches: &[String], what: &str) -> bool {
    breaches.iter().any(|b| b.contains(what))
}

#[test]
fn the_light_trail_is_vivid_print() {
    for palette in [CardPalette::Cruise, CardPalette::Land] {
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                dark: false,
                palette,
                kitty_rows,
                ..scene()
            };
            let trail = parts(&s, Clock::SETTLED).trail;
            let mut ends: Vec<(u16, u32, u8)> =
                trail.iter().map(|q| (q.y, q.color, q.alpha)).collect();
            ends.sort_unstable();
            ends.dedup_by_key(|e| (e.1, e.2));
            let stripes: Vec<(u32, u8)> = ends.iter().map(|e| (e.1, e.2)).collect();
            assert_eq!(
                vivid_on_paper(palette, &stripes),
                Vec::<String>::new(),
                "{palette:?}, {kitty_rows} rows"
            );
        }
    }
    const MAIN_TRAIL_LIGHT_ALPHA: f32 = 212.0;
    let lay = |rgb: u32, alpha: f32| {
        let p = ink(rgb, alpha);
        (p.color, p.alpha)
    };
    for palette in [CardPalette::Cruise, CardPalette::Land] {
        let leading = |alpha: f32| -> Vec<(u32, u8)> {
            (0..TRAIL_STOPS.len())
                .map(|k| lay(InkRole::Leading.ink(palette.stripe(k, true)), alpha))
                .collect()
        };
        let main = vivid_on_paper(palette, &leading(MAIN_TRAIL_LIGHT_ALPHA));
        assert!(breached(&main, "not opaque"), "{palette:?}: {main:?}");
        assert!(breached(&main, "washed out"), "{palette:?}: {main:?}");
        let opaque = vivid_on_paper(palette, &leading(PRINT_ALPHA));
        assert!(breached(&opaque, "washed out"), "{palette:?}: {opaque:?}");
        assert!(!breached(&opaque, "not opaque"), "{palette:?}: {opaque:?}");
    }
    let thin: Vec<(u32, u8)> = PRINT_CRUISE
        .iter()
        .map(|&c| lay(c, MAIN_TRAIL_LIGHT_ALPHA))
        .collect();
    let thin = vivid_on_paper(CardPalette::Cruise, &thin);
    assert!(breached(&thin, "not opaque"), "{thin:?}");
    let ramp: Vec<(u32, u8)> = (0..TRAIL_STOPS.len())
        .map(|k| lay(sunset(k as f32 / 5.0), PRINT_ALPHA))
        .collect();
    let ramp = vivid_on_paper(CardPalette::Land, &ramp);
    assert!(breached(&ramp, "2 and 4 are one colour"), "{ramp:?}");
    let flipped: Vec<(u32, u8)> = PRINT_CRUISE
        .iter()
        .rev()
        .map(|&c| lay(c, PRINT_ALPHA))
        .collect();
    let flipped = vivid_on_paper(CardPalette::Cruise, &flipped);
    assert_eq!(
        flipped
            .iter()
            .map(|b| b.contains("out of order"))
            .collect::<Vec<_>>(),
        vec![true; 5],
        "{flipped:?}"
    );
    const LEMON: u32 = 0x00FF_FF00;
    let lemon: Vec<(u32, u8)> = (0..PRINT_CRUISE.len())
        .map(|k| lay(if k == 2 { LEMON } else { PRINT_CRUISE[k] }, PRINT_ALPHA))
        .collect();
    let lemon = vivid_on_paper(CardPalette::Cruise, &lemon);
    assert!(
        lemon.len() == 1 && lemon[0].starts_with("stripe 2 (ffff00) vanishes"),
        "{lemon:?}"
    );
}

#[test]
fn the_light_border_keeps_its_chroma_and_reads_on_white() {
    use aterm_types::Rgb;
    use aterm_types::scheme_signature::chroma_hue;
    const RAMP_MIN_CHROMA: f64 = 0.11;
    let chroma = |c: u32| chroma_hue(Rgb::new((c >> 16) as u8, (c >> 8) as u8, c as u8)).0;
    let n = SPECTRUM_LUT_LEN - 1;
    let low = (0..=n)
        .map(|i| chroma(CardPalette::Cruise.art_at(i as f32 / n as f32, false)))
        .fold(f64::MAX, f64::min);
    assert!(
        low >= RAMP_MIN_CHROMA,
        "the light ramp dips to chroma {low:.3}"
    );
    let plain: Vec<(usize, u32)> = (0..PRINT_CRUISE.len())
        .map(|k| (stripe_at(k), PRINT_CRUISE[k]))
        .collect();
    let plain_low = (0..=n)
        .map(|i| chroma(knot_ramp(&plain, i as f32 / n as f32)))
        .fold(f64::MAX, f64::min);
    assert!(
        plain_low < RAMP_MIN_CHROMA,
        "CONTROL: without the knots the ramp holds {plain_low:.3}"
    );
    let luma = |c: u32| {
        0.2126 * ((c >> 16) & 0xff) as f32
            + 0.7152 * ((c >> 8) & 0xff) as f32
            + 0.0722 * (c & 0xff) as f32
    };
    let on_print = |palette: CardPalette| {
        move |t: f32| ink(print_line(palette.art_at(t, false)), PRINT_ALPHA).color
    };
    let washed = (0..=n)
        .map(|i| {
            let main = InkRole::Leading.ink(CardPalette::Cruise.at(i as f32 / n as f32));
            (
                gap_to_ramp(ink(main, PRINT_ALPHA).color, on_print(CardPalette::Cruise)),
                luma(main),
            )
        })
        .fold((0, 0.0), |(g, y), (g1, y1)| (g.max(g1), f32::max(y, y1)));
    assert!(
        washed.0 > 40 && washed.1 <= LIGHT_LINE_MAX_LUMA,
        "CONTROL: main's border ink, opaque, is {} off the print ramp at luma {}",
        washed.0,
        washed.1
    );
    for palette in [CardPalette::Cruise, CardPalette::Land, CardPalette::Off] {
        for kitty_rows in [2u8, 3] {
            let s = CardScene {
                dark: false,
                palette,
                kitty_rows,
                ..scene()
            };
            let border = parts(&s, Clock::SETTLED).border;
            assert!(!border.is_empty(), "{palette:?} {kitty_rows} rows");
            for q in &border {
                for c in [q.color, q.color2] {
                    let gap = gap_to_ramp(c, on_print(palette));
                    assert!(
                        gap <= 2,
                        "{palette:?} {kitty_rows} rows: border {c:06x} off its print ramp by {gap}"
                    );
                }
            }
            for clock in [
                Clock::SETTLED,
                Clock::seated(900),
                Clock::flowing(600),
                Clock::flowing(1400),
                Clock::flowing(2300),
            ] {
                let p = parts(&s, clock);
                let line = p
                    .border
                    .iter()
                    .flat_map(|q| [q.color, q.color2])
                    .map(luma)
                    .fold(0.0, f32::max);
                assert!(
                    line <= LIGHT_LINE_MAX_LUMA + 1.0,
                    "{palette:?} {kitty_rows} rows: the border's luma reaches {line}"
                );
            }
            let rim = parts(&s, Clock::SETTLED)
                .rim
                .iter()
                .map(|q| luma(q.color))
                .fold(0.0, f32::max);
            if palette == CardPalette::Cruise {
                assert!(
                    rim > LIGHT_LINE_MAX_LUMA + 1.0,
                    "{kitty_rows} rows: the rim keeps the full gold ({rim})"
                );
            }
        }
    }
    let gold = PRINT_CRUISE[2];
    let held = print_line(gold);
    assert!(luma(gold) > LIGHT_LINE_MAX_LUMA && luma(held) <= LIGHT_LINE_MAX_LUMA + 1.0);
    assert!(
        luma(held) >= LIGHT_LINE_MAX_LUMA - 1.0,
        "darkened to the bar, no further"
    );
    let hue = |c: u32| chroma_hue(Rgb::new((c >> 16) as u8, (c >> 8) as u8, c as u8)).1;
    assert!(
        (hue(held) - hue(gold)).abs() < 2.0,
        "the gold keeps its hue"
    );
    assert_eq!(
        print_line(PRINT_CRUISE[4]),
        PRINT_CRUISE[4],
        "blue is under the bar"
    );
}

#[test]
fn a_box_larger_than_its_clip_is_cut_to_it() {
    let whole = scene();
    let cut = clipped();
    let (cx0, cy0, cx1, cy1) = cut.clip;
    let t0 = Instant::now();
    for at in [0, 150, 1300, EPISODE_MS + 1] {
        let (mut a, mut b) = (HarnessCard::default(), HarnessCard::default());
        a.arm(t0, 7);
        b.arm(t0, 7);
        let (wu, wo, _) = frame(&mut a, t0 + ms(at), &whole, true);
        let (cu, co, fp) = frame(&mut b, t0 + ms(at), &cut, true);
        assert_ne!(fp, 0, "{at} ms: the pane's part is drawn");
        for q in cu.iter().chain(&co) {
            assert_in_band_and_box(q, &cut, &format!("{at} ms"));
        }
        let kitty = |v: &[GlowQuad]| -> Vec<GlowQuad> {
            v.iter().filter(|q| q.alpha == 255).copied().collect()
        };
        assert!(!kitty(&co).is_empty(), "{at} ms: the kitty fits the clip");
        assert_eq!(kitty(&wo), kitty(&co), "{at} ms");
        let inside = |q: &GlowQuad| {
            i32::from(q.x) >= cx0
                && i32::from(q.x) + i32::from(q.w) <= cx1
                && i32::from(q.y) >= cy0
                && i32::from(q.y) + i32::from(q.h) <= cy1
        };
        for q in wu.iter().filter(|q| inside(q)) {
            assert!(cu.contains(q), "{at} ms: {q:?} lost inside the clip");
        }
        assert!(
            wu.iter().any(|q| !inside(q)),
            "{at} ms: the whole box reaches past the clip"
        );
    }
    let g = Geo::new(&whole).expect("a scene");
    let (cu, _, _) = settled(&cut);
    assert!(
        cu.iter()
            .all(|q| i64::from(q.x) + i64::from(q.w) < g.outer.x1 - g.t)
    );
    let (wu, _, _) = settled(&whole);
    let top = g.outer.y0;
    let edge = |q: &&GlowQuad| i64::from(q.y) == top && i32::from(q.x) < cx1;
    let cut_seg = cu
        .iter()
        .filter(edge)
        .find(|q| i32::from(q.x) + i32::from(q.w) == cx1)
        .expect("a top segment cut at the clip");
    let whole_seg = wu
        .iter()
        .filter(edge)
        .find(|q| q.x == cut_seg.x && i32::from(q.x) + i32::from(q.w) > cx1)
        .expect("the same segment drawn whole");
    assert_eq!(cut_seg.color, whole_seg.color, "the left end stays");
    for sh in [16, 8, 0] {
        let ch = |c: u32| (c >> sh) & 0xff;
        let (a, b) = (ch(whole_seg.color), ch(whole_seg.color2));
        let at = ch(cut_seg.color2);
        assert!(
            a.min(b) <= at && at <= a.max(b),
            "the cut end {:06x} is on the ramp {:06x} → {:06x}",
            cut_seg.color2,
            whole_seg.color,
            whole_seg.color2
        );
    }
    for clip in [(0, 0, 5, 5), (cx1, cy0, cx0, cy1), (0, 0, 0, 0)] {
        let s = CardScene { clip, ..whole };
        let mut card = HarnessCard::default();
        card.arm(t0, 7);
        assert_eq!(
            frame(&mut card, t0 + ms(100), &s, true),
            (Vec::new(), Vec::new(), 0),
            "{clip:?}"
        );
    }
}

fn coverage(quads: &[GlowQuad]) -> std::collections::BTreeMap<(i64, i64), usize> {
    let mut seen = std::collections::BTreeMap::new();
    for q in quads {
        for y in i64::from(q.y)..i64::from(q.y) + i64::from(q.h) {
            for x in i64::from(q.x)..i64::from(q.x) + i64::from(q.w) {
                *seen.entry((x, y)).or_insert(0) += 1;
            }
        }
    }
    seen
}

const fn covers(q: &GlowQuad, (x, y): (i64, i64)) -> bool {
    let (x0, y0) = (q.x as i64, q.y as i64);
    x >= x0 && x < x0 + q.w as i64 && y >= y0 && y < y0 + q.h as i64
}

const fn overlap(a: &GlowQuad, b: &GlowQuad) -> bool {
    const fn meet(p0: u16, n0: u16, p1: u16, n1: u16) -> bool {
        let lo = if p0 > p1 { p0 } else { p1 };
        let end0 = p0 as u32 + n0 as u32;
        let end1 = p1 as u32 + n1 as u32;
        let hi = if end0 < end1 { end0 } else { end1 };
        (lo as u32) < hi
    }
    meet(a.x, a.w, b.x, b.w) && meet(a.y, a.h, b.y, b.h)
}

fn rim_ring(
    at: &KittyAt,
    frame: &[&str],
    rim: i64,
    inside: Rect,
) -> std::collections::BTreeSet<(i64, i64)> {
    let p = at.p;
    let cell = |x: i64, y: i64| {
        let (dx, dy) = (x - at.rect.x0, y - at.rect.y0);
        (dx >= 0 && dy >= 0).then(|| (dy / p, dx / p))
    };
    let solid = |x: i64, y: i64| cell(x, y).is_some_and(|(r, c)| rim_solid(frame, r, c));
    let notch = |r: i64, c: i64| rim_solid(frame, r, c) && !art_at(frame, r, c);
    let by_a_notch = |x: i64, y: i64| {
        let c = (x - at.rect.x0).div_euclid(p);
        let r = (y - at.rect.y0).div_euclid(p);
        notch(r - 1, c) || notch(r + 1, c)
    };
    let mut ring = std::collections::BTreeSet::new();
    for y in at.rect.y0 - rim..at.rect.y1 + rim {
        for x in at.rect.x0 - rim..at.rect.x1 + rim {
            if solid(x, y) || !inside.contains(x, y) || by_a_notch(x, y) {
                continue;
            }
            let near = (-rim..=rim).any(|dy| (-rim..=rim).any(|dx| solid(x + dx, y + dy)));
            if near {
                ring.insert((x, y));
            }
        }
    }
    ring
}

fn rings_once(
    quads: &[GlowQuad],
    ring: &std::collections::BTreeSet<(i64, i64)>,
) -> Result<(), String> {
    let lit = coverage(quads);
    if let Some((px, n)) = lit.iter().find(|(_, n)| **n > 1) {
        return Err(format!("{px:?} lit {n} times"));
    }
    let lit: std::collections::BTreeSet<_> = lit.into_keys().collect();
    if let Some(px) = lit.difference(ring).next() {
        return Err(format!("{px:?} lit off the ring"));
    }
    if let Some(px) = ring.difference(&lit).next() {
        return Err(format!("{px:?} on the ring left dark"));
    }
    Ok(())
}

#[test]
fn the_rim_rings_the_silhouette_once() {
    let mut checked = 0;
    let mut on_trail = 0;
    for (cw, ch) in [(9u16, 18u16), (8, 16), (16, 33), (17, 35), (20, 44)] {
        for kitty_rows in [2u8, 3] {
            for dark in [true, false] {
                let s = CardScene {
                    cw,
                    ch,
                    kitty_rows,
                    dark,
                    kitty_col: Some(44),
                    trail: Some((32, 44)),
                    ..scene()
                };
                let g = Geo::new(&s).expect("a scene");
                let at = kitty_rect(&g, &s).expect("a kitty");
                let rim = rim_px(at.p, g.t);
                assert!(rim >= 1, "{ch} px: a rim");
                for (name, t_ms) in [
                    ("open", None),
                    ("blink", Some(BLINK_AT_MS + 10)),
                    ("flick", Some(FLICK_AT_MS + 10)),
                ] {
                    let clock = t_ms.map_or(Clock::SETTLED, Clock::riding);
                    let frame = at.art.frame(kitty_frame(clock));
                    let at = ridden(&g, &s, at, clock);
                    let drawn = parts(&s, clock);
                    let quads = drawn.rim;
                    let mut ring = rim_ring(&at, frame, rim, g.inner().meet(g.clip));
                    let trail = coverage(&drawn.trail);
                    let before = ring.len();
                    ring.retain(|px| !trail.contains_key(px));
                    on_trail += before - ring.len();
                    let what = format!("{cw}x{ch}, {kitty_rows} rows, dark {dark}, {name}");
                    rings_once(&quads, &ring).unwrap_or_else(|e| panic!("{what}: {e}"));
                    for q in &quads {
                        assert_in_band_and_box(q, &s, &what);
                        if dark {
                            assert_eq!((q.alpha, q.alpha2), (0, 0), "{what}: light");
                        } else {
                            assert_eq!((q.alpha, q.alpha2), (255, 255), "{what}: print {q:?}");
                        }
                    }
                    let hues: std::collections::BTreeSet<u32> =
                        quads.iter().map(|q| q.color).collect();
                    assert!(hues.len() > 3, "{what}: a spectrum, {} hues", hues.len());
                    for c in &hues {
                        let gap = gap_to_ramp(*c, |t| {
                            if dark {
                                light(spectrum(t), f32::from(RIM_COV)).color
                            } else {
                                ink(CardPalette::Cruise.art_at(t, false), PRINT_ALPHA).color
                            }
                        });
                        assert!(gap <= 2, "{what}: {c:06x} off the spectrum by {gap}");
                    }
                    let mut short = quads.clone();
                    short.pop();
                    assert!(rings_once(&short, &ring).is_err(), "{what}: CONTROL short");
                    let mut twice = quads.clone();
                    twice.push(quads[0]);
                    assert!(rings_once(&twice, &ring).is_err(), "{what}: CONTROL twice");
                    checked += 1;
                }
            }
        }
    }
    assert_eq!(checked, 5 * 2 * 2 * 3);
    assert!(on_trail > 0, "the tail's ring meets the trail somewhere");
    let tiny = CardScene {
        cw: 8,
        ch: 14,
        ..scene()
    };
    assert_eq!(kitty_px(14), 1);
    assert!(
        parts(&tiny, Clock::SETTLED).rim.is_empty(),
        "no rim at one-pixel art"
    );
}

#[test]
fn the_rim_never_lies_on_the_trail() {
    for (cw, ch) in [(9u16, 18u16), (16, 33), (20, 44)] {
        for kitty_rows in [2u8, 3] {
            for dark in [true, false] {
                let s = CardScene {
                    cw,
                    ch,
                    kitty_rows,
                    dark,
                    kitty_col: Some(44),
                    trail: Some((32, 44)),
                    ..scene()
                };
                let what = format!("{cw}x{ch}, {kitty_rows} rows, dark {dark}");
                let clocks = (0..EPISODE_MS)
                    .step_by(FRAME_MS as usize)
                    .flat_map(|t| [Clock::riding(t), Clock::seated(t)])
                    .chain([Clock::SETTLED]);
                for clock in clocks {
                    let p = parts(&s, clock);
                    let both = p
                        .rim
                        .iter()
                        .flat_map(|r| p.trail.iter().map(move |t| (r, t)))
                        .find(|(r, t)| overlap(r, t));
                    assert_eq!(both, None, "{what}: {clock:?}");
                }
                let g = Geo::new(&s).expect("a scene");
                let seat = kitty_rect(&g, &s).expect("a kitty");
                let flick = Clock::seated(FLICK_AT_MS + 10);
                let ring = rim_ring(
                    &seat,
                    seat.art.frame(kitty_frame(flick)),
                    rim_px(seat.p, g.t),
                    g.inner(),
                );
                let trail = parts(&s, flick).trail;
                assert!(
                    ring.iter().any(|&px| trail.iter().any(|q| covers(q, px))),
                    "{what}: CONTROL"
                );
            }
        }
    }
}

#[test]
fn the_toes_gaps_open_onto_the_ground() {
    let (mut gaps, mut paws) = (0, 0);
    for (cw, ch) in [(9u16, 18u16), (16, 33), (20, 44)] {
        for kitty_rows in [2u8, 3] {
            for dark in [true, false] {
                let s = CardScene {
                    cw,
                    ch,
                    kitty_rows,
                    dark,
                    ..scene()
                };
                let g = Geo::new(&s).expect("a scene");
                let seat = kitty_rect(&g, &s).expect("a kitty");
                let (p, rim) = (seat.p, rim_px(seat.p, g.t));
                for clock in [
                    Clock::SETTLED,
                    Clock::seated(BLINK_AT_MS + 10),
                    Clock::seated(FLICK_AT_MS + 10),
                ] {
                    let frame = seat.art.frame(kitty_frame(clock));
                    let lit = coverage(&parts(&s, clock).rim);
                    let what = format!("{cw}x{ch}, {kitty_rows} rows, dark {dark}");
                    let under = |r: i64, c: i64| {
                        let y = seat.rect.y0 + (r + 1) * p;
                        (seat.rect.x0 + c * p..seat.rect.x0 + (c + 1) * p)
                            .flat_map(move |x| (y..y + rim).map(move |y| (x, y)))
                    };
                    for (r, row) in frame.iter().enumerate() {
                        let r = r as i64;
                        for c in 0..row.len() as i64 {
                            if art_at(frame, r, c) || !rim_solid(frame, r, c) {
                                continue;
                            }
                            gaps += 1;
                            assert!(
                                under(r, c).all(|px| !lit.contains_key(&px)),
                                "{what}: the gap at ({r}, {c}) is lined beneath"
                            );
                            for side in [c - 1, c + 1] {
                                if rim_solid(frame, r + 1, side) {
                                    continue;
                                }
                                paws += 1;
                                assert!(
                                    under(r, side).all(|px| lit.contains_key(&px)),
                                    "{what}: CONTROL, the paw at ({r}, {side})"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        gaps > 0 && paws > 0,
        "CONTROL: {gaps} gaps, {paws} paws beside them"
    );
}

#[test]
fn the_trail_spans_the_kittys_body() {
    for kitty_rows in [2u8, 3] {
        for (cw, ch) in [(9u16, 18u16), (16, 33), (17, 35)] {
            let s = CardScene {
                cw,
                ch,
                kitty_rows,
                kitty_col: Some(44),
                trail: Some((32, 44)),
                ..scene()
            };
            let g = Geo::new(&s).expect("a scene");
            let at = kitty_rect(&g, &s).expect("a kitty");
            let (b0, b1) = at.body();
            let what = format!("{cw}x{ch}, {kitty_rows} rows");
            let settled = parts(&s, Clock::SETTLED).trail;
            let y0 = settled
                .iter()
                .map(|q| i64::from(q.y))
                .min()
                .expect("a trail");
            let y1 = settled
                .iter()
                .map(|q| i64::from(q.y) + i64::from(q.h))
                .max()
                .expect("a trail");
            assert_eq!((y0, y1), (b0, b1), "{what}: the body band");
            assert!(
                at.rect.y0 <= y0 && y1 <= at.rect.y1,
                "{what}: inside the cat's height"
            );
            assert!(
                y1 - y0 < 2 * i64::from(ch) * 3 / 4,
                "{what}: narrower than two rows"
            );
            let x0 = g.col(32);
            assert!(
                settled
                    .iter()
                    .all(|q| i64::from(q.x) == x0 && i64::from(q.x) + i64::from(q.w) == at.rear()),
                "{what}: each stripe one quad from the trail's start to the kitty's rear"
            );
            for r in at.art.body.0..at.art.body.0 + at.art.body.1 {
                let first = at.art.open[r].find(|c| c != '.').expect("a body row") as i64;
                let x = at.rect.x0 + first * at.p - 1;
                let mut rows = at.rect.y0 + r as i64 * at.p..at.rect.y0 + (r as i64 + 1) * at.p;
                assert!(
                    rows.all(|y| settled.iter().any(|q| covers(q, (x, y)))),
                    "{what}: ground between the trail and the art's row {r}"
                );
            }
            let crossings = (g.top + 1..g.top + 3)
                .filter(|&r| (y0 + 1..y1).contains(&g.band(r)))
                .count();
            assert!(
                settled.len() <= TRAIL_STOPS.len() + crossings,
                "{what}: {} quads, six stripes and {crossings} band crossings",
                settled.len()
            );
            let colours: std::collections::BTreeSet<u32> =
                settled.iter().map(|q| q.color).collect();
            assert_eq!(colours.len(), TRAIL_STOPS.len(), "{what}: six stripes");
            for t in [100, 1500, 3000, WAVE_END_MS + 50] {
                let ep = parts(&s, Clock::riding(t)).trail;
                assert!(
                    ep.len() > settled.len(),
                    "{what}: CONTROL, segments at {t} ms"
                );
                let amp = (i64::from(ch) / 16).max(1);
                let under_cat = ridden(&g, &s, at, Clock::riding(t)).rect.x0;
                for q in &ep {
                    assert!(
                        i64::from(q.y) >= b0 - amp && i64::from(q.y) + i64::from(q.h) <= b1 + amp,
                        "{what}: {q:?} strays from the band at {t} ms"
                    );
                    if i64::from(q.x) + i64::from(q.w) > under_cat {
                        assert!(
                            i64::from(q.y) >= b0 && i64::from(q.y) + i64::from(q.h) <= b1,
                            "{what}: {q:?} steps under the kitty at {t} ms"
                        );
                    }
                }
            }
        }
    }
}
