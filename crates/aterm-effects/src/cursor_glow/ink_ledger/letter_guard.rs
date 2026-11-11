// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The letter guard: whatever a frame's lights add up to, every letter they
//! lie on keeps the legibility bar.
//!
//! Each style keeps its own lights off the letters against its own account,
//! so the frame's sum can still wash a letter out. The guard is the one law
//! over all of them, run once on the frame's finished streams before they are
//! published. For each lettered cell of the frame's text but the caret's, it
//! composites every light that reaches the cell exactly as the renderer will
//! (the under-text quads on the ground, then the text, then the over-text
//! quads and the halos on ground and ink alike) and reads the letter as the
//! trail judges read it, by the ring round its strokes ([`ring`]): each solid
//! ink pixel of the glyph's own raster against its ring's channel medians,
//! each pixel row's lower median, the worst row, and by its core against its
//! bed where that reads lower. Where the face's table holds
//! no raster for the cell's glyph it reads each pixel as solid ink
//! ([`SOLID_SHARE`] of the clean ratio) against each channel's least and
//! greatest ground within [`RING`] px, between which each of the judge's
//! medians lies. Where the cell falls under its bar it finds the largest
//! share of those lights that keeps it. The bar is [`LETTER_FLOOR`]; a letter
//! whose own bare reading is under the floor and at [`WCAG_AA`] as the judges
//! read it ([`AA_READ`]) or over keeps AA with a level in hand
//! ([`LEVEL_ROOM`]), and one under that keeps [`OWN_SHARE`] of its reading
//! ([`letter_bar`]), with [`OWN_ROOM`] over AA or the share for a letter read
//! by its solid ink, never past its reading, and a level over the share for
//! one read by its raster ([`raster_bar`]). Against the page shades alone
//! ([`CursorGlow::hold_page_shades`]), a letter read by its raster keeps the
//! lower of that bar and its own reading ([`shade_bar`]). A cell read by its
//! raster is read again on the lights the frame will draw, its cuts laid
//! ([`CursorGlow::verify_cuts`]).
//! Every light is then cut to that share over the cell and its ring, easing
//! back to its whole strength over the ledger's feather. A frame with no cut
//! is byte-identical to the one it would have drawn.
//!
//! A row's cuts are one eased ramp, not a cell-by-cell comb ([`RAMP_SLOPE`]):
//! between its first and last cut cell, every cell (a word gap, a letter the
//! whole light left at the floor) keeps no more than the least of each cut
//! cell's share plus the slope per column between them. Cut letter by letter,
//! Phaser's neon bar kept its whole light in each word gap between dimmed
//! letters and its spectrum underline went dashed (2026-10-23, looks6 N1).
//! A word gap ([`WORD_GAP_COLS`]) between two letters, one of them cut at
//! least, keeps no more than the straight line between their shares (an
//! uncut letter's being whole): climbing the slope from each, a one-column
//! gap between letters cut to a fifth of their light kept a share and a half
//! of theirs, and past the run's last cut a gap kept its whole light beside
//! its cut letter, so the guard left Phaser's dark bar brighter in the word
//! gaps than over their letters (the replay's typing scene, looks7 M4; the
//! gallery's stripe was the bed's own knot, `bed::bridge_word_gaps`). The
//! ramp only ever lowers a share, so every letter keeps its bar.

use super::*;
use aterm_core::render::TrailCell;
use aterm_render::{GlowEnd, GlowRamp, Laid, glow_end};

mod ring;
mod thin_caret;
pub(crate) use thin_caret::ThinCaret;

/// The bar every letter a light lies on keeps (WCAG AA for text, with a
/// hair of room for the judge's rounding).
pub(crate) const LETTER_FLOOR: f32 = 4.6;
/// WCAG AA for text: a letter that reads this or more on its bare page reads it
/// under the lights too ([`letter_bar`]), the legibility law's bar.
pub(crate) const WCAG_AA: f32 = 4.5;
/// [`WCAG_AA`] as the judges read it: `aterm-lab trail-contrast` gives a reading to two decimals,
/// so a letter of 4.495 or more reads 4.50, at AA, and keeps it ([`letter_bar`]). Held at the share
/// a letter under AA keeps, the flood's `o` (#c1479f on GitHub Light, 4.497 on the bare page) read
/// 4.47 under a word's wash (after-land round 3).
pub(crate) const AA_READ: f32 = WCAG_AA - 0.005;
/// The share of a letter's clean ratio its solid ink keeps (the trail judge's,
/// `prism_glass::colour::SOLID_SHARE`, which `aterm-lab trail-contrast` reads
/// by): the guard judges the dimmest ink the judge counts, not the face's full
/// colour.
const SOLID_SHARE: f32 = crate::rainbow_kitty::prism_glass::colour::SOLID_SHARE as f32;
/// The share of its own bare reading a letter that reads under [`WCAG_AA`] on
/// the bare page keeps under the lights ([`letter_bar`]), so the bar it cannot
/// reach never leaves it unguarded. The judges' dim-page rule. A letter that
/// reads AA or more keeps AA: a share of one of 4.54 was 4.40, and the flood's
/// and the coloured line's app inks on paper read that under the glass's wash
/// and Comet's tail (after-land round 3, W1, W2 and W8).
///
/// Read by its raster as the judge reads it ([`ring`]), a letter keeps this
/// share and a level of rounding ([`LEVEL_ROOM`]): until round 27 every letter kept the floor's room over it
/// ([`OWN_ROOM`]), which cost the dim pages light (the guard held Classic pink's
/// letters near their whole own reading and cut its rail beside the caret to a
/// third, `a_dim_page_s_rail_is_one_stroke`). A letter read by its solid ink
/// keeps the room still.
const OWN_SHARE: f32 = 0.97;
/// The room a letter read by its solid ink (one whose glyph the face's table
/// holds no raster for, or that does not draw it alone: an accented, Cyrillic,
/// Greek or Han letter, a slanted or decorated one) keeps over [`OWN_SHARE`]
/// ([`letter_bar`]), as [`LETTER_FLOOR`] keeps over WCAG AA's 4.5: the guard
/// reads every pixel of such a cell against its grounds' least and greatest
/// (by luminance until the underlined letters' review, 2026-11-06; by each
/// channel since) and the judge each stroke against its ring, and at the bare
/// share a dim letter the guard passed read under it by the judge (Solarized
/// Dark, Water typing: `u` at 4.39:1 of its own 4.57, 96%, looks7; Fire
/// bursting Cyrillic: `м` at 4.20:1 of its own 4.45, 94%, round 27's review).
const OWN_ROOM: f32 = LETTER_FLOOR / 4.5;
/// How far from an ink pixel its ground is read, px (the judge's ring),
/// and how far round a lettered cell the guard judges a light: a light
/// kept that far off a lettered row is never cut.
pub(crate) const RING: i32 = 2;
/// What the dark landing ring's rim keeps of its light where it passes
/// under a lettered cell ([`CursorGlow::pass_under_letters`]): enough that
/// the arc runs on unbroken through the text, faint as if seen between the
/// strokes. Replayed with the host's bloom and SDR crown through the trail
/// gallery's `enter-bottom` (looks8 round 2), the rim at up to a quarter of
/// its light leaves every style's worst letter where the rim cut off the
/// letters leaves it; at three tenths Lumen's `q` falls to 4.85:1, at seven
/// twentieths to 4.45. Cut off the letters whole, the rim's leftovers in
/// the word gaps and blank cells read as stray glyphs (a tick, a `)`).
pub(crate) const RIM_UNDER_LETTERS: f32 = 0.2;
/// The least share of the dark landing ring's laid rim that must keep its
/// whole light, off the letters ([`CursorGlow::pass_under_letters`]), for the
/// rim to be drawn at all. Under the letters it is a fifth of its light,
/// faint as if seen between the strokes, so what reads of the rim is its
/// whole part: where the letters and the grid's edge leave only a short
/// arc of it whole, that arc stands alone in the blank cells beside the
/// caret as a stray glyph (the trail gallery's `enter-bottom`, looks8
/// round 2: a `)` right of the caret at the Enter and a `|` at `e120`
/// under Phaser, Lumen, Comet, Sparkle and Laser on Solarized Dark and
/// dark, the letters of the line above and the `$` taking its top and
/// left and the grid's bottom its lower half). The landing's veil marks it
/// there instead.
pub(crate) const RIM_SHOWN_SHARE: f32 = 0.5;
/// The core coverage at and over which the guard judges a letter's ink as
/// its colour whole ([`Judge::cover`]): within a hair of it, the solid ink
/// the guard reads already lies under the core.
const CORE_SOLID: f32 = 0.995;

/// What the face lays of a letter's core ([`aterm_render::GlyphInk`]): its coverage `c` in
/// linear light, and where in the cell its glyph lays ink (`[x0, y0, x1, y1]` from the
/// cell's top left, end exclusive; `None`: anywhere in the cell).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Cover {
    c: f32,
    seat: Option<[i16; 4]>,
}

/// Bisection steps for a cell's share.
const STEPS: usize = 7;
/// The greatest share under the whole the bisection reaches (`127 / 128`),
/// the answer it gives a cell every one of its shares holds for but the
/// whole.
const NEAR_WHOLE: f32 = 1.0 - 1.0 / (1u32 << STEPS) as f32;
/// How much more of its light a cell of a cut row keeps than a cut cell one
/// column away (a share per column): the row's cuts ease as one ramp, and a
/// word gap between two cut letters is no brighter than they are beyond it.
const RAMP_SLOPE: f32 = 0.08;
/// The widest run of columns between two cut cells read as a word gap: it
/// keeps the straight line between their shares ([`ramp_row`]).
const WORD_GAP_COLS: i32 = 2;

/// Which stream a light rides, in the renderer's draw order. The four
/// streams of quads come first, each its index in [`Streams::quads`], the
/// two on the ground before the two over the ink.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Stream {
    /// `glow_under`: on the ground, beneath the ink.
    Under = 0,
    /// A `glow_under` ramp's light within the row being read: a quad of
    /// [`RampRows::under`].
    UnderRow = 1,
    /// `cursor_glow_add`: over ground and ink.
    Over = 2,
    /// A `cursor_glow_add` ramp's light within the row: a quad of
    /// [`RampRows::over`].
    OverRow = 3,
    /// `glow_halo`, [`HaloMode::Add`] (drawn before every `Over` halo).
    HaloAdd = 4,
    /// `glow_halo`, [`HaloMode::Over`].
    HaloOver = 5,
}

impl Stream {
    /// Whether the light lies on the ground alone, beneath the ink.
    #[inline]
    const fn on_ground(self) -> bool {
        (self as u8) <= Self::UnderRow as u8
    }
}

#[derive(Clone, Copy)]
struct RowLight {
    stream: Stream,
    idx: u32,
    x0: i32,
    x1: i32,
    /// A ramp's lights within the row, `idx .. idx + rows` of its
    /// [`RampRows`] stream (`0`: one light, `idx` itself).
    rows: u32,
}

/// The lights a text row's ramps ([`GlowRamps`]) lay within its band, each
/// the quad it stands for (a run one quad, read off the ramp's kept ends):
/// the guard reads a ramp's rows exactly as the legacy stream's quads, from
/// one row light a ramp ([`RowLight::rows`]).
#[derive(Default)]
pub(crate) struct RampRows {
    under: Vec<GlowQuad>,
    over: Vec<GlowQuad>,
}

/// The frame's lights a text row reads: the under-text quads, the over-text
/// quads and the row's ramp lights of each, indexed by [`Stream`], and the
/// halos.
#[derive(Clone, Copy)]
struct Streams<'a> {
    quads: [&'a [GlowQuad]; 4],
    halos: &'a [RainHalo],
}

impl<'a> Streams<'a> {
    /// The quad an `Under`, `Over`, `UnderRow` or `OverRow` light names.
    #[inline]
    const fn quad(self, stream: Stream, i: usize) -> &'a GlowQuad {
        &self.quads[stream as usize][i]
    }
}

/// A quad's whole light on a pixel of a cell's ring box that an earlier
/// light already lies on ([`LetterGuard::lay_lights`]): the pixel, and the
/// light as [`pack_light`] packs it.
#[derive(Clone, Copy)]
struct LitPx {
    k: u32,
    v: u32,
}

/// A quad's light on a pixel, its premultiplied colour `c` and opacity `a`
/// as [`quad_at`] reads them, as one word: `0` is a light that leaves
/// every pixel as it was, and no light at all.
#[inline]
const fn pack_light(c: u32, a: u8) -> u32 {
    (a as u32) << 24 | (c & 0x00FF_FFFF)
}

/// A share of a cell's lights [`LetterGuard::paint`] weighs: the whole
/// light, `k / 128` of it (every share the bisection weighs, [`STEPS`]
/// halvings of `(0, 1)`), or any other. A byte scaled as `scale_rgb` and
/// [`scale_u8`] scale it, `(c as f32 * s) as u32`, is exactly `c · k >> 7`
/// at `k / 128`: the product needs at most 15 of `f32`'s 24 bits.
#[derive(Clone, Copy)]
enum Share {
    Whole,
    Of128(u32),
    Any(f32),
}

impl Share {
    #[inline]
    const fn of(s: f32) -> Self {
        if s >= 1.0 {
            return Self::Whole;
        }
        let k = s * 128.0;
        if k > 0.0 && k == k.trunc() {
            Self::Of128(k as u32)
        } else {
            Self::Any(s)
        }
    }

    /// A byte at this share.
    #[inline(always)]
    const fn scale(self, c: u32) -> u32 {
        match self {
            Self::Whole => c,
            Self::Of128(k) => (c * k) >> 7,
            Self::Any(s) => (c as f32 * s) as u32,
        }
    }
}

/// A packed light ([`pack_light`]) source-over `dst` at share `s`, as
/// [`aterm_render::over_premul`] composes it, each channel and the opacity
/// scaled as [`Share::scale`] scales them. `over_premul` adds a light of no
/// opacity, which its source-over form meets exactly (`(255·d + 127) / 255
/// = d`); the alpha byte of the result is `0`, which no reading of it sees.
#[inline(always)]
const fn over_packed(dst: u32, v: u32, s: Share) -> u32 {
    #[inline(always)]
    const fn ch(dst: u32, v: u32, sh: u32, inv: u32, s: Share) -> u32 {
        let c = s.scale((v >> sh) & 0xff);
        let o = c + (((dst >> sh) & 0xff) * inv + 127) / 255;
        if o > 255 { 255 } else { o }
    }
    let inv = 255 - s.scale(v >> 24);
    (ch(dst, v, 16, inv, s) << 16) | (ch(dst, v, 8, inv, s) << 8) | ch(dst, v, 0, inv, s)
}

/// A halo as a cell's ring box meets it ([`LetterGuard::lay_lights`]): the
/// box pixels it covers, `[x0, x1) × [y0, y1)` from the box's corner, where
/// its weight per pixel starts in [`LetterGuard::wts`] (row by row), and its
/// colour.
#[derive(Clone, Copy)]
struct LaidHalo {
    stream: Stream,
    x0: u32,
    x1: u32,
    y0: u32,
    y1: u32,
    at: u32,
    color: u32,
}

/// A frame's caret, grid `(cw, ch, ox, oy)` and switches ([`FrameMemo`]).
type MemoFrame = (Option<(u16, u16)>, (i32, i32, i32, i32), u16);
/// Rail spans of a stream, `[from, to)` and whether each lies in its row's
/// foot band ([`InkLedger::note_rail`]).
type RailSpans = Vec<(u32, u32, bool)>;

/// What one frame gave the guard and what it made of it
/// ([`CursorGlow::guard_letters`]): its streams as they came and as it
/// left them, everything else its verdicts read, and its print. A frame
/// whose streams, rails, text and grid are this one's takes its streams
/// back without judging a cell.
#[derive(Default)]
struct FrameMemo {
    /// Whether a frame has been kept.
    kept: bool,
    /// The caret, the grid `(cw, ch, ox, oy)`, and whether the style's
    /// classic look skips the guard, with a test's switches.
    frame: MemoFrame,
    /// The frame's text as [`FrameText`] holds it: `lo`, `cols`, `col0`,
    /// its rows' extents and its cells.
    text: (i32, usize, i32),
    text_rows: Vec<Option<(i32, i32)>>,
    text_pairs: Vec<Option<(Option<u32>, u32)>>,
    /// Its cells' glyphs, whether each draws its table raster alone, and the face's ink for
    /// them ([`CursorGlow::observe_glyph_ink`]).
    text_glyphs: Vec<u8>,
    text_plain: Vec<bool>,
    /// The frame's cursor trail cells and their colour.
    trail: (Vec<TrailCell>, u32),
    ink: Option<std::sync::Arc<aterm_render::GlyphInk>>,
    /// The streams as they came: the under quads, the over quads, the
    /// halos, the under ramps and the over ramps.
    seen: (
        Vec<GlowQuad>,
        Vec<GlowQuad>,
        Vec<RainHalo>,
        GlowRamps,
        GlowRamps,
    ),
    /// The frame's rails as the guard read them, sorted: the under stream's
    /// quad spans and its ramp spans ([`InkLedger::note_rail`]).
    rails: (RailSpans, RailSpans),
    /// The frame's light-owned spans as the guard read them: the under
    /// stream's quad spans and its ramp spans
    /// ([`InkLedger::note_light_owned_laid`]).
    owned: (Vec<super::OwnedSpan>, Vec<super::OwnedSpan>),
    /// The frame's page shades as the guard read them, sorted: the under stream's quad spans
    /// and its ramp spans ([`InkLedger::note_page_shade_laid`]).
    shades: (Vec<super::OwnedSpan>, Vec<super::OwnedSpan>),
    /// The streams as the guard left them, in the same order.
    made: (
        Vec<GlowQuad>,
        Vec<GlowQuad>,
        Vec<RainHalo>,
        GlowRamps,
        GlowRamps,
    ),
    /// The frame's print.
    print: u64,
}

/// Whether two frames read their letters by the same face's ink.
#[inline]
fn same_ink(
    a: Option<&std::sync::Arc<aterm_render::GlyphInk>>,
    b: Option<&std::sync::Arc<aterm_render::GlyphInk>>,
) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// Copy `src` into `dst`, keeping `dst`'s room.
#[inline]
fn copy_ramps(dst: &mut GlowRamps, src: &GlowRamps) {
    dst.items.clone_from(&src.items);
    dst.ends.clone_from(&src.ends);
    dst.lit = src.lit;
}

/// [`LetterGuard::clear_of`]'s key, its `(li_lo, li_hi, bar)` bits, and its
/// bounds.
type ClearMemo = ((u32, u32, u32), (f32, f32));
/// [`LetterGuard::judge`]'s last text pair `(ink, ground)` and its solid
/// ink, luminances and bar.
type SolidMemo = ((u32, u32), (u32, f32, f32, f32));

/// The guard's resident scratch ([`CursorGlow::guard_letters`]).
pub(crate) struct LetterGuard {
    /// The lights that reach the row being read.
    row: Vec<RowLight>,
    /// Of those, the lights that reach the cell being read.
    cell: Vec<(Stream, u32)>,
    /// [`Self::cell`] as the ring box meets it, in its order
    /// ([`Self::lay_lights`]): per box pixel the first quad light on the
    /// ground and the first on the ink ([`pack_light`]), then each quad
    /// light on a pixel one already lies on, then the halos and their
    /// weights per pixel. A ribbon's quads are 1 px tall and a few wide, so
    /// the cell's hundreds of lights cover its box about once over: most
    /// pixels meet one light, which [`Self::paint`] lays row by row.
    first_ground: Vec<u32>,
    first_ink: Vec<u32>,
    more_ground: Vec<LitPx>,
    more_ink: Vec<LitPx>,
    laid: Vec<LaidHalo>,
    wts: Vec<u8>,
    /// Per ring box row, the columns `[lo, hi)` a laid light reaches (`lo >
    /// hi`: none); whether some box pixel lies outside them, and some inner
    /// one; and the bare ground's and ink's readings. A pixel no light
    /// reaches composites to the bare ground and ink at every share, so
    /// [`Self::lay_lights`] lays those once and each pass reads the rest.
    lit_rows: Vec<(u32, u32)>,
    unlit: (bool, bool),
    bare_lum: (f32, f32),
    /// The ring box composited at the share being weighed ([`Self::paint`]):
    /// the ground under every light, and the ink under the over-text ones.
    px_ground: Vec<u32>,
    px_ink: Vec<u32>,
    /// A cell read by its glyph's own pixels ([`ring`]): its reading, the under-text lights'
    /// first light per box pixel and every later one and whether any lies on the box
    /// ([`Self::lay_lights`]), and each pixel a reading reads as the frame draws it at the share
    /// being weighed ([`Self::paint_ring`]).
    ring: ring::RingCell,
    first_under: Vec<u32>,
    more_under: Vec<LitPx>,
    lit_under: bool,
    px_text: Vec<u32>,
    /// Where the letter's glyph lays less than its colour whole ([`Judge::cover`]): its colour
    /// under the over-text lights, and that colour's luminance per inner ring box pixel.
    px_fg: Vec<u32>,
    cover_lum: Vec<f32>,
    /// The ring box's ground luminance per pixel, and per pixel the least
    /// and greatest of it within [`RING`] columns (a test's control,
    /// [`Self::luma_ring`]).
    lum: Vec<f32>,
    ring_lo: Vec<f32>,
    ring_hi: Vec<f32>,
    /// Per ring box pixel each channel's least and greatest ground within
    /// [`RING`] columns ([`Self::holds`]), and per inner pixel of the row
    /// being read the reading of each channel's least and greatest within
    /// [`RING`] px of it.
    ring_lo_c: Vec<u32>,
    ring_hi_c: Vec<u32>,
    ground_lo: Vec<f32>,
    ground_hi: Vec<f32>,
    /// The ink's luminance per inner ring box pixel, where an over-text
    /// light lies on it.
    ink_lum: Vec<f32>,
    /// [`Self::clear_of`]'s last `(li_lo, li_hi, bar)` bits and its bounds.
    clear_memo: Option<ClearMemo>,
    /// An additive cell's whole light per ring-box pixel: under the ink
    /// (ground only) and over it (ground and ink), per channel.
    lift_under: Vec<[u16; 3]>,
    lift_over: Vec<[u16; 3]>,
    /// The frame's rows of cuts and their cells' `(column, share)`, and the cells read by their
    /// glyph's own pixels, read again on the frame the cuts make
    /// ([`CursorGlow::verify_cuts`]); the streams as they came, cut again from there; and the
    /// under stream and its ramps as the frame publishes them.
    rows_cut: Vec<RowCut>,
    row_cuts: Vec<(i32, f32)>,
    checks: Vec<Check>,
    seen: (
        Vec<GlowQuad>,
        Vec<GlowQuad>,
        Vec<RainHalo>,
        GlowRamps,
        GlowRamps,
    ),
    verify_under: Vec<GlowQuad>,
    verify_ramps: GlowRamps,
    /// The ledger's rail and light-owned spans as the frame noted them, which a cut spends
    /// ([`InkLedger::guard_mask_stream`]): a cut laid again reads them as noted.
    seen_spans: (
        RailSpans,
        RailSpans,
        Vec<super::OwnedSpan>,
        Vec<super::OwnedSpan>,
    ),
    /// The row being read's ramp lights.
    ramp_rows: RampRows,
    /// The page shades' cuts ([`CursorGlow::hold_page_shades`]), each judged cell's ring box and
    /// its share.
    shade_dims: Vec<Dim>,
    /// [`CursorGlow::holds_bloom`]'s last `(ink, ground)` and its answer.
    bloom_memo: Option<((u32, u32), bool)>,
    /// Each judged cell's verdict (the share of its lights it keeps,
    /// `1.0` uncut) by its [`Self::cell_key`]: the last frame's, sorted,
    /// and this frame's as they are judged. A cell whose ring box, text
    /// and lights are the last frame's takes its verdict back, so a frame
    /// judges only the cells its changed lights reach. A verdict is the
    /// weighing's, as [`Self::judge`] read those: what
    /// [`CursorGlow::verify_cuts`] lowers it to reads what the key does not
    /// (a neighbour's feather, the flames' and night grounds), so it stays
    /// that frame's, and the next frame's reading lowers it again if its
    /// own frame asks.
    verdicts: Vec<(u64, f32)>,
    fresh: Vec<(u64, f32)>,
    /// The ring boxes (corner, [`box_at`]) of the cells kept
    /// [`NEAR_WHOLE`] of their light: the last frame's, sorted, and this
    /// frame's. [`Self::share`] weighs such a cell there first.
    near_whole: Vec<u64>,
    near_whole_fresh: Vec<u64>,
    /// [`Self::vouch`]'s per-pixel sum of the stacked lights' shares and
    /// their count.
    stacked: Vec<[u16; 4]>,
    /// The last frame the guard read ([`FrameMemo`]).
    memo: Option<Box<FrameMemo>>,
    /// This frame is drawn nowhere ([`CursorGlow::set_frame_unseen`]).
    unseen: bool,
    /// A test's counters: the cells judged afresh, the cells whose
    /// verdict was the last frame's, and the frames that took the last
    /// frame's streams back.
    #[cfg(any(test, debug_assertions))]
    pub(crate) judged: u64,
    #[cfg(any(test, debug_assertions))]
    pub(crate) reused: u64,
    #[cfg(any(test, debug_assertions))]
    pub(crate) kept: u64,
    /// A test's counters: the passes that read a cut frame again ([`CursorGlow::verify_cuts`]),
    /// and of them the ones that called for another pass and changed no share.
    #[cfg(any(test, debug_assertions))]
    pub(crate) verify_passes: u64,
    #[cfg(any(test, debug_assertions))]
    pub(crate) verify_idle: u64,
    /// A test's negative control: every cell judged afresh every frame,
    /// and no frame taken back whole.
    #[cfg(any(test, debug_assertions))]
    pub(crate) no_reuse: bool,
    /// A test's negative control: the guard off.
    #[cfg(any(test, debug_assertions))]
    pub(crate) off: bool,
    /// A test's negative control: each cell cut alone, no ramp along the row.
    #[cfg(any(test, debug_assertions))]
    pub(crate) comb: bool,
    /// A test's negative control: every letter read by its solid ink ([`Self::holds`]), none by
    /// its glyph's own pixels ([`ring`]); with [`Self::luma_ring`], against its grounds' least
    /// and greatest luminance, as before round 27.
    #[cfg(any(test, debug_assertions))]
    pub(crate) ring_off: bool,
    /// A test's negative control: the guard blind to the cursor trail's fill, each letter's
    /// ring read on its page's ground under the lights, as before round 27.
    #[cfg(any(test, debug_assertions))]
    pub(crate) trail_off: bool,
    /// A test's negative control: a letter read by its solid ink keeps its bare share, no
    /// [`OWN_ROOM`] over it.
    #[cfg(any(test, debug_assertions))]
    pub(crate) no_room: bool,
    /// A test's negative control: a letter read by its solid ink is read against the least and
    /// greatest luminance of the grounds round each pixel, not each channel's least and greatest
    /// ([`Self::holds`]), as before the underlined letters' review (2026-11-06).
    #[cfg(any(test, debug_assertions))]
    pub(crate) luma_ring: bool,
    /// A test's negative control: Classic's spectrum face passes the guard by
    /// on paper too, as it did before looks8 round 2.
    #[cfg(any(test, debug_assertions))]
    pub(crate) classic_paper_off: bool,
    /// A test's negative control: the post passes take light from beside the
    /// letters too, as before looks10 ([`CursorGlow::bloom_spares_letters`]).
    #[cfg(any(test, debug_assertions))]
    pub(crate) bloom_on_letters: bool,
    /// A test's negative control: a page shade kept whole, unjudged
    /// ([`CursorGlow::hold_page_shades`]), as before the Linux reds of 2026-11-07.
    #[cfg(any(test, debug_assertions))]
    pub(crate) shades_whole: bool,
    /// A test's negative control: each letter under a page shade held to its raster bar
    /// ([`RasterHold::Bar`]), not the lower of that and its own reading
    /// ([`CursorGlow::hold_page_shades`]), as the hold's second pass held it (2026-11-07).
    #[cfg(any(test, debug_assertions))]
    pub(crate) shades_at_bar: bool,
}

