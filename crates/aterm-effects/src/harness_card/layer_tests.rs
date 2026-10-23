// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The card's fx layers held to the quad oracle and to the card's laws (`docs/DESIGN-card-engine-2026-10-19.md` §7, "B, the card").

use std::collections::BTreeSet;
use std::sync::Arc;

use aterm_core::render::{FxContent, FxInk, FxItem, FxLayer, FxSprite, PxRect};
use aterm_render::fx::{paint_layers, paint_quads};
use aterm_render::test_counters::take_art_builds;
use aterm_render::{FxFrame, GlowQuad};

use super::layers::{kitty_art, one_star, rim_art, rim_cover};
use super::oracle::{Sink, kitty_quads, lay_star, quads, rim_quads};
use super::tests::{aired, clipped, scene, scenes, widest};
use super::*;
use crate::cursor_glow::Geom;
use crate::effect_util::{push_twinkle_star, star_pieces};

/// SplitMix64: a deterministic stream for the random frames.
fn rng(seed: u64) -> impl FnMut() -> u64 {
    let mut s = seed;
    move || {
        s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// The card's fx frame of `s` at `clock` for `seed`, filled in `frames` as
/// [`HarnessCard::draw`] fills it: its shape's card-local frame, laid at the
/// scene's origin.
fn fx_frame(frames: &mut CardFrames, s: &CardScene, clock: Clock, seed: u64) -> Option<CardFrame> {
    let (shape, origin) = CardShape::of(s);
    let local = shape.scene();
    let geo = Geo::new(local)?;
    let (under, over) = frames.fill(&Emit::new(local, geo, clock, seed));
    Some(CardFrame {
        under,
        over,
        origin,
    })
}

/// The oracle's quads of the same frame, in the scene's own pixels.
fn oracle_quads(s: &CardScene, clock: Clock, seed: u64) -> (Vec<GlowQuad>, Vec<GlowQuad>) {
    let (mut under, mut over) = (Vec::new(), Vec::new());
    if let Some(geo) = Geo::new(s) {
        quads(&Emit::new(s, geo, clock, seed), &mut under, &mut over);
    }
    (under, over)
}

/// The scene's pane as an fx rectangle.
fn pane_of(s: &CardScene) -> PxRect {
    PxRect::new(s.clip.0, s.clip.1, s.clip.2, s.clip.3)
}

/// A quad's pixels as an fx rectangle.
fn quad_rect(q: &GlowQuad) -> PxRect {
    PxRect::at(
        i32::from(q.x),
        i32::from(q.y),
        u32::from(q.w),
        u32::from(q.h),
    )
}

/// A patch of the scene's pixel space, `[x0, x0 + w) × [y0, y0 + h)`, that
/// both sides are painted on.
#[derive(Clone, Copy, Debug)]
struct Patch {
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
}

impl Patch {
    /// The hull of everything either side could paint: the quads, and the
    /// frame's two contents at its origin within the pane.
    fn around(quads: &[&[GlowQuad]], frame: Option<&CardFrame>, pane: PxRect) -> Option<Self> {
        let mut hull = quads
            .iter()
            .flat_map(|qs| qs.iter())
            .fold(PxRect::EMPTY, |h, q| h.hull(quad_rect(q)));
        if let Some(f) = frame {
            for c in [&f.under, &f.over] {
                let e = c.extent().translate(f.origin.0, f.origin.1).intersect(pane);
                hull = hull.hull(e);
            }
        }
        let hull = hull.intersect(PxRect::new(0, 0, i32::MAX, i32::MAX));
        if hull.is_empty() {
            return None;
        }
        Some(Self {
            x0: hull.x0,
            y0: hull.y0,
            w: (i64::from(hull.x1) - i64::from(hull.x0)) as usize,
            h: (i64::from(hull.y1) - i64::from(hull.y0)) as usize,
        })
    }

    /// The patch's ground: a dark or a light one, every pixel its own, so a
    /// mark painted the wrong way (added where it should cover, or over a
    /// pixel off by one) shows.
    fn ground(self, dark: bool) -> Vec<u32> {
        let mut px = Vec::with_capacity(self.w * self.h);
        for y in 0..self.h {
            for x in 0..self.w {
                let (ax, ay) = (self.x0 as u32 + x as u32, self.y0 as u32 + y as u32);
                let n = ax.wrapping_mul(0x9E37_79B1) ^ ay.wrapping_mul(0x85EB_CA77);
                let n = n ^ (n >> 15);
                let base = if dark { 0x0C } else { 0xD4 };
                let c = |s: u32| base + ((n >> s) & 0x1F);
                px.push((c(0) << 16) | (c(5) << 8) | c(10));
            }
        }
        px
    }

    /// The oracle's quads painted over the ground: `under`, then `over`, each
    /// by the glow streams' one law.
    fn paint_quads(self, under: &[GlowQuad], over: &[GlowQuad], dark: bool) -> Vec<u32> {
        let mut px = self.ground(dark);
        let shift = |q: &GlowQuad| GlowQuad {
            x: (i32::from(q.x) - self.x0) as u16,
            y: (i32::from(q.y) - self.y0) as u16,
            ..*q
        };
        for stream in [under, over] {
            let moved: Vec<GlowQuad> = stream.iter().map(shift).collect();
            paint_quads(&mut px, self.w, self.h, &moved, (0, 0));
        }
        px
    }

    /// `frame`'s two layers ([`CardFrame::layers`], the grid at the
    /// window's origin) painted over the ground by the renderers' CPU path,
    /// every band open.
    fn paint_frame(self, frame: &CardFrame, pane: PxRect, ch: u16, dark: bool) -> Vec<u32> {
        let mut px = self.ground(dark);
        let layers = frame.layers(1, pane, (0, 0)).map(|l| self.moved(l));
        paint_layers(&mut px, &self.frame(ch), &layers, &[]);
        px
    }

    /// `content` alone as an Over layer at `origin`, painted over the ground.
    fn paint_content(
        self,
        content: FxContent,
        origin: (i32, i32),
        pane: PxRect,
        ch: u16,
        dark: bool,
    ) -> Vec<u32> {
        let mut px = self.ground(dark);
        let frame = CardFrame {
            under: Arc::new(FxContent::default()),
            over: Arc::new(content),
            origin,
        };
        let layers = frame.layers(1, pane, (0, 0)).map(|l| self.moved(l));
        paint_layers(&mut px, &self.frame(ch), &layers, &[]);
        px
    }

    /// `layer` moved into the patch's pixels.
    fn moved(self, mut l: FxLayer) -> FxLayer {
        l.offset = (l.offset.0 - self.x0, l.offset.1 - self.y0);
        l.clip = l.clip.translate(-self.x0, -self.y0);
        l
    }

    /// The patch as a frame for the renderers' slicer: every band open.
    fn frame(self, ch: u16) -> FxFrame {
        let cell_h = usize::from(ch).max(1);
        FxFrame {
            w: self.w,
            h: self.h,
            pad: 0,
            grid_top: 0,
            cell_h,
            rows: self.h / cell_h + 1,
        }
    }

    /// The first pixel where `a` and `b` differ, in the scene's pixels.
    fn first_difference(self, a: &[u32], b: &[u32]) -> Option<(i32, i32, u32, u32)> {
        let i = a.iter().zip(b).position(|(x, y)| x != y)?;
        Some((
            self.x0 + (i % self.w) as i32,
            self.y0 + (i / self.w) as i32,
            a[i],
            b[i],
        ))
    }
}

/// What a comparison saw, for the non-vacuity checks.
#[derive(Debug, Default)]
struct Seen {
    frames: usize,
    painted: usize,
    two_tone: usize,
    rim_cut: usize,
    peeled: usize,
    folded: usize,
    row0_air: usize,
    heights: BTreeSet<u16>,
    most_items: usize,
}

/// Hold `s`'s frame at `clock` for `seed` to the oracle's, pixel for pixel,
/// on a ground of the scene's polarity; and every pixel either paints to
/// the card's box (its air included) within the pane.
fn assert_same_pixels(
    frames: &mut CardFrames,
    s: &CardScene,
    clock: Clock,
    seed: u64,
    seen: &mut Seen,
    what: &str,
) {
    let (under, over) = oracle_quads(s, clock, seed);
    let frame = fx_frame(frames, s, clock, seed);
    let pane = pane_of(s);
    let Some(patch) = Patch::around(&[&under, &over], frame.as_ref(), pane) else {
        assert!(
            frame.as_ref().is_none_or(CardFrame::is_empty),
            "{what}: layers where the oracle lays nothing"
        );
        return;
    };
    let frame = frame.unwrap_or_else(|| panic!("{what}: the oracle lays quads, the layers none"));
    let want = patch.paint_quads(&under, &over, s.dark);
    let got = patch.paint_frame(&frame, pane, s.ch, s.dark);
    if let Some((x, y, a, b)) = patch.first_difference(&want, &got) {
        panic!(
            "{what}: at ({x}, {y}) the quads paint {a:06x}, the layers {b:06x} \
             ({} under + {} over quads; {} + {} items)",
            under.len(),
            over.len(),
            frame.under.items().len(),
            frame.over.items().len()
        );
    }
    let ground = patch.ground(s.dark);
    let (shape, origin) = CardShape::of(s);
    let outer = Geo::new(shape.scene()).expect("a frame was drawn").outer;
    let card = PxRect::new(
        outer.x0 as i32 + origin.0,
        outer.y0 as i32 + origin.1,
        outer.x1 as i32 + origin.0,
        outer.y1 as i32 + origin.1,
    )
    .intersect(pane);
    for (i, (g, p)) in ground.iter().zip(&got).enumerate() {
        if g != p {
            let (x, y) = (
                patch.x0 + (i % patch.w) as i32,
                patch.y0 + (i / patch.w) as i32,
            );
            assert!(
                holds(card, i64::from(x), i64::from(y)),
                "{what}: a pixel painted at ({x}, {y}) outside the box {card:?}"
            );
            seen.painted += 1;
        }
    }
    seen.frames += 1;
    seen.heights.insert(s.ch);
    let items = frame.under.items().len() + frame.over.items().len();
    seen.most_items = seen.most_items.max(items);
    assert!(items <= CARD_ITEMS_MAX, "{what}: {items} items");
    seen.row0_air += usize::from(s.top == 0 && s.pad_above > 0);
    let m = marks(frames, &frame, s, clock, seed);
    seen.rim_cut += usize::from(m.rims.len() > 1);
    if s.dark {
        seen.folded += m.stars.len();
    } else {
        seen.peeled += m.stars.len();
        let palette = frame.over.palette();
        seen.two_tone += usize::from(m.stars.iter().any(|sp| {
            palette
                .get(sp.ink as usize)
                .is_some_and(|i| i.alpha > 0 && i.color == premul_rgb(WHITE, i.alpha))
        }));
    }
}

/// Whether `r` holds the pixel `(x, y)`.
fn holds(r: PxRect, x: i64, y: i64) -> bool {
    i64::from(r.x0) <= x && x < i64::from(r.x1) && i64::from(r.y0) <= y && y < i64::from(r.y1)
}

/// A frame's over sprites told apart by their arts: the kitty's, the rim's
/// and the stars'.
#[derive(Debug, Default)]
struct Marks {
    kitty: Option<FxSprite>,
    rims: Vec<FxSprite>,
    stars: Vec<FxSprite>,
}

/// [`Marks`] of `frame`, `s`'s frame at `clock` for `seed` drawn with
/// `frames`' arts.
fn marks(frames: &CardFrames, frame: &CardFrame, s: &CardScene, clock: Clock, seed: u64) -> Marks {
    let mut m = Marks::default();
    let (shape, _) = CardShape::of(s);
    let local = shape.scene();
    let Some(geo) = Geo::new(local) else {
        return m;
    };
    let cx = Emit::new(local, geo, clock, seed);
    let arts = frames.arts().expect("a frame was drawn");
    let (kitty, rim) = match cx.kitty {
        Some((at, pose)) => (
            Some(kitty_art(arts, &at.art, pose)),
            rim_art(arts, &at.art, pose).map(|r| r.0),
        ),
        None => (None, None),
    };
    for item in frame.over.items() {
        let FxItem::Sprite(sp) = item else {
            continue;
        };
        if kitty.as_ref().is_some_and(|k| Arc::ptr_eq(k, &sp.art)) {
            m.kitty = Some(sp.clone());
        } else if rim.as_ref().is_some_and(|r| Arc::ptr_eq(r, &sp.art)) {
            m.rims.push(sp.clone());
        } else {
            m.stars.push(sp.clone());
        }
    }
    m
}

/// The sprites of `content` that draw `art`.
fn sprites_of<'a>(
    content: &'a FxContent,
    art: &'a Arc<aterm_core::render::FxArt>,
) -> impl Iterator<Item = &'a FxSprite> {
    content.items().iter().filter_map(move |i| match i {
        FxItem::Sprite(sp) if Arc::ptr_eq(&sp.art, art) => Some(sp),
        _ => None,
    })
}

