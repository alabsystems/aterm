// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CARD ON FX LAYERS (`docs/DESIGN-card-engine-2026-10-19.md` §4.8): the card's frame as two contents, card-local, the host lays at the card's origin as two layers ([`CardFrame::layers`]).

use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use aterm_core::render::{
    FxArt, FxBlend, FxContent, FxInk, FxItem, FxLayer, FxRamps, FxRect, FxSlot, FxSpace, FxSprite,
    PxRect,
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

/// How many contents pairs an episode cycles through: the frame the host is
/// building, the one the renderer last drew and one spare, so the pair a
/// frame refills is held by nobody in the steady state (`Arc::get_mut`).
const RING: usize = 3;

/// A card's SHAPE: its scene with the card's origin taken out ([`Self::of`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardShape {
    local: CardScene,
}

impl CardShape {
    /// `scene`'s shape, and the scene-space pixel its card-local `(0, 0)` lands on: the card's origin, the border's outer top-left.
    #[must_use]
    pub fn of(scene: &CardScene) -> (Self, (i32, i32)) {
        let s = scene;
        let (cw, ch) = (i64::from(s.cw), i64::from(s.ch));
        let pad = i64::from(s.pad_left).min(cw / 2);
        let above = if s.top > 0 {
            i64::from(s.pad_above).min(ch / 2)
        } else {
            0
        };
        let below = i64::from(s.pad_below).min(ch / 2);
        let origin = (
            sat_i32(i64::from(s.origin_x) - pad),
            sat_i32(i64::from(s.origin_y) + i64::from(s.top) * ch - above),
        );
        let (ox, oy) = (i64::from(origin.0), i64::from(origin.1));
        let outer = Rect {
            x0: 0,
            y0: 0,
            x1: pad + (i64::from(s.cols) + 1) * cw,
            y1: above + i64::from(s.rows) * ch + below,
        };
        let (cx0, cy0, cx1, cy1) = s.clip;
        let space = |v: i32| i64::from(v).clamp(0, i64::from(u16::MAX));
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
            pad_left: u16::try_from(pad).unwrap_or(u16::MAX),
            pad_above: u16::try_from(above).unwrap_or(u16::MAX),
            pad_below: u16::try_from(below).unwrap_or(u16::MAX),
            top: 1,
            clip,
            wand: s.wand.map(|(c, r)| (c, (r + 1).saturating_sub(s.top))),
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
fn sat_i32(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

/// One frame of a card: its two contents, card-local, and where the card's origin lands in the scene's pixel space ([`CardShape::of`]).
#[derive(Clone, Debug)]
pub struct CardFrame {
    /// The border and the trail, drawn beneath the glyph ink.
    pub under: Arc<FxContent>,
    /// The kitty, its rim and the stars, drawn over the glyphs.
    pub over: Arc<FxContent>,
    /// The card-local origin's pixel in the scene's space (window pixels).
    pub origin: (i32, i32),
}

/// The layer id of a card's `slot` content: the host's stable name for the card (its session) mixed with the slot, so a card's two layers keep their ids from frame to frame and two cards never share one.
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
    /// The frame as its two layers (design §4.8): `under` in window space at the card's origin, so a spill sees the border in the padding, and `over` in grid space; both [`FxBlend::Exact`], clipped to `pane` (window pixels), named by [`card_layer_id`] for `card`.
    #[must_use]
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
        if let Some((x, y)) = self
            .origin
            .0
            .checked_sub(grid.0)
            .zip(self.origin.1.checked_sub(grid.1))
        {
            over.offset = (x, y);
            let edge = |e: i32, g: i32| {
                if e == i32::MIN || e == i32::MAX {
                    e
                } else {
                    (i64::from(e) - i64::from(g))
                        .clamp(i64::from(i32::MIN) + 1, i64::from(i32::MAX) - 1)
                        as i32
                }
            };
            over.clip = PxRect::new(
                edge(pane.x0, grid.0),
                edge(pane.y0, grid.1),
                edge(pane.x1, grid.0),
                edge(pane.y1, grid.1),
            );
        } else {
            // A relative origin outside i32 cannot name this placement.
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

/// One card's last KEPT frame: its shape, which flow step of which grid it is (`None`: the SETTLED frame) and its two contents, kept so that a held card, or a flowing one between two of its steps, is replayed rather than drawn again ([`super::HarnessCard::replay`], [`super::HarnessCard::replay_step`]): the same `Arc`s, frame after frame.
#[derive(Clone, Debug)]
pub struct KeptFrame {
    pub(super) shape: CardShape,
    /// The flow step and the grid it was drawn on ([`super::HarnessCard::flow_step`]); `None` for the settled frame.
    pub(super) step: Option<(u64, u32)>,
    pub(super) under: Arc<FxContent>,
    pub(super) over: Arc<FxContent>,
}

impl KeptFrame {
    /// The flow step the kept frame is (`None`: the settled frame).
    #[must_use]
    #[inline]
    pub fn step(&self) -> Option<u64> {
        self.step.map(|(step, _)| step)
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

/// One card's frames, kept by its host beside the card ([`super::HarnessCard::draw`], [`super::HarnessCard::replay`]): the frame it keeps (settled, or the flow's current step), the ring of contents its frames are built in, and its cell height's arts.
#[derive(Debug, Default)]
pub struct CardFrames {
    kept: Option<KeptFrame>,
    ring: [Option<(Arc<FxContent>, Arc<FxContent>)>; RING],
    next: usize,
    arts: Option<Arc<CardArts>>,
}

impl CardFrames {
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

    /// The arts this card's frames are drawn with (its last frame's cell height), for a host that hands them to the renderer ahead of use.
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
    pub(super) fn fill(&mut self, cx: &Emit<'_>) -> (Arc<FxContent>, Arc<FxContent>) {
        let ch = cx.scene.ch;
        if self.arts.as_ref().is_none_or(|a| a.ch != ch) {
            self.arts = CardArts::for_cell(ch);
        }
        let Some(arts) = self.arts.as_deref() else {
            return Self::nothing();
        };
        let slot = self.next;
        self.next = (slot + 1) % RING;
        let (under, over) = self.ring[slot].get_or_insert_with(|| {
            (
                Arc::new(FxContent::default()),
                Arc::new(FxContent::default()),
            )
        });
        refill(under, |items, palette| fill_under(cx, items, palette));
        refill(over, |items, palette| fill_over(cx, arts, items, palette));
        (Arc::clone(under), Arc::clone(over))
    }
}

/// Refill `content` in place through `f`, re-sealed; a pair some frame still
/// holds (the renderer kept it longer than the ring's depth) is replaced by
/// a fresh one, the one allocation a miss costs.
fn refill(content: &mut Arc<FxContent>, f: impl FnOnce(&mut Vec<FxItem>, &mut Vec<FxInk>)) {
    if Arc::get_mut(content).is_none() {
        *content = Arc::new(FxContent::default());
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
    let (w, h) = (u16::try_from(w).ok()?, u16::try_from(h).ok()?);
    if w == 0
        || h == 0
        || w > aterm_core::render::FX_ART_MAX_SIDE
        || h > aterm_core::render::FX_ART_MAX_SIDE
    {
        return None;
    }
    Some((w, h, usize::from(w).checked_mul(usize::from(h))?))
}

/// The rim's strip-id art for one frame of the kitty at one pixel size: the
/// strips ([`rim_strips`]) as one Index8 picture, strip `k` texel `k + 1`
/// (they never overlap: one strip a texel), its top-left texel at `origin`
/// from the kitty's top-left; each strip's midpoint, doubled (`x0 + x1`,
/// `y0 + y1`, from the kitty's top-left), the input `rim_paint` reads; and
/// its cover ([`fx_art::cover`], at most [`RIM_COVER_MAX`] rectangles, from
/// the kitty's top-left): the rim is drawn one item a rectangle, so no
/// fragment runs over the kitty it rings or the air round it.
#[derive(Clone, Debug)]
struct RimArt {
    art: Arc<FxArt>,
    origin: (i64, i64),
    mids: Box<[(i64, i64)]>,
    cover: Box<[Rect]>,
}

impl RimArt {
    fn dimensions(frame: &[&str], p: i64, rim: i64) -> Option<(u16, u16, usize)> {
        if p <= 0 || rim <= 0 {
            return None;
        }
        let border = rim.checked_mul(2)?;
        let w = (KITTY_W as i64).checked_mul(p)?.checked_add(border)?;
        let h = i64::try_from(frame.len())
            .ok()?
            .checked_mul(p)?
            .checked_add(border)?;
        art_size(w, h)
    }

    /// The rim of `frame` at kitty pixel `p` and rim width `rim`; `None`
    /// with no rim (`rim == 0`), or an art past the registry's bounds.
    fn of(frame: &[&str], p: i64, rim: i64) -> Option<Self> {
        if rim == 0 || p == 0 {
            return None;
        }
        let (w16, h16, len) = Self::dimensions(frame, p, rim)?;
        let wu = usize::from(w16);
        let mut data = vec![0u8; len];
        let mut mids = Vec::new();
        let mut ok = true;
        rim_strips(frame, p, rim, |q| {
            let Ok(k) = u8::try_from(mids.len() + 1) else {
                ok = false;
                return;
            };
            mids.push((q.x0 + q.x1, q.y0 + q.y1));
            for y in q.y0..q.y1 {
                let row = (y + rim) as usize * wu;
                let x0 = row + (q.x0 + rim) as usize;
                let x1 = row + (q.x1 + rim) as usize;
                debug_assert!(
                    data[x0..x1].iter().all(|&t| t == 0),
                    "rim strips overlap on row {y}"
                );
                data[x0..x1].fill(k);
            }
        });
        if !ok {
            return None;
        }
        let art = FxArt::new(w16, h16, aterm_core::render::FxTexels::Index8, data)?;
        let cover = fx_art::cover(&art, RIM_COVER_MAX)
            .iter()
            .map(|t| {
                let px = fx_art::cover_px(*t, (w16, h16), (w16, h16), false);
                Rect {
                    x0: i64::from(px.x0) - rim,
                    y0: i64::from(px.y0) - rim,
                    x1: i64::from(px.x1) - rim,
                    y1: i64::from(px.y1) - rim,
                }
            })
            .collect();
        Some(Self {
            art: fx_art::registry().intern(art),
            origin: (-rim, -rim),
            mids: mids.into_boxed_slice(),
            cover,
        })
    }
}

/// One star's arts ([`star_pieces`] through [`fx_art::pieces`]): its folded
/// and peeled forms and each point step's taper, which its paints read.
#[derive(Clone, Debug)]
struct StarArt {
    pieces: PieceArt,
    tapers: [f32; STAR_TAPER_STEPS as usize],
}

impl StarArt {
    /// The star of half-length `arm` (glints when `gold`).
    fn of(arm: i32, gold: bool) -> Option<Self> {
        if arm < 1 {
            return None;
        }
        let side = i64::from(arm).checked_mul(2)?.checked_add(1)?;
        art_size(side, side)?;
        let laid = star_pieces(arm, gold);
        let mut tapers = [0.0; STAR_TAPER_STEPS as usize];
        let rects: Vec<(PxRect, u8)> = laid
            .iter()
            .map(|p| {
                if let StarPart::Tip { step, taper } = p.part {
                    tapers[usize::from(step)] = taper;
                }
                let r = PxRect::new(p.x, p.y, p.x + p.w, p.y + p.h);
                (r, p.part.id())
            })
            .collect();
        let pieces = fx_art::pieces(&rects)?;
        debug_assert!(
            pieces.depth() <= STAR_DEPTH_MAX,
            "a star of arm {arm} stacks {} deep",
            pieces.depth()
        );
        Some(Self { pieces, tapers })
    }
}

/// The card's arts at one cell height: the kitty's six frames (scale-free), the rim of each at the height's kitty pixel, and every star the height's card lays — each arm from 1 to the largest a light hero star reaches, plain and gold.
#[derive(Debug)]
pub struct CardArts {
    ch: u16,
    kitty: [Arc<FxArt>; 6],
    rim: [Option<RimArt>; 6],
    stars: Box<[[Option<StarArt>; 2]]>,
}

/// Which of the six kitty frames `art` (two rows or three) in `pose` is.
#[inline]
fn kitty_slot(art: &KittyArt, pose: KittyPose) -> usize {
    (art.rows() - 2).min(1) * 3 + pose as usize
}

impl CardArts {
    /// The arts for cell height `ch`, shared by every card of that height while any holds them: built on the first ask, kept weakly.
    #[must_use]
    pub fn for_cell(ch: u16) -> Option<Arc<Self>> {
        let dimensions = Self::dimensions(ch)?;
        #[cfg(test)]
        if super::stand_in_active() {
            return Some(Arc::new(Self::build(ch, dimensions)));
        }
        static MEMO: Mutex<Vec<(u16, Weak<CardArts>)>> = Mutex::new(Vec::new());
        let mut memo = MEMO.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(live) = memo
            .iter()
            .find(|(c, _)| *c == ch)
            .and_then(|(_, w)| w.upgrade())
        {
            return Some(live);
        }
        memo.retain(|(_, w)| w.strong_count() > 0);
        let arts = Arc::new(Self::build(ch, dimensions));
        memo.push((ch, Arc::downgrade(&arts)));
        Some(arts)
    }

    /// Check all six rim pictures and the largest star before any cache is allocated.
    pub(super) fn dimensions(ch: u16) -> Option<(i64, i64, i32)> {
        if ch == 0 {
            return None;
        }
        let p = i64::from(kitty_px(ch));
        let rim = rim_px(p, i64::from(stroke_px(ch)));
        if rim > 0 {
            for art in [KITTY_SHORT, kitty_tall()] {
                for pose in [KittyPose::Open, KittyPose::Blink, KittyPose::Flick] {
                    RimArt::dimensions(art.frame(pose), p, rim)?;
                }
            }
        }
        let arm_max = star_arm_px(star_arm(f32::from(ch), STAR_ARM_HERO * STAR_ARM_INK));
        let side = i64::from(arm_max).checked_mul(2)?.checked_add(1)?;
        art_size(side, side)?;
        Some((p, rim, arm_max))
    }

    /// Build the arts for an admitted cell height `ch`.
    fn build(ch: u16, (p, rim, arm_max): (i64, i64, i32)) -> Self {
        let arts = [KITTY_SHORT, kitty_tall()];
        let poses = [KittyPose::Open, KittyPose::Blink, KittyPose::Flick];
        let frame = |i: usize| arts[i / 3].frame(poses[i % 3]);
        let kitty = std::array::from_fn(|i| {
            fx_art::indexed(frame(i), &KITTY_PALETTE[1..]).expect("the kitty's art is well formed")
        });
        let rim = std::array::from_fn(|i| RimArt::of(frame(i), p, rim));
        let stars = (1..=arm_max)
            .map(|arm| [StarArt::of(arm, false), StarArt::of(arm, true)])
            .collect();
        Self {
            ch,
            kitty,
            rim,
            stars,
        }
    }

    /// The cell height these arts are for.
    #[must_use]
    #[inline]
    pub const fn ch(&self) -> u16 {
        self.ch
    }

    /// Every art, for a host that hands them to the renderer's atlas ahead of their first frame (`RenderInput::fx_prefetch`).
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
    fn kitty(&self, art: &KittyArt, pose: KittyPose) -> &Arc<FxArt> {
        &self.kitty[kitty_slot(art, pose)]
    }

    #[inline]
    fn rim(&self, art: &KittyArt, pose: KittyPose) -> Option<&RimArt> {
        self.rim[kitty_slot(art, pose)].as_ref()
    }

    #[inline]
    fn star(&self, arm: i64, gold: bool) -> Option<&StarArt> {
        let i = usize::try_from(arm.checked_sub(1)?).ok()?;
        self.stars.get(i)?[usize::from(gold)].as_ref()
    }
}

/// The under content's sink: every ramp clipped and re-ramped where a clip
/// cuts it ([`clip_ramp`]), the border's side slices run together into one
/// stack of one-row ramps a side.
struct UnderItems<'a> {
    clip: Rect,
    items: &'a mut Vec<FxItem>,
    palette: &'a mut Vec<FxInk>,
    /// The side stack the last item opened, which the next slice continues.
    side: Option<usize>,
}

impl Ramps for UnderItems<'_> {
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

    fn side(&mut self, clip: Rect, r: Rect, p: Paint) {
        let c = r.meet(clip).meet(self.clip);
        if c.is_empty() {
            return;
        }
        let (x, w) = (c.x0 as i32, (c.x1 - c.x0) as u16);
        let open = self.side.filter(|&i| {
            matches!(&self.items[i], FxItem::Ramps(s)
                if s.x == x && s.w == w && i64::from(s.y) + i64::from(s.h) == c.y0)
        });
        let i = open.unwrap_or_else(|| {
            self.items.push(FxItem::Ramps(FxRamps {
                x,
                y: c.y0 as i32,
                w,
                h: 0,
                ink: self.palette.len() as u32,
            }));
            self.items.len() - 1
        });
        if let FxItem::Ramps(s) = &mut self.items[i] {
            s.h += (c.y1 - c.y0) as u16;
        }
        let ink = fx_ink(p);
        let rows = (c.y1 - c.y0) as usize;
        self.palette.resize(self.palette.len() + 2 * rows, ink);
        self.side = Some(i);
    }
}

/// The under content of `cx`'s frame: the border, then (but in serious
/// mode) the trail.
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

/// The over content of `cx`'s frame (none in serious mode): the kitty, its
/// rim, then the burst, rainbow glints, field and ride sparkles
/// ([`Emit::each_star`]), using the same cached star arts.
fn fill_over(cx: &Emit<'_>, arts: &CardArts, items: &mut Vec<FxItem>, palette: &mut Vec<FxInk>) {
    if cx.scene.serious {
        return;
    }
    kitty(cx, arts, items, palette);
    rim(cx, arts, items, palette);
    let trail = cx.trail_ink();
    let dark = cx.scene.dark;
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
        star(items, palette, art, clip, s, trail.covers(), dark);
    });
    debug_assert!(
        items.len() <= CARD_OVER_ITEMS_MAX,
        "the card's over content holds {} items",
        items.len()
    );
}

/// The kitty: one sprite of its frame's art at its square pixel, its seven
/// colours opaque ([`KittyInk`]), drawn whole inside the clip or not at all
/// (`kitty_rect`).
fn kitty(cx: &Emit<'_>, arts: &CardArts, items: &mut Vec<FxItem>, palette: &mut Vec<FxInk>) {
    let Some((at, pose)) = cx.kitty else {
        return;
    };
    let r = at.rect;
    let (Ok(dw), Ok(dh)) = (u16::try_from(r.x1 - r.x0), u16::try_from(r.y1 - r.y0)) else {
        return;
    };
    let inks = KittyInk::new(cx.scene.coat, cx.scene.eye, cx.scene.dark);
    let base = palette.len() as u32;
    palette.extend(
        KITTY_PALETTE[1..]
            .iter()
            .map(|&c| FxInk::over(inks.of(c).unwrap_or(0), 255)),
    );
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

/// The rim: one sprite of its strip-id art round the kitty, each strip its
/// own entry in the border's spectrum ([`Emit::rim_paint`] at the strip's
/// midpoint), laid one item a rectangle of its cover ([`RimArt::cover`]),
/// inside the border's stroke and cut round the trail's hole
/// ([`Emit::rim_hole`]) — one item each side of the hole a rectangle
/// reaches.
fn rim(cx: &Emit<'_>, arts: &CardArts, items: &mut Vec<FxItem>, palette: &mut Vec<FxInk>) {
    let Some((at, pose)) = cx.kitty else {
        return;
    };
    let Some(rim) = arts.rim(&at.art, pose) else {
        return;
    };
    let r0 = at.rect;
    let centre = ((r0.x0 + r0.x1) as f32 * 0.5, (r0.y0 + r0.y1) as f32 * 0.5);
    let base = palette.len() as u32;
    palette.extend(rim.mids.iter().map(|&(mx, my)| {
        let mid = ((2 * r0.x0 + mx) as f32 * 0.5, (2 * r0.y0 + my) as f32 * 0.5);
        fx_ink(cx.rim_paint(mid, centre))
    }));
    let sprite = |clip: Rect| {
        FxItem::Sprite(FxSprite {
            x: (r0.x0 + rim.origin.0) as i32,
            y: (r0.y0 + rim.origin.1) as i32,
            dw: rim.art.w(),
            dh: rim.art.h(),
            flip_x: false,
            art: Arc::clone(&rim.art),
            ink: base,
            clip: px_rect(clip),
        })
    };
    let clip = cx.geo.inner().meet(cx.geo.clip);
    let hole = cx.rim_hole();
    for c in &rim.cover {
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
                    items.push(sprite(piece));
                    true
                });
            }
            None if !part.is_empty() => items.push(sprite(part)),
            None => {}
        }
    }
}

