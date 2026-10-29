// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CARD ON FX LAYERS (`docs/DESIGN-card-engine-2026-10-19.md` §4.8).

use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRamps, FxRect, FxSlot, FxSpace, FxSprite,
    FxTexels, PxRect,
};
use aterm_render::fx::{self as fx_art, PieceArt};
use aterm_render::{add_sat, premul_rgb};

use super::{
    CARD_OVER_ITEMS_MAX, CARD_UNDER_ITEMS_MAX, CardScene, Emit, Geo, KITTY_PALETTE, KITTY_SHORT,
    KITTY_W, KittyArt, KittyInk, KittyPose, MAX_TRAIL_SEGMENTS, Paint, RIM_COVER_MAX, Ramps, Rect,
    STAR_CORE_ADD, STAR_DEPTH_MAX, STAR_GLINT_COV, STAR_PRINT_FULL, StarCall, WHITE, clip_ramp,
    ink, kitty_px, kitty_tall, lay_cut, light, rim_px, rim_strips, stroke_px,
};
use crate::effect_util::{STAR_ARM_HERO, STAR_ARM_INK};
use crate::effect_util::{
    STAR_PAINTS, STAR_TAPER_STEPS, StarPart, star_arm, star_arm_px, star_pieces,
};

const RING: usize = 3;

/// A card's SHAPE: its scene with the card's origin taken out ([`Self::of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardShape {
    local: CardScene,
}

impl CardShape {
    /// `scene`'s shape.
    #[must_use]
    #[inline]
    pub const fn of(scene: &CardScene) -> (Self, (i32, i32)) {
        #[inline]
        const fn min_i64(a: i64, b: i64) -> i64 {
            if a < b { a } else { b }
        }
        #[inline]
        const fn space(v: i32) -> i64 {
            sat_u16(v as i64) as i64
        }
        #[inline]
        const fn sat_u16(v: i64) -> u16 {
            if v < 0 {
                0
            } else if v > u16::MAX as i64 {
                u16::MAX
            } else {
                v as u16
            }
        }
        let s = scene;
        let (cw, ch) = (s.cw as i64, s.ch as i64);
        let pad = min_i64(s.pad_left as i64, cw / 2);
        let above = if s.top > 0 {
            min_i64(s.pad_above as i64, ch / 2)
        } else {
            0
        };
        let below = min_i64(s.pad_below as i64, ch / 2);
        let origin = (
            sat_i32(s.origin_x as i64 - pad),
            sat_i32(s.origin_y as i64 + s.top as i64 * ch - above),
        );
        let (ox, oy) = (origin.0 as i64, origin.1 as i64);
        let outer = Rect {
            x0: 0,
            y0: 0,
            x1: pad + (s.cols as i64 + 1) * cw,
            y1: above + s.rows as i64 * ch + below,
        };
        let (cx0, cy0, cx1, cy1) = s.clip;
        let clip = Rect {
            x0: space(cx0) - ox,
            y0: space(cy0) - oy,
            x1: space(cx1) - ox,
            y1: space(cy1) - oy,
        }
        .meet(outer);
        let clip = if clip.is_empty() {
            (0, 0, 0, 0)
        } else {
            (
                sat_i32(clip.x0),
                sat_i32(clip.y0),
                sat_i32(clip.x1),
                sat_i32(clip.y1),
            )
        };
        let local = CardScene {
            origin_x: sat_i32(pad),
            origin_y: sat_i32(above - ch),
            row_base: 0,
            pad_left: sat_u16(pad),
            pad_above: sat_u16(above),
            pad_below: sat_u16(below),
            top: 1,
            clip,
            wand: match s.wand {
                Some((c, r)) => Some((c, (r + 1).saturating_sub(s.top))),
                None => None,
            },
            ..*s
        };
        (Self { local }, origin)
    }

    /// The card-local scene a frame of this shape is drawn from.
    #[inline]
    pub(super) const fn scene(&self) -> &CardScene {
        &self.local
    }

    /// Whether a frame of this shape draws anything ([`Geo::new`]).
    #[inline]
    pub(super) fn drawable(&self) -> bool {
        Geo::new(&self.local).is_some() && CardArts::dimensions(self.local.ch).is_some()
    }
}

/// `v` clamped into `i32`.
#[inline]
const fn sat_i32(v: i64) -> i32 {
    if v < i32::MIN as i64 {
        i32::MIN
    } else if v > i32::MAX as i64 {
        i32::MAX
    } else {
        v as i32
    }
}