/// The frames an episode shows: the settled frame, the ride every 100 ms
/// through the burst, the seated blink and flick, and its last frame.
fn clocks() -> Vec<Clock> {
    let mut v = vec![Clock::SETTLED];
    v.extend((0..=RIDE_MS + BURST_MS).step_by(100).map(Clock::riding));
    v.extend([
        Clock::seated(BLINK_AT_MS + 10),
        Clock::seated(FLICK_AT_MS + 10),
        Clock::riding(EPISODE_MS - 40),
    ]);
    v
}

/// A random card and frame for `seed`, laid as the host lays one
/// (`claude_card`'s `layout`): its cell height `8 + seed % 89`, so 89
/// seeds in a row cover every height from 8 to 96; the text, the trail
/// (eight to twenty cells, or none), the kitty's height, the rows, the air
/// (at the pane's first row too), the padding, the origin, a pane that
/// cuts the box or not, both themes and palettes, serious mode now and
/// then, the colours and the instant all drawn from the seed.
fn random_frame(seed: u64) -> (CardScene, Clock) {
    let mut next = rng(seed);
    let mut pick = |n: u64| next() % n;
    let ch = 8 + (seed % 89) as u16;
    let cw = (u32::from(ch) * (40 + pick(21) as u32) / 100).max(4) as u16;
    let kc = kitty_cols(cw, ch);
    let text_end = 6 + pick(24) as u16;
    let start = text_end + 2;
    let with_kitty = pick(8) != 0;
    let len = 8 + pick(13) as u16;
    let kitty_col = start + len;
    let cols = if with_kitty {
        (kitty_col + kc + 4).max(text_end)
    } else {
        text_end
    };
    let card_rows = 3 + pick(2) as u16;
    let kitty_rows = if card_rows >= 3 && pick(2) == 0 { 3 } else { 2 };
    let top = if pick(3) == 0 { 0 } else { 1 + pick(4) as u16 };
    let air = |p: &mut dyn FnMut(u64) -> u64| if p(2) == 0 { ch } else { 0 };
    let pad_above = air(&mut pick);
    let pad_below = air(&mut pick);
    let pad_left = if pick(2) == 0 {
        0
    } else {
        pick(u64::from(cw)) as u16
    };
    let origin_x = i32::from(pad_left) + pick(40) as i32;
    let origin_y = pick(80) as i32;
    let box_w = i32::from(pad_left) + (i32::from(cols) + 1) * i32::from(cw);
    let box_h = (i32::from(top) + i32::from(card_rows) + 1) * i32::from(ch);
    let clip = match pick(4) {
        0 => (
            origin_x - i32::from(pad_left) + pick(40) as i32,
            origin_y + pick(u64::from(ch)) as i32,
            origin_x + box_w - pick(u64::from(cw) * 3) as i32,
            origin_y + box_h - pick(u64::from(ch)) as i32,
        ),
        1 => (
            origin_x - i32::from(pad_left),
            origin_y,
            origin_x + 400 * i32::from(cw),
            origin_y + 200 * i32::from(ch),
        ),
        _ => (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
    };
    let dark = pick(2) == 0;
    let palette = match pick(3) {
        0 => CardPalette::Cruise,
        1 => CardPalette::Land,
        _ => CardPalette::Off,
    };
    let wand = (pick(3) != 0).then_some((pick(3) as u16, top));
    let s = CardScene {
        origin_x,
        origin_y,
        cw,
        ch,
        row_base: pick(5) as u16,
        pad_left,
        pad_above,
        pad_below,
        top,
        rows: card_rows,
        cols,
        trail: with_kitty.then_some((start, kitty_col)),
        kitty_col: with_kitty.then_some(kitty_col),
        kitty_rows,
        clip,
        palette,
        dark,
        ground: if dark { 0x0012_1721 } else { 0x00F8_FAFD },
        serious: pick(12) == 0,
        coat: (next() & 0x00FF_FFFF) as u32,
        eye: (next() & 0x00FF_FFFF) as u32,
        wand,
    };
    let t = next() % EPISODE_MS;
    let clock = match next() % 5 {
        0 => Clock::SETTLED,
        1 => Clock::seated(t),
        2 => Clock::flowing(next() % (4 * crate::ripple::GLINT_PERIOD_MS)),
        _ => Clock::riding(t % (RIDE_MS + BURST_MS + 400)),
    };
    (s, clock)
}

/// THE CARD'S LAYERS EQUAL THE LEGACY QUADS (design §7, migration
/// exactness): on a textured ground of its polarity, the card's two fx
/// layers, painted by the renderers' CPU slicer and stamp, put on the glass
/// exactly the pixels the quad oracle's frame puts there — every law scene
/// (both themes and palettes, both kitties, the box's air, a pane that cuts
/// it) through an episode on two seeds, and 300 seeded random frames that
/// cover every cell height from 8 to 96 (the card at the pane's first row
/// with air under it among them): the border's re-ramped edges and per-band
/// sides, the stepping trail, the kitty, its rim cut round the trail, the
/// stars folded on dark and peeled in two tones on light.
#[test]
fn the_card_layers_equal_the_legacy_quads() {
    let mut frames = CardFrames::default();
    let mut seen = Seen::default();
    let law_scenes: Vec<CardScene> = scenes()
        .into_iter()
        .chain(scenes().into_iter().map(aired))
        .chain([clipped()])
        .collect();
    for (i, s) in law_scenes.iter().enumerate() {
        for seed in [7u64, 42] {
            for clock in clocks() {
                let what = format!("law scene {i} ({}x{}), seed {seed}, {clock:?}", s.cw, s.ch);
                assert_same_pixels(&mut frames, s, clock, seed, &mut seen, &what);
            }
        }
    }
    for seed in 0..300u64 {
        let (s, clock) = random_frame(seed);
        let what = format!("random frame {seed}: {s:?} at {clock:?}");
        assert_same_pixels(&mut frames, &s, clock, seed, &mut seen, &what);
    }
    let long = CardScene {
        trail: Some((32, 52)),
        kitty_col: Some(52),
        cols: 63,
        ..scene()
    };
    let seed = 0x3162_8AF6_3C6D_78E5;
    let geo = Geo::new(&long).expect("a scene");
    assert_eq!(glint_spots(&geo, &long, seed).n, GLINTS_MAX);
    for clock in [Clock::SETTLED, Clock::seated(2100)] {
        assert_same_pixels(
            &mut frames,
            &long,
            clock,
            seed,
            &mut seen,
            "captured extreme stripe seed",
        );
    }
    assert_eq!(
        seen.heights.range(8..=96).count(),
        89,
        "every cell height from 8 to 96"
    );
    assert!(
        seen.frames > 2000
            && seen.painted > 0
            && seen.two_tone > 0
            && seen.rim_cut > 0
            && seen.peeled > 0
            && seen.folded > 0
            && seen.row0_air > 0,
        "CONTROL: the frames exercise every path: {seen:?}"
    );
    let s = scene();
    let frame = fx_frame(&mut frames, &s, Clock::SETTLED, 7).expect("a frame");
    let (under, over) = oracle_quads(&s, Clock::SETTLED, 7);
    let pane = pane_of(&s);
    let patch = Patch::around(&[&under, &over], Some(&frame), pane).expect("a patch");
    let mut palette = frame.over.palette().to_vec();
    palette[0] = FxInk::over(palette[0].color ^ 1, palette[0].alpha);
    let off = FxContent::new(frame.over.items().to_vec(), palette);
    let mut wrong = patch.ground(s.dark);
    let layers = [
        frame.layers(1, pane, (0, 0))[0].clone(),
        FxLayer {
            content: Arc::new(off),
            ..frame.layers(1, pane, (0, 0))[1].clone()
        },
    ]
    .map(|l| patch.moved(l));
    paint_layers(&mut wrong, &patch.frame(s.ch), &layers, &[]);
    assert!(
        patch
            .first_difference(&patch.paint_quads(&under, &over, s.dark), &wrong)
            .is_some(),
        "CONTROL: a palette entry off by one is seen"
    );
    eprintln!(
        "card layers vs quads: {} frames, {} pixels painted, at most {} items (cap {CARD_ITEMS_MAX})",
        seen.frames, seen.painted, seen.most_items
    );
}

/// The law scene at cell height `ch` (cell width half of it): the base
/// scene with the kitty `kitty_rows` tall, dark or light.
fn at_height(ch: u16, kitty_rows: u8, dark: bool) -> CardScene {
    CardScene {
        cw: (ch / 2).max(4),
        ch,
        kitty_rows,
        dark,
        ..scene()
    }
}

/// The poses, in [`KittyPose`]'s order.
const POSES: [KittyPose; 3] = [KittyPose::Open, KittyPose::Blink, KittyPose::Flick];

/// THE KITTY IS TODAY'S ART: each of its six registered arts (two heights,
/// three poses) is the art strings texel for texel — `.` transparent and
/// every other character its place in the palette — the same at every cell
/// height; and drawn as one sprite at its square pixel, its seven colours
/// opaque, it paints exactly the pixels its quads painted, at every cell
/// height from 8 to 96, both heights, both themes and every pose.
#[test]
fn the_kitty_is_todays_art() {
    for ch in [8u16, 18, 35, 96] {
        let arts = CardArts::for_cell(ch).expect("an admitted cell height");
        for art in [KITTY_SHORT, kitty_tall()] {
            for pose in POSES {
                let fx = kitty_art(&arts, &art, pose);
                let frame = art.frame(pose);
                assert_eq!(
                    (usize::from(fx.w()), usize::from(fx.h())),
                    (KITTY_W, frame.len())
                );
                for (y, row) in frame.iter().enumerate() {
                    for (x, b) in row.bytes().enumerate() {
                        let want = KITTY_PALETTE
                            .iter()
                            .position(|&c| c == b)
                            .expect("a palette character");
                        assert_eq!(fx.texel(x, y), want as u32, "{pose:?} at ({x}, {y})");
                    }
                }
            }
            let ids: BTreeSet<u128> = POSES
                .iter()
                .map(|&p| kitty_art(&arts, &art, p).id())
                .collect();
            assert_eq!(ids.len(), 3, "CONTROL: the poses' arts differ");
        }
    }
    let mut frames = CardFrames::default();
    let mut drawn = 0;
    for ch in 8..=96u16 {
        for kitty_rows in [2u8, 3] {
            for dark in [true, false] {
                let s = at_height(ch, kitty_rows, dark);
                for clock in [
                    Clock::SETTLED,
                    Clock::seated(BLINK_AT_MS + 10),
                    Clock::seated(FLICK_AT_MS + 10),
                ] {
                    let what = format!("{ch} px, {kitty_rows} rows, dark {dark}, {clock:?}");
                    let geo = Geo::new(&s).expect("a scene");
                    let cx = Emit::new(&s, geo, clock, 7);
                    let mut want = Vec::new();
                    kitty_quads(&cx, &Sink { geo }, &mut want);
                    let frame = fx_frame(&mut frames, &s, clock, 7).expect("a frame");
                    let Some(kitty) = marks(&frames, &frame, &s, clock, 7).kitty else {
                        assert!(want.is_empty(), "{what}: quads and no sprite");
                        continue;
                    };
                    assert_eq!(kitty.art.w(), KITTY_W as u16, "{what}");
                    assert_eq!(
                        (
                            u32::from(kitty.dw) % KITTY_W as u32,
                            kitty.dh % kitty.art.h()
                        ),
                        (0, 0),
                        "{what}: square pixels"
                    );
                    let inks = &frame.over.palette()[kitty.ink as usize..][..7];
                    assert!(inks.iter().all(|i| i.alpha == 255), "{what}: opaque");
                    drawn += 1;
                    let pane = pane_of(&s);
                    let only =
                        FxContent::new(vec![FxItem::Sprite(kitty)], frame.over.palette().to_vec());
                    let patch = Patch::around(&[&want], Some(&frame), pane).expect("a patch");
                    let a = patch.paint_quads(&[], &want, dark);
                    let b = patch.paint_content(only, frame.origin, pane, s.ch, dark);
                    assert_eq!(patch.first_difference(&a, &b), None, "{what}");
                }
            }
        }
    }
    assert!(drawn > 400, "CONTROL: the kitty is drawn ({drawn} frames)");
}

/// The rim's strips of `frame` at `p` and `rim` as a strip-id picture: what
/// the rim's art must hold, built from the strips the quads lay.
fn strip_picture(frame: &[&str], p: i64, rim: i64) -> (Vec<u8>, usize) {
    let w = KITTY_W as i64 * p + 2 * rim;
    let h = frame.len() as i64 * p + 2 * rim;
    let mut data = vec![0u8; (w * h) as usize];
    let mut k = 0usize;
    rim_strips(frame, p, rim, |q| {
        k += 1;
        for y in q.y0..q.y1 {
            for x in q.x0..q.x1 {
                let t = &mut data[((y + rim) * w + x + rim) as usize];
                assert_eq!(*t, 0, "strips overlap at ({x}, {y})");
                *t = k as u8;
            }
        }
    });
    (data, k)
}

/// THE RIM IS TODAY'S RIM: its strip-id art, for both kitties, every pose
/// and every cell height from 8 to 96, is the strips the quads laid, one id
/// a strip and one strip a texel (so it rings the silhouette once and its
/// light is never doubled), its top-left a rim's width up and left of the
/// kitty's, at most 59 strips (palette entries) a frame; and drawn — one
/// sprite, cut round the trail's hole — it paints exactly the pixels its
/// quads painted, through the ride and the seated blink and flick of every
/// law scene.
#[test]
fn the_rim_is_todays_rim() {
    let mut most = 0;
    for ch in 8..=96u16 {
        let arts = CardArts::for_cell(ch).expect("an admitted cell height");
        let p = i64::from(kitty_px(ch));
        let rim = rim_px(p, i64::from(stroke_px(ch)));
        for art in [KITTY_SHORT, kitty_tall()] {
            for pose in POSES {
                let what = format!("{ch} px, {} rows, {pose:?}", art.rows());
                let Some((fx, origin, strips)) = rim_art(&arts, &art, pose) else {
                    assert_eq!(rim, 0, "{what}: no rim art");
                    continue;
                };
                let (want, k) = strip_picture(art.frame(pose), p, rim);
                assert_eq!(fx.data(), &want[..], "{what}: the strips");
                assert_eq!((origin, strips), ((-rim, -rim), k), "{what}");
                most = most.max(strips);
            }
        }
    }
    assert!(most <= 59, "at most 59 strips a frame: {most}");
    let (mut short, _) = strip_picture(KITTY_SHORT.frame(KittyPose::Open), 3, 1);
    let last = *short.iter().max().expect("strips");
    short
        .iter_mut()
        .filter(|t| **t == last)
        .for_each(|t| *t = 0);
    let arts = CardArts::for_cell(18).expect("an admitted cell height");
    if let Some((fx, _, _)) = rim_art(&arts, &KITTY_SHORT, KittyPose::Open)
        && i64::from(kitty_px(18)) == 3
        && rim_px(3, i64::from(stroke_px(18))) == 1
    {
        assert_ne!(fx.data(), &short[..], "CONTROL: a strip short");
    }
    let mut frames = CardFrames::default();
    let mut cut = 0;
    for s in scenes() {
        for clock in clocks() {
            let what = format!("{}x{}, dark {}, {clock:?}", s.cw, s.ch, s.dark);
            let geo = Geo::new(&s).expect("a scene");
            let cx = Emit::new(&s, geo, clock, 7);
            let mut want = Vec::new();
            rim_quads(&cx, &Sink { geo }, &mut want);
            let frame = fx_frame(&mut frames, &s, clock, 7).expect("a frame");
            let (Some((at, pose)), Some(arts)) = (cx.kitty, frames.arts()) else {
                continue;
            };
            let Some((art, _, _)) = rim_art(arts, &at.art, pose) else {
                continue;
            };
            let rims: Vec<FxItem> = sprites_of(&frame.over, &art)
                .cloned()
                .map(FxItem::Sprite)
                .collect();
            let laid: i64 = sprites_of(&frame.over, &art)
                .map(|sp| i64::from(sp.clip.x1 - sp.clip.x0) * i64::from(sp.clip.y1 - sp.clip.y0))
                .sum();
            let rim_box = i64::from(art.w()) * i64::from(art.h());
            assert!(
                5 * laid <= rim_box,
                "{what}: the rim's items lay {laid} pixels of its {rim_box}-pixel box"
            );
            if let (Some(hole), Some(sp)) = (cx.rim_hole(), sprites_of(&frame.over, &art).next()) {
                let (ox, oy) = (i64::from(frame.origin.0), i64::from(frame.origin.1));
                let (sx, sy) = (i64::from(sp.x), i64::from(sp.y));
                let (w, h) = (i64::from(art.w()), i64::from(art.h()));
                let lit_in_hole = (hole.y0 - oy..hole.y1 - oy).any(|y| {
                    (hole.x0 - ox..hole.x1 - ox).any(|x| {
                        let (tx, ty) = (x - sx, y - sy);
                        (0..w).contains(&tx)
                            && (0..h).contains(&ty)
                            && art.data()[(ty * w + tx) as usize] != 0
                    })
                });
                cut += usize::from(lit_in_hole);
            }
            let pane = pane_of(&s);
            let only = FxContent::new(rims, frame.over.palette().to_vec());
            let Some(patch) = Patch::around(&[&want], Some(&frame), pane) else {
                continue;
            };
            let a = patch.paint_quads(&[], &want, s.dark);
            let b = patch.paint_content(only, frame.origin, pane, s.ch, s.dark);
            assert_eq!(patch.first_difference(&a, &b), None, "{what}");
        }
    }
    assert!(cut > 0, "CONTROL: frames whose rim the trail cuts");
}

/// THE RIM COVERS ITS OUTLINE, NOT ITS BOX: at every cell height, for both
/// arts and every pose, the rim is laid in at most [`RIM_COVER_MAX`]
/// pairwise-disjoint rectangles that hold every texel of its strips and at
/// most a fifth of its box (the shipped rims: 15-19 %, their strips 7-13 %),
/// so the kitty it rings and the air round it run no rim fragment.
#[test]
fn the_rim_covers_its_outline_not_its_box() {
    let (mut laid, mut boxed, mut lit) = (0i64, 0i64, 0i64);
    for ch in 8..=96u16 {
        let arts = CardArts::for_cell(ch).expect("an admitted cell height");
        for art in [KITTY_SHORT, kitty_tall()] {
            for pose in POSES {
                let what = format!("{ch} px, {} rows, {pose:?}", art.rows());
                let Some((fx, origin, _)) = rim_art(&arts, &art, pose) else {
                    continue;
                };
                let cover = rim_cover(&arts, &art, pose).expect("a rim has a cover");
                let (w, h) = (i64::from(fx.w()), i64::from(fx.h()));
                assert!(
                    cover.len() <= RIM_COVER_MAX,
                    "{what}: {} rectangles",
                    cover.len()
                );
                for (i, a) in cover.iter().enumerate() {
                    assert!(!a.is_empty(), "{what}: an empty rectangle");
                    for b in &cover[i + 1..] {
                        assert!(a.meet(*b).is_empty(), "{what}: {a:?} meets {b:?}");
                    }
                }
                let mut strips = 0;
                for ty in 0..h {
                    for tx in 0..w {
                        if fx.data()[(ty * w + tx) as usize] == 0 {
                            continue;
                        }
                        strips += 1;
                        let (x, y) = (tx + origin.0, ty + origin.1);
                        assert!(
                            cover
                                .iter()
                                .any(|r| (r.x0..r.x1).contains(&x) && (r.y0..r.y1).contains(&y)),
                            "{what}: strip texel ({tx}, {ty}) uncovered"
                        );
                    }
                }
                let area: i64 = cover.iter().map(|r| (r.x1 - r.x0) * (r.y1 - r.y0)).sum();
                assert!(
                    5 * area <= w * h,
                    "{what}: the cover holds {area} of {}",
                    w * h
                );
                let one = aterm_render::fx::cover(&fx, 1);
                let one_area: i64 = one
                    .iter()
                    .map(|r| i64::from(r.x1 - r.x0) * i64::from(r.y1 - r.y0))
                    .sum();
                assert!(
                    5 * one_area > w * h,
                    "CONTROL {what}: one rectangle is the box"
                );
                (laid, boxed, lit) = (laid + area, boxed + w * h, lit + strips);
            }
        }
    }
    assert!(boxed > 0, "CONTROL: rims were covered");
    eprintln!("the rim's cover: {laid} of {boxed} box pixels (its strips {lit})");
}

/// The pixels a sprite of `content` that draws `art` paints, card-local:
/// its non-zero texels inside its clip.
fn sprite_pixels(
    content: &FxContent,
    art: &Arc<aterm_core::render::FxArt>,
) -> BTreeSet<(i64, i64)> {
    let mut px = BTreeSet::new();
    for sp in sprites_of(content, art) {
        for y in 0..i64::from(sp.dh) {
            for x in 0..i64::from(sp.dw) {
                let (ax, ay) = (i64::from(sp.x) + x, i64::from(sp.y) + y);
                if holds(sp.clip, ax, ay) && art.texel(x as usize, y as usize) != 0 {
                    px.insert((ax, ay));
                }
            }
        }
    }
    px
}

/// THE RIM NEVER LIES ON THE TRAIL, on the layers: in every law scene,
/// through the ride, the seated blink and flick and the episode's end, no
/// pixel the rim's sprites paint lies on a segment of the trail as it is
/// drawn that frame.
#[test]
fn the_rim_never_lies_on_the_trail() {
    let mut frames = CardFrames::default();
    let (mut checked, mut uncut_on) = (0, 0);
    for s in scenes().into_iter().chain(scenes().into_iter().map(aired)) {
        for clock in clocks() {
            let frame = fx_frame(&mut frames, &s, clock, 7).expect("a frame");
            let (shape, _) = CardShape::of(&s);
            let local = shape.scene();
            let geo = Geo::new(local).expect("a scene");
            let cx = Emit::new(local, geo, clock, 7);
            let (Some((at, pose)), Some(lay), Some(arts)) =
                (cx.kitty, cx.trail_lay(), frames.arts())
            else {
                continue;
            };
            let Some((art, _, _)) = rim_art(arts, &at.art, pose) else {
                continue;
            };
            let segments = (lay.drawn - lay.tx0 + lay.seg - 1) / lay.seg;
            let on_trail = |&(x, y): &(i64, i64)| {
                (0..segments).any(|k| {
                    let c = cx.trail_cover(&lay, k);
                    (c.x0..c.x1).contains(&x) && (c.y0..c.y1).contains(&y)
                })
            };
            let lit = sprite_pixels(&frame.over, &art);
            let what = format!("{}x{}, dark {}, {clock:?}", s.cw, s.ch, s.dark);
            assert!(!lit.iter().any(on_trail), "{what}: the rim on the trail");
            checked += 1;
            let first = sprites_of(&frame.over, &art)
                .next()
                .cloned()
                .expect("a rim sprite");
            let whole = geo.inner().meet(geo.clip);
            let uncut = FxContent::new(
                vec![FxItem::Sprite(FxSprite {
                    clip: PxRect::new(
                        whole.x0 as i32,
                        whole.y0 as i32,
                        whole.x1 as i32,
                        whole.y1 as i32,
                    ),
                    ..first
                })],
                frame.over.palette().to_vec(),
            );
            uncut_on += usize::from(sprite_pixels(&uncut, &art).iter().any(on_trail));
        }
    }
    assert!(
        checked > 100 && uncut_on > 0,
        "CONTROL: {checked} frames, {uncut_on} whose uncut rim lies on the trail"
    );
}

/// The `Geom` the crate's family star is laid in, at cell height `ch`.
fn family_geom(ch: u16) -> Geom {
    Geom {
        cw: 1,
        ch: usize::from(ch),
        rows: 64,
        cols: 4000,
        origin_x: 0,
        origin_y: 0,
        win_w: 4000,
        win_h: 2000,
        head: 0,
    }
}

/// THE STAR IS THE FAMILY STAR, on the layers: on dark, one folded sprite
/// of the card's star art — each class the `add_sat` sum of its pieces'
/// light — paints exactly what the crate's one star (`push_twinkle_star`)
/// paints, for every arm from 1 to 32, plain and gold, at four coverages;
/// on light, its peeled sprites in tone rectangles paint what the oracle's
/// star paints, a trail's two-segment edge crossing it.
#[test]
fn the_star_is_the_family_star() {
    let (w, h) = (160usize, 160usize);
    let patch = Patch { x0: 0, y0: 0, w, h };
    let centre = (80i64, 80i64);
    let whole = Rect {
        x0: 0,
        y0: 0,
        x1: w as i64,
        y1: h as i64,
    };
    let pane = PxRect::new(0, 0, w as i32, h as i32);
    let mut fold_breaks = 0;
    for arm in 1..=32i64 {
        for gold in [false, true] {
            for cov in [255.0f32, 235.0, 200.0, 90.0] {
                let what = format!("arm {arm}, gold {gold}, cov {cov}");
                let call = StarCall {
                    clip: whole,
                    centre,
                    arm,
                    cov,
                    gold,
                    color: 0x00FF_C878,
                };
                let mut family = Vec::new();
                assert!(push_twinkle_star(
                    &mut family,
                    family_geom(18),
                    centre.0 as i32,
                    centre.1 as i32,
                    arm as i32,
                    cov as u8,
                    gold,
                    call.color,
                    4096
                ));
                let (items, palette) = one_star(call, whole, &[], true);
                assert_eq!(items.len(), 1, "{what}: one folded sprite");
                let a = patch.paint_quads(&[], &family, true);
                let b = patch.paint_content(FxContent::new(items, palette), (0, 0), pane, 18, true);
                assert_eq!(patch.first_difference(&a, &b), None, "{what}: dark");
                let edge = [
                    Rect {
                        x0: 0,
                        y0: centre.1,
                        x1: centre.0,
                        y1: centre.1 + 30,
                    },
                    Rect {
                        x0: centre.0,
                        y0: centre.1 + 1,
                        x1: w as i64,
                        y1: centre.1 + 31,
                    },
                ];
                let light = StarCall {
                    color: 0x00B0_3060,
                    ..call
                };
                let mut want = Vec::new();
                let geo = Geo::new(&scene()).expect("a scene");
                let sink = Sink {
                    geo: Geo { clip: whole, ..geo },
                };
                lay_star(
                    &mut want,
                    &sink,
                    whole,
                    centre,
                    arm,
                    cov,
                    gold,
                    light.color,
                    &edge,
                    false,
                );
                let (items, palette) = one_star(light, whole, &edge, false);
                let peeled = items.len();
                let a = patch.paint_quads(&[], &want, false);
                let b = patch.paint_content(
                    FxContent::new(items.clone(), palette.clone()),
                    (0, 0),
                    pane,
                    18,
                    false,
                );
                assert_eq!(patch.first_difference(&a, &b), None, "{what}: light");
                assert!(
                    peeled <= STAR_DEPTH_MAX * TONE_RECTS_MAX,
                    "{what}: {peeled} items"
                );
                if let Some(art) = fx_pieces(arm, gold) {
                    let (ox, oy) = art.origin();
                    let FxItem::Sprite(first) = &items[0] else {
                        panic!("{what}: a sprite");
                    };
                    let inks: Vec<FxInk> = art
                        .classes()
                        .iter()
                        .map(|m| {
                            palette[first.ink as usize + usize::from(*m.last().expect("a member"))]
                        })
                        .collect();
                    let folded = FxItem::Sprite(FxSprite {
                        x: centre.0 as i32 + ox,
                        y: centre.1 as i32 + oy,
                        dw: art.folded().w(),
                        dh: art.folded().h(),
                        flip_x: false,
                        art: Arc::clone(art.folded()),
                        ink: 0,
                        clip: PxRect::EVERYWHERE,
                    });
                    let c = patch.paint_content(
                        FxContent::new(vec![folded], inks),
                        (0, 0),
                        pane,
                        18,
                        false,
                    );
                    let crossed = patch.paint_quads(&[], &want, false);
                    fold_breaks += usize::from(patch.first_difference(&crossed, &c).is_some());
                }
            }
        }
    }
    assert!(
        fold_breaks > 0,
        "CONTROL: a source-over fold is not the laying"
    );
}

/// The card's star art built afresh, for an arm past a cell's arts.
fn fx_pieces(arm: i64, gold: bool) -> Option<aterm_render::fx::PieceArt> {
    let laid: Vec<(PxRect, u8)> = star_pieces(arm as i32, gold)
        .iter()
        .map(|p| (PxRect::new(p.x, p.y, p.x + p.w, p.y + p.h), p.part.id()))
        .collect();
    aterm_render::fx::pieces(&laid)
}

/// A STAR'S PIECES STACK AT MOST FOUR DEEP: for every arm from 1 to 32,
/// plain and gold, no pixel lies under more than [`STAR_DEPTH_MAX`] of its
/// pieces, so a light star is at most that many peeled sprites a tone
/// rectangle.
#[test]
fn a_stars_pieces_stack_at_most_four_deep() {
    let mut deepest = 0;
    for arm in 1..=32 {
        for gold in [false, true] {
            let art = fx_pieces(arm, gold).expect("a star");
            assert!(
                art.depth() <= STAR_DEPTH_MAX,
                "arm {arm}, gold {gold}: {}",
                art.depth()
            );
            deepest = deepest.max(art.depth());
        }
    }
    let short = deepest - 1;
    let broken = (1..=32).any(|arm| {
        [false, true]
            .iter()
            .any(|&gold| fx_pieces(arm, gold).is_some_and(|a| a.depth() > short))
    });
    assert!(broken, "CONTROL: a bound of {short} is broken");
    eprintln!("stars stack {deepest} deep at most (bound {STAR_DEPTH_MAX})");
}

/// The tall kitty dithered: every opaque pixel of each of its 24 × 21
/// frames the outline or the coat by turns, its silhouette kept — so every
/// run of one colour is one pixel long (the art the quad oracle pays most
/// for), while everything the card reads off the art's shape (its rear, its
/// rim, its seat) is the real kitty's.
fn dithered() -> KittyArt {
    let dither = |frame: &[&str]| -> &'static [&'static str] {
        let rows: Vec<&'static str> = frame
            .iter()
            .enumerate()
            .map(|(y, row)| {
                let row: String = row
                    .bytes()
                    .enumerate()
                    .map(|(x, b)| match b {
                        b'.' => '.',
                        _ if (x + y) % 2 == 0 => 'o',
                        _ => 'c',
                    })
                    .collect();
                &*Box::leak(row.into_boxed_str())
            })
            .collect();
        Box::leak(rows.into_boxed_slice())
    };
    KittyArt {
        open: dither(KITTY_TALL.open),
        blink: dither(KITTY_TALL.blink),
        flick: dither(KITTY_TALL.flick),
        ..KITTY_TALL
    }
}