/// A card rectangle as an fx one (card-local pixels fit in `i32`).
#[inline]
const fn px_rect(r: Rect) -> PxRect {
    PxRect::new(r.x0 as i32, r.y0 as i32, r.x1 as i32, r.y1 as i32)
}

/// One star ([`StarCall`]): on dark one FOLDED sprite, each class of its
/// pieces the `add_sat` sum of their light; on light its PEELED sprites, a
/// layer of its depth each, every piece in its print ink — the paper's
/// white where the pixel lies on the trail's ink (`on_trail`, segment by
/// segment), its ink off it, so a star the trail's edge crosses is whole in
/// two tones, cut into tone rectangles ([`tone_rects`]).
fn star(
    items: &mut Vec<FxItem>,
    palette: &mut Vec<FxInk>,
    art: &StarArt,
    geo_clip: Rect,
    s: StarCall,
    on_trail: &[Rect],
    dark: bool,
) {
    let StarCall {
        clip,
        centre: (sx, sy),
        cov,
        color,
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
    let mut covs = [0.0f32; STAR_PAINTS];
    let mut ended = false;
    for (id, c) in covs.iter_mut().enumerate() {
        *c = match id {
            0 => cov.floor(),
            1 if dark => (cov * STAR_CORE_ADD).floor(),
            1 => cov.floor(),
            id if id == STAR_PAINTS - 1 => {
                if dark {
                    (cov * STAR_GLINT_COV).floor()
                } else {
                    cov.floor()
                }
            }
            id => {
                let tip = (cov * art.tapers[id - 2]).floor();
                ended |= dark && premul_rgb(color, tip as u8) == 0;
                if ended { 0.0 } else { tip }
            }
        };
    }
    let sprite = |art: &Arc<FxArt>, ink: u32, clip: Rect| {
        FxItem::Sprite(FxSprite {
            x: x as i32,
            y: y as i32,
            dw: w,
            dh: h,
            flip_x: false,
            art: Arc::clone(art),
            ink,
            clip: px_rect(clip),
        })
    };
    if dark {
        let base = palette.len() as u32;
        let tones: [u32; STAR_PAINTS] = std::array::from_fn(|id| light(color, covs[id]).color);
        palette.extend(p.classes().iter().map(|members| {
            FxInk::add(members.iter().fold(0, |sum, &id| {
                add_sat(sum, tones[usize::from(id)]) & 0x00FF_FFFF
            }))
        }));
        items.push(sprite(folded, base, b));
        return;
    }
    let base = palette.len() as u32;
    for rgb in [color, WHITE] {
        palette.extend(
            covs.iter()
                .map(|&c| fx_ink(ink(rgb, c * 255.0 / STAR_PRINT_FULL))),
        );
    }
    tone_rects(b, on_trail, |r, white| {
        let ink = base + if white { STAR_PAINTS as u32 } else { 0 };
        for layer in p.peeled() {
            items.push(sprite(layer, ink, r));
        }
    });
}

/// The tone rectangles of a light star's box `b` (clipped) under the trail's
/// ink `covers` (its segments, left to right, each starting where the last
/// ends): `(rect, white)` for each, `white` where the pixels lie on the ink.
fn tone_rects(b: Rect, covers: &[Rect], mut lay: impl FnMut(Rect, bool)) {
    let lo = covers.partition_point(|c| c.x1 <= b.x0);
    let span = covers[lo..].partition_point(|c| c.x0 < b.x1);
    let mut cols = [(0i64, 0i64, (0i64, 0i64)); 2 * MAX_TRAIL_SEGMENTS as usize + 1];
    let mut n = 0usize;
    let mut add = |x0: i64, x1: i64, white: (i64, i64)| {
        if x0 >= x1 {
            return;
        }
        let white = if white.0 < white.1 { white } else { (0, 0) };
        let cap = cols.len();
        match n.checked_sub(1).map(|last| &mut cols[last]) {
            Some(prev) if prev.1 == x0 && prev.2 == white => prev.1 = x1,
            _ if n < cap => {
                cols[n] = (x0, x1, white);
                n += 1;
            }
            _ => debug_assert!(false, "more column runs than a trail has segments"),
        }
    };
    let mut x = b.x0;
    for c in &covers[lo..lo + span] {
        let (cx0, cx1) = (c.x0.max(b.x0), c.x1.min(b.x1));
        add(x, cx0, (0, 0));
        add(cx0, cx1, (c.y0.clamp(b.y0, b.y1), c.y1.clamp(b.y0, b.y1)));
        x = x.max(cx1);
    }
    add(x, b.x1, (0, 0));
    for &(x0, x1, (w0, w1)) in &cols[..n] {
        let run = Rect { x0, x1, ..b };
        if w0 >= w1 {
            lay(run, false);
            continue;
        }
        for (y0, y1, white) in [(b.y0, w0, false), (w0, w1, true), (w1, b.y1, false)] {
            if y0 < y1 {
                lay(Rect { y0, y1, ..run }, white);
            }
        }
    }
}

/// The items and palette the card lays for the one star `s` (any arm, its art built afresh), inside `geo_clip`, under the light trail's ink `on_trail`: [`star`], for the tests.
#[cfg(test)]
pub(super) fn one_star(
    s: StarCall,
    geo_clip: Rect,
    on_trail: &[Rect],
    dark: bool,
) -> (Vec<FxItem>, Vec<FxInk>) {
    let (mut items, mut palette) = (Vec::new(), Vec::new());
    let arm = i32::try_from(s.arm).expect("a test arm");
    if let Some(art) = StarArt::of(arm, s.gold) {
        star(&mut items, &mut palette, &art, geo_clip, s, on_trail, dark);
    }
    (items, palette)
}

/// The kitty's art in the arts for one frame, for the tests.
#[cfg(test)]
pub(super) fn kitty_art(arts: &CardArts, art: &KittyArt, pose: KittyPose) -> Arc<FxArt> {
    Arc::clone(arts.kitty(art, pose))
}

/// The rim's strip-id art, its top-left's place from the kitty's top-left and its strip count for one frame, for the tests.
#[cfg(test)]
pub(super) fn rim_art(
    arts: &CardArts,
    art: &KittyArt,
    pose: KittyPose,
) -> Option<(Arc<FxArt>, (i64, i64), usize)> {
    arts.rim(art, pose)
        .map(|r| (Arc::clone(&r.art), r.origin, r.mids.len()))
}

/// The rectangles the rim of one frame is laid in, from the kitty's top-left ([`RimArt::cover`]), for the tests.
#[cfg(test)]
pub(super) fn rim_cover(arts: &CardArts, art: &KittyArt, pose: KittyPose) -> Option<Box<[Rect]>> {
    arts.rim(art, pose).map(|r| r.cover.clone())
}