/// One frame of a card.
#[derive(Clone, Debug)]
pub struct CardFrame {
    /// The border and the trail, drawn beneath the glyph ink.
    pub under: Arc<FxContent>,
    /// The kitty, its rim and the stars, drawn over the glyphs.
    pub over: Arc<FxContent>,
    /// The card-local origin's pixel in the scene's space (window pixels).
    pub origin: (i32, i32),
}

/// The layer id of a card's `slot` content.
#[must_use]
#[inline]
pub const fn card_layer_id(card: u64, slot: FxSlot) -> u64 {
    let salt = match slot {
        FxSlot::Under => 0x4341_5244_5544_4552,
        FxSlot::Glow => 0x4341_5244_474C_4F57,
        FxSlot::Over => 0x4341_5244_4F56_4552,
    };
    crate::genome::mix(card ^ crate::genome::mix(salt))
}

impl CardFrame {
    /// The frame as its two layers (design §4.8).
    #[must_use]
    #[inline]
    pub fn layers(&self, card: u64, pane: PxRect, grid: (i32, i32)) -> [FxLayer; 2] {
        let mut under = FxLayer::new(
            card_layer_id(card, FxSlot::Under),
            Arc::clone(&self.under),
            FxSlot::Under,
            FxSpace::Window,
            FxBlend::Exact,
        );
        under.offset = self.origin;
        under.clip = pane;
        let mut over = FxLayer::new(
            card_layer_id(card, FxSlot::Over),
            Arc::clone(&self.over),
            FxSlot::Over,
            FxSpace::Grid,
            FxBlend::Exact,
        );
        if let (Some(x), Some(y)) = (
            self.origin.0.checked_sub(grid.0),
            self.origin.1.checked_sub(grid.1),
        ) {
            #[inline]
            const fn edge(e: i32, g: i32) -> i32 {
                if e == i32::MIN || e == i32::MAX {
                    e
                } else {
                    let diff = (e as i64) - (g as i64);
                    let lo = (i32::MIN as i64) + 1;
                    let hi = (i32::MAX as i64) - 1;
                    (if diff < lo {
                        lo
                    } else if diff > hi {
                        hi
                    } else {
                        diff
                    }) as i32
                }
            }
            over.offset = (x, y);
            over.clip = PxRect::new(
                edge(pane.x0, grid.0),
                edge(pane.y0, grid.1),
                edge(pane.x1, grid.0),
                edge(pane.y1, grid.1),
            );
        } else {
            over.clip = PxRect::EMPTY;
        }
        [under, over]
    }

    /// Whether the frame draws nothing: both contents empty.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.under.items().is_empty() && self.over.items().is_empty()
    }
}

/// One card's last KEPT frame.
#[derive(Clone, Debug)]
pub struct KeptFrame {
    pub(super) shape: CardShape,
    /// The flow step and the grid it was drawn on ([`super::HarnessCard::flow_step`]).
    pub(super) step: Option<(u64, u32)>,
    pub(super) under: Arc<FxContent>,
    pub(super) over: Arc<FxContent>,
}

impl KeptFrame {
    /// The flow step the kept frame is (`None`: the settled frame).
    #[must_use]
    #[inline]
    pub const fn step(&self) -> Option<u64> {
        match self.step {
            Some((step, _)) => Some(step),
            None => None,
        }
    }

    /// The kept frame laid at `origin`.
    #[inline]
    pub(super) fn at(&self, origin: (i32, i32)) -> CardFrame {
        CardFrame {
            under: Arc::clone(&self.under),
            over: Arc::clone(&self.over),
            origin,
        }
    }
}

/// One card's frames.
#[derive(Debug)]
pub struct CardFrames {
    kept: Option<KeptFrame>,
    ring: [Option<(Arc<FxContent>, Arc<FxContent>)>; RING],
    next: usize,
    arts: Option<Arc<CardArts>>,
}