/// THE CARD IS BOUNDED WITHOUT ITS ART: the tall kitty swapped for one
/// dithered all over (every run a pixel), the card's frame holds exactly as
/// many items as with the real kitty — the art is a picture, not a count —
/// at every law scene with the tall kitty, settled and through the ride,
/// and never more than [`CARD_ITEMS_MAX`].
#[test]
fn the_card_is_bounded_without_its_art() {
    let tall: Vec<CardScene> = scenes().into_iter().filter(|s| s.kitty_rows == 3).collect();
    let count = |frames: &mut CardFrames| {
        let mut items = Vec::new();
        let mut quads = Vec::new();
        for s in &tall {
            for clock in [
                Clock::SETTLED,
                Clock::riding(RIDE_MS / 2),
                Clock::riding(RIDE_MS + 100),
            ] {
                let f = fx_frame(frames, s, clock, 7).expect("a frame");
                items.push(f.under.items().len() + f.over.items().len());
                let (u, o) = oracle_quads(s, clock, 7);
                quads.push(u.len() + o.len());
            }
        }
        (items, quads)
    };
    let (real_items, real_quads) = count(&mut CardFrames::default());
    stand_in(Some(dithered()));
    let (dith_items, dith_quads) = count(&mut CardFrames::default());
    stand_in(None);
    assert_eq!(dith_items, real_items, "the items do not count the art");
    assert!(
        dith_items.iter().all(|&n| n <= CARD_ITEMS_MAX),
        "{dith_items:?}"
    );
    let (r, d) = (
        real_quads.iter().max().copied().unwrap_or(0),
        dith_quads.iter().max().copied().unwrap_or(0),
    );
    assert!(
        d > r,
        "CONTROL: the oracle's quads rise with the art ({r} -> {d})"
    );
    let kitty_alone = || {
        let s = CardScene {
            kitty_rows: 3,
            ..scene()
        };
        let geo = Geo::new(&s).expect("a scene");
        let mut q = Vec::new();
        kitty_quads(
            &Emit::new(&s, geo, Clock::SETTLED, 7),
            &Sink { geo },
            &mut q,
        );
        q.len()
    };
    let real_kitty = kitty_alone();
    stand_in(Some(dithered()));
    let dith_kitty = kitty_alone();
    stand_in(None);
    assert!(
        dith_kitty > real_kitty,
        "CONTROL: {real_kitty} -> {dith_kitty}"
    );
    eprintln!(
        "bounded without its art: items at most {:?} (both arts); oracle frame quads {r} -> {d}, \
         the kitty's own {real_kitty} -> {dith_kitty}",
        real_items.iter().max()
    );
}