impl LetterGuard {
    pub(crate) const EMPTY: Self = Self {
        row: Vec::new(),
        cell: Vec::new(),
        first_ground: Vec::new(),
        first_ink: Vec::new(),
        more_ground: Vec::new(),
        more_ink: Vec::new(),
        laid: Vec::new(),
        wts: Vec::new(),
        lit_rows: Vec::new(),
        unlit: (false, false),
        bare_lum: (0.0, 0.0),
        px_ground: Vec::new(),
        px_ink: Vec::new(),
        ring: ring::RingCell::EMPTY,
        first_under: Vec::new(),
        more_under: Vec::new(),
        lit_under: false,
        px_text: Vec::new(),
        px_fg: Vec::new(),
        cover_lum: Vec::new(),
        lum: Vec::new(),
        ring_lo: Vec::new(),
        ring_hi: Vec::new(),
        ring_lo_c: Vec::new(),
        ring_hi_c: Vec::new(),
        ground_lo: Vec::new(),
        ground_hi: Vec::new(),
        ink_lum: Vec::new(),
        clear_memo: None,
        lift_under: Vec::new(),
        lift_over: Vec::new(),
        rows_cut: Vec::new(),
        row_cuts: Vec::new(),
        checks: Vec::new(),
        seen: (
            Vec::new(),
            Vec::new(),
            Vec::new(),
            GlowRamps::EMPTY,
            GlowRamps::EMPTY,
        ),
        verify_under: Vec::new(),
        verify_ramps: GlowRamps::EMPTY,
        seen_spans: (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        ramp_rows: RampRows {
            under: Vec::new(),
            over: Vec::new(),
        },
        shade_dims: Vec::new(),
        bloom_memo: None,
        verdicts: Vec::new(),
        fresh: Vec::new(),
        near_whole: Vec::new(),
        near_whole_fresh: Vec::new(),
        stacked: Vec::new(),
        memo: None,
        unseen: false,
        #[cfg(any(test, debug_assertions))]
        judged: 0,
        #[cfg(any(test, debug_assertions))]
        reused: 0,
        #[cfg(any(test, debug_assertions))]
        kept: 0,
        #[cfg(any(test, debug_assertions))]
        verify_passes: 0,
        #[cfg(any(test, debug_assertions))]
        verify_idle: 0,
        #[cfg(any(test, debug_assertions))]
        no_reuse: false,
        #[cfg(any(test, debug_assertions))]
        off: false,
        #[cfg(any(test, debug_assertions))]
        comb: false,
        #[cfg(any(test, debug_assertions))]
        ring_off: false,
        #[cfg(any(test, debug_assertions))]
        trail_off: false,
        #[cfg(any(test, debug_assertions))]
        no_room: false,
        #[cfg(any(test, debug_assertions))]
        luma_ring: false,
        #[cfg(any(test, debug_assertions))]
        classic_paper_off: false,
        #[cfg(any(test, debug_assertions))]
        bloom_on_letters: false,
        #[cfg(any(test, debug_assertions))]
        shades_whole: false,
        #[cfg(any(test, debug_assertions))]
        shades_at_bar: false,
    };
}

impl Default for LetterGuard {
    #[inline]
    fn default() -> Self {
        Self::EMPTY
    }
}

#[inline]
const fn find_cut(cuts: &[(i32, f32)], col: i32) -> Option<usize> {
    let mut l = 0usize;
    let mut r = cuts.len();
    while l < r {
        let mid = (l + r) >> 1;
        let c = cuts[mid].0;
        if c < col {
            l = mid + 1;
        } else if c > col {
            r = mid;
        } else {
            return Some(mid);
        }
    }
    None
}

/// Ease a row's cuts (`(column, share)`, columns ascending) into one ramp
/// ([`RAMP_SLOPE`]): from the first cut column to the last, each column but
/// `skip` (the caret's) keeps the least of every cut's share plus the slope
/// per column between them, where that is under its own (`1.0` uncut). A
/// word gap (up to [`WORD_GAP_COLS`] columns no letter covers, `lettered`
/// telling which do) between two letters at least one of which is cut keeps
/// no more than the straight line between their shares, an uncut letter's
/// whole (unless `climb`), within the run or at its ends. Calls
/// `cut(column, share)` for each column whose share falls, in column order.
#[inline]
fn ramp_row(
    cuts: &[(i32, f32)],
    skip: Option<i32>,
    climb: bool,
    lettered: impl Fn(i32) -> bool,
    mut cut: impl FnMut(i32, f32),
) {
    let (Some(&(lo, _)), Some(&(hi, _))) = (cuts.first(), cuts.last()) else {
        return;
    };
    #[inline]
    const fn cut_share_of(cuts: &[(i32, f32)], col: i32) -> f32 {
        match find_cut(cuts, col) {
            Some(i) => cuts[i].1,
            None => 1.0,
        }
    }
    let reach = WORD_GAP_COLS + 1;
    let mut col = (lo - reach).max(0);
    let col_end = hi + reach;
    while col <= col_end {
        if skip == Some(col) {
            col += 1;
            continue;
        }
        let own = cut_share_of(cuts, col);
        let mut eased = own;
        if col >= lo && col <= hi {
            let mut ci = 0;
            while ci < cuts.len() {
                let (c, s) = cuts[ci];
                eased = eased.min(s + RAMP_SLOPE * (c - col).abs() as f32);
                ci += 1;
            }
        }
        if !climb && !lettered(col) {
            let mut left = None;
            let mut d = 1;
            while d <= reach {
                let c = col - d;
                if c < 0 {
                    break;
                }
                if lettered(c) {
                    left = Some(c);
                    break;
                }
                d += 1;
            }
            if let Some(a) = left {
                let max_b = (col + reach).min(a + 1 + WORD_GAP_COLS);
                let mut right = None;
                let mut c = col + 1;
                while c <= max_b {
                    if lettered(c) {
                        right = Some(c);
                        break;
                    }
                    c += 1;
                }
                if let Some(b) = right {
                    let (sa, sb) = (cut_share_of(cuts, a), cut_share_of(cuts, b));
                    if sa.min(sb) < 1.0 {
                        let u = (col - a) as f32 / (b - a) as f32;
                        eased = eased.min(sa + (sb - sa) * u);
                    }
                }
            }
        }
        if eased < own {
            cut(col, eased);
        }
        col += 1;
    }
}

/// The passes [`CursorGlow::guard_letters`] reads a cell read by its glyph's own pixels again on
/// the frame its cuts make, each lowering the share of a cell under its bar there by
/// [`VERIFY_STEP`]; after the last such a cell keeps none of its light.
const VERIFY_PASSES: usize = 6;
/// What a share a cell under its bar on the frame its cuts make keeps of it, a pass.
const VERIFY_STEP: f32 = 0.75;
/// The share under which a cell under its bar on the frame its cuts make keeps none.
const VERIFY_FLOOR: f32 = 1.0 / 64.0;

/// A text row's cuts ([`CursorGlow::lay_cuts`]): its grid row, its ring band `(y0, y1)`, the
/// caret's column on it, and where its cells' `(column, share)` lie in the frame's cuts.
#[derive(Clone, Copy)]
struct RowCut {
    row: i32,
    band: (i32, i32),
    skip: Option<i32>,
    cuts: (usize, usize),
}

/// A cell read by its glyph's own pixels, read again on the frame its cuts make
/// ([`CursorGlow::verify_cuts`]): its row's index among the [`RowCut`]s, its column, its ink and
/// ground, the share it was weighed at and its verdict's key.
#[derive(Clone, Copy)]
struct Check {
    row: usize,
    col: i32,
    ink: u32,
    ground: u32,
    share: f32,
    key: u64,
}

/// A stream of lights as the guard reads it: its quads and its ramps.
type LightStream<'a> = (&'a [GlowQuad], &'a GlowRamps);
/// The frame's caret cell, and whether it is drawn as a block filling its cell.
type CaretCell = (Option<(u16, u16)>, bool);
/// The frame's cursor trail: its cells and their colour.
type Trail<'a> = (&'a [TrailCell], u32);

/// Each stream's lit extent down the page, its ramps' boxes counted, `(y0, y1)`: a text row
/// none of a stream's lights reach skips it.
struct StreamBounds {
    under: (i32, i32),
    over: (i32, i32),
    halo_add: (i32, i32),
    halo_over: (i32, i32),
    all: (i32, i32),
}

impl StreamBounds {
    #[inline]
    fn of(under: LightStream<'_>, over: LightStream<'_>, halos: &[RainHalo]) -> Self {
        #[inline]
        fn y_bounds((qs, ramps): LightStream<'_>) -> (i32, i32) {
            let (mut y0, mut y1) = (i32::MAX, i32::MIN);
            for q in qs.iter().filter(|q| lit(q)) {
                y0 = y0.min(i32::from(q.y));
                y1 = y1.max(i32::from(q.y) + i32::from(q.h));
            }
            for r in ramps.items.iter().filter(|r| r.w > 0 && r.h > 0) {
                y0 = y0.min(i32::from(r.y));
                y1 = y1.max(i32::from(r.y) + i32::from(r.h));
            }
            (y0, y1)
        }
        let (under, over) = (y_bounds(under), y_bounds(over));
        let (mut halo_add, mut halo_over) = ((i32::MAX, i32::MIN), (i32::MAX, i32::MIN));
        for h in halos
            .iter()
            .filter(|h| h.w > 0 && h.h > 0 && h.color & 0x00FF_FFFF != 0)
        {
            let b = if h.mode == HaloMode::Add {
                &mut halo_add
            } else {
                &mut halo_over
            };
            b.0 = b.0.min(i32::from(h.y));
            b.1 = b.1.max(i32::from(h.y) + i32::from(h.h));
        }
        let all = (
            under.0.min(over.0).min(halo_add.0).min(halo_over.0),
            under.1.max(over.1).max(halo_add.1).max(halo_over.1),
        );
        Self {
            under,
            over,
            halo_add,
            halo_over,
            all,
        }
    }
}

/// The lights of the frame's streams (`under`, `over`, each its quads and ramps, and the halos)
/// that reach the text row band `ry0 .. ry1`, in the renderer's draw order, into `row` (a ramp's
/// rows laid into `ramp_rows`); with `ledger`, the under stream's foot rails and light-owned
/// lights left out, as the guard never weighs them with the others ([`Weighs::Guard`]). Returns
/// the lights' extent across, `(x0, x1)`.
#[inline]
fn collect_row(
    row: &mut Vec<RowLight>,
    ramp_rows: &mut RampRows,
    (under, over, halos): (LightStream<'_>, LightStream<'_>, &[RainHalo]),
    bounds: &StreamBounds,
    (ry0, ry1): (i32, i32),
    ledger: Option<&InkLedger>,
) -> (i32, i32) {
    row.clear();
    ramp_rows.under.clear();
    ramp_rows.over.clear();
    let mut row_bounds = (i32::MAX, i32::MIN);
    let meets = |(y0, y1): (i32, i32)| ry1 > y0 && ry0 < y1;
    if meets(bounds.under) {
        row_lights(
            under,
            (Stream::Under, Stream::UnderRow),
            (ry0, ry1),
            &mut ramp_rows.under,
            (row, &mut row_bounds),
            ledger.map_or(Weighs::All, Weighs::Guard),
        );
    }
    if meets(bounds.over) {
        row_lights(
            over,
            (Stream::Over, Stream::OverRow),
            (ry0, ry1),
            &mut ramp_rows.over,
            (row, &mut row_bounds),
            Weighs::All,
        );
    }
    for (mode, stream, band) in [
        (HaloMode::Add, Stream::HaloAdd, bounds.halo_add),
        (HaloMode::Over, Stream::HaloOver, bounds.halo_over),
    ] {
        if !meets(band) {
            continue;
        }
        for (k, h) in halos.iter().enumerate() {
            if h.mode == mode
                && h.w > 0
                && h.h > 0
                && h.color & 0x00FF_FFFF != 0
                && i32::from(h.y) < ry1
                && i32::from(h.y) + i32::from(h.h) > ry0
            {
                push_row_light(row, &mut row_bounds, stream, k, (h.x, h.w), 0);
            }
        }
    }
    row_bounds
}

/// Cell `(row, col)` of `text`'s neighbourhood as its ring is read from it ([`ring`]), where
/// `inks` holds its glyph's raster and the cell draws that raster alone ([`FrameText::plain`]):
/// its own cell and the eight round it, each its ground, what the frame lays on that ground
/// under every light (a cell of the cursor trail `trail` in its colour, its fill as the renderer
/// draws it, `blend`) and its glyph (a cell the frame did not read as the page under the letter,
/// `ground`; a block caret's cell, `caret_block`, and a glyph with no raster as no ground), and
/// their key ([`LetterGuard::cell_key`]'s `model`: the face's table, and each cell's colours,
/// fill and glyph). `None` for a cell read otherwise.
#[inline]
fn ring_cells(
    (text, inks): (&FrameText, &aterm_render::GlyphInk),
    (row, col): (i32, i32),
    ground: u32,
    (caret, caret_block): CaretCell,
    (trail, trail_color): Trail<'_>,
) -> Option<(u64, [ring::Neighbour; 9])> {
    #[inline]
    fn raster_of<'a>(
        inks: &'a aterm_render::GlyphInk,
        text: &FrameText,
        r: i32,
        c: i32,
    ) -> Option<&'a aterm_render::GlyphRaster> {
        if text.plain(r, c) {
            inks.raster(text.glyph(r, c))
        } else {
            None
        }
    }
    raster_of(inks, text, row, col)?;
    let mut cells = [ring::Neighbour::Opaque; 9];
    let mut model = fold(0x5249_4E47, inks.fingerprint());
    for (k, cell) in cells.iter_mut().enumerate() {
        let (r, c) = (row + k as i32 / 3 - 1, col + k as i32 % 3 - 1);
        let block = caret_block
            && k != 4
            && caret.is_some_and(|(cr, cc)| (i32::from(cr), i32::from(cc)) == (r, c));
        let pair = text.pair(r, c);
        let g = pair.map_or(ground, |(_, g)| g & 0x00FF_FFFF);
        // The trail's cell, filled with its colour at its coverage over the cell's ground, as
        // the renderer's `draw_trail` blends it under every light.
        let alpha = trail
            .iter()
            .find(|t| (t.row as i64, t.col as i64) == (i64::from(r), i64::from(c)))
            .map_or(0, |t| t.alpha);
        let base = aterm_render::blend_rgb(g, trail_color & 0x00FF_FFFF, alpha) & 0x00FF_FFFF;
        *cell = match pair {
            _ if block => ring::Neighbour::Opaque,
            None | Some((None, _)) => ring::Neighbour::Known {
                ground: g,
                base,
                glyph: None,
            },
            Some((Some(fg), _)) => match raster_of(inks, text, r, c) {
                Some(_) => ring::Neighbour::Known {
                    ground: g,
                    base,
                    glyph: Some((text.glyph(r, c), fg & 0x00FF_FFFF)),
                },
                None => ring::Neighbour::Opaque,
            },
        };
        let (fg, g) = match (block, pair) {
            (true, _) | (false, None) => (u32::MAX, u32::MAX),
            (false, Some((fg, g))) => (fg.unwrap_or(u32::MAX), g),
        };
        model = fold(
            model,
            u64::from(fg)
                | u64::from(g) << 32
                | u64::from(text.glyph(r, c)) << 56
                | u64::from(text.plain(r, c)) << 63,
        );
        model = fold(model, u64::from(base));
    }
    Some((model, cells))
}

/// One cell being judged: its ring box (`[x0, x1) × [y0, y1)`, window px),
/// its solid ink and its ground.
#[derive(Clone, Copy)]
struct Judge {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    ink: u32,
    ground: u32,
    l_ink: f32,
    l_ground: f32,
    /// The letter's colour and its luminance.
    fg: u32,
    l_fg: f32,
    /// Where the face's raster lays the glyph's core at less than its colour whole
    /// ([`aterm_render::GlyphInk::core`]): that coverage `c` in linear light, and the bar its
    /// core keeps ([`letter_bar`] of its bare reading). Every pixel short of full coverage
    /// shows the light laid under it, as the renderer blends text ([`aterm_render::blend_text`]),
    /// so the core's ink reads `c` of the colour and `1 - c` of the ground lit beneath it: the
    /// core is judged against each ground of its ring as lit by that ground's own light.
    cover: Option<(f32, f32)>,
    /// The ring box's pixels the core is judged at, `[x0, y0, x1, y1]` from its top left: where
    /// the glyph lays ink, a pixel either side, within the cell.
    core_box: [i32; 4],
    /// Whether the cell is read by its glyph's own pixels ([`LetterGuard::ring`]), as the judge
    /// reads it, rather than by its solid ink against its grounds' least and greatest
    /// ([`LetterGuard::holds`]).
    ring: bool,
    /// The bars a cell read by its glyph's own pixels keeps ([`RasterHold`]).
    hold: RasterHold,
}

impl Judge {
    /// A cell read by its glyph's own pixels ([`Judge::ring`]) in the ring box `(x0, y0, x1,
    /// y1)`, its letter `ink` on `ground`: what [`LetterGuard::lay_lights`] and
    /// [`LetterGuard::holds_ring`] read of it.
    #[inline]
    const fn ring((x0, y0, x1, y1): (i32, i32, i32, i32), (ink, ground): (u32, u32)) -> Self {
        Self {
            x0,
            y0,
            x1,
            y1,
            ink,
            ground,
            l_ink: 0.0,
            l_ground: 0.0,
            fg: ink,
            l_fg: 0.0,
            cover: None,
            core_box: [0; 4],
            ring: true,
            hold: RasterHold::Bar,
        }
    }
}

#[inline]
const fn scale_u8(v: u32, s: f32) -> u32 {
    let v = (v as f32 * s) as u32;
    if v > 255 { 255 } else { v }
}

/// Lay a run of `ramp`'s cut rows from `y` ([`InkLedger::guard_mask_ramps`]):
/// one ramp a slice between `knots`, each row's ends `ends[row][slice]`,
/// its dark rows at either end left off and a slice dark throughout none.
/// `held` names the rows that are one light, `(first row, rows after it)`:
/// each slice's ramp keeps them its run where they are lit.
#[inline]
fn lay_run(
    out: &mut GlowRamps,
    ramp: &GlowRamp,
    y: i32,
    knots: &[i32],
    ends: &[GlowEnd],
    held: Option<(i32, u16)>,
) {
    #[inline]
    const fn slice_row(ends: &[GlowEnd], n: usize, s: usize, i: usize) -> (GlowEnd, GlowEnd) {
        (ends[2 * (i * n + s)], ends[2 * (i * n + s) + 1])
    }
    let n = knots.len() - 1;
    let rows = ends.len() / (2 * n);
    let mut s = 0;
    while s < n {
        let mut first_opt = None;
        let mut i = 0;
        while i < rows {
            if slice_row(ends, n, s, i) != (0, 0) {
                first_opt = Some(i);
                break;
            }
            i += 1;
        }
        let Some(first) = first_opt else {
            s += 1;
            continue;
        };
        let mut last = first;
        let mut i = rows;
        while i > first {
            i -= 1;
            if slice_row(ends, n, s, i) != (0, 0) {
                last = i;
                break;
            }
        }
        let (join, joined) = if let Some((hy, after)) = held
            && hy >= y
            && let Some(k) = ((hy - y) as usize).checked_sub(first)
            && k + usize::from(after) <= last - first
            && slice_row(ends, n, s, first + k) != (0, 0)
        {
            (k as u16, after)
        } else {
            (0, 0)
        };
        let laid = GlowRamp {
            at: ramp.at,
            row: ramp.row,
            x: knots[s] as u16,
            y: (y + first as i32) as u16,
            w: (knots[s + 1] - knots[s]) as u16,
            h: (last + 1 - first) as u16,
            join,
            joined,
            ink: out.ends.len() as u32,
        };
        out.items.push(laid);
        let mut i = first;
        while i <= last {
            if !laid.continues((i - first) as u16) {
                let (a, b) = slice_row(ends, n, s, i);
                out.ends.extend([a, b]);
            }
            i += 1;
        }
        s += 1;
    }
}

/// Whether a quad lays light (a lit row light of the guard's).
#[inline]
const fn lit(q: &GlowQuad) -> bool {
    q.w > 0 && q.h > 0 && (q.color | q.color2) & 0x00FF_FFFF != 0
}

#[inline]
fn push_row_light(
    row: &mut Vec<RowLight>,
    bounds: &mut (i32, i32),
    stream: Stream,
    k: usize,
    (x, w): (u16, u16),
    rows: usize,
) {
    let x0 = i32::from(x);
    let x1 = x0 + i32::from(w);
    bounds.0 = bounds.0.min(x0);
    bounds.1 = bounds.1.max(x1);
    row.push(RowLight {
        stream,
        idx: k as u32,
        x0,
        x1,
        rows: rows as u32,
    });
}