impl Default for CardFrames {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl CardFrames {
    /// Construct an empty card frame cache.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            kept: None,
            ring: [const { None }; RING],
            next: 0,
            arts: None,
        }
    }
    /// The frame kept, if any: the settled frame or a flow step.
    #[inline]
    #[must_use]
    pub const fn kept(&self) -> Option<&KeptFrame> {
        self.kept.as_ref()
    }

    /// Keep `kept` (an intro frame keeps nothing).
    #[inline]
    pub(super) fn keep(&mut self, kept: Option<KeptFrame>) {
        self.kept = kept;
    }

    /// The arts this card's frames are drawn with (its last frame's cell height).
    #[must_use]
    #[inline]
    pub const fn arts(&self) -> Option<&Arc<CardArts>> {
        self.arts.as_ref()
    }

    /// Two empty contents: a frame with nothing to draw.
    #[inline]
    pub(super) fn nothing() -> (Arc<FxContent>, Arc<FxContent>) {
        static EMPTY: OnceLock<Arc<FxContent>> = OnceLock::new();
        let empty = EMPTY.get_or_init(|| Arc::new(FxContent::default()));
        (Arc::clone(empty), Arc::clone(empty))
    }

    /// Fill the ring's next pair with `cx`'s frame and hand it back.
    #[inline]
    pub(super) fn fill(&mut self, cx: &Emit<'_>) -> (Arc<FxContent>, Arc<FxContent>) {
        let ch = cx.scene.ch;
        if !matches!(self.arts.as_deref(), Some(a) if a.ch == ch) {
            self.arts = CardArts::for_cell(ch);
        }
        let Some(arts) = self.arts.as_deref() else {
            return Self::nothing();
        };
        let slot = self.next;
        self.next = (slot + 1) % RING;
        if self.ring[slot].is_none() {
            self.ring[slot] = Some((
                Arc::new(FxContent::default()),
                Arc::new(FxContent::default()),
            ));
        }
        let Some((under, over)) = &mut self.ring[slot] else {
            unreachable!()
        };
        refill(under, |items, palette| fill_under(cx, items, palette));
        refill(over, |items, palette| fill_over(cx, arts, items, palette));
        (Arc::clone(under), Arc::clone(over))
    }
}

#[inline]
fn refill(content: &mut Arc<FxContent>, f: impl FnOnce(&mut Vec<FxItem>, &mut Vec<FxInk>)) {
    if Arc::get_mut(content).is_none() {
        *content = Arc::new(content.empty_like());
    }
    let Some(c) = Arc::get_mut(content) else {
        return;
    };
    c.edit(|items, palette| {
        items.clear();
        palette.clear();
        f(items, palette);
    });
}

/// A paint as a palette entry.
#[inline]
const fn fx_ink(p: Paint) -> FxInk {
    FxInk::over(p.color, p.alpha)
}

/// Validate a producer's dimensions before allocating any texels.
#[inline]
fn art_size(w: i64, h: i64) -> Option<(u16, u16, usize)> {
    if w < 0 || h < 0 {
        return None;
    }
    fx_art::art_dimensions(w as usize, h as usize)
}

/// The rim's strip-id art for one frame of the kitty at one pixel size.
#[derive(Clone, Debug)]
struct RimArt {
    art: Arc<FxArt>,
    origin: (i64, i64),
    mids: Box<[(i64, i64)]>,
    cover: Box<[Rect]>,
}

impl RimArt {
    #[inline]
    fn dimensions(frame: &[&str], p: i64, rim: i64) -> Option<(u16, u16, usize)> {
        if p <= 0 || rim <= 0 || frame.len() > i64::MAX as usize {
            return None;
        }
        let border = rim.checked_mul(2)?;
        let w = (KITTY_W as i64).checked_mul(p)?.checked_add(border)?;
        let h = (frame.len() as i64).checked_mul(p)?.checked_add(border)?;
        art_size(w, h)
    }

    #[inline]
    fn of(frame: &[&str], p: i64, rim: i64) -> Option<Self> {
        #[inline]
        const fn all_zero(slice: &[u8]) -> bool {
            let mut i = 0;
            while i < slice.len() {
                if slice[i] != 0 {
                    return false;
                }
                i += 1;
            }
            true
        }
        if rim == 0 || p == 0 {
            return None;
        }
        let (w16, h16, len) = Self::dimensions(frame, p, rim)?;
        let wu = usize::from(w16);
        let mut data = vec![0u8; len];
        let mut mids = Vec::new();
        let mut ok = true;
        rim_strips(frame, p, rim, |q| {
            let next_id = mids.len() + 1;
            if next_id > u8::MAX as usize {
                ok = false;
                return;
            }
            let k = next_id as u8;
            mids.push((q.x0 + q.x1, q.y0 + q.y1));
            let (rx0, rx1) = ((q.x0 + rim) as usize, (q.x1 + rim) as usize);
            let mut y = q.y0;
            while y < q.y1 {
                let row = (y + rim) as usize * wu;
                let x0 = row + rx0;
                let x1 = row + rx1;
                debug_assert!(all_zero(&data[x0..x1]), "rim strips overlap on row {y}");
                data[x0..x1].fill(k);
                y += 1;
            }
        });
        if !ok {
            return None;
        }
        let art = fx_art::bitmap(FxTexels::Index8, w16, h16, data)?;
        let raw_cover = fx_art::cover(&art, RIM_COVER_MAX);
        let mut cover_vec = Vec::with_capacity(raw_cover.len());
        let mut ci = 0;
        while ci < raw_cover.len() {
            let px = fx_art::cover_px(raw_cover[ci], (w16, h16), (w16, h16), false);
            cover_vec.push(Rect {
                x0: i64::from(px.x0) - rim,
                y0: i64::from(px.y0) - rim,
                x1: i64::from(px.x1) - rim,
                y1: i64::from(px.y1) - rim,
            });
            ci += 1;
        }
        let cover = cover_vec.into_boxed_slice();
        Some(Self {
            art,
            origin: (-rim, -rim),
            mids: mids.into_boxed_slice(),
            cover,
        })
    }
}