/// THE UNDER CONTENT REACHES ITS BOUND AND STOPS THERE: the widest box and
/// longest trail ([`widest`]) lay each edge in [`MAX_EDGE_SEGMENTS`] ramps and
/// the stepping trail in [`MAX_TRAIL_SEGMENTS`] segments of its six
/// stripes: the border alone holds `2 · MAX_EDGE_SEGMENTS + 2` items and
/// border and trail exactly [`CARD_UNDER_ITEMS_MAX`], on both grounds,
/// seated and at the episode's end, and the layers still put the quad
/// oracle's pixels on the glass.
#[test]
fn the_under_content_reaches_its_bound_and_stops_there() {
    let wide = widest();
    let border_only = CardScene {
        trail: None,
        ..wide
    };
    let stripes = TRAIL_STOPS.len();
    let edges = 2 * MAX_EDGE_SEGMENTS as usize + 2;
    let mut frames = CardFrames::default();
    let mut seen = Seen::default();
    let under = |frames: &mut CardFrames, s: &CardScene, clock: Clock| {
        fx_frame(frames, s, clock, 7)
            .expect("a frame")
            .under
            .items()
            .len()
    };
    for dark in [true, false] {
        let (wide, border_only) = (
            CardScene { dark, ..wide },
            CardScene {
                dark,
                ..border_only
            },
        );
        for clock in [
            Clock::seated(BLINK_AT_MS + 10),
            Clock::riding(EPISODE_MS - 40),
        ] {
            assert_eq!(
                (
                    under(&mut frames, &border_only, clock),
                    under(&mut frames, &wide, clock)
                ),
                (edges, CARD_UNDER_ITEMS_MAX),
                "dark {dark}, {clock:?}: (the border's items, the border's and the trail's)"
            );
            let what = format!("the widest box and longest trail, dark {dark}, {clock:?}");
            assert_same_pixels(&mut frames, &wide, clock, 7, &mut seen, &what);
        }
        assert_eq!(
            under(&mut frames, &wide, Clock::SETTLED),
            edges + stripes,
            "CONTROL: dark {dark}: the trail at rest is one segment"
        );
    }
}