/// Which of the under stream's lights a text row reads ([`row_lights`]).
#[derive(Clone, Copy)]
enum Weighs<'a> {
    /// Every light.
    All,
    /// The lights the guard weighs: all but the rails in their rows' foot bands
    /// ([`InkLedger::note_foot_rail`], the page shades among them), quads or ramps, and
    /// the light-owned lights ([`InkLedger::note_light_owned_laid`]).
    Guard(&'a InkLedger),
    /// The page shades alone ([`InkLedger::note_page_shade_laid`]).
    Shades(&'a InkLedger),
}

/// A text row's lights of a stream with ramps, in its draw order, those `weighs` names: each
/// lit quad meeting the band `ry0 .. ry1` a row light (`push_row_light(row, bounds, stream,
/// index, (x, w), 0)`), and each ramp meeting it one row light of its lights there, laid into
/// `laid` (`push_row_light(row, bounds, row stream, first, (x, w), lights)`).
#[inline]
fn row_lights(
    (quads, ramps): (&[GlowQuad], &GlowRamps),
    (sq, sr): (Stream, Stream),
    (ry0, ry1): (i32, i32),
    laid: &mut Vec<GlowQuad>,
    (row, bounds): (&mut Vec<RowLight>, &mut (i32, i32)),
    weighs: Weighs<'_>,
) {
    #[inline]
    const fn band_meets_y(y: u16, h: u16, ry0: i32, ry1: i32) -> bool {
        (y as i32) < ry1 && (y as i32) + (h as i32) > ry0
    }
    let (mut foot_q, mut foot_r) = (0usize, 0usize);
    let (mut owned_q, mut owned_r) = (0usize, 0usize);
    ramps.in_order(quads.len(), |at| match at {
        Laid::Quads(r) => {
            let mut k = r.start;
            while k < r.end {
                let q = &quads[k];
                if lit(q)
                    && band_meets_y(q.y, q.h, ry0, ry1)
                    && match weighs {
                        Weighs::All => true,
                        Weighs::Guard(l) => {
                            !(l.in_foot_rail(k, &mut foot_q) || l.in_light_owned(k, &mut owned_q))
                        }
                        Weighs::Shades(l) => l.in_page_shade(k, &mut foot_q),
                    }
                {
                    push_row_light(row, bounds, sq, k, (q.x, q.w), 0);
                }
                k += 1;
            }
        }
        Laid::Ramp(k) => {
            let r = &ramps.items[k];
            if r.w == 0
                || !band_meets_y(r.y, r.h, ry0, ry1)
                || !match weighs {
                    Weighs::All => true,
                    Weighs::Guard(l) => {
                        !(l.in_foot_ramp(k, &mut foot_r) || l.in_light_owned_ramp(k, &mut owned_r))
                    }
                    Weighs::Shades(l) => l.in_page_shade_ramp(k, &mut foot_r),
                }
            {
                return;
            }
            let from = laid.len();
            for q in r.quads_within(&ramps.ends, (ry0, ry1)) {
                if lit(&q) {
                    laid.push(q);
                }
            }
            if laid.len() > from {
                push_row_light(row, bounds, sr, from, (r.x, r.w), laid.len() - from);
            }
        }
    });
}

/// A ring box's corner `(x0, y0)` as one word ([`LetterGuard::near_whole`]).
#[inline]
const fn box_at(x0: i32, y0: i32) -> u64 {
    (y0 as u32 as u64) << 32 | x0 as u32 as u64
}

/// Four `u16`s as one word ([`LetterGuard::cell_key`]).
#[inline]
const fn pack_u16x4(a: u16, b: u16, c: u16, d: u16) -> u64 {
    a as u64 | (b as u64) << 16 | (c as u64) << 32 | (d as u64) << 48
}

/// Fold `word` into the hash `h` ([`LetterGuard::cell_key`]): splitmix64's
/// finaliser over the word, so each of its bits reaches every bit of the
/// hash, then a step that keeps the words' order.
#[inline]
const fn fold(h: u64, word: u64) -> u64 {
    let mut z = word.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (h.rotate_left(23) ^ z ^ (z >> 31)).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// A quad's light at window column `x`, scaled to `s`: its premultiplied
/// colour and opacity, as the renderer's gradient law reads them.
#[inline]
const fn quad_at(q: &GlowQuad, x: i32, s: f32) -> (u32, u8) {
    let w = if q.w > 1 { q.w as u32 } else { 1 };
    let dx = x - q.x as i32;
    let i = if dx > 0 { dx as u32 } else { 0 };
    let c = aterm_render::glow_lerp_rgb(q.color & 0x00FF_FFFF, q.color2 & 0x00FF_FFFF, i, w);
    let a = aterm_render::glow_lerp(q.alpha as u32, q.alpha2 as u32, i, w);
    if s >= 1.0 {
        return (c, if a > 255 { 255 } else { a as u8 });
    }
    (scale_rgb(c, s), scale_u8(a, s) as u8)
}

/// The widest quad [`quad_light`] reads through [`RECIP`].
const RECIP_W: usize = 2048;

/// `⌈2^36 / 2w⌉` for each quad width `w` up to [`RECIP_W`]: the gradient
/// law ([`aterm_render::glow_lerp`]) divides a numerator under `511·w + 1`
/// (so under `2^21`) by `d = 2w` (at most `2^12`), and for such `n`,
/// `n · ⌈2^36/d⌉ >> 36` is `n / d`: the product overshoots `n · 2^36 / d`
/// by `n · e / d` with `e < d`, under `2^33 / d`, which leaves the quotient's
/// floor where it was (`n mod d ≤ d − 1`).
static RECIP: [u64; RECIP_W + 1] = {
    let mut t = [0u64; RECIP_W + 1];
    let mut w = 1;
    while w <= RECIP_W {
        let d = 2 * w as u64;
        t[w] = (1u64 << 36).div_ceil(d);
        w += 1;
    }
    t
};

/// A quad's whole light at window column `x`, [`quad_at`] at share `1.0`
/// packed ([`pack_light`]), with no division: a flat quad's one light, a
/// gradient's quotients through [`RECIP`].
#[inline]
const fn quad_light(q: &GlowQuad, x: i32) -> u32 {
    #[inline(always)]
    const fn lerp(a: u32, b: u32, w: u32, m: u32, r: u64) -> u32 {
        (((a * (2 * w - m) + b * m + w) as u64 * r) >> 36) as u32
    }
    let (c0, c1) = (q.color & 0x00FF_FFFF, q.color2 & 0x00FF_FFFF);
    if c0 == c1 && q.alpha == q.alpha2 {
        return pack_light(c0, q.alpha);
    }
    let w = if q.w >= 1 { q.w as u32 } else { 1 };
    if w as usize > RECIP_W {
        let (c, a) = quad_at(q, x, 1.0);
        return pack_light(c, a);
    }
    let dx = x - (q.x as i32);
    let dx_u32 = if dx > 0 { dx as u32 } else { 0 };
    let i = if dx_u32 < w - 1 { dx_u32 } else { w - 1 };
    let (m, r) = (2 * i + 1, RECIP[w as usize]);
    let rgb = lerp((c0 >> 16) & 0xff, (c1 >> 16) & 0xff, w, m, r) << 16
        | lerp((c0 >> 8) & 0xff, (c1 >> 8) & 0xff, w, m, r) << 8
        | lerp(c0 & 0xff, c1 & 0xff, w, m, r);
    pack_light(rgb, lerp(q.alpha as u32, q.alpha2 as u32, w, m, r) as u8)
}

/// A halo's weight at window pixel `(x, y)`, as the renderer draws it.
#[inline]
const fn halo_wt(h: &RainHalo, x: i32, y: i32) -> u8 {
    let ry = h.ry as i32;
    let rx = h.rx as i32;
    let ry2 = if ry * ry > 1 { ry * ry } else { 1 };
    let rx2 = if rx * rx > 1 { rx * rx } else { 1 };
    let ny = aterm_render::halo_row_ny(y - h.cy as i32, ry2);
    if ny >= 256 {
        return 0;
    }
    let span = ((((256 - ny) * rx2) >> 8) as u32).isqrt() as i32 + 1;
    let dx = x - h.cx as i32;
    if dx < -span || dx > span {
        return 0;
    }
    let w = aterm_render::halo_weight(dx, ny, rx2);
    if w <= 0 {
        0
    } else if w >= 255 {
        255
    } else {
        w as u8
    }
}

/// An `Over` halo's opacity ceiling scaled to `s`; `None` once it is gone.
#[inline]
const fn over_cap(color: u32, s: f32) -> Option<u8> {
    let cap = aterm_render::halo_over_cap(color) as u32;
    let cap = if s >= 1.0 { cap } else { scale_u8(cap, s) };
    if cap > 0 { Some(cap as u8) } else { None }
}

/// Lay each pixel's lights of one stream over `base` (`(colour, box width)`) at share `s` into
/// `out`: on each lit row's span ([`LetterGuard::lit_rows`]) the pixel's first light, then every
/// later one, in their order ([`LetterGuard::lay_lights`]).
#[inline(always)]
const fn layer(
    out: &mut [u32],
    (base, w): (u32, usize),
    first: &[u32],
    more: &[LitPx],
    rows: &[(u32, u32)],
    s: Share,
) {
    #[inline(always)]
    const fn at(
        out: &mut [u32],
        (base, w): (u32, usize),
        first: &[u32],
        more: &[LitPx],
        rows: &[(u32, u32)],
        s: Share,
    ) {
        let mut y = 0;
        while y < rows.len() {
            let (lo, hi) = rows[y];
            let (a, b) = (y * w + lo as usize, y * w + hi as usize);
            let mut k = a;
            while k < b {
                out[k] = over_packed(base, first[k], s);
                k += 1;
            }
            y += 1;
        }
        let mut mi = 0;
        while mi < more.len() {
            let m = more[mi];
            let k = m.k as usize;
            out[k] = over_packed(out[k], m.v, s);
            mi += 1;
        }
    }
    // One loop per share kind, each the same few steps per pixel.
    match s {
        Share::Whole => at(out, (base, w), first, more, rows, Share::Whole),
        Share::Of128(k) => at(out, (base, w), first, more, rows, Share::Of128(k)),
        Share::Any(s) => at(out, (base, w), first, more, rows, Share::Any(s)),
    }
}

impl LetterGuard {
    /// Composite the ring box under the cell's lights at share `s` (`1.0`,
    /// or a share in `(0, 1)` [`Self::share`] weighs), so each pixel meets
    /// its own lights in the order the renderer draws them: its first quad
    /// light, every later one, then the halos ([`Self::lay_lights`]). Into
    /// [`Self::px_ground`] the ground under every light, and where
    /// `has_over` into [`Self::px_ink`] the ink under the over-text quads
    /// and the halos.
    #[inline]
    fn paint(&mut self, j: Judge, s: f32, has_over: bool) {
        let w = (j.x1 - j.x0) as usize;
        let n = w * (j.y1 - j.y0) as usize;
        let mut ground = std::mem::take(&mut self.px_ground);
        let mut ink = std::mem::take(&mut self.px_ink);
        let mut fg = std::mem::take(&mut self.px_fg);
        let fg_too = has_over && j.cover.is_some();
        let whole = s >= 1.0;
        let share = Share::of(s);
        // Only the lit rows' spans: every other pixel is the bare ground or
        // ink `lay_lights` left there.
        let rows = self.lit_rows.as_slice();
        ground.resize(n, j.ground);
        layer(
            &mut ground[..n],
            (j.ground, w),
            &self.first_ground[..n],
            &self.more_ground,
            rows,
            share,
        );
        if has_over {
            ink.resize(n, j.ink);
            layer(
                &mut ink[..n],
                (j.ink, w),
                &self.first_ink[..n],
                &self.more_ink,
                rows,
                share,
            );
        }
        if fg_too {
            fg.resize(n, j.fg);
            layer(
                &mut fg[..n],
                (j.fg, w),
                &self.first_ink[..n],
                &self.more_ink,
                rows,
                share,
            );
        }
        // The halos come after every quad in `cell` (its streams' order).
        let laid = self.laid.as_slice();
        let mut li = 0;
        while li < laid.len() {
            let l = laid[li];
            li += 1;
            let (x0, x1, y0, y1) = (l.x0 as usize, l.x1 as usize, l.y0 as usize, l.y1 as usize);
            let rgb = l.color & 0x00FF_FFFF;
            let add = l.stream == Stream::HaloAdd;
            let (c, cap) = if add {
                (if whole { rgb } else { scale_rgb(rgb, s) }, 0)
            } else if let Some(cap) = over_cap(l.color, s) {
                (rgb, cap)
            } else {
                continue;
            };
            let mut at = l.at as usize;
            let mut y = y0;
            while y < y1 {
                let mut x = x0;
                while x < x1 {
                    let wt = self.wts[at];
                    at += 1;
                    if wt > 0 {
                        let k = y * w + x;
                        if add {
                            let p = aterm_render::premul_rgb(c, wt);
                            ground[k] = aterm_render::add_sat(ground[k], p);
                            if has_over {
                                ink[k] = aterm_render::add_sat(ink[k], p);
                            }
                            if fg_too {
                                fg[k] = aterm_render::add_sat(fg[k], p);
                            }
                        } else {
                            let a = wt.min(cap);
                            ground[k] = aterm_render::over_rgb(ground[k], c, a);
                            if has_over {
                                ink[k] = aterm_render::over_rgb(ink[k], c, a);
                            }
                            if fg_too {
                                fg[k] = aterm_render::over_rgb(fg[k], c, a);
                            }
                        }
                    }
                    x += 1;
                }
                y += 1;
            }
        }
        self.px_ground = ground;
        self.px_ink = ink;
        self.px_fg = fg;
    }

    /// Of [`Self::row`]'s lights, those that reach the ring box columns `cx0 .. cx1`, into
    /// [`Self::cell`], in order.
    #[inline]
    fn cell_lights(&mut self, cx0: i32, cx1: i32) {
        self.cell.clear();
        for light in &self.row {
            if light.x0 < cx1 && light.x1 > cx0 {
                if light.rows == 0 {
                    self.cell.push((light.stream, light.idx));
                } else {
                    self.cell
                        .extend((light.idx..light.idx + light.rows).map(|k| (light.stream, k)));
                }
            }
        }
    }

    /// Composite what a reading reads of the ring box of a cell read by its glyph's own pixels
    /// ([`Judge::ring`]) under its lights at share `s`, as the renderer draws the frame, into
    /// [`Self::px_text`]: each pixel a glyph lays text on drawn over the light beneath it (the
    /// under-text lights on the frame's ground, a trail cell's fill) with the over-text lights
    /// and the halos laid over it, every other pixel the ground under every light. Each light
    /// laid on a pixel no reading reads leaves a value nothing reads.
    #[inline]
    fn paint_ring(&mut self, s: f32, w: usize, has_over: bool) {
        let share = Share::of(s);
        let whole = s >= 1.0;
        let mut ground = std::mem::take(&mut self.px_ground);
        let mut text = std::mem::take(&mut self.px_text);
        for &(k, base) in self.ring.plain_px() {
            let k = k as usize;
            let first = self.first_ground[k];
            ground[k] = if first == 0 {
                base
            } else {
                over_packed(base, first, share)
            };
        }
        for m in &self.more_ground {
            ground[m.k as usize] = over_packed(ground[m.k as usize], m.v, share);
        }
        if self.lit_under {
            for &(k, base, _) in self.ring.text_px() {
                let k = k as usize;
                text[k] = over_packed(base, self.first_under[k], share);
            }
            for m in &self.more_under {
                text[m.k as usize] = over_packed(text[m.k as usize], m.v, share);
            }
            for &(k, level) in self.ring.text() {
                let k = k as usize;
                text[k] = level.over(text[k]);
            }
        } else {
            // No light beneath the text: at every share it is what the read laid on the ground.
            for &(k, _, on_base) in self.ring.text_px() {
                text[k as usize] = on_base;
            }
        }
        if has_over {
            for &(k, _, _) in self.ring.text_px() {
                let k = k as usize;
                let first = self.first_ink[k];
                if first != 0 {
                    text[k] = over_packed(text[k], first, share);
                }
            }
            for m in &self.more_ink {
                text[m.k as usize] = over_packed(text[m.k as usize], m.v, share);
            }
        }
        // The halos come after every quad and the text.
        for l in &self.laid {
            let (x0, x1, y0, y1) = (l.x0 as usize, l.x1 as usize, l.y0 as usize, l.y1 as usize);
            let rgb = l.color & 0x00FF_FFFF;
            let add = l.stream == Stream::HaloAdd;
            let (c, cap) = if add {
                (if whole { rgb } else { scale_rgb(rgb, s) }, 0)
            } else if let Some(cap) = over_cap(l.color, s) {
                (rgb, cap)
            } else {
                continue;
            };
            let mut at = l.at as usize;
            for y in y0..y1 {
                for x in x0..x1 {
                    let wt = self.wts[at];
                    at += 1;
                    if wt == 0 {
                        continue;
                    }
                    let k = y * w + x;
                    if add {
                        let p = aterm_render::premul_rgb(c, wt);
                        ground[k] = aterm_render::add_sat(ground[k], p);
                        text[k] = aterm_render::add_sat(text[k], p);
                    } else {
                        let a = wt.min(cap);
                        ground[k] = aterm_render::over_rgb(ground[k], c, a);
                        text[k] = aterm_render::over_rgb(text[k], c, a);
                    }
                }
            }
        }
        for &(k, _) in self.ring.plain_px() {
            text[k as usize] = ground[k as usize];
        }
        self.px_ground = ground;
        self.px_text = text;
    }

    /// Whether a cell read by its glyph's own pixels ([`Judge::ring`]) reads at least `bar`
    /// with its lights at share `s`: its reading ([`ring::RingCell::holds`]) on the frame
    /// [`Self::paint_ring`] composites, its whole reading keeping its own bar there
    /// ([`ring::RingCell::whole_holds`], at [`Judge::hold`]).
    #[inline]
    fn holds_ring(&mut self, j: Judge, s: f32, bar: f32, has_over: bool) -> bool {
        struct Frame<'a>(&'a [u32]);
        impl ring::FramePx for Frame<'_> {
            #[inline]
            fn at(&self, k: usize) -> u32 {
                self.0[k]
            }
        }
        self.paint_ring(s, (j.x1 - j.x0) as usize, has_over);
        let text = std::mem::take(&mut self.px_text);
        let frame = Frame(&text);
        let held = self.ring.holds(&frame, bar) && self.ring.whole_holds(&frame, j.hold);
        self.px_text = text;
        held
    }

    /// Whether every light that reaches the cell adds (no source-over
    /// light): then a share's composite is the ground or ink plus that
    /// share of the whole light, saturating, as `add_sat` composes.
    #[inline]
    const fn additive(&self, streams: Streams<'_>) -> bool {
        let cell_slice = self.cell.as_slice();
        let mut ci = 0;
        while ci < cell_slice.len() {
            let (stream, i) = cell_slice[ci];
            let ok = match stream {
                Stream::Under | Stream::Over | Stream::UnderRow | Stream::OverRow => {
                    let q = streams.quad(stream, i as usize);
                    q.alpha | q.alpha2 == 0
                }
                Stream::HaloAdd => true,
                Stream::HaloOver => false,
            };
            if !ok {
                return false;
            }
            ci += 1;
        }
        true
    }

    /// Fill [`Self::lift_under`]/[`Self::lift_over`] with an additive
    /// cell's whole light ([`Self::additive`]).
    #[inline]
    fn prepare(&mut self, j: Judge, streams: Streams<'_>, has_over: bool) {
        #[inline]
        const fn unpack_rgb(c: u32) -> [u16; 3] {
            [
                ((c >> 16) & 0xff) as u16,
                ((c >> 8) & 0xff) as u16,
                (c & 0xff) as u16,
            ]
        }
        #[inline]
        const fn add_rgb(acc: &mut [u16; 3], rgb: [u16; 3]) {
            acc[0] = acc[0].saturating_add(rgb[0]);
            acc[1] = acc[1].saturating_add(rgb[1]);
            acc[2] = acc[2].saturating_add(rgb[2]);
        }
        let halos = streams.halos;
        let cell_slice = self.cell.as_slice();
        let (w, h) = ((j.x1 - j.x0) as usize, (j.y1 - j.y0) as usize);
        let mut lu = std::mem::take(&mut self.lift_under);
        let mut lo = std::mem::take(&mut self.lift_over);
        lu.clear();
        lu.resize(w * h, [0; 3]);
        if has_over {
            lo.clear();
            lo.resize(w * h, [0; 3]);
        }
        let mut ci = 0;
        while ci < cell_slice.len() {
            let (stream, i) = cell_slice[ci];
            ci += 1;
            let i = i as usize;
            let (bx0, by0, bw, bh) = match stream {
                Stream::HaloAdd | Stream::HaloOver => {
                    (halos[i].x, halos[i].y, halos[i].w, halos[i].h)
                }
                _ => {
                    let q = streams.quad(stream, i);
                    (q.x, q.y, q.w, q.h)
                }
            };
            let (x0, x1) = (
                i32::from(bx0).max(j.x0),
                (i32::from(bx0) + i32::from(bw)).min(j.x1),
            );
            let (y0, y1) = (
                i32::from(by0).max(j.y0),
                (i32::from(by0) + i32::from(bh)).min(j.y1),
            );
            if x0 >= x1 || y0 >= y1 {
                continue;
            }
            match stream {
                Stream::Under | Stream::Over | Stream::UnderRow | Stream::OverRow => {
                    let q = streams.quad(stream, i);
                    let dst = if stream.on_ground() { &mut lu } else { &mut lo };
                    if (q.color ^ q.color2) & 0x00FF_FFFF == 0 {
                        let c = q.color & 0x00FF_FFFF;
                        if c != 0 {
                            let rgb = unpack_rgb(c);
                            let (rx0, rx1) = ((x0 - j.x0) as usize, (x1 - j.x0) as usize);
                            let mut y = y0;
                            while y < y1 {
                                let row_off = (y - j.y0) as usize * w;
                                let row_slice = &mut dst[row_off + rx0..row_off + rx1];
                                let mut ri = 0;
                                while ri < row_slice.len() {
                                    add_rgb(&mut row_slice[ri], rgb);
                                    ri += 1;
                                }
                                y += 1;
                            }
                        }
                    } else {
                        let mut x = x0;
                        while x < x1 {
                            let c = quad_at(q, x, 1.0).0;
                            if c != 0 {
                                let rgb = unpack_rgb(c);
                                let col_off = (x - j.x0) as usize;
                                let mut y = y0;
                                while y < y1 {
                                    add_rgb(&mut dst[(y - j.y0) as usize * w + col_off], rgb);
                                    y += 1;
                                }
                            }
                            x += 1;
                        }
                    }
                }
                Stream::HaloAdd | Stream::HaloOver => {
                    let h = &halos[i];
                    let color = h.color & 0x00FF_FFFF;
                    if color == 0 {
                        continue;
                    }
                    let ry2 = (i32::from(h.ry) * i32::from(h.ry)).max(1);
                    let rx2 = (i32::from(h.rx) * i32::from(h.rx)).max(1);
                    let cx = i32::from(h.cx);
                    let cy = i32::from(h.cy);
                    let mut y = y0;
                    while y < y1 {
                        let ny = aterm_render::halo_row_ny(y - cy, ry2);
                        if ny < 256 {
                            let span = ((((256 - ny) * rx2) >> 8) as u32).isqrt() as i32 + 1;
                            let hx0 = x0.max(cx - span);
                            let hx1 = x1.min(cx + span + 1);
                            let row_off = (y - j.y0) as usize * w;
                            let mut x = hx0;
                            while x < hx1 {
                                let wt =
                                    aterm_render::halo_weight(x - cx, ny, rx2).clamp(0, 255) as u8;
                                if wt != 0 {
                                    add_rgb(
                                        &mut lo[row_off + (x - j.x0) as usize],
                                        unpack_rgb(aterm_render::premul_rgb(color, wt)),
                                    );
                                }
                                x += 1;
                            }
                        }
                        y += 1;
                    }
                }
            }
        }
        self.lift_under = lu;
        self.lift_over = lo;
    }

    /// Lay [`Self::cell`]'s lights on `j`'s ring box, each clipped to it:
    /// a quad as its whole light on each pixel it covers ([`Self::lit`], as
    /// [`quad_at`] reads it), a halo as its weight per pixel ([`Self::laid`]),
    /// so each share [`Self::paint`] weighs only scales them.
    #[inline]
    fn lay_lights(&mut self, j: Judge, streams: Streams<'_>, has_over: bool) {
        #[inline]
        const fn clamp_u32(v: i32, n: i32) -> u32 {
            if v <= 0 {
                0
            } else if v >= n {
                n as u32
            } else {
                v as u32
            }
        }
        #[inline]
        const fn span(at: u16, len: u16, o: i32, n: i32) -> (u32, u32) {
            let lo = clamp_u32(at as i32 - o, n);
            let hi = clamp_u32(at as i32 + len as i32 - o, n);
            (lo, hi)
        }
        let halos = streams.halos;
        let (w, h) = ((j.x1 - j.x0).max(0), (j.y1 - j.y0).max(0));
        let n = (w * h) as usize;
        self.first_ground.clear();
        self.first_ground.resize(n, 0);
        self.more_ground.clear();
        if has_over {
            self.first_ink.clear();
            self.first_ink.resize(n, 0);
        }
        self.more_ink.clear();
        if j.ring {
            self.first_under.clear();
            self.first_under.resize(n, 0);
        }
        self.more_under.clear();
        self.lit_under = false;
        self.laid.clear();
        self.wts.clear();
        self.lit_rows.clear();
        self.lit_rows.resize(h as usize, (w as u32, 0));
        #[inline]
        const fn mark_reach(rows: &mut [(u32, u32)], (x0, x1): (u32, u32), (y0, y1): (u32, u32)) {
            let mut ri = y0 as usize;
            let end = y1 as usize;
            while ri < end {
                let (lo, hi) = rows[ri];
                rows[ri] = (if lo < x0 { lo } else { x0 }, if hi > x1 { hi } else { x1 });
                ri += 1;
            }
        }
        let cell_slice = self.cell.as_slice();
        let mut ci = 0;
        while ci < cell_slice.len() {
            let (stream, i) = cell_slice[ci];
            ci += 1;
            let i = i as usize;
            if let Stream::HaloAdd | Stream::HaloOver = stream {
                let hl = &halos[i];
                let (x0, x1) = span(hl.x, hl.w, j.x0, w);
                let (y0, y1) = span(hl.y, hl.h, j.y0, h);
                if x0 >= x1 || y0 >= y1 {
                    continue;
                }
                mark_reach(&mut self.lit_rows, (x0, x1), (y0, y1));
                let at = self.wts.len() as u32;
                let mut y = y0;
                while y < y1 {
                    let mut x = x0;
                    while x < x1 {
                        self.wts.push(halo_wt(hl, j.x0 + x as i32, j.y0 + y as i32));
                        x += 1;
                    }
                    y += 1;
                }
                self.laid.push(LaidHalo {
                    stream,
                    x0,
                    x1,
                    y0,
                    y1,
                    at,
                    color: hl.color,
                });
                continue;
            }
            debug_assert!(
                self.laid.is_empty(),
                "a quad after a halo in the cell's lights"
            );
            let q = streams.quad(stream, i);
            let (x0, x1) = span(q.x, q.w, j.x0, w);
            let (y0, y1) = span(q.y, q.h, j.y0, h);
            let ink = has_over && !stream.on_ground();
            let under = j.ring && stream.on_ground();
            if x0 >= x1 || y0 >= y1 {
                continue;
            }
            mark_reach(&mut self.lit_rows, (x0, x1), (y0, y1));
            let mut x = x0;
            while x < x1 {
                let v = quad_light(q, j.x0 + x as i32);
                x += 1;
                if v == 0 {
                    // No light, at any share: every pixel as it was.
                    continue;
                }
                let mut k = (y0 * w as u32 + x - 1) as usize;
                let mut y = y0;
                while y < y1 {
                    if self.first_ground[k] == 0 {
                        self.first_ground[k] = v;
                    } else {
                        self.more_ground.push(LitPx { k: k as u32, v });
                    }
                    if ink {
                        if self.first_ink[k] == 0 {
                            self.first_ink[k] = v;
                        } else {
                            self.more_ink.push(LitPx { k: k as u32, v });
                        }
                    }
                    if under {
                        self.lit_under = true;
                        if self.first_under[k] == 0 {
                            self.first_under[k] = v;
                        } else {
                            self.more_under.push(LitPx { k: k as u32, v });
                        }
                    }
                    k += w as usize;
                    y += 1;
                }
            }
        }
        // A cell read by its glyph's own pixels takes every pixel from these two.
        if j.ring {
            self.px_ground.resize(n, 0);
            self.px_text.resize(n, 0);
            return;
        }
        // The pixels no light reaches, bare at every share, laid once.
        let table = crate::color_math::relative_luminance_table();
        self.bare_lum = (
            crate::color_math::relative_luminance_with(table, j.ground),
            crate::color_math::relative_luminance_with(table, j.ink),
        );
        self.px_ground.clear();
        self.px_ground.resize(n, j.ground);
        self.lum.clear();
        self.lum.resize(n, self.bare_lum.0);
        if has_over {
            self.px_ink.clear();
            self.px_ink.resize(n, j.ink);
            self.ink_lum.clear();
            self.ink_lum.resize(n, self.bare_lum.1);
            if j.cover.is_some() {
                self.px_fg.clear();
                self.px_fg.resize(n, j.fg);
            }
        }
        let (wu, hu, r) = (w as u32, h as u32, RING as u32);
        let mut unlit = (false, false);
        let lit_rows = self.lit_rows.as_slice();
        let mut y = 0;
        while y < lit_rows.len() {
            let (lo, hi) = lit_rows[y];
            let yu = y as u32;
            unlit.0 |= lo > 0 || hi < wu;
            unlit.1 |= yu >= r && yu + r < hu && (lo > r || hi + r < wu);
            y += 1;
        }
        self.unlit = unlit;
    }

    /// Whether every solid ink pixel of the cell reads at least `bar`
    /// against the ground within [`RING`] of it, with the cell's lights at
    /// share `s`. The judge reads a pixel against each channel's median of
    /// its ring, which lies between that channel's least and greatest ground
    /// within [`RING`] px of the pixel, so its reading lies between those two
    /// colours' readings, not between the least and greatest luminance of the
    /// grounds: a bed at the page's luminance in its own hue reads no brighter
    /// than the page, yet where its edge meets the page round a stroke the
    /// medians take the bed's red and the page's green and blue and read
    /// brighter than either (the underline cat's wash under an underlined `h`
    /// on Solarized Dark, 90.5% of its own reading, 2026-11-06). Every ground
    /// of the box clear of the dimmest and the brightest ink
    /// ([`Self::clear_of`]), read by each channel's least and greatest of the
    /// whole box, settles it at once; else each ink pixel is read against the
    /// least and greatest of its ring, and the first under the bar settles it.
    #[inline]
    fn holds(&mut self, j: Judge, s: f32, bar: f32, (additive, has_over): (bool, bool)) -> bool {
        #[inline]
        const fn lift_rgb(c: u32, a: [u16; 3], s: f32) -> u32 {
            #[inline]
            const fn ch(c: u32, sh: u32, ak: u16, s: f32) -> u32 {
                let l = (ak as f32 * s) as u32;
                let v = ((c >> sh) & 0xff) + l;
                if v < 255 { v } else { 255 }
            }
            (ch(c, 16, a[0], s) << 16) | (ch(c, 8, a[1], s) << 8) | ch(c, 0, a[2], s)
        }
        #[inline]
        const fn ratio(a: f32, b: f32) -> f32 {
            (a.max(b) + 0.05) / (a.min(b) + 0.05)
        }
        /// Each channel's least (`max` false) or greatest of two colours.
        #[inline(always)]
        const fn ch_pick(a: u32, b: u32, max: bool) -> u32 {
            let mut out = 0;
            let mut sh = 0u32;
            while sh <= 16 {
                let (x, y) = (a & (0xff << sh), b & (0xff << sh));
                out |= if (x < y) == max { y } else { x };
                sh += 8;
            }
            out
        }
        let (w, h) = ((j.x1 - j.x0) as usize, (j.y1 - j.y0) as usize);
        let r = RING as usize;
        if w <= 2 * r || h <= 2 * r {
            return true;
        }
        if !additive {
            self.paint(j, s, has_over);
        }
        // The judge's medians, read by each channel's least and greatest (a test's control reads
        // the grounds' least and greatest luminance instead, [`Self::luma_ring`]).
        #[cfg(any(test, debug_assertions))]
        let channels = !self.luma_ring;
        #[cfg(not(any(test, debug_assertions)))]
        let channels = true;
        let mut px_ground = std::mem::take(&mut self.px_ground);
        let px_ink = std::mem::take(&mut self.px_ink);
        let px_fg = std::mem::take(&mut self.px_fg);
        let mut cover_lum = std::mem::take(&mut self.cover_lum);
        let mut lum = std::mem::take(&mut self.lum);
        let mut ink_lum = std::mem::take(&mut self.ink_lum);
        let mut ring_lo = std::mem::take(&mut self.ring_lo);
        let mut ring_hi = std::mem::take(&mut self.ring_hi);
        let mut ring_lo_c = std::mem::take(&mut self.ring_lo_c);
        let mut ring_hi_c = std::mem::take(&mut self.ring_hi_c);
        let mut ground_lo = std::mem::take(&mut self.ground_lo);
        let mut ground_hi = std::mem::take(&mut self.ground_hi);
        let n = w * h;
        let iw = w - 2 * r;
        ink_lum.resize(n, 0.0);
        ground_lo.resize(iw, 0.0);
        ground_hi.resize(iw, 0.0);
        if channels {
            ring_lo_c.resize(n, 0);
            ring_hi_c.resize(n, 0);
        } else {
            lum.resize(n, 0.0);
            ring_lo.resize(n, 0.0);
            ring_hi.resize(n, 0.0);
        }
        let table = crate::color_math::relative_luminance_table();
        // The ink's readings on the letter's own pixels, where a light lies
        // on it: the dimmest and the brightest set the ground's clear
        // readings.
        let (mut li_lo, mut li_hi) = (j.l_ink, j.l_ink);
        if has_over {
            (li_lo, li_hi) = (f32::INFINITY, f32::NEG_INFINITY);
            let mut last_ink: Option<([u16; 3], f32)> = None;
            let mut last_ink_px = (j.ink, j.l_ink);
            let mut y = r;
            while y < h - r {
                let (k0, k1) = (y * w + r, y * w + w - r);
                if additive {
                    let mut k = k0;
                    while k < k1 {
                        let b = self.lift_over[k];
                        let li = if b == [0; 3] {
                            j.l_ink
                        } else if let Some((prev_b, prev_lum)) = last_ink
                            && prev_b == b
                        {
                            prev_lum
                        } else {
                            let l = crate::color_math::relative_luminance_with(
                                table,
                                lift_rgb(j.ink, b, s),
                            );
                            last_ink = Some((b, l));
                            l
                        };
                        ink_lum[k] = li;
                        li_lo = li_lo.min(li);
                        li_hi = li_hi.max(li);
                        k += 1;
                    }
                } else {
                    // The row's lit span alone: the bare ink is read below.
                    let (lo, hi) = self.lit_rows[y];
                    let (k0, k1) = (k0.max(y * w + lo as usize), k1.min(y * w + hi as usize));
                    let mut k = k0;
                    while k < k1 {
                        let px = px_ink[k];
                        let li = if px == last_ink_px.0 {
                            last_ink_px.1
                        } else {
                            let l = crate::color_math::relative_luminance_with(table, px);
                            last_ink_px = (px, l);
                            l
                        };
                        ink_lum[k] = li;
                        li_lo = li_lo.min(li);
                        li_hi = li_hi.max(li);
                        k += 1;
                    }
                }
                y += 1;
            }
            if !additive && self.unlit.1 {
                li_lo = li_lo.min(self.bare_lum.1);
                li_hi = li_hi.max(self.bare_lum.1);
            }
        }
        let (dark, light) = self.clear_of(li_lo, li_hi, bar);
        // The ground's readings, and whether any reads between those.
        let mut near = false;
        if additive && channels {
            // Each pixel's ground as the lights lift it.
            px_ground.clear();
            px_ground.resize(n, j.ground);
            let mut k = 0;
            while k < n {
                let a = self.lift_under[k];
                let ab = if has_over {
                    let b = self.lift_over[k];
                    [
                        a[0].saturating_add(b[0]),
                        a[1].saturating_add(b[1]),
                        a[2].saturating_add(b[2]),
                    ]
                } else {
                    a
                };
                if ab != [0; 3] {
                    px_ground[k] = lift_rgb(j.ground, ab, s);
                }
                k += 1;
            }
        } else if additive {
            let mut last_g: Option<([u16; 3], f32)> = None;
            let mut k = 0;
            while k < n {
                let a = self.lift_under[k];
                let ab = if has_over {
                    let b = self.lift_over[k];
                    [
                        a[0].saturating_add(b[0]),
                        a[1].saturating_add(b[1]),
                        a[2].saturating_add(b[2]),
                    ]
                } else {
                    a
                };
                let l = if ab == [0; 3] {
                    j.l_ground
                } else if let Some((prev_ab, prev_lum)) = last_g
                    && prev_ab == ab
                {
                    prev_lum
                } else {
                    let l = crate::color_math::relative_luminance_with(
                        table,
                        lift_rgb(j.ground, ab, s),
                    );
                    last_g = Some((ab, l));
                    l
                };
                lum[k] = l;
                near |= l > dark && l < light;
                k += 1;
            }
        } else if !channels {
            // Each row's lit span; the pixels no light reaches read the bare
            // ground `lay_lights` left in `lum`.
            let lit_rows = self.lit_rows.as_slice();
            let mut last_ground_px = (j.ground, j.l_ground);
            let mut y = 0;
            while y < lit_rows.len() {
                let (lo, hi) = lit_rows[y];
                let (a, b) = (y * w + lo as usize, y * w + hi as usize);
                let mut k = a;
                while k < b {
                    let px = px_ground[k];
                    let v = if px == last_ground_px.0 {
                        last_ground_px.1
                    } else {
                        let l = crate::color_math::relative_luminance_with(table, px);
                        last_ground_px = (px, l);
                        l
                    };
                    lum[k] = v;
                    near |= v > dark && v < light;
                    k += 1;
                }
                y += 1;
            }
            let bare = self.bare_lum.0;
            near |= self.unlit.0 && bare > dark && bare < light;
        }
        if channels {
            // A median of every ring in the box reads between the box's least and greatest of
            // each channel: clear of the ink when that span of readings is.
            let (mut lo_c, mut hi_c) = (px_ground[0], px_ground[0]);
            let mut k = 1;
            while k < n {
                lo_c = ch_pick(lo_c, px_ground[k], false);
                hi_c = ch_pick(hi_c, px_ground[k], true);
                k += 1;
            }
            let lo = crate::color_math::relative_luminance_with(table, lo_c);
            let hi = crate::color_math::relative_luminance_with(table, hi_c);
            near = lo < light && hi > dark;
        }
        // The core's colour per inner pixel, as the over-text lights leave it. The core reads
        // `c` of it and `1 - c` of the ground lit beneath it, so it moves with its ground and
        // no bound on the ground alone clears it.
        let core = j.cover;
        if core.is_some() {
            cover_lum.resize(n, 0.0);
            let mut last_fg: Option<(u32, f32)> = None;
            let mut last_lift: Option<([u16; 3], f32)> = None;
            let mut y = r;
            while y < h - r {
                let mut k = y * w + r;
                let k1 = y * w + w - r;
                while k < k1 {
                    let l_fg = if !has_over {
                        j.l_fg
                    } else if additive {
                        let b = self.lift_over[k];
                        if b == [0; 3] {
                            j.l_fg
                        } else if let Some((prev, l)) = last_lift
                            && prev == b
                        {
                            l
                        } else {
                            let l = crate::color_math::relative_luminance_with(
                                table,
                                lift_rgb(j.fg, b, s),
                            );
                            last_lift = Some((b, l));
                            l
                        }
                    } else {
                        let px = px_fg[k];
                        if px == j.fg {
                            j.l_fg
                        } else if let Some((prev, l)) = last_fg
                            && prev == px
                        {
                            l
                        } else {
                            let l = crate::color_math::relative_luminance_with(table, px);
                            last_fg = Some((px, l));
                            l
                        }
                    };
                    cover_lum[k] = l_fg;
                    k += 1;
                }
                y += 1;
            }
        }
        let clear = !near && core.is_none();
        let mut holds = true;
        if !clear {
            // Each ink pixel against the least and greatest ground of its
            // ring, row by row: each row's least and greatest within `RING`
            // columns, read once a ring reaches the row, then each inner
            // row's within `RING` rows of those.
            let x_end = w - r;
            let mut ready = 0;
            let mut last_lo: (u32, f32) = (j.ground, j.l_ground);
            let mut last_hi: (u32, f32) = (j.ground, j.l_ground);
            let mut y = r;
            while holds && y < h - r {
                while ready <= y + r {
                    if channels {
                        let row = &px_ground[ready * w..ready * w + w];
                        let (lo_row, hi_row) = (
                            &mut ring_lo_c[ready * w + r..ready * w + x_end],
                            &mut ring_hi_c[ready * w + r..ready * w + x_end],
                        );
                        let mut x = 0;
                        while x < iw {
                            let ring = &row[x..x + 2 * r + 1];
                            let (mut l, mut g) = (ring[0], ring[0]);
                            let mut rx = 1;
                            while rx < ring.len() {
                                l = ch_pick(l, ring[rx], false);
                                g = ch_pick(g, ring[rx], true);
                                rx += 1;
                            }
                            lo_row[x] = l;
                            hi_row[x] = g;
                            x += 1;
                        }
                    } else {
                        let row = &lum[ready * w..ready * w + w];
                        let (lo_row, hi_row) = (
                            &mut ring_lo[ready * w + r..ready * w + x_end],
                            &mut ring_hi[ready * w + r..ready * w + x_end],
                        );
                        let mut x = 0;
                        while x < iw {
                            let ring = &row[x..x + 2 * r + 1];
                            let (mut l, mut g) = (ring[0], ring[0]);
                            let mut rx = 1;
                            while rx < ring.len() {
                                let v = ring[rx];
                                l = l.min(v);
                                g = g.max(v);
                                rx += 1;
                            }
                            lo_row[x] = l;
                            hi_row[x] = g;
                            x += 1;
                        }
                    }
                    ready += 1;
                }
                // Each inner pixel's least and greatest reading of its ring's grounds.
                let from_ring = (y - r) * w + r;
                let mut x = 0;
                while x < iw {
                    let (lo, hi) = if channels {
                        let at = from_ring + x;
                        let (mut lo_c, mut hi_c) = (ring_lo_c[at], ring_hi_c[at]);
                        let mut i = 1;
                        while i <= 2 * r {
                            lo_c = ch_pick(lo_c, ring_lo_c[from_ring + i * w + x], false);
                            hi_c = ch_pick(hi_c, ring_hi_c[from_ring + i * w + x], true);
                            i += 1;
                        }
                        if lo_c != last_lo.0 {
                            let l = crate::color_math::relative_luminance_with(table, lo_c);
                            last_lo = (lo_c, l);
                        }
                        if hi_c != last_hi.0 {
                            let l = crate::color_math::relative_luminance_with(table, hi_c);
                            last_hi = (hi_c, l);
                        }
                        (last_lo.1, last_hi.1)
                    } else {
                        let (mut lo, mut hi) = (ring_lo[from_ring + x], ring_hi[from_ring + x]);
                        let mut i = 1;
                        while i <= 2 * r {
                            lo = lo.min(ring_lo[from_ring + i * w + x]);
                            hi = hi.max(ring_hi[from_ring + i * w + x]);
                            i += 1;
                        }
                        (lo, hi)
                    };
                    ground_lo[x] = lo;
                    ground_hi[x] = hi;
                    x += 1;
                }
                let row_at = y * w + r;
                let inks = &ink_lum[row_at..row_at + iw];
                let mut fail = false;
                if has_over || bar <= 1.0 {
                    // An ink whose readings span the ring's reads it at 1:1.
                    let mut x = 0;
                    while x < iw {
                        let li = if has_over { inks[x] } else { j.l_ink };
                        let (lo, hi) = (ground_lo[x], ground_hi[x]);
                        fail |= (channels && 1.0 < bar && lo <= li && li <= hi)
                            || ratio(li, lo) < bar
                            || ratio(li, hi) < bar;
                        x += 1;
                    }
                } else {
                    // One ink reading, `j.l_ink`, whose clear grounds are
                    // `dark` and `light` (`clear_of`, the very quotient
                    // above, which only falls as a ground nears the ink,
                    // down to `1.0` at the ink itself, under a bar over
                    // `1.0`): a ground under its bar reads between them, and
                    // a span of the judge's readings that meets them holds one.
                    let mut x = 0;
                    while x < iw {
                        let (lo, hi) = (ground_lo[x], ground_hi[x]);
                        fail |= if channels {
                            lo < light && hi > dark
                        } else {
                            (lo > dark && lo < light) || (hi > dark && hi < light)
                        };
                        x += 1;
                    }
                }
                let [bx0, by0, bx1, by1] = j.core_box;
                if let Some((c, bar_c)) = core
                    && (y as i32) >= by0
                    && (y as i32) < by1
                {
                    // Where the glyph lays ink, each ground of the ring against the core laid
                    // over that ground's own light, as a light under the letter lies under its
                    // core and the ground beside it alike: the reading falls as the ground
                    // nears the core's colour from either side, so the ring's least and
                    // greatest settle it, and a ring that spans the colour holds a ground the
                    // core cannot leave.
                    let colours = &cover_lum[row_at..row_at + iw];
                    let mut x = (bx0 - r as i32).max(0) as usize;
                    let x_end = ((bx1 - r as i32).max(0) as usize).min(iw);
                    while x < x_end {
                        let f = colours[x];
                        let (lo, hi) = (ground_lo[x], ground_hi[x]);
                        let on = |g: f32| ratio(c * f + (1.0 - c) * g, g);
                        fail |= (lo <= f && f <= hi) || on(lo) < bar_c || on(hi) < bar_c;
                        x += 1;
                    }
                }
                holds = !fail;
                y += 1;
            }
        }
        self.px_ground = px_ground;
        self.px_ink = px_ink;
        self.px_fg = px_fg;
        self.cover_lum = cover_lum;
        self.lum = lum;
        self.ink_lum = ink_lum;
        self.ring_lo = ring_lo;
        self.ring_hi = ring_hi;
        self.ring_lo_c = ring_lo_c;
        self.ring_hi_c = ring_hi_c;
        self.ground_lo = ground_lo;
        self.ground_hi = ground_hi;
        holds
    }

    /// Whether every share the bisection weighs on its way up to `s` (each
    /// mid while every one holds) holds, by the ring box [`Self::paint`]
    /// just composited at `s` for a cell with no over-text light and no
    /// halo: the box's greatest of each channel reads at most `dark`
    /// ([`Self::clear_of`]) under every such share, so no ring's medians
    /// read near the ink and [`Self::holds`] holds at once (a test's
    /// control, [`Self::luma_ring`], asks it of each ground pixel alone).
    ///
    /// A pixel one quad light lies on, `o' = c' + ⌊(d·(255 − a') + 127) /
    /// 255⌋` at share `k' / 128` (`c'`, `a'` its colour and opacity at that
    /// share, [`over_packed`]), is within `1.5` of the line `d + t·(c − d·a
    /// / 255)` at both shares (the floors' room), and at a lesser share the
    /// line lies between the bare `d` and its value at `k'`: every channel
    /// stays at most `max(d + 1, o' + 2)`. A pixel more lights lie on gains
    /// at most each light's share at every step: at most the ground plus
    /// their shares at `s`. A box neither bound clears is composited at
    /// each of those shares as `paint` composites it. A reading never falls
    /// as a channel rises (the table's steps rise, and its sums' roundings
    /// keep order).
    #[inline]
    fn vouch(&mut self, j: Judge, s: f32, bar: f32) -> bool {
        #[inline]
        const fn ch(c: u32, i: usize) -> u32 {
            (c >> (16 - 8 * i as u32)) & 0xff
        }
        let Share::Of128(top) = Share::of(s) else {
            return false;
        };
        let (w, h) = ((j.x1 - j.x0) as usize, (j.y1 - j.y0) as usize);
        let r = RING as usize;
        if w <= 2 * r || h <= 2 * r {
            // `holds` holds at every share.
            return true;
        }
        if !self.laid.is_empty() {
            return false;
        }
        let (dark, _) = self.clear_of(j.l_ink, j.l_ink, bar);
        if self.unlit.0 && self.bare_lum.0 > dark {
            return false;
        }
        // The mids the bisection tries below `top` while each holds.
        let mut mids = [Share::Whole; STEPS];
        let (mut n, mut lo) = (0, 0u32);
        while (lo + (1 << STEPS)) / 2 < top {
            lo = (lo + (1 << STEPS)) / 2;
            mids[n] = Share::Of128(lo);
            n += 1;
        }
        let mids = &mids[..n];
        let table = crate::color_math::relative_luminance_table();
        let share = Share::Of128(top);
        let d = [ch(j.ground, 0), ch(j.ground, 1), ch(j.ground, 2)];
        let first = self.first_ground.as_slice();
        let more = self.more_ground.as_slice();
        #[inline(always)]
        const fn exact_pixel(
            ground: u32,
            first_k: u32,
            has_more: bool,
            more: &[LitPx],
            k: usize,
            at: Share,
        ) -> u32 {
            let mut o = over_packed(ground, first_k, at);
            if has_more {
                let mut mi = 0;
                while mi < more.len() {
                    let m = more[mi];
                    if m.k as usize == k {
                        o = over_packed(o, m.v, at);
                    }
                    mi += 1;
                }
            }
            o
        }
        /// A pixel's greatest colour under every share up to `share`: one light, `max(d + 1, o +
        /// 2)` each channel; more, the ground plus each light's share at `share`.
        #[inline(always)]
        const fn pixel_bound(
            (_, d): (u32, [u32; 3]),
            share: Share,
            (first_k, sum, o): (u32, [u16; 4], u32),
        ) -> u32 {
            if sum[3] == 0 {
                #[inline(always)]
                const fn top(di: u32, o: u32, i: usize) -> u32 {
                    let a = di + 1;
                    let b = ch(o, i) + 2;
                    let m = if a > b { a } else { b };
                    if m < 255 { m } else { 255 }
                }
                (top(d[0], o, 0) << 16) | (top(d[1], o, 1) << 8) | top(d[2], o, 2)
            } else {
                #[inline(always)]
                const fn top(di: u32, share: Share, first_k: u32, sum_i: u16, i: usize) -> u32 {
                    let m = di + share.scale(ch(first_k, i)) + (sum_i as u32);
                    if m < 255 { m } else { 255 }
                }
                (top(d[0], share, first_k, sum[0], 0) << 16)
                    | (top(d[1], share, first_k, sum[1], 1) << 8)
                    | top(d[2], share, first_k, sum[2], 2)
            }
        }
        #[inline(always)]
        const fn pixel_clears(
            table: &crate::color_math::RelativeLuminanceTable,
            (ground, d): (u32, [u32; 3]),
            share: Share,
            mids: &[Share],
            dark: f32,
            (first_k, sum, o): (u32, [u16; 4], u32),
            (more, k): (&[LitPx], usize),
        ) -> bool {
            let bound = pixel_bound((ground, d), share, (first_k, sum, o));
            if crate::color_math::relative_luminance_with(table, bound) <= dark {
                return true;
            }
            let has_more = sum[3] > 0;
            let mut mi = 0;
            while mi < mids.len() {
                if crate::color_math::relative_luminance_with(
                    table,
                    exact_pixel(ground, first_k, has_more, more, k, mids[mi]),
                ) > dark
                {
                    return false;
                }
                mi += 1;
            }
            true
        }
        #[inline(always)]
        const fn ch_max(a: u32, b: u32) -> u32 {
            let mut out = 0;
            let mut sh = 0u32;
            while sh <= 16 {
                let (x, y) = (a & (0xff << sh), b & (0xff << sh));
                out |= if x < y { y } else { x };
                sh += 8;
            }
            out
        }
        // Per pixel the shares at `s` of the lights past its first, and
        // how many there are.
        let mut stacked = std::mem::take(&mut self.stacked);
        stacked.clear();
        if !more.is_empty() {
            stacked.resize(w * h, [0; 4]);
            let mut mi = 0;
            while mi < more.len() {
                let m = more[mi];
                let sum = &mut stacked[m.k as usize];
                sum[0] = sum[0].saturating_add(share.scale(ch(m.v, 0)) as u16);
                sum[1] = sum[1].saturating_add(share.scale(ch(m.v, 1)) as u16);
                sum[2] = sum[2].saturating_add(share.scale(ch(m.v, 2)) as u16);
                sum[3] = sum[3].saturating_add(1);
                mi += 1;
            }
        }
        let sum_at = |k: usize| {
            if stacked.is_empty() {
                [0; 4]
            } else {
                stacked[k]
            }
        };
        let lit_rows = self.lit_rows.as_slice();
        #[cfg(any(test, debug_assertions))]
        let channels = !self.luma_ring;
        #[cfg(not(any(test, debug_assertions)))]
        let channels = true;
        let mut every = true;
        if channels {
            // The box's greatest of each channel: every pixel's bound, else at each mid every
            // pixel as `paint` composites it, the pixels no light reaches the bare ground.
            let unlit = if self.unlit.0 { j.ground } else { 0 };
            let mut top_c = unlit;
            let mut y = 0;
            while y < lit_rows.len() {
                let (lo, hi) = lit_rows[y];
                let mut k = y * w + lo as usize;
                while k < y * w + hi as usize {
                    top_c = ch_max(
                        top_c,
                        pixel_bound(
                            (j.ground, d),
                            share,
                            (first[k], sum_at(k), self.px_ground[k]),
                        ),
                    );
                    k += 1;
                }
                y += 1;
            }
            if crate::color_math::relative_luminance_with(table, top_c) > dark {
                let mut mi = 0;
                'mids: while mi < mids.len() {
                    let mut at_mid = unlit;
                    let mut y = 0;
                    while y < lit_rows.len() {
                        let (lo, hi) = lit_rows[y];
                        let mut k = y * w + lo as usize;
                        while k < y * w + hi as usize {
                            let lit_k = sum_at(k)[3] > 0;
                            let o = exact_pixel(j.ground, first[k], lit_k, more, k, mids[mi]);
                            at_mid = ch_max(at_mid, o);
                            k += 1;
                        }
                        y += 1;
                    }
                    if crate::color_math::relative_luminance_with(table, at_mid) > dark {
                        every = false;
                        break 'mids;
                    }
                    mi += 1;
                }
            }
        } else {
            'rows: for (y, &(lo, hi)) in lit_rows.iter().enumerate() {
                let mut k = y * w + lo as usize;
                while k < y * w + hi as usize {
                    if !pixel_clears(
                        table,
                        (j.ground, d),
                        share,
                        mids,
                        dark,
                        (first[k], sum_at(k), self.px_ground[k]),
                        (more, k),
                    ) {
                        every = false;
                        break 'rows;
                    }
                    k += 1;
                }
            }
        }
        self.stacked = stacked;
        every
    }

    /// The ground readings every ink reading in `li_lo ..= li_hi` keeps
    /// `bar` against, as [`Self::holds`] reads a ratio: at most `dark`
    /// (under the dimmest ink) or at least `light` (over the brightest).
    /// Each is the last reading the bar admits, found over `f32`'s ordered
    /// bits with the very sum and quotient `holds` takes, so a ground the
    /// bounds admit is one `holds` admits against every such ink. The last
    /// answer is kept: a frame's cells share their ink.
    #[inline]
    const fn clear_of(&mut self, li_lo: f32, li_hi: f32, bar: f32) -> (f32, f32) {
        if let Some((key, ans)) = self.clear_memo
            && key.0 == li_lo.to_bits()
            && key.1 == li_hi.to_bits()
            && key.2 == bar.to_bits()
        {
            return ans;
        }
        // The largest `g` in `[0, li_lo)` with `(li_lo + 0.05) / (g + 0.05) >= bar`.
        let dark = if (li_lo + 0.05) / 0.05 >= bar {
            let (mut ok, mut no) = (0u32, li_lo.to_bits());
            while no - ok > 1 {
                let mid = ok + (no - ok) / 2;
                if (li_lo + 0.05) / (f32::from_bits(mid) + 0.05) >= bar {
                    ok = mid;
                } else {
                    no = mid;
                }
            }
            f32::from_bits(ok)
        } else {
            f32::NEG_INFINITY
        };
        // The smallest `g` past `li_hi` with `(g + 0.05) / (li_hi + 0.05) >= bar`
        // (a reading runs to 1.0).
        let top = 2.0f32;
        let light = if li_hi < top && (top + 0.05) / (li_hi + 0.05) >= bar {
            let (mut no, mut ok) = (li_hi.to_bits(), top.to_bits());
            while ok - no > 1 {
                let mid = no + (ok - no) / 2;
                if (f32::from_bits(mid) + 0.05) / (li_hi + 0.05) >= bar {
                    ok = mid;
                } else {
                    no = mid;
                }
            }
            f32::from_bits(ok)
        } else {
            f32::INFINITY
        };
        self.clear_memo = Some((
            (li_lo.to_bits(), li_hi.to_bits(), bar.to_bits()),
            (dark, light),
        ));
        (dark, light)
    }

    /// The hash of everything a cell's verdict ([`Self::judge`]) reads: its
    /// ring box `(x0, y0, x1, y1)`, its ink and ground, its bar's room and
    /// how its grounds are read (tests' controls, [`OWN_ROOM`],
    /// [`Self::luma_ring`]), its face's core or
    /// (`model`, `0` for none) the glyphs and grounds of the neighbourhood
    /// its ring is read from ([`Judge::ring`]) and the bars it keeps there
    /// (`hold`), and each of [`Self::cell`]'s lights, in order, by what it
    /// draws (not its index, which a light laid before it moves).
    #[inline]
    const fn cell_key(
        &self,
        (x0, y0, x1, y1): (i32, i32, i32, i32),
        (ink, ground): (u32, u32),
        (cover, model, hold): (Option<Cover>, u64, RasterHold),
        streams: Streams<'_>,
    ) -> u64 {
        #[cfg(any(test, debug_assertions))]
        let (no_room, luma_ring) = (self.no_room, self.luma_ring);
        #[cfg(not(any(test, debug_assertions)))]
        let (no_room, luma_ring) = (false, false);
        let seams = (no_room as u64) << 48
            | (luma_ring as u64) << 49
            | (matches!(hold, RasterHold::Own) as u64) << 50;
        let mut h = fold(
            fold(
                fold(
                    fold(0, model),
                    (x0 as u32 as u64) | (y0 as u32 as u64) << 32,
                ),
                (x1 as u32 as u64) | (y1 as u32 as u64) << 32,
            ),
            (ink as u64) | (ground as u64) << 24 | seams,
        );
        if let Some(Cover { c, seat }) = cover {
            h = fold(h, c.to_bits() as u64 | 1 << 32);
            if let Some([x0, y0, x1, y1]) = seat {
                h = fold(h, pack_u16x4(x0 as u16, y0 as u16, x1 as u16, y1 as u16));
            }
        }
        let halos = streams.halos;
        let cell_slice = self.cell.as_slice();
        let mut ci = 0;
        while ci < cell_slice.len() {
            let (stream, i) = cell_slice[ci];
            ci += 1;
            let i = i as usize;
            // Each stream its own starting state: a light reads differently
            // on the ground alone and over the ink.
            h ^= ((stream as u8 as u64) + 1).wrapping_mul(0xD6E8_FEB8_6659_FD93);
            match stream {
                Stream::HaloAdd | Stream::HaloOver => {
                    let r = &halos[i];
                    h = fold(
                        fold(
                            fold(h, pack_u16x4(r.x, r.y, r.w, r.h)),
                            (r.color as u64) | (r.cx as u64) << 32 | (r.cy as u64) << 48,
                        ),
                        pack_u16x4(r.rx, r.ry, 0, 0),
                    );
                }
                _ => {
                    // A quad's verdict reads its colours' channels alone.
                    let q = streams.quad(stream, i);
                    h = fold(
                        fold(h, pack_u16x4(q.x, q.y, q.w, q.h)),
                        ((q.color & 0x00FF_FFFF) as u64)
                            | (q.alpha as u64) << 24
                            | ((q.color2 & 0x00FF_FFFF) as u64) << 32
                            | (q.alpha2 as u64) << 56,
                    );
                }
            }
        }
        h
    }

    /// The verdict the last frame judged for a cell of key `key`
    /// ([`Self::cell_key`]), kept for the next frame as this one's.
    #[inline]
    fn verdict(&mut self, key: u64) -> Option<f32> {
        #[inline]
        const fn find_verdict(verdicts: &[(u64, f32)], key: u64) -> Option<usize> {
            let mut l = 0usize;
            let mut r = verdicts.len();
            while l < r {
                let mid = (l + r) >> 1;
                let k = verdicts[mid].0;
                if k < key {
                    l = mid + 1;
                } else if k > key {
                    r = mid;
                } else {
                    return Some(mid);
                }
            }
            None
        }
        #[cfg(any(test, debug_assertions))]
        if self.no_reuse {
            return None;
        }
        let at = find_verdict(&self.verdicts, key)?;
        let share = self.verdicts[at].1;
        self.fresh.push((key, share));
        #[cfg(any(test, debug_assertions))]
        {
            self.reused += 1;
        }
        Some(share)
    }

    /// Make this frame's verdicts the ones the next frame reuses
    /// ([`Self::verdict`]).
    #[inline]
    fn settle_verdicts(&mut self) {
        self.fresh.sort_unstable_by_key(|v| v.0);
        std::mem::swap(&mut self.verdicts, &mut self.fresh);
        self.fresh.clear();
        self.near_whole_fresh.sort_unstable();
        std::mem::swap(&mut self.near_whole, &mut self.near_whole_fresh);
        self.near_whole_fresh.clear();
    }

    /// The share of [`Self::cell`]'s lights the letter of `ink` on `ground`
    /// in the ring box `(x0, y0, x1, y1)` keeps (`1.0` uncut): none of them
    /// cut where their brightest sum keeps its bar, else [`Self::share`].
    /// `solid` memoises the last pair's solid ink, luminances and bar.
    /// `cover`: where the face lays the letter's core at less than its
    /// colour whole, that coverage and where the glyph lays ink
    /// ([`Judge::cover`], [`Judge::core_box`]), its core judged too. `ring`:
    /// the cell is read by its glyph's own pixels, as [`Self::ring`] holds it
    /// ([`Judge::ring`]), each reading kept at the bar that hold names.
    #[inline]
    fn judge(
        &mut self,
        (x0, y0, x1, y1): (i32, i32, i32, i32),
        (ink, ground): (u32, u32),
        solid: &mut Option<SolidMemo>,
        (cover, ring): (Option<Cover>, Option<RasterHold>),
        streams: Streams<'_>,
    ) -> f32 {
        #[inline]
        const fn lum_ratio(la: f32, lb: f32) -> f32 {
            (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
        }
        let table = crate::color_math::relative_luminance_table();
        #[cfg(any(test, debug_assertions))]
        {
            self.judged += 1;
        }
        let fg = ink;
        // A letter read by its solid ink keeps the room over its share ([`OWN_ROOM`]).
        #[cfg(any(test, debug_assertions))]
        let room = !self.no_room;
        #[cfg(not(any(test, debug_assertions)))]
        let room = true;
        let (ink, l_ink, l_ground, bar) = match *solid {
            Some((pair, cached)) if pair == (ink, ground) => cached,
            _ => {
                let l_g = crate::color_math::relative_luminance_with(table, ground);
                let s = solid_ink_with(table, ink, ground, l_g);
                let l_i = crate::color_math::relative_luminance_with(table, s);
                // The bare page's reading: every light at share 0 composes
                // to the ground and the ink themselves.
                let b = letter_bar(lum_ratio(l_i, l_g), room);
                let cached = (s, l_i, l_g, b);
                *solid = Some(((ink, ground), cached));
                cached
            }
        };
        let l_fg = crate::color_math::relative_luminance_with(table, fg);
        // The core's bare reading: its coverage of the colour over the bare ground.
        let seat = cover.and_then(|cover| cover.seat);
        let cover = cover.map(|Cover { c, .. }| {
            let l_c = c * l_fg + (1.0 - c) * l_ground;
            (c, letter_bar(lum_ratio(l_c, l_ground), room))
        });
        // The ring box's inner pixels (the cell) where the glyph lays ink, a pixel either side.
        let r = RING;
        let (w, h) = (x1 - x0, y1 - y0);
        let core_box = match seat {
            Some([sx0, sy0, sx1, sy1]) => [
                (r + sx0 as i32 - 1).max(r),
                (r + sy0 as i32 - 1).max(r),
                (r + sx1 as i32 + 1).min(w - r),
                (r + sy1 as i32 + 1).min(h - r),
            ],
            None => [r, r, w - r, h - r],
        };
        let j = Judge {
            x0,
            y0,
            x1,
            y1,
            ink,
            ground,
            l_ink,
            l_ground,
            fg,
            l_fg,
            cover,
            core_box,
            ring: ring.is_some(),
            hold: ring.unwrap_or(RasterHold::Bar),
        };
        if let Some(hold) = ring {
            // Read as the judge reads it: the bar of its own reading on the bare page, and where
            // every light adds, the brightest ground the lights can make (each channel's sum
            // bounds each channel's median of a ring) against its dimmest solid ink.
            let bar = hold.bar(self.ring.own);
            if !self.ring.ring_text
                && !self.ring.filled()
                && self.ring.l_min > l_ground
                && let Some((_, all)) = self.bound(j, streams)
            {
                let lg = crate::color_math::relative_luminance_with(table, lifted(ground, all));
                if lg < self.ring.l_min
                    && lum_ratio(self.ring.l_min, lg) >= bar
                    && self.ring.whole_clears(lg, hold)
                {
                    return 1.0;
                }
            }
            return self.share(j, bar, streams);
        }
        if let Some((on_ink, all)) = self.bound(j, streams) {
            // Light ink on a dark page reads worst against the
            // brightest ground the lights can make; dark ink on
            // paper, lifted the most, against the page itself.
            let clear = if l_ink >= l_ground {
                let lg = crate::color_math::relative_luminance_with(table, lifted(ground, all));
                lg < l_ink && lum_ratio(l_ink, lg) >= bar
            } else {
                let l_i = crate::color_math::relative_luminance_with(table, lifted(ink, on_ink));
                l_i < l_ground && lum_ratio(l_i, l_ground) >= bar
            };
            // The core alike: its least ink (the bare core: an added light only lifts it)
            // against the brightest ground, or its most (the colour lifted over the ink and
            // its ground's share lifted by every light) against the page itself.
            let clear = clear
                && cover.is_none_or(|(c, bar_c)| {
                    let l_c = c * l_fg + (1.0 - c) * l_ground;
                    let lg = crate::color_math::relative_luminance_with(table, lifted(ground, all));
                    if l_c >= l_ground {
                        lg < l_c && lum_ratio(l_c, lg) >= bar_c
                    } else {
                        let l_top = c * crate::color_math::relative_luminance_with(
                            table,
                            lifted(fg, on_ink),
                        ) + (1.0 - c) * lg;
                        l_top < l_ground && lum_ratio(l_top, l_ground) >= bar_c
                    }
                });
            if clear {
                return 1.0;
            }
        }
        self.share(j, bar, streams)
    }

    /// The largest share of the cell's lights that keeps the letter's `bar`
    /// ([`letter_bar`]; `1.0` when the whole light does).
    #[inline]
    fn share(&mut self, j: Judge, bar: f32, streams: Streams<'_>) -> f32 {
        let mut has_over = false;
        let cell_slice = self.cell.as_slice();
        let mut ci = 0;
        while ci < cell_slice.len() {
            if !cell_slice[ci].0.on_ground() {
                has_over = true;
                break;
            }
            ci += 1;
        }
        if j.ring {
            self.lay_lights(j, streams, has_over);
            if self.holds_ring(j, 1.0, bar, has_over) {
                return 1.0;
            }
            let (mut lo, mut hi) = (0.0f32, 1.0f32);
            let mut step = 0;
            while step < STEPS {
                let mid = 0.5 * (lo + hi);
                if self.holds_ring(j, mid, bar, has_over) {
                    lo = mid;
                } else {
                    hi = mid;
                }
                step += 1;
            }
            return lo;
        }
        let additive = self.additive(streams);
        if additive {
            self.prepare(j, streams, has_over);
        } else {
            self.lay_lights(j, streams, has_over);
        }
        let flags = (additive, has_over);
        if self.holds(j, 1.0, bar, flags) {
            return 1.0;
        }
        // A cell the last frame kept `NEAR_WHOLE` of its light is weighed
        // there first: where every share under it holds by what that
        // composite shows ([`Self::vouch`]), every mid the bisection tries
        // holds, and its answer is that share.
        let mut near_whole = None;
        if !additive
            && !has_over
            && j.cover.is_none()
            && self.near_whole.binary_search(&box_at(j.x0, j.y0)).is_ok()
        {
            let held = self.holds(j, NEAR_WHOLE, bar, flags);
            if held && self.vouch(j, NEAR_WHOLE, bar) {
                return NEAR_WHOLE;
            }
            near_whole = Some(held);
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        let mut step = 0;
        while step < STEPS {
            let mid = 0.5 * (lo + hi);
            let held = match near_whole {
                Some(held) if mid == NEAR_WHOLE => held,
                _ => self.holds(j, mid, bar, flags),
            };
            if held {
                lo = mid;
            } else {
                hi = mid;
            }
            step += 1;
        }
        lo
    }

    /// The upper bound of the light the cell's lights can add to a pixel of
    /// the ring box, per channel, over the ink (`over`) and the ground
    /// (`all`); `None` when a source-over light reaches it (no bound).
    #[inline]
    const fn bound(&self, j: Judge, streams: Streams<'_>) -> Option<([u32; 3], [u32; 3])> {
        #[inline]
        const fn add(acc: &mut [u32; 3], c: u32) {
            acc[0] += (c >> 16) & 0xff;
            acc[1] += (c >> 8) & 0xff;
            acc[2] += c & 0xff;
        }
        #[inline]
        const fn max_ch(a: u32, b: u32, sh: u32) -> u32 {
            let ca = (a >> sh) & 0xff;
            let cb = (b >> sh) & 0xff;
            if ca > cb { ca } else { cb }
        }
        #[inline]
        const fn clamp_i32(v: i32, lo: i32, hi: i32) -> i32 {
            if v < lo {
                lo
            } else if v > hi {
                hi
            } else {
                v
            }
        }
        let halos = streams.halos;
        let (mut on_ink, mut all) = ([0u32; 3], [0u32; 3]);
        let cell_slice = self.cell.as_slice();
        let mut ci = 0;
        while ci < cell_slice.len() {
            let (stream, i) = cell_slice[ci];
            ci += 1;
            let i = i as usize;
            let c = match stream {
                Stream::Under | Stream::Over | Stream::UnderRow | Stream::OverRow => {
                    let q = streams.quad(stream, i);
                    if q.alpha != 0 || q.alpha2 != 0 {
                        return None;
                    }
                    let (a, b) = (q.color, q.color2);
                    (max_ch(a, b, 16) << 16) | (max_ch(a, b, 8) << 8) | max_ch(a, b, 0)
                }
                Stream::HaloAdd => {
                    let h = &halos[i];
                    let (cx, cy) = (h.cx as i32, h.cy as i32);
                    let hx = h.x as i32;
                    let hy = h.y as i32;
                    let x_lo = if j.x0 > hx { j.x0 } else { hx };
                    let x_hi = if j.x1 - 1 > j.x0 { j.x1 - 1 } else { j.x0 };
                    let y_lo = if j.y0 > hy { j.y0 } else { hy };
                    let y_hi = if j.y1 - 1 > j.y0 { j.y1 - 1 } else { j.y0 };
                    let x = clamp_i32(cx, x_lo, x_hi);
                    let y = clamp_i32(cy, y_lo, y_hi);
                    let wt = halo_wt(h, x, y);
                    aterm_render::premul_rgb(h.color & 0x00FF_FFFF, if wt > 1 { wt } else { 1 })
                }
                Stream::HaloOver => return None,
            };
            if !stream.on_ground() {
                add(&mut on_ink, c);
            }
            add(&mut all, c);
        }
        Some((on_ink, all))
    }
}

#[inline(always)]
const fn mask_cut_end(
    x: i32,
    x0: i32,
    inv_w: f32,
    c0: [f32; 4],
    dc: [f32; 4],
    m: f32,
    round_cuts: bool,
) -> (u32, u8) {
    #[inline(always)]
    const fn fit(v: f32, m: f32, round_cuts: bool) -> u32 {
        let u = if m < 1.0 && !round_cuts {
            v.trunc()
        } else {
            v.round()
        } as u32;
        if u < 255 { u } else { 255 }
    }
    let t = (x - x0) as f32 * inv_w;
    let r = fit((c0[0] + dc[0] * t) * m, m, round_cuts);
    let g = fit((c0[1] + dc[1] * t) * m, m, round_cuts);
    let b = fit((c0[2] + dc[2] * t) * m, m, round_cuts);
    let a = fit((c0[3] + dc[3] * t) * m, m, round_cuts) as u8;
    ((r << 16) | (g << 8) | b, a)
}

/// The bar a letter keeps under the lights, from its reading on the bare page, `bare` (a letter
/// read by its raster, the judge's own reading, [`ring::RingCell::own`]; else its solid ink's):
/// [`LETTER_FLOOR`], which is AA with the judge's rounding in hand; [`WCAG_AA`] with a level in
/// hand ([`LEVEL_ROOM`]) where `bare` reads under the floor and at AA as the judges read it
/// ([`AA_READ`]) or over, so a letter that reads under that on the bare page takes no light that
/// lowers it; and under that, [`OWN_SHARE`] of `bare`. A letter read by its solid ink (`room`)
/// keeps [`OWN_ROOM`] over AA or the share, never past `bare`: at AA or over, its whole reading.
#[inline]
#[must_use]
pub fn letter_bar(bare: f32, room: bool) -> f32 {
    if bare >= LETTER_FLOOR {
        LETTER_FLOOR
    } else if bare >= aa_read() && !share_over_aa() {
        if room {
            (WCAG_AA * OWN_ROOM).min(bare)
        } else {
            WCAG_AA * LEVEL_ROOM
        }
    } else if room {
        (bare * OWN_SHARE * OWN_ROOM).min(bare)
    } else {
        bare * OWN_SHARE
    }
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static SHARE_OVER_AA: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static AA_UNROUNDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A test's negative control: `f` run with [`letter_bar`] keeping AA only for a letter that reads
/// [`WCAG_AA`] or more on its bare page, not [`AA_READ`], the reading the judges give as 4.50.
/// Debug and test builds only.
#[cfg(any(test, debug_assertions))]
pub fn with_aa_unrounded<R>(f: impl FnOnce() -> R) -> R {
    AA_UNROUNDED.with(|c| c.set(true));
    let out = f();
    AA_UNROUNDED.with(|c| c.set(false));
    out
}

/// The least bare reading [`letter_bar`] keeps AA for: [`AA_READ`] ([`with_aa_unrounded`]).
#[inline]
fn aa_read() -> f32 {
    #[cfg(any(test, debug_assertions))]
    if AA_UNROUNDED.with(std::cell::Cell::get) {
        return WCAG_AA;
    }
    AA_READ
}

/// A test's negative control: `f` run with a letter that reads [`WCAG_AA`] or more on its bare
/// page, and under [`LETTER_FLOOR`], keeping [`OWN_SHARE`] of that reading as a dimmer one does,
/// after-land A7's bar, under which it may read under AA. Debug and test builds only.
#[cfg(any(test, debug_assertions))]
pub fn with_share_over_aa<R>(f: impl FnOnce() -> R) -> R {
    SHARE_OVER_AA.with(|c| c.set(true));
    let out = f();
    SHARE_OVER_AA.with(|c| c.set(false));
    out
}

/// Whether [`letter_bar`] keeps a letter at AA or over its share ([`with_share_over_aa`]).
#[inline]
fn share_over_aa() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        SHARE_OVER_AA.with(std::cell::Cell::get)
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// The room a letter read by its raster, on a dim page, keeps over [`OWN_SHARE`]: a level. This
/// guard's frame matches a terminal's to the level on all but a rare pixel, and a row of two solid
/// pixels stands on the lower of them, so one pixel's green read a level down moves the judge's
/// reading past a share kept to the letter: Fire's `c` on Solarized Dark read 4.154 here and 4.12
/// by the judge (`#929692` in this frame, `#929592` in the window's), its bar 4.15. A level of green
/// is 0.8% of that reading on the ink and 1.3% on its ring, and the Cursor card's `.` 0.6% on its
/// bed. The solid-ink reading's room ([`OWN_ROOM`]) is half as wide again, and that one cost dim
/// pages light. A letter that reads AA or more on its bare page, under the floor, keeps the level
/// over AA ([`letter_bar`]), the glass's yield reading its glyph's levels as this guard reads its
/// ring: held at AA there, the line's `p` read 4.484 in a window
/// (`an_app_ink_at_aa_keeps_aa_under_the_glass`), and held at its whole reading of its own here,
/// Beam's `d` read 4.490 (`a_letter_at_aa_keeps_aa`).
const LEVEL_ROOM: f32 = 1.014;

/// The bar the guard holds a letter read by its raster to, from its own reading `bare`:
/// [`letter_bar`], with [`LEVEL_ROOM`] in hand where that is a share of `bare`. A `letter_bar`
/// under AA is what the judges ask; this is what the frame must read here for a window's frame to
/// meet that. At AA or over the bar has its room already: the floor's, or a level over AA.
#[inline]
#[must_use]
pub(super) fn raster_bar(bare: f32) -> f32 {
    let bar = letter_bar(bare, false);
    if bar >= WCAG_AA {
        bar
    } else {
        bar * LEVEL_ROOM
    }
}

/// The bar [`CursorGlow::hold_page_shades`] holds a letter read by its raster to under the page
/// shades, from its own reading `bare`: [`raster_bar`], or `bare` itself where that is lower. A
/// letter that reads at AA on its bare page and under AA's level in hand ([`LEVEL_ROOM`]) is under
/// its raster bar before any light falls on it. Held to that bar, it lost a shade that lifted it
/// short of the bar: Beam's body on Solarized Dark was cut from under the `w`, `j` and `g` (4.5086
/// of their own) where the whole body read them higher, Typing's `w` at 4.5481 whole and 4.5086
/// held. Of the cells behind the caret that `trail_letters_read`'s W6 reads at its bar with the
/// body whole, 18 of 227 fell under it typing and 48 of 610 bursting (Linux arm64, 2026-11-07).
/// Held here, a letter keeps every shade under which it reads its own reading or more, and loses
/// only one that lowers it. Under a page shade such a letter gives up [`LEVEL_ROOM`]'s level in
/// hand: held at its whole reading the guard let Beam's `d` read 4.490 in a window, and here it is
/// held at its own reading with no level over it. Replayed on Linux's bundled face no letter at AA
/// reads under AA in Beam's scenes on Solarized Dark (Linux arm64, 2026-11-07); no window and no
/// Mac has read it.
#[inline]
#[must_use]
fn shade_bar(bare: f32) -> f32 {
    raster_bar(bare).min(bare)
}

/// Which bar a letter read by its raster ([`Judge::ring`]) keeps, from each of its readings on the
/// bare page: its ring's ([`ring::RingCell::own`]) and its whole reading's
/// ([`ring::RingCell::whole`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RasterHold {
    /// [`raster_bar`]: under the frame's lights, weighed together.
    Bar,
    /// [`shade_bar`]: under the page shades alone ([`CursorGlow::hold_page_shades`]).
    Own,
}

impl RasterHold {
    /// The bar a reading of `bare` on the bare page keeps.
    #[inline]
    #[must_use]
    fn bar(self, bare: f32) -> f32 {
        match self {
            Self::Bar => raster_bar(bare),
            Self::Own => shade_bar(bare),
        }
    }
}

/// The most luminance a light against a glyph's foot may read and leave its letter at the bar
/// the guard holds it to ([`raster_bar`]): a rail's foot quads go unweighed
/// ([`InkLedger::note_foot_rail`]), so this bar is theirs to keep. It stands on the raster's
/// lowest row the judges read ([`ring`]), by the least of that row's solid pixels (they read its
/// lower median), because a light on the pixel row under it is all of that row's ring. A
/// descender's last row is laid under the letter's colour: `q` in `#858585` on dim-grey reads 5.04
/// by its colour and 4.51 by that row, so a bar from the colour left Comet's thread at 4.23 of
/// that 4.51, and the reference dark's `q` at 4.46. A glyph with no raster, or whose ink does not
/// stand brighter than its ground, leaves its ground's.
#[must_use]
pub(crate) fn foot_ring_light(
    inks: &aterm_render::GlyphInk,
    glyph: u8,
    ink: u32,
    ground: u32,
) -> f32 {
    let table = crate::color_math::relative_luminance_table();
    let l_ground = crate::color_math::relative_luminance_with(table, ground);
    let Some(raster) = inks.raster(glyph) else {
        return l_ground;
    };
    let reads = |cov: u8| {
        let laid = aterm_render::TextLevel::new(ink, ground, cov, inks.corrected()).over(ground);
        (crate::color_math::relative_luminance_with(table, laid & 0x00FF_FFFF) + 0.05)
            / (l_ground + 0.05)
    };
    let full = reads(raster.cov.iter().copied().max().unwrap_or(0));
    if full <= 1.0 {
        return l_ground;
    }
    for row in raster.cov.rchunks_exact(raster.w) {
        let least = row
            .iter()
            .copied()
            .filter(|&cov| reads(cov) >= SOLID_SHARE * full)
            .min();
        if let Some(cov) = least {
            let own = reads(cov);
            return (own * (l_ground + 0.05) / raster_bar(own) - 0.05).max(l_ground);
        }
    }
    l_ground
}

/// `c` lifted by `lift` per channel, saturating.
#[inline]
const fn lifted(c: u32, lift: [u32; 3]) -> u32 {
    #[inline]
    const fn ch(c: u32, sh: u32, l: u32) -> u32 {
        let sum = ((c >> sh) & 0xff) + l;
        if sum < 255 { sum } else { 255 }
    }
    (ch(c, 16, lift[0]) << 16) | (ch(c, 8, lift[1]) << 8) | ch(c, 0, lift[2])
}

/// The solid ink of a letter drawn in `ink` on `ground`: the colour on the
/// way from `ink` to `ground` that reads [`SOLID_SHARE`] of the clean ratio.
#[cfg(test)]
#[inline]
const fn solid_ink(ink: u32, ground: u32) -> u32 {
    let table = crate::color_math::relative_luminance_table();
    let lg = crate::color_math::relative_luminance_with(table, ground);
    solid_ink_with(table, ink, ground, lg)
}

#[inline]
const fn solid_ink_with(
    table: &crate::color_math::RelativeLuminanceTable,
    ink: u32,
    ground: u32,
    lg: f32,
) -> u32 {
    #[inline]
    const fn ratio(table: &crate::color_math::RelativeLuminanceTable, lg: f32, a: u32) -> f32 {
        let la = crate::color_math::relative_luminance_with(table, a);
        (la.max(lg) + 0.05) / (la.min(lg) + 0.05)
    }
    #[inline]
    const fn mix_ch(a: f32, diff: f32, t: f32) -> u32 {
        let u = (a + diff * t).round() as u32;
        if u < 255 { u } else { 255 }
    }
    let ink_r = ((ink >> 16) & 0xff) as f32;
    let ink_g = ((ink >> 8) & 0xff) as f32;
    let ink_b = (ink & 0xff) as f32;
    let dr = ((ground >> 16) & 0xff) as f32 - ink_r;
    let dg = ((ground >> 8) & 0xff) as f32 - ink_g;
    let db = (ground & 0xff) as f32 - ink_b;
    let want = SOLID_SHARE * ratio(table, lg, ink);
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (ink & 0x00FF_FFFF, ground & 0x00FF_FFFF);
    let mut hi_checked = false;
    let mut step = 0;
    while step < 10 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb =
            (mix_ch(ink_r, dr, mid) << 16) | (mix_ch(ink_g, dg, mid) << 8) | mix_ch(ink_b, db, mid);
        if mid_rgb == lo_rgb
            || ((!hi_checked || mid_rgb != hi_rgb) && ratio(table, lg, mid_rgb) >= want)
        {
            lo = mid;
            lo_rgb = mid_rgb;
        } else {
            hi = mid;
            hi_rgb = mid_rgb;
            hi_checked = true;
        }
        if lo_rgb == hi_rgb {
            break;
        }
        step += 1;
    }
    lo_rgb
}