/// One star's arts ([`star_pieces`] through [`fx_art::pieces`]).
#[derive(Clone, Debug)]
struct StarArt {
    pieces: PieceArt,
    tapers: [f32; STAR_TAPER_STEPS as usize],
}

impl StarArt {
    #[inline]
    fn of(arm: i32, gold: bool) -> Option<Self> {
        if arm < 1 {
            return None;
        }
        let side = i64::from(arm).checked_mul(2)?.checked_add(1)?;
        art_size(side, side)?;
        let laid = star_pieces(arm, gold);
        let mut tapers = [0.0; STAR_TAPER_STEPS as usize];
        let mut rects = Vec::with_capacity(laid.len());
        let mut li = 0;
        while li < laid.len() {
            let p = &laid[li];
            li += 1;
            if let StarPart::Tip { step, taper } = p.part {
                tapers[usize::from(step)] = taper;
            }
            let r = PxRect::new(p.x, p.y, p.x + p.w, p.y + p.h);
            rects.push((r, p.part.id()));
        }
        let pieces = fx_art::pieces(&rects)?;
        debug_assert!(
            pieces.depth() <= STAR_DEPTH_MAX,
            "a star of arm {arm} stacks {} deep",
            pieces.depth()
        );
        Some(Self { pieces, tapers })
    }
}

/// The card's arts at one cell height.
#[derive(Debug)]
pub struct CardArts {
    ch: u16,
    kitty: [Arc<FxArt>; 6],
    rim: [Option<RimArt>; 6],
    stars: Box<[[Option<StarArt>; 2]]>,
}

/// Which of the six kitty frames `art` (two rows or three) in `pose` is.
#[inline]
const fn kitty_slot(art: &KittyArt, pose: KittyPose) -> usize {
    let r = art.rows() - 2;
    (if r < 1 { r } else { 1 }) * 3 + pose as usize
}

impl CardArts {
    /// The arts for cell height `ch`.
    #[must_use]
    #[inline]
    pub fn for_cell(ch: u16) -> Option<Arc<Self>> {
        let dimensions = Self::dimensions(ch)?;
        #[cfg(test)]
        if super::stand_in_active() {
            return Some(Arc::new(Self::build(ch, dimensions)));
        }
        static MEMO: Mutex<Vec<(u16, Weak<CardArts>)>> = Mutex::new(Vec::new());
        let mut memo = MEMO.lock().unwrap_or_else(PoisonError::into_inner);
        let mut mi = 0;
        while mi < memo.len() {
            let (c, w) = &memo[mi];
            if *c == ch
                && let Some(live) = w.upgrade()
            {
                return Some(live);
            }
            mi += 1;
        }
        memo.retain(|(_, w)| w.strong_count() > 0);
        let arts = Arc::new(Self::build(ch, dimensions));
        memo.push((ch, Arc::downgrade(&arts)));
        Some(arts)
    }

    /// Check all six rim pictures and the largest star before any cache is allocated.
    #[inline]
    pub(super) fn dimensions(ch: u16) -> Option<(i64, i64, i32)> {
        if ch == 0 {
            return None;
        }
        let p = i64::from(kitty_px(ch));
        let rim = rim_px(p, i64::from(stroke_px(ch)));
        if rim > 0 {
            let arts = [KITTY_SHORT, kitty_tall()];
            let poses = [KittyPose::Open, KittyPose::Blink, KittyPose::Flick];
            let mut ai = 0;
            while ai < arts.len() {
                let mut pi = 0;
                while pi < poses.len() {
                    RimArt::dimensions(arts[ai].frame(poses[pi]), p, rim)?;
                    pi += 1;
                }
                ai += 1;
            }
        }
        let arm_max = star_arm_px(star_arm(f32::from(ch), STAR_ARM_HERO * STAR_ARM_INK));
        let side = i64::from(arm_max).checked_mul(2)?.checked_add(1)?;
        art_size(side, side)?;
        Some((p, rim, arm_max))
    }