/// A THEME SWITCH REBAKES NO ART: a card's frames drawn once at a cell
/// height, every later frame — dark to light and back, the other palette,
/// another coat and eye, serious mode, the ride — builds no art.
#[test]
fn a_theme_switch_rebakes_no_art() {
    let mut frames = CardFrames::default();
    let s = scene();
    fx_frame(&mut frames, &s, Clock::SETTLED, 7).expect("a frame");
    let _ = take_art_builds();
    for dark in [false, true, false] {
        for palette in [CardPalette::Land, CardPalette::Cruise] {
            for serious in [false, true] {
                let v = CardScene {
                    dark,
                    palette,
                    serious,
                    coat: 0x0020_2020,
                    eye: 0x00FF_0000,
                    ..s
                };
                for clock in [
                    Clock::SETTLED,
                    Clock::riding(300),
                    Clock::seated(BLINK_AT_MS + 10),
                ] {
                    fx_frame(&mut frames, &v, clock, 7).expect("a frame");
                }
            }
        }
    }
    assert_eq!(take_art_builds(), 0, "a theme switch builds no art");
    fx_frame(
        &mut frames,
        &CardScene {
            cw: 100,
            ch: 211,
            ..s
        },
        Clock::SETTLED,
        7,
    );
    assert!(take_art_builds() > 0, "CONTROL: a new cell height builds");
    for ch in [0, 14_000, u16::MAX] {
        assert!(CardArts::for_cell(ch).is_none(), "height {ch}");
        assert_eq!(take_art_builds(), 0, "height {ch}: no partial art cache");
    }
}