impl InkLedger {
    /// Cut each page shade's light ([`Self::note_page_shade_laid`]) in place to the least share
    /// of `dims` whose box it meets (`CursorGlow::hold_page_shades`): a quad, or a ramp's row of
    /// kept ends, scaled whole, its colour and opacity truncated as a cut piece's are
    /// ([`mask_cut_end`]). No light moves in the stream, so every span the ledger noted still
    /// names its lights.
    #[inline]
    fn cut_page_shades(&self, dims: &[Dim], quads: &mut [GlowQuad], ramps: &mut GlowRamps) {
        let share_over = |(x0, y0, x1, y1): (i32, i32, i32, i32)| -> f32 {
            let (x0, y0, x1, y1) = (x0 as f32, y0 as f32, x1 as f32, y1 as f32);
            dims.iter()
                .filter(|d| x0 < d.x1 && x1 > d.x0 && y0 < d.y1 && y1 > d.y0)
                .fold(1.0f32, |s, d| s.min(d.share))
        };
        for &(from, to) in &self.page_shades {
            let to = (to as usize).min(quads.len());
            for q in quads.iter_mut().take(to).skip(from as usize) {
                let (x0, y0) = (i32::from(q.x), i32::from(q.y));
                let s = share_over((x0, y0, x0 + i32::from(q.w), y0 + i32::from(q.h)));
                if s < 1.0 {
                    q.color = scale_rgb(q.color, s);
                    q.color2 = scale_rgb(q.color2, s);
                    q.alpha = scale_u8(u32::from(q.alpha), s) as u8;
                    q.alpha2 = scale_u8(u32::from(q.alpha2), s) as u8;
                }
            }
        }
        let scale_end = |e: GlowEnd, s: f32| {
            glow_end(scale_rgb(e & 0x00FF_FFFF, s), scale_u8(e >> 24, s) as u8)
        };
        let mut cut = false;
        for &(from, to) in &self.page_shade_ramps {
            for k in from as usize..(to as usize).min(ramps.items.len()) {
                let r = ramps.items[k];
                if r.w == 0 || r.h == 0 {
                    continue;
                }
                let (x0, x1) = (i32::from(r.x), i32::from(r.x) + i32::from(r.w));
                // Each row of kept ends and the rows it lays ([`GlowRamp::end_row`]).
                for slot in 0..=r.end_row(r.h - 1) {
                    let y0 = i32::from(r.y)
                        + i32::from(slot)
                        + if slot > r.join {
                            i32::from(r.joined)
                        } else {
                            0
                        };
                    let h = if slot == r.join {
                        i32::from(r.joined) + 1
                    } else {
                        1
                    };
                    let s = share_over((x0, y0, x1, y0 + h));
                    if s < 1.0 {
                        let at = r.ink as usize + 2 * usize::from(slot);
                        ramps.ends[at] = scale_end(ramps.ends[at], s);
                        ramps.ends[at + 1] = scale_end(ramps.ends[at + 1], s);
                        cut = true;
                    }
                }
            }
        }
        if cut {
            ramps.lit = ramps.count_lit();
        }
    }