    #[inline]
    fn build(ch: u16, (p, rim, arm_max): (i64, i64, i32)) -> Self {
        let arts = [KITTY_SHORT, kitty_tall()];
        let poses = [KittyPose::Open, KittyPose::Blink, KittyPose::Flick];
        let frame = |i: usize| arts[i / 3].frame(poses[i % 3]);
        let kitty = std::array::from_fn(|i| {
            fx_art::indexed(frame(i), &KITTY_PALETTE[1..]).expect("the kitty's art is well formed")
        });
        let rim = std::array::from_fn(|i| RimArt::of(frame(i), p, rim));
        let stars_len = if arm_max > 0 { arm_max as usize } else { 0 };
        let mut stars = Vec::with_capacity(stars_len);
        let mut arm = 1;
        while arm <= arm_max {
            stars.push([StarArt::of(arm, false), StarArt::of(arm, true)]);
            arm += 1;
        }
        Self {
            ch,
            kitty,
            rim,
            stars: stars.into_boxed_slice(),
        }
    }

    /// The cell height these arts are for.
    #[must_use]
    #[inline]
    pub const fn ch(&self) -> u16 {
        self.ch
    }

    /// Every art.
    #[inline]
    pub fn arts(&self) -> impl Iterator<Item = &Arc<FxArt>> {
        let kitty = self.kitty.iter();
        let rim = self.rim.iter().flatten().map(|r| &r.art);
        let stars = self
            .stars
            .iter()
            .flatten()
            .flatten()
            .flat_map(|s| std::iter::once(s.pieces.folded()).chain(s.pieces.peeled().iter()));
        kitty.chain(rim).chain(stars)
    }

    #[inline]
    const fn kitty(&self, art: &KittyArt, pose: KittyPose) -> &Arc<FxArt> {
        &self.kitty[kitty_slot(art, pose)]
    }

    #[inline]
    const fn rim(&self, art: &KittyArt, pose: KittyPose) -> Option<&RimArt> {
        self.rim[kitty_slot(art, pose)].as_ref()
    }

    #[inline]
    const fn star(&self, arm: i64, gold: bool) -> Option<&StarArt> {
        if arm < 1 {
            return None;
        }
        let i = (arm - 1) as usize;
        if i < self.stars.len() {
            self.stars[i][gold as usize].as_ref()
        } else {
            None
        }
    }
}

struct UnderItems<'a> {
    clip: Rect,
    items: &'a mut Vec<FxItem>,
    palette: &'a mut Vec<FxInk>,
    /// The side stack the last item opened, which the next slice continues.
    side: Option<usize>,
}

impl Ramps for UnderItems<'_> {
    #[inline]
    fn ramp(&mut self, clip: Rect, r: Rect, l: Paint, rt: Paint) {
        self.side = None;
        if l.alpha == 0 && l.color == 0 && rt.color == 0 {
            return;
        }
        let c = r.meet(clip).meet(self.clip);
        if c.is_empty() {
            return;
        }
        let (l, rt) = clip_ramp(r, c, l, rt);
        let ink = self.palette.len() as u32;
        self.palette.extend([fx_ink(l), fx_ink(rt)]);
        self.items.push(FxItem::Rect(FxRect {
            x: c.x0 as i32,
            y: c.y0 as i32,
            w: (c.x1 - c.x0) as u16,
            h: (c.y1 - c.y0) as u16,
            ink,
        }));
    }

    #[inline]
    fn side(&mut self, clip: Rect, r: Rect, p: Paint) {
        let c = r.meet(clip).meet(self.clip);
        if c.is_empty() {
            return;
        }
        let (x, w) = (c.x0 as i32, (c.x1 - c.x0) as u16);
        let i = match self.side {
            Some(idx)
                if matches!(
                    &self.items[idx],
                    FxItem::Ramps(s)
                        if s.x == x && s.w == w && i64::from(s.y) + i64::from(s.h) == c.y0
                ) =>
            {
                idx
            }
            _ => {
                self.items.push(FxItem::Ramps(FxRamps {
                    x,
                    y: c.y0 as i32,
                    w,
                    h: 0,
                    ink: self.palette.len() as u32,
                }));
                self.items.len() - 1
            }
        };
        if let FxItem::Ramps(s) = &mut self.items[i] {
            s.h += (c.y1 - c.y0) as u16;
        }
        let ink = fx_ink(p);
        let rows = (c.y1 - c.y0) as usize;
        self.palette.resize(self.palette.len() + 2 * rows, ink);
        self.side = Some(i);
    }
}