/// THE KITTY CLEARS THE BORDER AT EVERY CELL HEIGHT, on the layers: its
/// sprite lies inside the border's stroke at every height from 8 to 96,
/// both kitties, with air and without.
#[test]
fn the_kitty_clears_the_border_at_every_cell_height() {
    let mut frames = CardFrames::default();
    let mut drawn = 0;
    for ch in 8..=96u16 {
        for kitty_rows in [2u8, 3] {
            for s in [
                at_height(ch, kitty_rows, true),
                aired(at_height(ch, kitty_rows, true)),
            ] {
                let frame = fx_frame(&mut frames, &s, Clock::SETTLED, 7).expect("a frame");
                let (shape, _) = CardShape::of(&s);
                let inner = Geo::new(shape.scene()).expect("a scene").inner();
                let Some(k) = marks(&frames, &frame, &s, Clock::SETTLED, 7).kitty else {
                    continue;
                };
                let r = Rect {
                    x0: i64::from(k.x),
                    y0: i64::from(k.y),
                    x1: i64::from(k.x) + i64::from(k.dw),
                    y1: i64::from(k.y) + i64::from(k.dh),
                };
                assert_eq!(
                    r.meet(inner),
                    r,
                    "{ch} px, {kitty_rows} rows: {r:?} in {inner:?}"
                );
                drawn += 1;
            }
        }
    }
    assert_eq!(drawn, 89 * 4, "CONTROL: the kitty is drawn at every height");
}