    /// Cut every quad of `quads` (additive or source-over) to the guard's
    /// dims ([`Self::mask_at_with`]), easing back to the whole light off them,
    /// and the stream's `ramps` with them ([`Self::guard_mask_ramps`]). With
    /// `rails` (the under stream), a light of a rail laid clear of its row's
    /// ring ([`Self::note_rail`]) is cut only where it meets a dimmed cell's
    /// box, and a rail in its row's foot band ([`Self::note_foot_rail`]) is
    /// never cut, nor is a light-owned light
    /// ([`Self::note_light_owned_laid`]).
    #[inline]
    fn guard_mask_stream(&mut self, quads: &mut Vec<GlowQuad>, ramps: &mut GlowRamps, rails: bool) {
        if ramps.is_empty() {
            self.guard_mask_quads::<false>(quads, &mut [], rails);
            if rails {
                self.rail_ramps.clear();
                self.light_owned_ramps.clear();
            }
            return;
        }
        self.guard_mask_quads::<true>(quads, &mut ramps.items, rails);
        self.guard_mask_ramps(ramps, rails);
    }

    /// Cut every quad of `quads` to the guard's dims ([`Self::guard_mask_stream`]),
    /// each of `ramps` (in draw order, where `RAMPS`) drawn before the first
    /// piece of the quad it was drawn before (after the last where it was
    /// drawn after every quad). With `rails`, a quad of a rail laid clear of
    /// its row's ring ([`Self::note_rail`]) is cut only where it meets a
    /// dimmed cell's box, and a quad of a rail in its row's foot band
    /// ([`Self::note_foot_rail`]) is never cut, nor is a light-owned quad
    /// ([`Self::note_light_owned_laid`]).
    #[inline]
    fn guard_mask_quads<const RAMPS: bool>(
        &mut self,
        quads: &mut Vec<GlowQuad>,
        ramps: &mut [GlowRamp],
        rails: bool,
    ) {
        let owned = if rails {
            std::mem::take(&mut self.light_owned)
        } else {
            Vec::new()
        };
        let mut owned_at = 0usize;
        let (bx0, by0, bx1, by1) = self.dims_bounds();
        let feather = self.feather();
        #[cfg(any(test, debug_assertions))]
        let round_cuts = self.round_cuts;
        #[cfg(not(any(test, debug_assertions)))]
        let round_cuts = false;
        #[cfg(any(test, debug_assertions))]
        let rails = rails && !self.rails_feathered;
        let feet_whole = self.foot_rails_whole();
        let spans = if rails {
            let mut spans = std::mem::take(&mut self.rails);
            spans.sort_unstable();
            spans
        } else {
            Vec::new()
        };
        let mut span = 0usize;
        let mut pieces = std::mem::take(&mut self.pieces);
        pieces.clear();
        let mut cut = false;
        let mut next = 0;
        let mut i = 0;
        while i < quads.len() {
            let q = quads[i];
            if RAMPS {
                while next < ramps.len() && ramps[next].at as usize <= i {
                    ramps[next].at = pieces.len() as u32;
                    next += 1;
                }
            }
            let (x0, y0) = (i32::from(q.x), i32::from(q.y));
            let (x1, y1) = (x0 + i32::from(q.w), y0 + i32::from(q.h));
            while span < spans.len() && spans[span].1 as usize <= i {
                span += 1;
            }
            let rail = span < spans.len() && spans[span].0 as usize <= i;
            if (rail && feet_whole && spans[span].2)
                || Self::in_owned_span(&owned, i, &mut owned_at)
                || q.w == 0
                || q.h == 0
                || x1 <= bx0
                || x0 >= bx1
                || y1 <= by0
                || y0 >= by1
                || (rail && !self.meets_dim_box((x0, y0, x1, y1), feather.0))
                || !self.knots((x0, y0, x1, y1))
            {
                pieces.push(q);
                i += 1;
                continue;
            }
            cut = true;
            let inv_w = 1.0 / f32::from(q.w);
            #[inline]
            const fn at_ch(c: u32, sh: u32) -> f32 {
                ((c >> sh) & 0xff) as f32
            }
            let c0 = [
                at_ch(q.color, 16),
                at_ch(q.color, 8),
                at_ch(q.color, 0),
                f32::from(q.alpha),
            ];
            let dc = [
                at_ch(q.color2, 16) - c0[0],
                at_ch(q.color2, 8) - c0[1],
                at_ch(q.color2, 0) - c0[2],
                f32::from(q.alpha2) - c0[3],
            ];
            // A piece keeps the quad's own gradient where it keeps its whole
            // light; a cut piece keeps no more than the share the guard
            // weighed, truncated as its model truncates (`quad_at`). Rounded
            // up, each cut slab of a pile laid a byte of light the guard never
            // weighed: six of a bed's slabs cut to an eighth lit Solarized
            // Dark's page by six (`#00313c` under `fox`, looks7 round 2).
            let ny = self.knots_y.len() - 1;
            let nx = self.knots_x.len() - 1;
            let x0_knot = self.knots_x[0];
            let mut yi = 0;
            while yi < ny {
                let (ya, yb) = (self.knots_y[yi], self.knots_y[yi + 1]);
                let ym = 0.5 * (ya + yb) as f32;
                let ya_u = ya as u16;
                let h_u = (yb - ya) as u16;
                let mut xa = x0_knot;
                let mut ca = mask_cut_end(
                    xa,
                    x0,
                    inv_w,
                    c0,
                    dc,
                    self.mask_at_with(xa as f32, ym, feather),
                    round_cuts,
                );
                let mut xi = 0;
                while xi < nx {
                    let xb = self.knots_x[xi + 1];
                    let cb = mask_cut_end(
                        xb,
                        x0,
                        inv_w,
                        c0,
                        dc,
                        self.mask_at_with(xb as f32, ym, feather),
                        round_cuts,
                    );
                    if ca.0 | cb.0 != 0 || ca.1 | cb.1 != 0 {
                        pieces.push(GlowQuad {
                            row: q.row,
                            x: xa as u16,
                            y: ya_u,
                            w: (xb - xa) as u16,
                            h: h_u,
                            color: ca.0,
                            alpha: ca.1,
                            color2: cb.0,
                            alpha2: cb.1,
                        });
                    }
                    xa = xb;
                    ca = cb;
                    xi += 1;
                }
                yi += 1;
            }
            i += 1;
        }
        if RAMPS {
            while next < ramps.len() {
                ramps[next].at = pieces.len() as u32;
                next += 1;
            }
        }
        if cut {
            std::mem::swap(quads, &mut pieces);
        }
        pieces.clear();
        self.pieces = pieces;
        if rails {
            // The quads' indices moved with the cuts: the spans are spent.
            self.rails = spans;
            self.rails.clear();
        }
        if rails {
            // Kept whole, the light-owned lights moved with the cuts too.
            self.light_owned = owned;
            self.light_owned.clear();
        }
    }