#[inline]
fn fill_under(cx: &Emit<'_>, items: &mut Vec<FxItem>, palette: &mut Vec<FxInk>) {
    let mut out = UnderItems {
        clip: cx.geo.clip,
        items,
        palette,
        side: None,
    };
    cx.border(&mut out);
    if !cx.scene.serious {
        cx.trail(&mut out);
    }
    debug_assert!(
        out.items.len() <= CARD_UNDER_ITEMS_MAX,
        "the card's under content holds {} items",
        out.items.len()
    );
}

#[inline]
fn fill_over(cx: &Emit<'_>, arts: &CardArts, items: &mut Vec<FxItem>, palette: &mut Vec<FxInk>) {
    if cx.scene.serious {
        return;
    }
    kitty(cx, arts, items, palette);
    rim(cx, arts, items, palette);
    let trail = cx.trail_ink();
    let clip = cx.geo.clip;
    cx.each_star(&mut |s| {
        if s.cov < 1.0 || s.arm < 1 {
            return;
        }
        let Some(art) = arts.star(s.arm, s.gold) else {
            debug_assert!(
                false,
                "no star art of arm {} at a {} px cell",
                s.arm, arts.ch
            );
            return;
        };
        star(items, palette, art, clip, s, trail.covers());
    });
    debug_assert!(
        items.len() <= CARD_OVER_ITEMS_MAX,
        "the card's over content holds {} items",
        items.len()
    );
}

#[inline]
fn kitty(cx: &Emit<'_>, arts: &CardArts, items: &mut Vec<FxItem>, palette: &mut Vec<FxInk>) {
    let Some((at, pose)) = cx.kitty else {
        return;
    };
    let r = at.rect;
    let (rw, rh) = (r.x1 - r.x0, r.y1 - r.y0);
    if rw < 0 || rw > i64::from(u16::MAX) || rh < 0 || rh > i64::from(u16::MAX) {
        return;
    }
    let (dw, dh) = (rw as u16, rh as u16);
    let inks = KittyInk::new(cx.scene.coat, cx.scene.eye, cx.scene.dark);
    let base = palette.len() as u32;
    palette.extend([
        FxInk::over(inks.outline, 255),
        FxInk::over(inks.coat, 255),
        FxInk::over(inks.belly, 255),
        FxInk::over(inks.pupil, 255),
        FxInk::over(inks.eye, 255),
        FxInk::over(inks.nose, 255),
        FxInk::over(inks.glint, 255),
    ]);
    items.push(FxItem::Sprite(FxSprite {
        x: r.x0 as i32,
        y: r.y0 as i32,
        dw,
        dh,
        flip_x: false,
        art: Arc::clone(arts.kitty(&at.art, pose)),
        ink: base,
        clip: px_rect(cx.geo.clip),
    }));
}

#[inline]
fn rim(cx: &Emit<'_>, arts: &CardArts, items: &mut Vec<FxItem>, palette: &mut Vec<FxInk>) {
    let Some((at, pose)) = cx.kitty else {
        return;
    };
    let Some(rim) = arts.rim(&at.art, pose) else {
        return;
    };
    let r0 = at.rect;
    let base = palette.len() as u32;
    let (sx, sy) = ((r0.x0 + rim.origin.0) as i32, (r0.y0 + rim.origin.1) as i32);
    let (dw, dh) = (rim.art.w(), rim.art.h());
    #[inline]
    fn rim_sprite(
        (sx, sy): (i32, i32),
        (dw, dh): (u16, u16),
        rim: &RimArt,
        base: u32,
        clip: Rect,
    ) -> FxItem {
        FxItem::Sprite(FxSprite {
            x: sx,
            y: sy,
            dw,
            dh,
            flip_x: false,
            art: Arc::clone(&rim.art),
            ink: base,
            clip: px_rect(clip),
        })
    }
    let items_before = items.len();
    let clip = cx.geo.inner().meet(cx.geo.clip);
    let hole = cx.rim_hole();
    let mut ci = 0;
    while ci < rim.cover.len() {
        let c = &rim.cover[ci];
        ci += 1;
        let part = Rect {
            x0: r0.x0 + c.x0,
            y0: r0.y0 + c.y0,
            x1: r0.x0 + c.x1,
            y1: r0.y0 + c.y1,
        }
        .meet(clip);
        match hole {
            Some(hole) => {
                lay_cut(part, 0, 0, &|_| hole, &mut |piece| {
                    items.push(rim_sprite((sx, sy), (dw, dh), rim, base, piece));
                    true
                });
            }
            None if !part.is_empty() => {
                items.push(rim_sprite((sx, sy), (dw, dh), rim, base, part));
            }
            None => {}
        }
    }
    if items.len() == items_before {
        return;
    }
    let centre = ((r0.x0 + r0.x1) as f32 * 0.5, (r0.y0 + r0.y1) as f32 * 0.5);
    let (two_x0, two_y0) = (2 * r0.x0, 2 * r0.y0);
    palette.extend(rim.mids.iter().map(|&(mx, my)| {
        let mid = ((two_x0 + mx) as f32 * 0.5, (two_y0 + my) as f32 * 0.5);
        fx_ink(cx.rim_paint(mid, centre))
    }));
}