/// DARK MARKS ARE LIGHT, AND LIGHT IS SOURCE-OVER THROUGHOUT, on the
/// layers: on a dark ground every palette entry but the kitty's seven is
/// added light (opacity 0), so a mark only brightens the ground; on a
/// light one every entry of both contents is source-over ink (opacity
/// above 0), so nothing doubles to white — at every law scene through the
/// episode.
#[test]
fn dark_marks_are_light_and_light_marks_are_ink() {
    let mut frames = CardFrames::default();
    let (mut added, mut inked) = (0, 0);
    for s in scenes() {
        for clock in clocks() {
            let frame = fx_frame(&mut frames, &s, clock, 7).expect("a frame");
            let kitty = marks(&frames, &frame, &s, clock, 7)
                .kitty
                .map(|k| k.ink as usize..k.ink as usize + 7);
            for (content, over) in [(&frame.under, false), (&frame.over, true)] {
                for (i, ink) in content.palette().iter().enumerate() {
                    let kitty_ink = over && kitty.as_ref().is_some_and(|k| k.contains(&i));
                    if s.dark && !kitty_ink {
                        assert_eq!(
                            ink.alpha,
                            0,
                            "dark: entry {i} of {}",
                            if over { "over" } else { "under" }
                        );
                        added += 1;
                    } else if !s.dark {
                        assert!(ink.alpha > 0, "light: entry {i}");
                        inked += 1;
                    }
                }
            }
        }
    }
    assert!(
        added > 0 && inked > 0,
        "CONTROL: {added} added, {inked} inked"
    );
}