    /// Whether the window px box `[x0, x1) × [y0, y1)` meets a dimmed
    /// cell's box itself (not only its feather) in its rows, within the
    /// feather's reach `fx` across.
    #[inline]
    const fn meets_dim_box(&self, (x0, y0, x1, y1): (i32, i32, i32, i32), fx: f32) -> bool {
        let (x0f, y0f, x1f, y1f) = (x0 as f32, y0 as f32, x1 as f32, y1 as f32);
        let dims = self.dims.as_slice();
        let mut i = 0;
        while i < dims.len() {
            let d = &dims[i];
            if d.share < 1.0 && y0f < d.y1 && y1f > d.y0 && x0f < d.x1 + fx && x1f > d.x0 - fx {
                return true;
            }
            i += 1;
        }
        false
    }

    /// Cut every ramp of `ramps` to the guard's dims as
    /// [`Self::guard_mask_quads`] cuts the quads its rows stand for, piece
    /// for piece: a run of rows the same knots slice lays each slice
    /// between them as one ramp, its rows the pieces' ends (a piece the cut
    /// darkens whole a dark row). A row's pieces lie side by side, so the
    /// slices of a ramp may be laid in any order. A ramp's run is the one
    /// quad its rows stand for: its knots are the run's, each of its rows
    /// takes the cut of the knot interval it lies in, and each interval's
    /// pieces are one light apiece (a run of each slice's ramp), as the
    /// quad's are. With `rails`, a ramp of a rail laid clear of its row's
    /// ring ([`Self::note_rail_laid`]) has a row (a run, its one quad) cut
    /// only where it meets a dimmed cell's box, as its quad would be, and a
    /// light-owned ramp ([`Self::note_light_owned_laid`]) is never cut.
    #[inline]
    fn guard_mask_ramps(&mut self, ramps: &mut GlowRamps, rails: bool) {
        let owned = if rails {
            std::mem::take(&mut self.light_owned_ramps)
        } else {
            Vec::new()
        };
        let mut owned_at = 0usize;
        let (bx0, by0, bx1, by1) = self.dims_bounds();
        let feather = self.feather();
        #[cfg(any(test, debug_assertions))]
        let round_cuts = self.round_cuts;
        #[cfg(not(any(test, debug_assertions)))]
        let round_cuts = false;
        #[cfg(any(test, debug_assertions))]
        let rails = rails && !self.rails_feathered;
        let feet_whole = self.foot_rails_whole();
        let spans = if rails {
            let mut spans = std::mem::take(&mut self.rail_ramps);
            spans.sort_unstable();
            spans
        } else {
            Vec::new()
        };
        let mut span = 0usize;
        let mut out = std::mem::take(&mut self.ramp_pieces);
        out.clear();
        let mut masks = std::mem::take(&mut self.row_masks);
        let mut masks_for: Option<u64> = None;
        self.row_near_on = true;
        let mut cut = false;
        let mut k = 0;
        while k < ramps.items.len() {
            let r = ramps.items[k];
            let (x0, y0) = (i32::from(r.x), i32::from(r.y));
            let (x1, y1) = (x0 + i32::from(r.w), y0 + i32::from(r.h));
            while span < spans.len() && spans[span].1 as usize <= k {
                span += 1;
            }
            let rail = span < spans.len() && spans[span].0 as usize <= k;
            #[inline]
            fn keep(out: &mut GlowRamps, ends: &[u32], r: &GlowRamp) {
                out.items.push(GlowRamp {
                    ink: out.ends.len() as u32,
                    ..*r
                });
                out.ends.extend_from_slice(r.kept_ends(ends));
            }
            if (rail && feet_whole && spans[span].2)
                || Self::in_owned_span(&owned, k, &mut owned_at)
                || r.w == 0
                || x1 <= bx0
                || x0 >= bx1
                || y1 <= by0
                || y0 >= by1
            {
                // Whole: laid again only once the stream is rebuilt.
                if cut {
                    keep(&mut out, &ramps.ends, &r);
                }
                k += 1;
                continue;
            }
            if !cut {
                let mut bi = 0;
                while bi < k {
                    keep(&mut out, &ramps.ends, &ramps.items[bi]);
                    bi += 1;
                }
            }
            cut = true;
            // The run of rows being gathered: its first row, its slices'
            // knots, and each row's ends per slice, row-major.
            let mut run_y = y0;
            let mut run_ends = std::mem::take(&mut self.run_ends);
            run_ends.clear();
            let mut run_knots = std::mem::take(&mut self.run_knots);
            run_knots.clear();
            run_knots.extend([x0, x1]);
            let inv_w = 1.0 / f32::from(r.w);
            let held = (
                y0 + i32::from(r.join),
                y0 + i32::from(r.join) + i32::from(r.joined) + 1,
            );
            let mut held_knotted = false;
            // The gathered rows' one light, if they hold one (`lay_run`).
            let mut run_held = None;
            let mut y = y0;
            while y < y1 {
                let (a, b) = r.row_ends(&ramps.ends, (y - y0) as u16);
                let in_held = r.joined > 0 && y >= held.0 && y < held.1;
                if in_held && y == held.0 {
                    held_knotted = a | b != 0
                        && held.0 < by1
                        && held.1 > by0
                        && (!rail || self.meets_dim_box((x0, held.0, x1, held.1), feather.0))
                        && self.knots((x0, held.0, x1, held.1));
                }
                let knotted = if in_held {
                    held_knotted
                } else {
                    a | b != 0
                        && y >= by0
                        && y < by1
                        && (!rail || self.meets_dim_box((x0, y, x1, y + 1), feather.0))
                        && self.knots((x0, y, x1, y + 1))
                };
                #[inline]
                const fn knot_after(knots: &[i32], v: i32) -> usize {
                    let mut l = 0usize;
                    let mut r = knots.len();
                    while l < r {
                        let mid = (l + r) >> 1;
                        if knots[mid] <= v {
                            l = mid + 1;
                        } else {
                            r = mid;
                        }
                    }
                    l
                }
                // The held rows' light from `y`: the whole run uncut, or the
                // knot interval `y` begins.
                let interval = if in_held && knotted {
                    let kp = knot_after(&self.knots_y, y);
                    Some((self.knots_y[kp - 1], self.knots_y[kp]))
                } else {
                    None
                };
                let opens = match interval {
                    Some((ya, yb)) => {
                        if y == ya {
                            Some((ya, (yb - ya - 1) as u16))
                        } else {
                            None
                        }
                    }
                    None => {
                        if in_held && y == held.0 {
                            Some((held.0, r.joined))
                        } else {
                            None
                        }
                    }
                };
                let same = if knotted {
                    self.knots_x == run_knots
                } else {
                    a | b == 0 || run_knots == [x0, x1]
                };
                if !same || (interval.is_some() && opens.is_some() && y > run_y) {
                    lay_run(&mut out, &r, run_y, &run_knots, &run_ends, run_held);
                    run_y = y;
                    run_held = None;
                    run_ends.clear();
                    run_knots.clear();
                    if knotted {
                        run_knots.extend_from_slice(&self.knots_x);
                    } else {
                        run_knots.extend([x0, x1]);
                    }
                }
                if opens.is_some() {
                    run_held = opens;
                }
                if !knotted {
                    if run_knots.len() == 2 {
                        run_ends.extend([a, b]);
                    } else {
                        run_ends.resize(run_ends.len() + 2 * (run_knots.len() - 1), 0);
                    }
                    y += 1;
                    continue;
                }
                if interval.is_some() && opens.is_none() {
                    // A held row past its interval's first: the row above's
                    // ends, its pieces cut at the same `ym`.
                    let n = run_ends.len();
                    run_ends.extend_from_within(n - 2 * (run_knots.len() - 1)..n);
                    y += 1;
                    continue;
                }
                #[inline]
                const fn at_ch(c: u32, sh: u32) -> f32 {
                    ((c >> sh) & 0xff) as f32
                }
                // A cut piece truncated as `guard_mask_quads` truncates it.
                let c0 = [at_ch(a, 16), at_ch(a, 8), at_ch(a, 0), at_ch(a, 24)];
                let dc = [
                    at_ch(b, 16) - c0[0],
                    at_ch(b, 8) - c0[1],
                    at_ch(b, 0) - c0[2],
                    at_ch(b, 24) - c0[3],
                ];
                let ym = if in_held {
                    let kp = knot_after(&self.knots_y, y);
                    0.5 * (self.knots_y[kp - 1] + self.knots_y[kp]) as f32
                } else {
                    0.5 * (2 * y + 1) as f32
                };
                // Within every near dim's rows the mask reads each knot
                // alike row after row: the last such row's reading holds.
                let near_slice = self.near.as_slice();
                let mut flat = true;
                let mut ni = 0;
                while ni < near_slice.len() {
                    let d = &near_slice[ni];
                    if !(d.y0 <= ym && ym <= d.y1) {
                        flat = false;
                        break;
                    }
                    ni += 1;
                }
                if !(flat && masks_for == Some(self.near_gen) && masks.len() == run_knots.len()) {
                    masks.clear();
                    let mut ki = 0;
                    while ki < run_knots.len() {
                        masks.push(self.mask_at_with(run_knots[ki] as f32, ym, feather));
                        ki += 1;
                    }
                    masks_for = if flat { Some(self.near_gen) } else { None };
                }
                let (rgb0, al0) =
                    mask_cut_end(run_knots[0], x0, inv_w, c0, dc, masks[0], round_cuts);
                let mut ca = glow_end(rgb0, al0);
                let mut xi = 1;
                while xi < run_knots.len() {
                    let xb = run_knots[xi];
                    let (rgbb, alb) = mask_cut_end(xb, x0, inv_w, c0, dc, masks[xi], round_cuts);
                    let cb = glow_end(rgbb, alb);
                    // A piece of no light is never laid: a dark row (two
                    // zero ends already).
                    run_ends.extend([ca, cb]);
                    ca = cb;
                    xi += 1;
                }
                y += 1;
            }
            lay_run(&mut out, &r, run_y, &run_knots, &run_ends, run_held);
            self.run_ends = run_ends;
            self.run_knots = run_knots;
            k += 1;
        }
        self.row_near_on = false;
        self.row_near = None;
        self.row_masks = masks;
        if cut {
            out.lit = out.count_lit();
            std::mem::swap(ramps, &mut out);
        }
        out.clear();
        self.ramp_pieces = out;
        if rails {
            // The ramps' indices moved with the cuts: the spans are spent.
            self.rail_ramps = spans;
            self.rail_ramps.clear();
        }
        if rails {
            // Kept whole, the light-owned lights moved with the cuts too.
            self.light_owned_ramps = owned;
            self.light_owned_ramps.clear();
        }
    }