/// A card rectangle as an fx one (card-local pixels fit in `i32`).
#[inline]
const fn px_rect(r: Rect) -> PxRect {
    PxRect::new(r.x0 as i32, r.y0 as i32, r.x1 as i32, r.y1 as i32)
}

#[inline]
fn star(
    items: &mut Vec<FxItem>,
    palette: &mut Vec<FxInk>,
    art: &StarArt,
    geo_clip: Rect,
    s: StarCall,
    on_trail: &[Rect],
) {
    #[inline]
    fn star_sprite(
        (x, y): (i32, i32),
        (w, h): (u16, u16),
        art: &Arc<FxArt>,
        ink: u32,
        clip: Rect,
    ) -> FxItem {
        FxItem::Sprite(FxSprite {
            x,
            y,
            dw: w,
            dh: h,
            flip_x: false,
            art: Arc::clone(art),
            ink,
            clip: px_rect(clip),
        })
    }
    let StarCall {
        clip,
        centre: (sx, sy),
        cov,
        color,
        light: added,
        ..
    } = s;
    let p = &art.pieces;
    let (ox, oy) = p.origin();
    let folded = p.folded();
    let (x, y) = (sx + i64::from(ox), sy + i64::from(oy));
    let (w, h) = (folded.w(), folded.h());
    let at = Rect {
        x0: x,
        y0: y,
        x1: x + i64::from(w),
        y1: y + i64::from(h),
    };
    let b = at.meet(clip).meet(geo_clip);
    if b.is_empty() {
        return;
    }
    let cov_floor = cov.floor();
    let mut covs = [0.0f32; STAR_PAINTS];
    let mut ended = false;
    let mut id = 0;
    while id < STAR_PAINTS {
        covs[id] = match id {
            0 => cov_floor,
            1 if added => (cov * STAR_CORE_ADD).floor(),
            1 => cov_floor,
            id if id == STAR_PAINTS - 1 => {
                if added {
                    (cov * STAR_GLINT_COV).floor()
                } else {
                    cov_floor
                }
            }
            id => {
                let tip = (cov * art.tapers[id - 2]).floor();
                ended |= added && premul_rgb(color, tip as u8) == 0;
                if ended { 0.0 } else { tip }
            }
        };
        id += 1;
    }
    let xy_i32 = (x as i32, y as i32);
    let wh = (w, h);
    if added {
        let base = palette.len() as u32;
        let mut tones = [0u32; STAR_PAINTS];
        let mut tid = 0;
        while tid < STAR_PAINTS {
            tones[tid] = light(color, covs[tid]).color;
            tid += 1;
        }
        let classes = p.classes();
        let mut ci = 0;
        while ci < classes.len() {
            let members = &classes[ci];
            let mut sum = 0u32;
            let mut mi = 0;
            while mi < members.len() {
                sum = add_sat(sum, tones[usize::from(members[mi])]) & 0x00FF_FFFF;
                mi += 1;
            }
            palette.push(FxInk::add(sum));
            ci += 1;
        }
        items.push(star_sprite(xy_i32, wh, folded, base, b));
        return;
    }
    let base = palette.len() as u32;
    let rgbs = [color, WHITE];
    let mut ri = 0;
    while ri < rgbs.len() {
        let rgb = rgbs[ri];
        let mut ci = 0;
        while ci < covs.len() {
            palette.push(fx_ink(ink(rgb, covs[ci] * (255.0 / STAR_PRINT_FULL))));
            ci += 1;
        }
        ri += 1;
    }
    tone_rects(b, on_trail, |r, white| {
        let ink = base + if white { STAR_PAINTS as u32 } else { 0 };
        let peeled = p.peeled();
        let mut pi = 0;
        while pi < peeled.len() {
            items.push(star_sprite(xy_i32, wh, &peeled[pi], ink, r));
            pi += 1;
        }
    });
}