/// THE LAYERS ARE DETERMINISTIC: the same scene, instant and seed give the
/// same items and palettes, from two cards' frames; another seed moves the
/// stars.
#[test]
fn the_layers_are_deterministic() {
    let text = |f: &CardFrame| {
        format!(
            "{:?} {:?} {:?} {:?} {:?}",
            f.under.items(),
            f.under.palette(),
            f.over.items(),
            f.over.palette(),
            f.origin
        )
    };
    let (mut a, mut b) = (CardFrames::default(), CardFrames::default());
    for s in scenes() {
        for clock in clocks() {
            let fa = fx_frame(&mut a, &s, clock, 42).expect("a frame");
            let fb = fx_frame(&mut b, &s, clock, 42).expect("a frame");
            assert_eq!(text(&fa), text(&fb));
        }
    }
    let s = scene();
    let one = fx_frame(&mut a, &s, Clock::SETTLED, 1).expect("a frame");
    let two = fx_frame(&mut b, &s, Clock::SETTLED, 2).expect("a frame");
    assert_ne!(text(&one), text(&two), "CONTROL: the seed moves the stars");
}

/// AN INLINE SCROLL IS THE SAME FRAME, MOVED: a scene moved down the
/// window by whole rows (the pane's text scrolling under a settled card),
/// its pane moved with it, has the kept frame's shape, and the frame drawn
/// at the new place is the kept one's contents at the new origin.
#[test]
fn an_inline_scroll_is_the_same_frame_moved() {
    let empty = Arc::new(FxContent::default());
    for (origin, grid, pane, want) in [
        (
            (i32::MIN, i32::MIN),
            (i32::MIN, i32::MIN),
            PxRect::new(i32::MIN + 1, i32::MIN + 1, i32::MIN + 2, i32::MIN + 2),
            PxRect::new(1, 1, 2, 2),
        ),
        (
            (i32::MAX, i32::MAX),
            (i32::MAX, i32::MAX),
            PxRect::new(i32::MAX - 2, i32::MAX - 2, i32::MAX - 1, i32::MAX - 1),
            PxRect::new(-2, -2, -1, -1),
        ),
    ] {
        let frame = CardFrame {
            under: Arc::clone(&empty),
            over: Arc::clone(&empty),
            origin,
        };
        let [under, over] = frame.layers(7, pane, grid);
        assert_eq!(under.offset, origin);
        assert_eq!(under.clip, pane);
        assert_eq!(over.offset, (0, 0));
        assert_eq!(over.clip, want);
        assert_eq!(
            frame.layers(7, PxRect::EVERYWHERE, grid)[1].clip,
            PxRect::EVERYWHERE
        );
    }
    let frame = CardFrame {
        under: Arc::clone(&empty),
        over: empty,
        origin: (0, 0),
    };
    assert!(
        frame.layers(7, PxRect::EVERYWHERE, (i32::MIN, 0))[1]
            .clip
            .is_empty(),
        "a relative origin outside i32 is refused"
    );
    for s in scenes() {
        let (shape, origin) = CardShape::of(&s);
        for rows in [1i32, 3, 7] {
            let dy = rows * i32::from(s.ch);
            let moved = CardScene {
                origin_y: s.origin_y + dy,
                clip: (
                    s.clip.0,
                    s.clip.1.saturating_add(dy),
                    s.clip.2,
                    s.clip.3.saturating_add(dy),
                ),
                ..s
            };
            let (shape2, origin2) = CardShape::of(&moved);
            assert_eq!(shape2, shape, "{rows} rows down");
            assert_eq!(origin2, (origin.0, origin.1 + dy));
        }
    }
    let s = scene();
    let cut = CardScene {
        clip: (
            s.origin_x,
            0,
            i32::MAX,
            s.origin_y + i32::from(s.top + 2) * i32::from(s.ch),
        ),
        ..s
    };
    assert_ne!(
        CardShape::of(&cut).0,
        CardShape::of(&s).0,
        "CONTROL: a cut box"
    );
}