    /// Cut every halo of `halos` to the guard's dims: an additive halo's
    /// colour, a source-over halo's opacity ceiling.
    #[inline]
    fn guard_mask_halos(&mut self, halos: &mut Vec<RainHalo>) {
        let (bx0, by0, bx1, by1) = self.dims_bounds();
        let feather = self.feather();
        let mut pieces = std::mem::take(&mut self.halo_pieces);
        pieces.clear();
        let mut cut = false;
        let mut hi = 0;
        while hi < halos.len() {
            let h = halos[hi];
            let (x0, y0) = (i32::from(h.x), i32::from(h.y));
            let (x1, y1) = (x0 + i32::from(h.w), y0 + i32::from(h.h));
            if h.w == 0
                || h.h == 0
                || x1 <= bx0
                || x0 >= bx1
                || y1 <= by0
                || y0 >= by1
                || !self.knots((x0, y0, x1, y1))
            {
                pieces.push(h);
                hi += 1;
                continue;
            }
            cut = true;
            let ny = self.knots_y.len() - 1;
            let nx = self.knots_x.len() - 1;
            let x0_knot = self.knots_x[0];
            let mut ya = self.knots_y[0];
            let mut yi = 0;
            while yi < ny {
                yi += 1;
                let yb = self.knots_y[yi];
                let ym = 0.5 * (ya + yb) as f32;
                let ya_u = ya as u16;
                let h_u = (yb - ya) as u16;
                ya = yb;
                let mut xa = x0_knot;
                let mut xi = 0;
                while xi < nx {
                    xi += 1;
                    let xb = self.knots_x[xi];
                    let m = self.mask_at_with(0.5 * (xa + xb) as f32, ym, feather);
                    let color = match h.mode {
                        HaloMode::Add => scale_rgb(h.color & 0x00FF_FFFF, m),
                        HaloMode::Over => match over_cap(h.color, m) {
                            Some(cap) if m < 1.0 => {
                                (h.color & 0x00FF_FFFF) | (u32::from(cap) << 24)
                            }
                            Some(_) => h.color,
                            None => {
                                xa = xb;
                                continue;
                            }
                        },
                    };
                    if h.mode == HaloMode::Add && color == 0 {
                        xa = xb;
                        continue;
                    }
                    pieces.push(RainHalo {
                        x: xa as u16,
                        y: ya_u,
                        w: (xb - xa) as u16,
                        h: h_u,
                        color,
                        ..h
                    });
                    xa = xb;
                }
            }
            hi += 1;
        }
        if cut {
            std::mem::swap(halos, &mut pieces);
        }
        pieces.clear();
        self.halo_pieces = pieces;
    }
}

impl CursorGlow {
    /// Whether the page whose text is `ink` on `ground` (the theme's
    /// resolved default foreground and background) holds the GPU's bloom off
    /// its frames ([`aterm_core::render::RenderInput::bloom_gate`]): its
    /// letters' solid ink reads under [`LETTER_FLOOR`] on the bare page, so
    /// each keeps only [`OWN_SHARE`] of its own reading under the lights
    /// ([`letter_bar`]), and the bloom blurs the over-text light as far as
    /// [`aterm_render::bloom::reach_px`] (12 px, most of a cell) past every
    /// cut the guard made on a letter, where no cut follows it. Solarized
    /// Dark's newest letter read 85% of its own reading under Comet's bloom
    /// and 93% under the plain caret's typing glow; without the bloom every
    /// letter the gallery measured kept 97%. A page whose text clears the
    /// floor has the room and keeps its bloom.
    #[inline]
    pub(crate) const fn holds_bloom(&mut self, ink: u32, ground: u32) -> bool {
        let key = (ink & 0x00FF_FFFF, ground & 0x00FF_FFFF);
        if let Some((at, held)) = self.letter_guard.bloom_memo
            && at.0 == key.0
            && at.1 == key.1
        {
            return held;
        }
        let table = crate::color_math::relative_luminance_table();
        let lg = crate::color_math::relative_luminance_with(table, key.1);
        let li = crate::color_math::relative_luminance_with(
            table,
            solid_ink_with(table, key.0, key.1, lg),
        );
        let held = (li.max(lg) + 0.05) / (li.min(lg) + 0.05) < LETTER_FLOOR;
        self.letter_guard.bloom_memo = Some((key, held));
        held
    }

    /// Whether the host's post passes (the GPU's bloom and the SDR crown,
    /// [`aterm_core::render::BloomGate::SparesLetters`]) keep their source off
    /// the frame's lettered cells. The guard weighs each letter on the light
    /// the frame composites, and the passes blur and screen the over-text
    /// light as far as [`aterm_render::bloom::reach_px`] past it: on the dark
    /// page Lumen's newest letter, at 6.25:1 on the composited light, read
    /// 4.37 bloomed (looks10 L1; Phaser and Sparkle 4.37 and 4.38 at an Enter
    /// at the bottom row, Beam 4.48 in the gallery). Withheld from beside the
    /// text, the spill lays no light on a letter, and the trail's own light is
    /// drawn whole. Classic's spectrum face on a dark theme keeps its v0.28
    /// look, bloom and all ([`Self::classic_keeps_its_bytes`]).
    #[inline]
    #[must_use]
    pub const fn bloom_spares_letters(&self, style: GlowStyle) -> bool {
        #[cfg(any(test, debug_assertions))]
        if self.letter_guard.bloom_on_letters {
            return false;
        }
        !self.classic_keeps_its_bytes(style)
    }