#[inline]
fn tone_rects(b: Rect, covers: &[Rect], mut lay: impl FnMut(Rect, bool)) {
    #[inline]
    const fn add_col_run(
        cols: &mut [(i64, i64, (i64, i64))],
        n: &mut usize,
        x0: i64,
        x1: i64,
        white: (i64, i64),
    ) {
        if x0 >= x1 {
            return;
        }
        let white = if white.0 < white.1 { white } else { (0, 0) };
        let cap = cols.len();
        let cur = *n;
        if cur > 0
            && cols[cur - 1].1 == x0
            && cols[cur - 1].2.0 == white.0
            && cols[cur - 1].2.1 == white.1
        {
            cols[cur - 1].1 = x1;
        } else if cur < cap {
            cols[cur] = (x0, x1, white);
            *n = cur + 1;
        } else {
            debug_assert!(false, "more column runs than a trail has segments");
        }
    }
    let (lo, hi) = b.x_span(covers);
    if lo >= hi {
        if b.x0 < b.x1 {
            lay(b, false);
        }
        return;
    }
    let mut cols = [(0i64, 0i64, (0i64, 0i64)); 2 * MAX_TRAIL_SEGMENTS as usize + 1];
    let mut n = 0usize;
    let mut x = b.x0;
    let cover_slice = &covers[lo..hi];
    let mut ci = 0;
    while ci < cover_slice.len() {
        let c = &cover_slice[ci];
        ci += 1;
        let (cx0, cx1) = (c.x0.max(b.x0), c.x1.min(b.x1));
        add_col_run(&mut cols, &mut n, x, cx0, (0, 0));
        add_col_run(
            &mut cols,
            &mut n,
            cx0,
            cx1,
            (c.y0.clamp(b.y0, b.y1), c.y1.clamp(b.y0, b.y1)),
        );
        x = x.max(cx1);
    }
    add_col_run(&mut cols, &mut n, x, b.x1, (0, 0));
    let mut ri = 0;
    while ri < n {
        let (x0, x1, (w0, w1)) = cols[ri];
        ri += 1;
        let run = Rect { x0, x1, ..b };
        if w0 >= w1 {
            lay(run, false);
            continue;
        }
        let parts = [(b.y0, w0, false), (w0, w1, true), (w1, b.y1, false)];
        let mut pi = 0;
        while pi < parts.len() {
            let (y0, y1, white) = parts[pi];
            if y0 < y1 {
                lay(Rect { y0, y1, ..run }, white);
            }
            pi += 1;
        }
    }
}

/// The items and palette the card lays for the one star `s` (any arm, its art built afresh).
#[cfg(test)]
pub(super) fn one_star(
    s: StarCall,
    geo_clip: Rect,
    on_trail: &[Rect],
) -> (Vec<FxItem>, Vec<FxInk>) {
    let (mut items, mut palette) = (Vec::new(), Vec::new());
    assert!(
        s.arm >= i64::from(i32::MIN) && s.arm <= i64::from(i32::MAX),
        "a test arm"
    );
    let arm = s.arm as i32;
    if let Some(art) = StarArt::of(arm, s.gold) {
        star(&mut items, &mut palette, &art, geo_clip, s, on_trail);
    }
    (items, palette)
}

/// The kitty's art in the arts for one frame, for the tests.
#[cfg(test)]
pub(super) fn kitty_art(arts: &CardArts, art: &KittyArt, pose: KittyPose) -> Arc<FxArt> {
    Arc::clone(arts.kitty(art, pose))
}

/// The rim's strip-id art.
#[cfg(test)]
pub(super) fn rim_art(
    arts: &CardArts,
    art: &KittyArt,
    pose: KittyPose,
) -> Option<(Arc<FxArt>, (i64, i64), usize)> {
    arts.rim(art, pose)
        .map(|r| (Arc::clone(&r.art), r.origin, r.mids.len()))
}

/// The rectangles the rim of one frame is laid in.
#[cfg(test)]
pub(super) fn rim_cover(arts: &CardArts, art: &KittyArt, pose: KittyPose) -> Option<Box<[Rect]>> {
    arts.rim(art, pose).map(|r| r.cover.clone())
}