    /// A test's negative control: the post passes take light from beside the
    /// letters too ([`Self::bloom_spares_letters`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_bloom_spares_letters(&mut self, on: bool) {
        self.letter_guard.bloom_on_letters = !on;
    }

    /// Whether this frame's lights are Classic's spectrum face on a dark
    /// theme, which keeps its ruled v0.28 bytes: neither the letter guard
    /// ([`Self::guard_letters`]) nor the caret-letter punch
    /// ([`Self::keep_light_off_caret_letter`]) touches them. Its one-colour
    /// face, and either face on paper, is guarded.
    #[inline]
    #[must_use]
    pub(crate) const fn classic_keeps_its_bytes(&self, style: GlowStyle) -> bool {
        matches!(style, GlowStyle::Classic) && !self.last_classic_mono && self.last_classic_dark
    }

    /// The letter guard ([`self`]): cut this frame's lights (`over`, the
    /// frame's `cursor_glow_add`, and the glow's own under-text quads and
    /// halos) so every lettered cell of the frame's text but the caret's
    /// keeps its bar ([`letter_bar`]). Classic's spectrum face on a dark theme
    /// keeps its ruled v0.28 bytes and is not guarded; on paper it is, as its
    /// one-colour face is everywhere: the ruling names dark, and the paper
    /// landing ring's square, laid in ink over the letters beside the caret,
    /// read a `(` at 3.19:1 in the trail gallery's `move-up` (looks8 round 2).
    /// Returns a print of the cuts. `caret_block`: the caret is drawn as a
    /// block, filling its cell, so no pixel of it is a neighbour's ground.
    /// `trail`: the frame's cursor trail cells and their colour, each cell's
    /// fill the ground its letters' rings read under the lights.
    ///
    /// A frame whose streams, rails, text, caret and grid are the last
    /// frame's takes the streams the guard left that frame ([`FrameMemo`])
    /// and judges no cell: its work follows what moved.
    #[inline]
    pub(crate) fn guard_letters(
        &mut self,
        over: &mut Vec<GlowQuad>,
        (caret, caret_block): (Option<(u16, u16)>, bool),
        (geom, cfg): (Geom, &GlowConfig),
        trail: Trail<'_>,
    ) -> u64 {
        if self.letter_guard.unseen {
            return 0;
        }
        let style = cfg.style;
        #[cfg(any(test, debug_assertions))]
        let trail = if self.letter_guard.trail_off {
            (&[][..], trail.1)
        } else {
            trail
        };
        #[cfg(any(test, debug_assertions))]
        if self.letter_guard.off {
            return 0;
        }
        #[cfg(any(test, debug_assertions))]
        let (seams, reuse) = {
            let g = &self.letter_guard;
            let l = &self.ink_ledger;
            (
                u16::from(g.no_room)
                    | u16::from(g.comb) << 1
                    | u16::from(l.round_cuts) << 3
                    | u16::from(l.rails_feathered) << 4
                    | u16::from(l.foot_rails_weighed) << 5
                    | u16::from(g.ring_off) << 8
                    | u16::from(g.luma_ring) << 9
                    | u16::from(g.ring.whole_off) << 10
                    | u16::from(g.shades_whole) << 11
                    | u16::from(g.shades_at_bar) << 12,
                !g.no_reuse,
            )
        };
        #[cfg(not(any(test, debug_assertions)))]
        let (seams, reuse) = (0u16, true);
        #[cfg(any(test, debug_assertions))]
        let paper_spared = self.letter_guard.classic_paper_off
            && matches!(style, GlowStyle::Classic)
            && !self.last_classic_mono;
        #[cfg(not(any(test, debug_assertions)))]
        let paper_spared = false;
        let spared = self.classic_keeps_its_bytes(style) || paper_spared;
        #[cfg(any(test, debug_assertions))]
        {
            self.ink_ledger.guard_judged = self.guard_judges(over, geom, spared);
        }
        let frame = (
            caret,
            (
                geom.cw as i32,
                geom.ch as i32,
                i32::from(geom.origin_x),
                i32::from(geom.origin_y),
            ),
            seams | u16::from(caret_block) << 6 | u16::from(spared) << 7,
        );
        // The rails and the page shades are read in order, kept or not.
        self.ink_ledger.rails.sort_unstable();
        self.ink_ledger.rail_ramps.sort_unstable();
        self.ink_ledger.page_shades.sort_unstable();
        self.ink_ledger.page_shade_ramps.sort_unstable();
        let mut memo = self.letter_guard.memo.take().unwrap_or_default();
        let text = &self.frame_text;
        if reuse
            && memo.kept
            && memo.frame == frame
            && memo.seen.0 == self.under_out
            && memo.seen.1 == *over
            && memo.seen.2 == self.halo_out
            && memo.seen.3 == self.under_ramps
            && memo.seen.4 == self.out_ramps
            && memo.rails.0 == self.ink_ledger.rails
            && memo.rails.1 == self.ink_ledger.rail_ramps
            && memo.owned.0 == self.ink_ledger.light_owned
            && memo.owned.1 == self.ink_ledger.light_owned_ramps
            && memo.shades.0 == self.ink_ledger.page_shades
            && memo.shades.1 == self.ink_ledger.page_shade_ramps
            && memo.text == (text.lo, text.cols, text.col0)
            && memo.text_rows == text.rows
            && memo.text_pairs == text.pairs
            && memo.text_glyphs == text.glyph
            && memo.text_plain == text.plain
            && memo.trail.0 == trail.0
            && memo.trail.1 == trail.1
            && same_ink(memo.ink.as_ref(), self.glyph_ink.as_ref())
        {
            self.under_out.clone_from(&memo.made.0);
            over.clone_from(&memo.made.1);
            self.halo_out.clone_from(&memo.made.2);
            copy_ramps(&mut self.under_ramps, &memo.made.3);
            copy_ramps(&mut self.out_ramps, &memo.made.4);
            let print = memo.print;
            self.letter_guard.memo = Some(memo);
            #[cfg(any(test, debug_assertions))]
            {
                self.letter_guard.kept += 1;
            }
            return print;
        }
        memo.frame = frame;
        memo.text = (text.lo, text.cols, text.col0);
        memo.text_rows.clone_from(&text.rows);
        memo.text_pairs.clone_from(&text.pairs);
        memo.text_glyphs.clone_from(&text.glyph);
        memo.text_plain.clone_from(&text.plain);
        memo.trail.0.clear();
        memo.trail.0.extend_from_slice(trail.0);
        memo.trail.1 = trail.1;
        memo.ink.clone_from(&self.glyph_ink);
        memo.seen.0.clone_from(&self.under_out);
        memo.seen.1.clone_from(over);
        memo.seen.2.clone_from(&self.halo_out);
        copy_ramps(&mut memo.seen.3, &self.under_ramps);
        copy_ramps(&mut memo.seen.4, &self.out_ramps);
        memo.rails.0.clone_from(&self.ink_ledger.rails);
        memo.rails.1.clone_from(&self.ink_ledger.rail_ramps);
        memo.owned.0.clone_from(&self.ink_ledger.light_owned);
        memo.owned.1.clone_from(&self.ink_ledger.light_owned_ramps);
        memo.shades.0.clone_from(&self.ink_ledger.page_shades);
        memo.shades.1.clone_from(&self.ink_ledger.page_shade_ramps);
        let print =
            self.guard_letters_inner(over, (caret, caret_block), (geom, cfg), (spared, trail));
        memo.made.0.clone_from(&self.under_out);
        memo.made.1.clone_from(over);
        memo.made.2.clone_from(&self.halo_out);
        copy_ramps(&mut memo.made.3, &self.under_ramps);
        copy_ramps(&mut memo.made.4, &self.out_ramps);
        memo.print = print;
        memo.kept = true;
        self.letter_guard.memo = Some(memo);
        print
    }

    /// Whether [`Self::guard_letters`] judges this frame's lights against its
    /// letters: the style does not pass it by (`spared`), the frame holds
    /// text and some light, and the grid has cells.
    #[inline]
    fn guard_judges(&self, over: &[GlowQuad], geom: Geom, spared: bool) -> bool {
        !spared
            && !self.frame_text.rows.is_empty()
            && !(over.is_empty()
                && self.under_out.is_empty()
                && self.halo_out.is_empty()
                && self.under_ramps.is_empty()
                && self.out_ramps.is_empty())
            && geom.cw as i32 > 0
            && geom.ch as i32 > 0
    }

    /// Whether the last cursor step's letter guard judged its lights against
    /// its letters ([`InkLedger::guard_judged`]). Debug and test builds only.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn guard_judged(&self) -> bool {
        self.ink_ledger.guard_judged
    }

    /// [`Self::guard_letters`] on a frame it has not kept; `spared` when
    /// the style's spectrum face passes the guard by
    /// ([`Self::classic_keeps_its_bytes`]).
    ///
    /// Each cell is weighed with its lights scaled as one ([`LetterGuard::judge`]), and the
    /// cuts are laid ([`Self::lay_cuts`]). A cell read by its glyph's own pixels
    /// ([`ring`]) is then read again on the lights as the frame will draw them: the cut
    /// streams, a neighbour's feather and the night grounds laid after the guard
    /// ([`Self::ground_the_frame`]) with them. One that reads under its bar there takes a
    /// lesser share, and the cuts are laid again from the streams as they came, until every
    /// such cell keeps its bar or keeps none of its light already, or [`VERIFY_PASSES`] have
    /// run, when it keeps none.
    #[inline]
    fn guard_letters_inner(
        &mut self,
        over: &mut Vec<GlowQuad>,
        (caret, caret_block): (Option<(u16, u16)>, bool),
        (geom, cfg): (Geom, &GlowConfig),
        (spared, trail): (bool, Trail<'_>),
    ) -> u64 {
        if !self.guard_judges(over, geom, spared) {
            return 0;
        }
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        let mut guard = std::mem::take(&mut self.letter_guard);
        self.ink_ledger.grid = (ox as f32, oy as f32, cw as f32, ch as f32);
        self.ink_ledger.dims.clear();
        let bounds = StreamBounds::of(
            (&self.under_out, &self.under_ramps),
            (over, &self.out_ramps),
            &self.halo_out,
        );
        if bounds.all.0 >= bounds.all.1 {
            self.letter_guard = guard;
            return 0;
        }
        self.hold_page_shades(&mut guard, &bounds, (caret, caret_block), trail);
        let mut ramp_rows = std::mem::take(&mut guard.ramp_rows);
        let mut rows_cut = std::mem::take(&mut guard.rows_cut);
        let mut cuts = std::mem::take(&mut guard.row_cuts);
        let mut checks = std::mem::take(&mut guard.checks);
        rows_cut.clear();
        cuts.clear();
        checks.clear();
        {
            let ledger = &self.ink_ledger;
            let text = &self.frame_text;
            let (under, halos) = (self.under_out.as_slice(), self.halo_out.as_slice());
            let over_q = over.as_slice();
            let mut solid = None;
            // What the face lays of each letter's core ([`CursorGlow::observe_glyph_ink`]), by
            // the cell's glyph and colours: `None` where it lays the colour whole (or no host
            // said).
            let inks = self.glyph_ink.as_deref();
            let mut cover_memo: Option<((u8, u32, u32), Option<Cover>)> = None;
            let mut core_of = |glyph: u8, ink: u32, ground: u32| -> Option<Cover> {
                let inks = inks?;
                if let Some((key, c)) = cover_memo
                    && key == (glyph, ink, ground)
                {
                    return c;
                }
                let c = inks
                    .core(
                        glyph,
                        aterm_render::linear_luminance(ink),
                        aterm_render::linear_luminance(ground),
                    )
                    .filter(|&c| c < CORE_SOLID)
                    .map(|c| Cover {
                        c,
                        seat: inks.cell_box(glyph),
                    });
                cover_memo = Some(((glyph, ink, ground), c));
                c
            };
            let mut i = 0;
            while i < text.rows.len() {
                let Some((first, end)) = text.rows[i] else {
                    i += 1;
                    continue;
                };
                let row = text.lo + i as i32;
                let (ry0, ry1) = (oy + row * ch - RING, oy + (row + 1) * ch + RING);
                if ry1 <= bounds.all.0 || ry0 >= bounds.all.1 {
                    i += 1;
                    continue;
                }
                let row_bounds = collect_row(
                    &mut guard.row,
                    &mut ramp_rows,
                    ((under, &self.under_ramps), (over_q, &self.out_ramps), halos),
                    &bounds,
                    (ry0, ry1),
                    Some(ledger),
                );
                if guard.row.is_empty() {
                    i += 1;
                    continue;
                }
                let (min_x, max_x) = row_bounds;
                let skip = match caret {
                    Some((r, c)) if i32::from(r) == row => Some(i32::from(c)),
                    _ => None,
                };
                let from = cuts.len();
                let mut col = first;
                while col < end {
                    let (cx0, cx1) = (ox + col * cw - RING, ox + (col + 1) * cw + RING);
                    if max_x <= cx0 {
                        break;
                    }
                    if min_x >= cx1 || skip == Some(col) {
                        col += 1;
                        continue;
                    }
                    let Some((Some(ink), ground)) = text.pair(row, col) else {
                        col += 1;
                        continue;
                    };
                    let (ink, ground) = (ink & 0x00FF_FFFF, ground & 0x00FF_FFFF);
                    guard.cell_lights(cx0, cx1);
                    if guard.cell.is_empty() {
                        col += 1;
                        continue;
                    }
                    let streams = Streams {
                        quads: [under, &ramp_rows.under, over_q, &ramp_rows.over],
                        halos,
                    };
                    // A lettered cell whose glyph's raster the face's table holds is read by
                    // its own pixels and its neighbours', as the judge reads it ([`ring`]);
                    // else by its solid ink and its face's core.
                    #[cfg(any(test, debug_assertions))]
                    let inks = inks.filter(|_| !guard.ring_off);
                    let ringed = inks.and_then(|inks| {
                        ring_cells(
                            (text, inks),
                            (row, col),
                            ground,
                            (caret, caret_block),
                            trail,
                        )
                        .map(|(model, cells)| (model, cells, inks))
                    });
                    let model = ringed.as_ref().map(|r| r.0);
                    // A cell whose ring box, text and lights are the last
                    // frame's keeps the last frame's weighing.
                    let cover = if model.is_some() {
                        None
                    } else {
                        core_of(text.glyph(row, col), ink, ground)
                    };
                    let key = guard.cell_key(
                        (cx0, ry0, cx1, ry1),
                        (ink, ground),
                        (cover, model.unwrap_or(0), RasterHold::Bar),
                        streams,
                    );
                    let share = if let Some(share) = guard.verdict(key) {
                        share
                    } else {
                        if let Some((_, cells, inks)) = &ringed {
                            guard.ring.read(
                                ((cx1 - cx0) as usize, (ry1 - ry0) as usize),
                                (cw as usize, ch as usize),
                                RING as usize,
                                cells,
                                (inks.raster_table(), inks.corrected()),
                            );
                        }
                        let share = guard.judge(
                            (cx0, ry0, cx1, ry1),
                            (ink, ground),
                            &mut solid,
                            (cover, model.map(|_| RasterHold::Bar)),
                            streams,
                        );
                        guard.fresh.push((key, share));
                        share
                    };
                    if share == NEAR_WHOLE {
                        guard.near_whole_fresh.push(box_at(cx0, ry0));
                    }
                    if model.is_some() {
                        checks.push(Check {
                            row: rows_cut.len(),
                            col,
                            ink,
                            ground,
                            share,
                            key,
                        });
                    }
                    if share < 1.0 {
                        cuts.push((col, share));
                    }
                    col += 1;
                }
                rows_cut.push(RowCut {
                    row,
                    band: (ry0, ry1),
                    skip,
                    cuts: (from, cuts.len()),
                });
                i += 1;
            }
        }
        guard.ramp_rows = ramp_rows;
        // The streams as they came, to lay the cuts again from where a cell reads under its
        // bar on the frame they make.
        if !checks.is_empty() {
            let seen = &mut guard.seen;
            seen.0.clone_from(&self.under_out);
            seen.1.clone_from(over);
            seen.2.clone_from(&self.halo_out);
            copy_ramps(&mut seen.3, &self.under_ramps);
            copy_ramps(&mut seen.4, &self.out_ramps);
            let (spans, l) = (&mut guard.seen_spans, &self.ink_ledger);
            spans.0.clone_from(&l.rails);
            spans.1.clone_from(&l.rail_ramps);
            spans.2.clone_from(&l.light_owned);
            spans.3.clone_from(&l.light_owned_ramps);
        }
        let mut print = self.lay_cuts(over, &guard, (&rows_cut, &cuts));
        let night = (!checks.is_empty())
            .then(|| self.night_ground(cfg))
            .flatten();
        let mut pass = 0;
        while !checks.is_empty() {
            #[cfg(any(test, debug_assertions))]
            let before = cuts.clone();
            let lowered = self.verify_cuts(
                &mut guard,
                over,
                (&checks, &mut rows_cut),
                &mut cuts,
                (geom, cfg, night),
                ((caret, caret_block), trail),
                pass == VERIFY_PASSES,
            );
            #[cfg(any(test, debug_assertions))]
            {
                guard.verify_passes += 1;
                guard.verify_idle += u64::from(lowered && cuts == before);
            }
            if !lowered {
                break;
            }
            pass += 1;
            self.under_out.clone_from(&guard.seen.0);
            over.clone_from(&guard.seen.1);
            self.halo_out.clone_from(&guard.seen.2);
            copy_ramps(&mut self.under_ramps, &guard.seen.3);
            copy_ramps(&mut self.out_ramps, &guard.seen.4);
            let (spans, l) = (&guard.seen_spans, &mut self.ink_ledger);
            l.rails.clone_from(&spans.0);
            l.rail_ramps.clone_from(&spans.1);
            l.light_owned.clone_from(&spans.2);
            l.light_owned_ramps.clone_from(&spans.3);
            print = self.lay_cuts(over, &guard, (&rows_cut, &cuts));
            if pass > VERIFY_PASSES {
                break;
            }
        }
        guard.settle_verdicts();
        guard.rows_cut = rows_cut;
        guard.row_cuts = cuts;
        guard.checks = checks;
        self.letter_guard = guard;
        print
    }

    /// Hold each letter a page shade reaches ([`InkLedger::note_page_shade_laid`]) to its bar
    /// against the shades alone, before the guard weighs the frame's other lights, which leave the
    /// shades out ([`Weighs::Guard`]). Each lettered cell but the caret's whose ring box a shade
    /// meets and that the guard reads by its glyph's own pixels ([`ring`]) is judged as the guard
    /// judges a cell ([`LetterGuard::judge`]) under the shades' lights there, and each shade's
    /// light is cut in place to the least share of the judged cells whose ring box it meets
    /// ([`InkLedger::cut_page_shades`]). The cuts do not ease along the row as the other lights'
    /// do ([`ramp_row`]): eased, they cut the shade off the held letters' ring boxes too, 2547 px
    /// of Beam's burst on Solarized Dark against 2281 and 4926 of its line past the margin against
    /// 3620 (Linux arm64, 2026-11-07), and the unit law
    /// `a_letter_under_its_raster_bar_keeps_a_shade_that_does_not_lower_it` fails on its edge rows.
    /// When the hold judged each letter at its raster bar, easing cut Beam's body from under every
    /// letter between two held ones (`n fox` between the burst's `w` and `j`), to 0.005 off the
    /// page in a cell behind the caret where `trail_letters_read`'s W6 keeps 0.05.
    ///
    /// Each reading of a letter is held to the lower of its raster bar and its own reading on the
    /// bare page ([`shade_bar`], [`RasterHold::Own`]): a letter under its raster bar there keeps
    /// every shade under which it reads its own reading or more, and the hold cuts only a shade
    /// that lowers it.
    ///
    /// Weighed with the other lights instead, a shade was cut with the caret's halo under the last
    /// word, Beam's body a navy box stopping a word short of the caret
    /// (`bed::BedScratch::shade_runs`); kept whole, it lowered a letter at AA under AA (the
    /// ledger's note). A cell read by its solid ink keeps the shade whole: that reading bounds each
    /// channel's median by the least and the greatest ground round the letter apart, so on a ring
    /// that holds the shade and the page it reads the brighter of each channel, and judged so,
    /// Beam's body was cut from under the CJK letters behind the caret (0.006 off the page there,
    /// where `trail_letters_read`'s W6 keeps 0.05; Linux arm64, 2026-11-07).
    #[inline]
    fn hold_page_shades(
        &mut self,
        guard: &mut LetterGuard,
        bounds: &StreamBounds,
        (caret, caret_block): CaretCell,
        trail: Trail<'_>,
    ) {
        #[cfg(any(test, debug_assertions))]
        let (whole, ring_off, at_bar) = (guard.shades_whole, guard.ring_off, guard.shades_at_bar);
        #[cfg(not(any(test, debug_assertions)))]
        let (whole, ring_off, at_bar) = (false, false, false);
        let hold = if at_bar {
            RasterHold::Bar
        } else {
            RasterHold::Own
        };
        let ledger = &self.ink_ledger;
        // With the foot rails weighed (a test's control), the guard weighs the shades with them.
        if whole || !ledger.has_page_shades() || !ledger.foot_rails_whole() {
            return;
        }
        let (ox, oy) = (ledger.grid.0 as i32, ledger.grid.1 as i32);
        let (cw, ch) = (ledger.grid.2 as i32, ledger.grid.3 as i32);
        let text = &self.frame_text;
        let under = (self.under_out.as_slice(), &self.under_ramps);
        let ring_inks = self.glyph_ink.as_deref().filter(|_| !ring_off);
        let mut ramp_rows = std::mem::take(&mut guard.ramp_rows);
        let mut dims = std::mem::take(&mut guard.shade_dims);
        dims.clear();
        let mut solid = None;
        for (i, extent) in text.rows.iter().enumerate() {
            let Some((first, end)) = *extent else {
                continue;
            };
            let row = text.lo + i as i32;
            let (ry0, ry1) = (oy + row * ch - RING, oy + (row + 1) * ch + RING);
            if ry1 <= bounds.under.0 || ry0 >= bounds.under.1 {
                continue;
            }
            guard.row.clear();
            ramp_rows.under.clear();
            let mut row_bounds = (i32::MAX, i32::MIN);
            row_lights(
                under,
                (Stream::Under, Stream::UnderRow),
                (ry0, ry1),
                &mut ramp_rows.under,
                (&mut guard.row, &mut row_bounds),
                Weighs::Shades(ledger),
            );
            if guard.row.is_empty() {
                continue;
            }
            let skip = match caret {
                Some((r, c)) if i32::from(r) == row => Some(i32::from(c)),
                _ => None,
            };
            for col in first..end {
                let (cx0, cx1) = (ox + col * cw - RING, ox + (col + 1) * cw + RING);
                if row_bounds.1 <= cx0 {
                    break;
                }
                if row_bounds.0 >= cx1 || skip == Some(col) {
                    continue;
                }
                let Some((Some(ink), ground)) = text.pair(row, col) else {
                    continue;
                };
                let (ink, ground) = (ink & 0x00FF_FFFF, ground & 0x00FF_FFFF);
                guard.cell_lights(cx0, cx1);
                if guard.cell.is_empty() {
                    continue;
                }
                let streams = Streams {
                    quads: [under.0, &ramp_rows.under, &[], &[]],
                    halos: &[],
                };
                let Some((model, cells, inks)) = ring_inks.and_then(|inks| {
                    ring_cells(
                        (text, inks),
                        (row, col),
                        ground,
                        (caret, caret_block),
                        trail,
                    )
                    .map(|(model, cells)| (model, cells, inks))
                }) else {
                    continue;
                };
                let key = guard.cell_key(
                    (cx0, ry0, cx1, ry1),
                    (ink, ground),
                    (None, model, hold),
                    streams,
                );
                let share = if let Some(share) = guard.verdict(key) {
                    share
                } else {
                    guard.ring.read(
                        ((cx1 - cx0) as usize, (ry1 - ry0) as usize),
                        (cw as usize, ch as usize),
                        RING as usize,
                        &cells,
                        (inks.raster_table(), inks.corrected()),
                    );
                    let share = guard.judge(
                        (cx0, ry0, cx1, ry1),
                        (ink, ground),
                        &mut solid,
                        (None, Some(hold)),
                        streams,
                    );
                    guard.fresh.push((key, share));
                    share
                };
                if share < 1.0 {
                    dims.push(Dim {
                        x0: cx0 as f32,
                        y0: ry0 as f32,
                        x1: cx1 as f32,
                        y1: ry1 as f32,
                        share,
                    });
                }
            }
        }
        guard.ramp_rows = ramp_rows;
        if !dims.is_empty() {
            self.ink_ledger
                .cut_page_shades(&dims, &mut self.under_out, &mut self.under_ramps);
        }
        guard.shade_dims = dims;
    }

    /// Lay the rows' cuts (`rows`, each its cells' `(column, share)` in `cuts`) as the ledger's
    /// dims, each row's eased as one ramp ([`ramp_row`]), and cut the frame's streams to them
    /// ([`InkLedger::guard_mask_stream`]). Returns a print of the cuts.
    #[inline]
    fn lay_cuts(
        &mut self,
        over: &mut Vec<GlowQuad>,
        guard: &LetterGuard,
        (rows, cuts): (&[RowCut], &[(i32, f32)]),
    ) -> u64 {
        let (ox, cw) = (self.ink_ledger.grid.0 as i32, self.ink_ledger.grid.2 as i32);
        #[cfg(any(test, debug_assertions))]
        let comb = guard.comb;
        #[cfg(not(any(test, debug_assertions)))]
        let comb = {
            let _ = guard;
            false
        };
        let text = &self.frame_text;
        let ledger = &mut self.ink_ledger;
        ledger.dims.clear();
        let mut print = 0u64;
        for r in rows {
            let row_cuts = &cuts[r.cuts.0..r.cuts.1];
            let (row, (ry0, ry1)) = (r.row, r.band);
            let row_from = ledger.dims.len();
            for &(col, share) in row_cuts {
                ledger.dims.push(Dim {
                    x0: (ox + col * cw - RING) as f32,
                    y0: ry0 as f32,
                    x1: (ox + (col + 1) * cw + RING) as f32,
                    y1: ry1 as f32,
                    share,
                });
                print = aterm_hash::fnv_mix_u64(
                    print | 1,
                    ((row as u64) << 48) ^ ((col as u64) << 32) ^ u64::from(share.to_bits()),
                );
            }
            if comb || row_cuts.is_empty() {
                continue;
            }
            ramp_row(
                row_cuts,
                r.skip,
                false,
                |col| matches!(text.pair(row, col), Some((Some(_), _))),
                |col, share| {
                    match find_cut(row_cuts, col) {
                        Some(idx) => ledger.dims[row_from + idx].share = share,
                        None => ledger.dims.push(Dim {
                            x0: (ox + col * cw - RING) as f32,
                            y0: ry0 as f32,
                            x1: (ox + (col + 1) * cw + RING) as f32,
                            y1: ry1 as f32,
                            share,
                        }),
                    }
                    print = aterm_hash::fnv_mix_u64(
                        print | 1,
                        ((row as u64) << 48)
                            ^ ((col as u64) << 32)
                            ^ u64::from(share.to_bits())
                            ^ 1,
                    );
                },
            );
        }
        if !ledger.dims.is_empty() {
            ledger.guard_mask_stream(&mut self.under_out, &mut self.under_ramps, true);
            ledger.guard_mask_stream(over, &mut self.out_ramps, false);
            ledger.guard_mask_halos(&mut self.halo_out);
        }
        ledger.dims.clear();
        print
    }

    /// Read every cell of `checks` (each read by its glyph's own pixels) on the frame the cut
    /// streams make, as [`Self::ground_the_frame`] will publish them: the flames' grounds
    /// and the night grounds (`night`) of the cut lights first in the under stream, every
    /// light the guard does not weigh (a foot rail, the light-owned glass) with them. A cell
    /// under its bar there takes a lesser share in `cuts` (`last`: none of its light).
    /// Returns whether any share fell.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn verify_cuts(
        &mut self,
        guard: &mut LetterGuard,
        over: &[GlowQuad],
        (checks, rows): (&[Check], &mut [RowCut]),
        cuts: &mut Vec<(i32, f32)>,
        (geom, cfg, night): (Geom, &GlowConfig, Option<NightGround>),
        (caret, trail): (CaretCell, Trail<'_>),
        last: bool,
    ) -> bool {
        // The under stream as the frame publishes it.
        let mut under = std::mem::take(&mut guard.verify_under);
        let mut under_ramps = std::mem::take(&mut guard.verify_ramps);
        under.clear();
        under.extend_from_slice(&self.flame_night);
        if let Some(ng) = night {
            self.lay_frame_night_grounds(cfg, geom, over, ng, &mut under);
        }
        let moved = under.len().min(u32::MAX as usize) as u32;
        copy_ramps(&mut under_ramps, &self.under_ramps);
        for r in &mut under_ramps.items {
            r.at = r.at.saturating_add(moved);
        }
        under.extend_from_slice(&self.under_out);
        under_ramps.truncate_laid(&mut under, Self::MAX_QUADS);
        let bounds = StreamBounds::of(
            (&under, &under_ramps),
            (over, &self.out_ramps),
            &self.halo_out,
        );
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        let ox = i32::from(geom.origin_x);
        let text = &self.frame_text;
        let mut ramp_rows = std::mem::take(&mut guard.ramp_rows);
        let mut lowered = false;
        let mut at_row = usize::MAX;
        let mut row_bounds = (i32::MAX, i32::MIN);
        for c in checks {
            let r = rows[c.row];
            if at_row != c.row {
                at_row = c.row;
                row_bounds = collect_row(
                    &mut guard.row,
                    &mut ramp_rows,
                    (
                        (&under, &under_ramps),
                        (over, &self.out_ramps),
                        &self.halo_out,
                    ),
                    &bounds,
                    r.band,
                    None,
                );
            }
            let (cx0, cx1) = (ox + c.col * cw - RING, ox + (c.col + 1) * cw + RING);
            if row_bounds.1 <= cx0 || row_bounds.0 >= cx1 {
                continue;
            }
            guard.cell_lights(cx0, cx1);
            if guard.cell.is_empty() {
                continue;
            }
            let Some(inks) = self.glyph_ink.as_deref() else {
                continue;
            };
            let Some((model, cells)) =
                ring_cells((text, inks), (r.row, c.col), c.ground, caret, trail)
            else {
                continue;
            };
            let (ry0, ry1) = r.band;
            let streams = Streams {
                quads: [&under, &ramp_rows.under, over, &ramp_rows.over],
                halos: &self.halo_out,
            };
            // A cell its weighing left whole whose lights the frame draws as they were weighed
            // keeps the reading it was weighed at.
            if c.share >= 1.0
                && guard.cell_key(
                    (cx0, ry0, cx1, ry1),
                    (c.ink, c.ground),
                    (None, model, RasterHold::Bar),
                    streams,
                ) == c.key
            {
                continue;
            }
            guard.ring.read(
                ((cx1 - cx0) as usize, (ry1 - ry0) as usize),
                (cw as usize, ch as usize),
                RING as usize,
                &cells,
                (inks.raster_table(), inks.corrected()),
            );
            let bar = raster_bar(guard.ring.own);
            let j = Judge::ring((cx0, ry0, cx1, ry1), (c.ink, c.ground));
            let has_over = guard.cell.iter().any(|&(s, _)| !s.on_ground());
            guard.lay_lights(j, streams, has_over);
            if guard.holds_ring(j, 1.0, bar, has_over) {
                continue;
            }
            // Under its bar on the frame: a lesser share.
            let row_cuts = &cuts[r.cuts.0..r.cuts.1];
            let (share, at) = match find_cut(row_cuts, c.col) {
                Some(k) => (row_cuts[k].1, Some(r.cuts.0 + k)),
                None => (1.0, None),
            };
            let less = if last || share <= VERIFY_FLOOR {
                0.0
            } else {
                share * VERIFY_STEP
            };
            // A cell cut to none already has no light left to give: lowered again it lays the
            // same cuts, and another pass reads it under its bar as this one did.
            if less >= share {
                continue;
            }
            match at {
                Some(k) => cuts[k].1 = less,
                // A cell its lights left uncut takes its place in its row's cuts, and the
                // rows after it move along.
                None => {
                    let k = r.cuts.0 + row_cuts.partition_point(|&(col, _)| col < c.col);
                    cuts.insert(k, (c.col, less));
                    rows[c.row].cuts.1 += 1;
                    for later in &mut rows[c.row + 1..] {
                        later.cuts.0 += 1;
                        later.cuts.1 += 1;
                    }
                }
            }
            lowered = true;
        }
        guard.ramp_rows = ramp_rows;
        guard.verify_under = under;
        guard.verify_ramps = under_ramps;
        lowered
    }

    /// A test's negative control: every letter read by its solid ink (`false`,
    /// [`LetterGuard::holds`]), none by its glyph's own pixels ([`ring`]); with
    /// [`Self::set_letter_guard_channels`] off too, against its grounds' least and greatest
    /// luminance, the guard before round 27.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_ring(&mut self, on: bool) {
        self.letter_guard.ring_off = !on;
    }

    /// A test's negative control: each page shade kept whole (`false`), unjudged
    /// ([`Self::hold_page_shades`]), as before the Linux reds of 2026-11-07.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_shades(&mut self, on: bool) {
        self.letter_guard.shades_whole = !on;
    }

    /// A test's negative control: each letter under a page shade held to its raster bar
    /// (`false`), not the lower of that and its own reading ([`Self::hold_page_shades`]), as the
    /// hold's second pass held it (2026-11-07).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_shades_to_own(&mut self, on: bool) {
        self.letter_guard.shades_at_bar = !on;
    }

    /// A test's negative control: each letter kept by its ring alone (`false`), its whole reading
    /// ([`ring::RingCell::whole`]) keeping no bar.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_whole(&mut self, on: bool) {
        self.letter_guard.ring.whole_off = !on;
    }

    /// A test's negative control: the guard blind to the cursor trail's fill (`false`), each
    /// letter's ring read on its page's ground under the lights.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_trail(&mut self, on: bool) {
        self.letter_guard.trail_off = !on;
    }

    /// A test's negative control: a letter read by its solid ink keeps its bare share
    /// ([`OWN_SHARE`]), the guard keeping no room over it (`false`, [`OWN_ROOM`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_room(&mut self, on: bool) {
        self.letter_guard.no_room = !on;
    }

    /// A test's negative control: a letter read by its solid ink is read against the least and
    /// greatest luminance of the grounds round each pixel (`false`), not each channel's least and
    /// greatest, which bound the judge's ring medians ([`LetterGuard::holds`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_channels(&mut self, on: bool) {
        self.letter_guard.luma_ring = !on;
    }

    /// Whether the coming frames only carry the engines on to a later one, drawn nowhere (a
    /// Settings card replays its scene up to the frame it shows), so the guard spends nothing on
    /// them. What it cuts is the frame's own, and what it remembers is keyed on the exact lights
    /// and text it read: the frame shown is the one a replay guarding every step gives
    /// (`aterm-gui`'s `a_card_is_the_frame_it_would_be_with_every_step_before_it_guarded` and
    /// `a_card_in_the_host_s_face_carries_nothing_over_from_a_step_it_guarded`).
    #[inline]
    pub const fn set_frame_unseen(&mut self, unseen: bool) {
        self.letter_guard.unseen = unseen;
    }

    /// A test's negative control: lights drawn without the letter guard.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard(&mut self, on: bool) {
        self.letter_guard.off = !on;
    }

    /// A test's negative control: the guard cuts each cell alone, with no
    /// ramp along the row ([`RAMP_SLOPE`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_ramp(&mut self, on: bool) {
        self.letter_guard.comb = !on;
    }

    /// Pass the one-pixel slabs `quads[from..]` (the dark landing ring's rim,
    /// as [`super::super::raster::push_ripple_ring`] lays it) under every
    /// lettered cell of the frame's text and the judge's [`RING`] round it,
    /// keeping `share` of their light there ([`RIM_UNDER_LETTERS`]) and the
    /// whole of it elsewhere, so the arc runs on through the text as if the
    /// letters stood over it. The letters keep their room, which the guard
    /// would otherwise spend on the whole rim down to its floor and the
    /// host's bloom of the trail's own light then take (Laser's `q` at
    /// 4.20:1, looks8 round 2). A `share` of 0 cuts the rim off the letters.
    /// A slab on a row the frame did not read, or one not flat, is kept whole.
    /// Returns the share of the rim's laid pixels that keep their whole light
    /// (1 for a rim that meets no letter, or none at all).
    #[inline]
    pub(crate) fn pass_under_letters(
        &self,
        quads: &mut Vec<GlowQuad>,
        from: usize,
        geom: Geom,
        share: f32,
    ) -> f32 {
        let text = &self.frame_text;
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        if text.rows.is_empty() || cw <= 0 || ch <= 0 || from >= quads.len() {
            return 1.0;
        }
        let (mut whole_px, mut laid_px) = (0i64, 0i64);
        let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        let end = quads.len();
        let mut i = from;
        while i < end {
            let q = quads[i];
            i += 1;
            if q.w == 0 || q.h == 0 || !q.is_flat() {
                let px = i64::from(q.w) * i64::from(q.h);
                (whole_px, laid_px) = (whole_px + px, laid_px + px);
                continue;
            }
            let (x0, x1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
            let (y0, y1) = (i32::from(q.y), i32::from(q.y) + i32::from(q.h));
            let r_lo = (y0 - RING - oy).div_euclid(ch);
            let r_hi = (y1 - 1 + RING - oy).div_euclid(ch);
            let c_hi = (x1 - 1 + RING - ox).div_euclid(cw);
            let mut c = (x0 - RING - ox).div_euclid(cw);
            let mut start = x0;
            let mut first = true;
            #[inline]
            fn lay_rim_piece(
                quads: &mut Vec<GlowQuad>,
                slot: usize,
                first: &mut bool,
                q: GlowQuad,
                (a, b): (i32, i32),
                keep: f32,
            ) {
                let mut piece = q;
                piece.x = a as u16;
                piece.w = (b - a) as u16;
                if keep < 1.0 {
                    if piece.alpha == 0 {
                        piece.color = scale_rgb(piece.color, keep);
                    } else {
                        piece.alpha = (f32::from(piece.alpha) * keep) as u8;
                    }
                    (piece.color2, piece.alpha2) = (piece.color, piece.alpha);
                }
                if *first {
                    quads[slot] = piece;
                    *first = false;
                } else {
                    quads.push(piece);
                }
            }
            let slot = i - 1;
            while c <= c_hi && start < x1 {
                let mut lettered = false;
                let mut r = r_lo;
                while r <= r_hi && !lettered {
                    lettered = matches!(text.pair(r, c), Some((Some(_), _)));
                    r += 1;
                }
                if lettered {
                    let cut0 = ox + c * cw - RING;
                    let cut1 = ox + (c + 1) * cw + RING;
                    if cut0 > start {
                        lay_rim_piece(quads, slot, &mut first, q, (start, cut0.min(x1)), 1.0);
                        let px = i64::from(cut0.min(x1) - start) * i64::from(q.h);
                        (whole_px, laid_px) = (whole_px + px, laid_px + px);
                    }
                    let (under0, under1) = (start.max(cut0), cut1.min(x1));
                    if under1 > under0 {
                        laid_px += i64::from(under1 - under0) * i64::from(q.h);
                        if share > 0.0 {
                            lay_rim_piece(quads, slot, &mut first, q, (under0, under1), share);
                        }
                    }
                    start = start.max(cut1);
                }
                c += 1;
            }
            if start < x1 {
                lay_rim_piece(quads, slot, &mut first, q, (start, x1), 1.0);
                let px = i64::from(x1 - start) * i64::from(q.h);
                (whole_px, laid_px) = (whole_px + px, laid_px + px);
            }
            if first {
                quads[slot].w = 0;
            }
        }
        let mut keep = from;
        let mut k = from;
        while k < quads.len() {
            if quads[k].w > 0 {
                quads[keep] = quads[k];
                keep += 1;
            }
            k += 1;
        }
        quads.truncate(keep);
        if laid_px > 0 {
            whole_px as f32 / laid_px as f32
        } else {
            1.0
        }
    }

    /// A test's negative control: the dark landing ring's rim is laid over
    /// the text, where the host's bloom spreads it onto the letters the guard
    /// held to their bar ([`Self::dark_rim_over`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_dark_rim_over(&mut self, over: bool) {
        self.dark_rim_over = over;
    }

    /// A test's negative control: the dark landing ring's rim passes under
    /// the letters keeping `share` of its light there, not
    /// [`RIM_UNDER_LETTERS`] ([`Self::pass_under_letters`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_dark_rim_under_letters(&mut self, share: f32) {
        self.dark_rim_share = Some(share);
    }

    /// A test's negative control: the dark landing ring's rim is drawn
    /// however little of it keeps its whole light ([`RIM_SHOWN_SHARE`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_dark_rim_fragments(&mut self, on: bool) {
        self.dark_rim_fragments = on;
    }

    /// A test's negative control: a particle the effects box cuts at its edge
    /// is drawn cut, not left out whole ([`Self::emit_particles`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_motes_cut(&mut self, on: bool) {
        self.motes_cut = on;
    }

    /// A test's negative control: a particle the effects box's edge cut is
    /// judged afresh each frame, not left out for the rest of its life
    /// ([`Self::emit_particles`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_edge_cut_each_frame(&mut self, on: bool) {
        self.edge_cut_each_frame = on;
    }

    /// A test's negative control: the edge's latch is held by each cut
    /// particle's birth and origin, as it was, not by the particle, so a
    /// scroll (which moves the origin) lets a cut particle back and a cut
    /// particle holds out every sibling born with it at its origin
    /// ([`Self::emit_particles`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub fn set_edge_cut_by_origin(&mut self, on: bool) {
        self.edge_cut_by_origin = on.then(Vec::new);
    }

    /// A test's negative control: a light that rises past the effects box's
    /// top (the rainbow ribbon's body, Fire's cores and paper tongues) is cut
    /// flat there by the box's clip, not laid shorter inside it
    /// ([`super::super::RISE_CLEAR_PX`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub fn set_rise_cut(&mut self, on: bool) {
        self.rise_cut = on;
        self.v2.ribbon_mut().rise_cut = on;
    }

    /// The highest window y a rising light reaches in `geom`
    /// ([`Geom::rise_ceiling`]; with [`Self::set_rise_cut`], none: the box's
    /// clip cuts it).
    #[inline]
    pub(crate) const fn rise_ceiling(&self, geom: &Geom) -> f32 {
        #[cfg(any(test, debug_assertions))]
        if self.rise_cut {
            return f32::NEG_INFINITY;
        }
        geom.rise_ceiling()
    }

    /// A test's negative control: Classic's spectrum face passes the guard by
    /// on paper too ([`LetterGuard::classic_paper_off`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_classic_paper(&mut self, on: bool) {
        self.letter_guard.classic_paper_off = !on;
    }

    /// A test's negative control: Classic's paper landing ring laid at its
    /// growing size wherever it lands, over the letters, and cut where the
    /// letter floor cuts it, as it was before looks10 L12
    /// ([`crate::classic_wake::ClassicWake::ring_cut`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_classic_paper_ring_cut(&mut self, on: bool) {
        self.classic.ring_cut = on;
    }

    /// A test's negative control: Classic's one-colour tracer up a dim page
    /// lays its shade under the letters at its own share of its light and
    /// weighed and cut with the lights in its cells, as looks10 round 2 laid
    /// it ([`crate::classic_wake::ClassicWake::tracer_up_faint_shade`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_classic_faint_shade(&mut self, on: bool) {
        self.classic.tracer_up_faint_shade = on;
    }

    /// A test's negative control: the dark bed held to its budget alone,
    /// never lit at the page's luminance ([`crate::cursor_glow::page_lit_ink`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_bed_page_lit(&mut self, on: bool) {
        self.bed_page_unlit = !on;
    }

    /// A test's negative control: the guard feathers its cuts into a rail
    /// laid clear of the ring as into any light (`on` false;
    /// [`InkLedger::note_rail`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_rails_clear(&mut self, on: bool) {
        self.ink_ledger.rails_feathered = !on;
    }

    /// A test's negative control: every cell the guard reaches judged
    /// afresh every frame, no verdict reused ([`LetterGuard::verdicts`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_reuse(&mut self, on: bool) {
        self.letter_guard.no_reuse = !on;
    }

    /// A test's counters: the cells the guard has judged afresh, the cells
    /// whose verdict it took back from the frame before, and the frames
    /// whose streams it took back whole ([`Self::guard_letters`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn letter_guard_work(&self) -> (u64, u64, u64) {
        (
            self.letter_guard.judged,
            self.letter_guard.reused,
            self.letter_guard.kept,
        )
    }

    /// A test's counters of the guard's passes over its cut frames ([`Self::guard_letters`]):
    /// the passes it read a cut frame again in, and of them the ones that called for another pass
    /// and changed no share.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    #[must_use]
    pub const fn letter_guard_passes(&self) -> (u64, u64) {
        (
            self.letter_guard.verify_passes,
            self.letter_guard.verify_idle,
        )
    }
}

#[cfg(test)]
#[path = "letter_guard_tests.rs"]
mod tests;
