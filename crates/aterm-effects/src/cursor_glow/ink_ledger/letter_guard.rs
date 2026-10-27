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
//! (the under-text quads on the ground, then the over-text quads and the halos
//! on ground and ink alike), reads each pixel as solid ink ([`SOLID_SHARE`] of
//! the clean ratio) against the worst ground within [`RING`] px, and where the
//! cell falls under its bar finds the largest share of those lights that keeps
//! it. The bar is [`LETTER_FLOOR`], or [`OWN_SHARE`] of the letter's own bare
//! reading where that is under the floor (a clean ratio under about 5.4:1),
//! with the floor's room over it ([`OWN_ROOM`]).
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
use aterm_render::{GlowEnd, GlowRamp, Laid, glow_end};

/// The bar every letter a light lies on keeps (WCAG AA for text, with a
/// hair of room for the judge's rounding).
pub(crate) const LETTER_FLOOR: f32 = 4.6;
/// The share of a letter's clean ratio its solid ink keeps (`aterm-lab
/// trail-contrast`'s `SOLID_SHARE`): the guard judges the dimmest ink the
/// judge counts, not the face's full colour.
const SOLID_SHARE: f32 = 0.85;
/// The share of its own bare reading a letter whose solid ink reads under
/// [`LETTER_FLOOR`] on the bare page keeps under the lights ([`letter_bar`]),
/// so the floor it cannot reach never leaves it unguarded.
const OWN_SHARE: f32 = 0.97;
/// The room the guard keeps over [`OWN_SHARE`] for the judge's rounding, as
/// [`LETTER_FLOOR`] keeps over WCAG AA's 4.5: the guard reads solid ink, the
/// judge each glyph's ring, and at the bare share a dim theme's letter the
/// guard passed read under it by the judge (Solarized Dark, Water typing:
/// `u` at 4.39:1 of its own 4.57, 96%; looks7).
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
type MemoFrame = (Option<(u16, u16)>, (i32, i32, i32, i32), u8);
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
    /// The ring box's ground luminance per pixel, and per pixel the least
    /// and greatest of it within [`RING`] columns.
    lum: Vec<f32>,
    ring_lo: Vec<f32>,
    ring_hi: Vec<f32>,
    /// The ink's luminance per inner ring box pixel, where an over-text
    /// light lies on it.
    ink_lum: Vec<f32>,
    /// [`Self::clear_of`]'s last `(li_lo, li_hi, bar)` bits and its bounds.
    clear_memo: Option<ClearMemo>,
    /// An additive cell's whole light per ring-box pixel: under the ink
    /// (ground only) and over it (ground and ink), per channel.
    lift_under: Vec<[u16; 3]>,
    lift_over: Vec<[u16; 3]>,
    /// The row being read's cuts, `(column, share)`, for its ramp.
    cuts: Vec<(i32, f32)>,
    /// The row being read's ramp lights.
    ramp_rows: RampRows,
    /// [`CursorGlow::holds_bloom`]'s last `(ink, ground)` and its answer.
    bloom_memo: Option<((u32, u32), bool)>,
    /// Each judged cell's verdict (the share of its lights it keeps,
    /// `1.0` uncut) by its [`Self::cell_key`]: the last frame's, sorted,
    /// and this frame's as they are judged. A cell whose ring box, text
    /// and lights are the last frame's takes its verdict back, so a frame
    /// judges only the cells its changed lights reach.
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
    /// A test's counters: the cells judged afresh, the cells whose
    /// verdict was the last frame's, and the frames that took the last
    /// frame's streams back.
    #[cfg(any(test, debug_assertions))]
    pub(crate) judged: u64,
    #[cfg(any(test, debug_assertions))]
    pub(crate) reused: u64,
    #[cfg(any(test, debug_assertions))]
    pub(crate) kept: u64,
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
    /// A test's negative control: the ramp's slopes alone, a word gap
    /// climbing them from each letter (looks7 M4).
    #[cfg(any(test, debug_assertions))]
    pub(crate) gaps_climb: bool,
    /// A test's negative control: a dim letter's bar at its bare share, no
    /// [`OWN_ROOM`].
    #[cfg(any(test, debug_assertions))]
    pub(crate) no_room: bool,
    /// A test's negative control: Classic's spectrum face passes the guard by
    /// on paper too, as it did before looks8 round 2.
    #[cfg(any(test, debug_assertions))]
    pub(crate) classic_paper_off: bool,
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
        lum: Vec::new(),
        ring_lo: Vec::new(),
        ring_hi: Vec::new(),
        ink_lum: Vec::new(),
        clear_memo: None,
        lift_under: Vec::new(),
        lift_over: Vec::new(),
        cuts: Vec::new(),
        ramp_rows: RampRows {
            under: Vec::new(),
            over: Vec::new(),
        },
        bloom_memo: None,
        verdicts: Vec::new(),
        fresh: Vec::new(),
        near_whole: Vec::new(),
        near_whole_fresh: Vec::new(),
        stacked: Vec::new(),
        memo: None,
        #[cfg(any(test, debug_assertions))]
        judged: 0,
        #[cfg(any(test, debug_assertions))]
        reused: 0,
        #[cfg(any(test, debug_assertions))]
        kept: 0,
        #[cfg(any(test, debug_assertions))]
        no_reuse: false,
        #[cfg(any(test, debug_assertions))]
        off: false,
        #[cfg(any(test, debug_assertions))]
        comb: false,
        #[cfg(any(test, debug_assertions))]
        gaps_climb: false,
        #[cfg(any(test, debug_assertions))]
        no_room: false,
        #[cfg(any(test, debug_assertions))]
        classic_paper_off: false,
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

/// A text row's lights of a stream with ramps, in its draw order: each lit
/// quad meeting the band `ry0 .. ry1` a row light (`push_row_light(row, bounds, stream, index, (x, w), 0)`), and each ramp meeting it one row light of its lights there,
/// laid into `laid` (`push_row_light(row, bounds, row stream, first, (x, w), lights)`).
#[inline]
fn row_lights(
    (quads, ramps): (&[GlowQuad], &GlowRamps),
    (sq, sr): (Stream, Stream),
    (ry0, ry1): (i32, i32),
    laid: &mut Vec<GlowQuad>,
    (row, bounds): (&mut Vec<RowLight>, &mut (i32, i32)),
    feet: Option<&InkLedger>,
) {
    #[inline]
    const fn band_meets_y(y: u16, h: u16, ry0: i32, ry1: i32) -> bool {
        (y as i32) < ry1 && (y as i32) + (h as i32) > ry0
    }
    // The under stream's rails in their rows' foot bands are never weighed
    // ([`InkLedger::note_foot_rail`]), quads or ramps.
    let (mut foot_q, mut foot_r) = (0usize, 0usize);
    ramps.in_order(quads.len(), |at| match at {
        Laid::Quads(r) => {
            let mut k = r.start;
            while k < r.end {
                let q = &quads[k];
                if lit(q)
                    && band_meets_y(q.y, q.h, ry0, ry1)
                    && !match feet {
                        Some(l) => l.in_foot_rail(k, &mut foot_q),
                        None => false,
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
                || match feet {
                    Some(l) => l.in_foot_ramp(k, &mut foot_r),
                    None => false,
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
        let w = (j.x1 - j.x0) as usize;
        let n = w * (j.y1 - j.y0) as usize;
        let mut ground = std::mem::take(&mut self.px_ground);
        let mut ink = std::mem::take(&mut self.px_ink);
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
                        } else {
                            let a = wt.min(cap);
                            ground[k] = aterm_render::over_rgb(ground[k], c, a);
                            if has_over {
                                ink[k] = aterm_render::over_rgb(ink[k], c, a);
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
                    k += w as usize;
                    y += 1;
                }
            }
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
    /// share `s`. Every ground pixel reading clear of the dimmest and the
    /// brightest ink ([`Self::clear_of`]) settles it at once; else each ink
    /// pixel is read against the least and greatest ground of its ring, and
    /// the first under the bar settles it.
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
        let (w, h) = ((j.x1 - j.x0) as usize, (j.y1 - j.y0) as usize);
        let r = RING as usize;
        if w <= 2 * r || h <= 2 * r {
            return true;
        }
        if !additive {
            self.paint(j, s, has_over);
        }
        let px_ground = std::mem::take(&mut self.px_ground);
        let px_ink = std::mem::take(&mut self.px_ink);
        let mut lum = std::mem::take(&mut self.lum);
        let mut ink_lum = std::mem::take(&mut self.ink_lum);
        let mut ring_lo = std::mem::take(&mut self.ring_lo);
        let mut ring_hi = std::mem::take(&mut self.ring_hi);
        let n = w * h;
        lum.resize(n, 0.0);
        ink_lum.resize(n, 0.0);
        ring_lo.resize(n, 0.0);
        ring_hi.resize(n, 0.0);
        let table = crate::color_math::relative_luminance_table();
        // The ink's readings on the letter's own pixels, where a light lies
        // on it: the dimmest and the brightest set the ground's clear
        // readings.
        let (mut li_lo, mut li_hi) = (j.l_ink, j.l_ink);
        if has_over {
            (li_lo, li_hi) = (f32::INFINITY, f32::NEG_INFINITY);
            let mut last_ink: Option<([u16; 3], f32)> = None;
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
                        let li = crate::color_math::relative_luminance_with(table, px_ink[k]);
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
        if additive {
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
        } else {
            // Each row's lit span; the pixels no light reaches read the bare
            // ground `lay_lights` left in `lum`.
            let lit_rows = self.lit_rows.as_slice();
            let mut y = 0;
            while y < lit_rows.len() {
                let (lo, hi) = lit_rows[y];
                let (a, b) = (y * w + lo as usize, y * w + hi as usize);
                let mut k = a;
                while k < b {
                    let v = crate::color_math::relative_luminance_with(table, px_ground[k]);
                    lum[k] = v;
                    near |= v > dark && v < light;
                    k += 1;
                }
                y += 1;
            }
            let bare = self.bare_lum.0;
            near |= self.unlit.0 && bare > dark && bare < light;
        }
        let clear = !near;
        let mut holds = true;
        if !clear {
            // Each ink pixel against the least and greatest ground of its
            // ring, row by row: each row's least and greatest within `RING`
            // columns, read once a ring reaches the row, then each inner
            // row's within `RING` rows of those.
            let x_end = w - r;
            let iw = x_end - r;
            let mut ready = 0;
            let mut y = r;
            while holds && y < h - r {
                while ready <= y + r {
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
                    ready += 1;
                }
                #[inline(always)]
                const fn ring_rows(
                    rows: &[f32],
                    from: usize,
                    w: usize,
                    iw: usize,
                ) -> [&[f32]; 2 * RING as usize + 1] {
                    let mut out: [&[f32]; 2 * RING as usize + 1] = [&[]; 2 * RING as usize + 1];
                    let mut i = 0;
                    while i < out.len() {
                        out[i] = rows.split_at(from + i * w).1.split_at(iw).0;
                        i += 1;
                    }
                    out
                }
                #[inline(always)]
                const fn ring_minmax(
                    los: &[&[f32]; 2 * RING as usize + 1],
                    his: &[&[f32]; 2 * RING as usize + 1],
                    x: usize,
                ) -> (f32, f32) {
                    let (mut lo, mut hi) = (los[0][x], his[0][x]);
                    let mut i = 1;
                    while i < los.len() {
                        lo = lo.min(los[i][x]);
                        hi = hi.max(his[i][x]);
                        i += 1;
                    }
                    (lo, hi)
                }
                let from_ring = (y - r) * w + r;
                let (los, his) = (
                    ring_rows(&ring_lo, from_ring, w, iw),
                    ring_rows(&ring_hi, from_ring, w, iw),
                );
                let row_at = y * w + r;
                let inks = &ink_lum[row_at..row_at + iw];
                let mut fail = false;
                if has_over {
                    let mut x = 0;
                    while x < inks.len() {
                        let li = inks[x];
                        let (lo, hi) = ring_minmax(&los, &his, x);
                        fail |= ratio(li, lo) < bar || ratio(li, hi) < bar;
                        x += 1;
                    }
                } else if bar <= 1.0 {
                    let li = j.l_ink;
                    let mut x = 0;
                    while x < iw {
                        let (lo, hi) = ring_minmax(&los, &his, x);
                        fail |= ratio(li, lo) < bar || ratio(li, hi) < bar;
                        x += 1;
                    }
                } else {
                    // One ink reading, `j.l_ink`, whose clear grounds are
                    // `dark` and `light` (`clear_of`, the very quotient
                    // above, which only falls as a ground nears the ink,
                    // down to `1.0` at the ink itself, under a bar over
                    // `1.0`): a ground under its bar reads between them.
                    let mut x = 0;
                    while x < iw {
                        let (lo, hi) = ring_minmax(&los, &his, x);
                        fail |= (lo > dark && lo < light) || (hi > dark && hi < light);
                        x += 1;
                    }
                }
                holds = !fail;
                y += 1;
            }
        }
        self.px_ground = px_ground;
        self.px_ink = px_ink;
        self.lum = lum;
        self.ink_lum = ink_lum;
        self.ring_lo = ring_lo;
        self.ring_hi = ring_hi;
        holds
    }

    /// Whether every share the bisection weighs on its way up to `s` (each
    /// mid while every one holds) holds, by the ring box [`Self::paint`]
    /// just composited at `s` for a cell with no over-text light and no
    /// halo: each ground pixel reads at most `dark` ([`Self::clear_of`])
    /// under every such share, so none reads near the ink and
    /// [`Self::holds`] holds at once.
    ///
    /// A pixel one quad light lies on, `o' = c' + ⌊(d·(255 − a') + 127) /
    /// 255⌋` at share `k' / 128` (`c'`, `a'` its colour and opacity at that
    /// share, [`over_packed`]), is within `1.5` of the line `d + t·(c − d·a
    /// / 255)` at both shares (the floors' room), and at a lesser share the
    /// line lies between the bare `d` and its value at `k'`: every channel
    /// stays at most `max(d + 1, o' + 2)`. A pixel more lights lie on gains
    /// at most each light's share at every step: at most the ground plus
    /// their shares at `s`. A pixel neither bound clears is composited at
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
        // Per pixel the shares at `s` of the lights past its first, and
        // how many there are.
        let mut stacked = std::mem::take(&mut self.stacked);
        stacked.clear();
        stacked.resize(w * h, [0; 4]);
        let more_ground = self.more_ground.as_slice();
        let mut mi = 0;
        while mi < more_ground.len() {
            let m = more_ground[mi];
            let sum = &mut stacked[m.k as usize];
            sum[0] = sum[0].saturating_add(share.scale(ch(m.v, 0)) as u16);
            sum[1] = sum[1].saturating_add(share.scale(ch(m.v, 1)) as u16);
            sum[2] = sum[2].saturating_add(share.scale(ch(m.v, 2)) as u16);
            sum[3] = sum[3].saturating_add(1);
            mi += 1;
        }
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
            let bound = if sum[3] == 0 {
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
            };
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
        let mut every = true;
        let lit_rows = self.lit_rows.as_slice();
        let mut y = 0;
        'rows: while y < lit_rows.len() {
            let (lo, hi) = lit_rows[y];
            let (a, b) = (y * w + lo as usize, y * w + hi as usize);
            let mut k = a;
            while k < b {
                if !pixel_clears(
                    table,
                    (j.ground, d),
                    share,
                    mids,
                    dark,
                    (first[k], stacked[k], self.px_ground[k]),
                    (more, k),
                ) {
                    every = false;
                    break 'rows;
                }
                k += 1;
            }
            y += 1;
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
    /// each of [`Self::cell`]'s lights, in order, by what it draws (not its
    /// index, which a light laid before it moves).
    #[inline]
    const fn cell_key(
        &self,
        (x0, y0, x1, y1): (i32, i32, i32, i32),
        (ink, ground): (u32, u32),
        no_room: bool,
        streams: Streams<'_>,
    ) -> u64 {
        let mut h = fold(
            fold(
                fold(0, (x0 as u32 as u64) | (y0 as u32 as u64) << 32),
                (x1 as u32 as u64) | (y1 as u32 as u64) << 32,
            ),
            (ink as u64) | (ground as u64) << 24 | (no_room as u64) << 48,
        );
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
    #[inline]
    fn judge(
        &mut self,
        (x0, y0, x1, y1): (i32, i32, i32, i32),
        (ink, ground): (u32, u32),
        solid: &mut Option<SolidMemo>,
        no_room: bool,
        streams: Streams<'_>,
    ) -> f32 {
        #[inline]
        const fn lum_ratio(la: f32, lb: f32) -> f32 {
            (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
        }
        let lum = crate::color_math::relative_luminance;
        #[cfg(any(test, debug_assertions))]
        {
            self.judged += 1;
        }
        let (ink, l_ink, l_ground, bar) = match *solid {
            Some((pair, cached)) if pair == (ink, ground) => cached,
            _ => {
                let s = solid_ink(ink, ground);
                let l_i = lum(s);
                let l_g = lum(ground);
                // The bare page's reading: every light at share 0 composes
                // to the ground and the ink themselves.
                let b = letter_bar(lum_ratio(l_i, l_g), no_room);
                let cached = (s, l_i, l_g, b);
                *solid = Some(((ink, ground), cached));
                cached
            }
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
        };
        if let Some((on_ink, all)) = self.bound(j, streams) {
            // Light ink on a dark page reads worst against the
            // brightest ground the lights can make; dark ink on
            // paper, lifted the most, against the page itself.
            let clear = if l_ink >= l_ground {
                let lg = lum(lifted(ground, all));
                lg < l_ink && lum_ratio(l_ink, lg) >= bar
            } else {
                let l_i = lum(lifted(ink, on_ink));
                l_i < l_ground && lum_ratio(l_i, l_ground) >= bar
            };
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
        if !additive && !has_over && self.near_whole.binary_search(&box_at(j.x0, j.y0)).is_ok() {
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

/// The text reading under which a letter the pet lies beneath is veiled
/// ([`CursorGlow::veil_letters_on_pet`]): a dim theme's.
const PET_VEIL_THEME_BARE: f32 = 7.0;
/// The margin past a letter's bar the veil keeps for the renderer's rounding.
const PET_VEIL_GUARD: f32 = 0.08;
/// How far past the pet's noted box the veil reaches, px (the box is noted
/// once a frame and the body moves within it).
const PET_VEIL_PAD_PX: i32 = 3;
/// The run over which a veil's ends ease in, px.
const PET_VEIL_EASE_PX: i32 = 3;
/// The share of a cell's height over which a veil eases in above and below
/// its lettered row, where the row beside it holds no letter under the
/// body, at most as deep as the coat reaches into the row
/// ([`CursorGlow::veil_letters_on_pet`]): a feather at the cut, not a fade
/// (over a quarter of a cell it darkened the eyes of a face cut at the
/// chin, and a paw grazing the row's top wore a dark block).
const PET_VEIL_RISE_SHARE: f32 = 0.12;

/// The bar a letter keeps under the lights, from its solid ink's reading
/// on the bare page, `bare`: [`LETTER_FLOOR`], or [`OWN_SHARE`] of `bare`
/// where the bare page itself reads under the floor, with [`OWN_ROOM`]
/// over it (unless `bare_share`, a test's control), never past `bare`.
#[inline]
const fn letter_bar(bare: f32, bare_share: bool) -> f32 {
    if bare >= LETTER_FLOOR {
        LETTER_FLOOR
    } else if bare_share {
        bare * OWN_SHARE
    } else {
        (bare * OWN_SHARE * OWN_ROOM).min(bare)
    }
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
#[inline]
const fn solid_ink(ink: u32, ground: u32) -> u32 {
    let lg = crate::color_math::relative_luminance(ground);
    #[inline]
    const fn ratio(lg: f32, a: u32) -> f32 {
        let la = crate::color_math::relative_luminance(a);
        (la.max(lg) + 0.05) / (la.min(lg) + 0.05)
    }
    #[inline]
    const fn mix_ch(ink: u32, ground: u32, sh: u32, t: f32) -> u32 {
        let (a, b) = (((ink >> sh) & 0xff) as f32, ((ground >> sh) & 0xff) as f32);
        let u = (a + (b - a) * t).round() as u32;
        if u < 255 { u } else { 255 }
    }
    #[inline]
    const fn mix(ink: u32, ground: u32, t: f32) -> u32 {
        (mix_ch(ink, ground, 16, t) << 16)
            | (mix_ch(ink, ground, 8, t) << 8)
            | mix_ch(ink, ground, 0, t)
    }
    let want = SOLID_SHARE * ratio(lg, ink);
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    let (mut lo_rgb, mut hi_rgb) = (ink & 0x00FF_FFFF, ground & 0x00FF_FFFF);
    let mut hi_checked = false;
    let mut step = 0;
    while step < 10 {
        let mid = 0.5 * (lo + hi);
        let mid_rgb = mix(ink, ground, mid);
        if mid_rgb == lo_rgb || ((!hi_checked || mid_rgb != hi_rgb) && ratio(lg, mid_rgb) >= want) {
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
    /// Cut every quad of `quads` (additive or source-over) to the guard's
    /// dims ([`Self::mask_at_with`]), easing back to the whole light off them,
    /// and the stream's `ramps` with them ([`Self::guard_mask_ramps`]). With
    /// `rails` (the under stream), a light of a rail laid clear of its row's
    /// ring ([`Self::note_rail`]) is cut only where it meets a dimmed cell's
    /// box, and a rail in its row's foot band ([`Self::note_foot_rail`]) is
    /// never cut.
    #[inline]
    fn guard_mask_stream(&mut self, quads: &mut Vec<GlowQuad>, ramps: &mut GlowRamps, rails: bool) {
        if ramps.is_empty() {
            self.guard_mask_quads::<false>(quads, &mut [], rails);
            if rails {
                self.rail_ramps.clear();
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
    /// ([`Self::note_foot_rail`]) is never cut.
    #[inline]
    fn guard_mask_quads<const RAMPS: bool>(
        &mut self,
        quads: &mut Vec<GlowQuad>,
        ramps: &mut [GlowRamp],
        rails: bool,
    ) {
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
    /// only where it meets a dimmed cell's box, as its quad would be.
    #[inline]
    fn guard_mask_ramps(&mut self, ramps: &mut GlowRamps, rails: bool) {
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
            let mut yi = 0;
            while yi < ny {
                let (ya, yb) = (self.knots_y[yi], self.knots_y[yi + 1]);
                let ym = 0.5 * (ya + yb) as f32;
                let ya_u = ya as u16;
                let h_u = (yb - ya) as u16;
                let mut xi = 0;
                while xi < nx {
                    let (xa, xb) = (self.knots_x[xi], self.knots_x[xi + 1]);
                    let m = self.mask_at_with(0.5 * (xa + xb) as f32, ym, feather);
                    let color = match h.mode {
                        HaloMode::Add => scale_rgb(h.color & 0x00FF_FFFF, m),
                        HaloMode::Over => match over_cap(h.color, m) {
                            Some(cap) if m < 1.0 => {
                                (h.color & 0x00FF_FFFF) | (u32::from(cap) << 24)
                            }
                            Some(_) => h.color,
                            None => {
                                xi += 1;
                                continue;
                            }
                        },
                    };
                    if h.mode == HaloMode::Add && color == 0 {
                        xi += 1;
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
                    xi += 1;
                }
                yi += 1;
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
        let li = crate::color_math::relative_luminance(solid_ink(key.0, key.1));
        let lg = crate::color_math::relative_luminance(key.1);
        let held = (li.max(lg) + 0.05) / (li.min(lg) + 0.05) < LETTER_FLOOR;
        self.letter_guard.bloom_memo = Some((key, held));
        held
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
    /// Returns a print of the cuts.
    ///
    /// A frame whose streams, rails, text, caret and grid are the last
    /// frame's takes the streams the guard left that frame ([`FrameMemo`])
    /// and judges no cell: its work follows what moved.
    #[inline]
    pub(crate) fn guard_letters(
        &mut self,
        over: &mut Vec<GlowQuad>,
        caret: Option<(u16, u16)>,
        geom: Geom,
        style: GlowStyle,
    ) -> u64 {
        #[cfg(any(test, debug_assertions))]
        if self.letter_guard.off {
            return 0;
        }
        #[cfg(any(test, debug_assertions))]
        let (seams, reuse) = {
            let g = &self.letter_guard;
            let l = &self.ink_ledger;
            (
                u8::from(g.no_room)
                    | u8::from(g.comb) << 1
                    | u8::from(g.gaps_climb) << 2
                    | u8::from(l.round_cuts) << 3
                    | u8::from(l.rails_feathered) << 4
                    | u8::from(l.foot_rails_weighed) << 5,
                !g.no_reuse,
            )
        };
        #[cfg(not(any(test, debug_assertions)))]
        let (seams, reuse) = (0u8, true);
        #[cfg(any(test, debug_assertions))]
        let paper_spared = self.letter_guard.classic_paper_off
            && matches!(style, GlowStyle::Classic)
            && !self.last_classic_mono;
        #[cfg(not(any(test, debug_assertions)))]
        let paper_spared = false;
        let spared = self.classic_keeps_its_bytes(style) || paper_spared;
        let frame = (
            caret,
            (
                geom.cw as i32,
                geom.ch as i32,
                i32::from(geom.origin_x),
                i32::from(geom.origin_y),
            ),
            seams | u8::from(spared) << 7,
        );
        // The rails are read in order, kept or not.
        self.ink_ledger.rails.sort_unstable();
        self.ink_ledger.rail_ramps.sort_unstable();
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
            && memo.text == (text.lo, text.cols, text.col0)
            && memo.text_rows == text.rows
            && memo.text_pairs == text.pairs
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
        memo.seen.0.clone_from(&self.under_out);
        memo.seen.1.clone_from(over);
        memo.seen.2.clone_from(&self.halo_out);
        copy_ramps(&mut memo.seen.3, &self.under_ramps);
        copy_ramps(&mut memo.seen.4, &self.out_ramps);
        memo.rails.0.clone_from(&self.ink_ledger.rails);
        memo.rails.1.clone_from(&self.ink_ledger.rail_ramps);
        let print = self.guard_letters_inner(over, caret, geom, spared);
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

    /// [`Self::guard_letters`] on a frame it has not kept; `spared` when
    /// the style's spectrum face passes the guard by
    /// ([`Self::classic_keeps_its_bytes`]).
    #[inline]
    fn guard_letters_inner(
        &mut self,
        over: &mut Vec<GlowQuad>,
        caret: Option<(u16, u16)>,
        geom: Geom,
        spared: bool,
    ) -> u64 {
        if spared
            || self.frame_text.rows.is_empty()
            || (over.is_empty()
                && self.under_out.is_empty()
                && self.halo_out.is_empty()
                && self.under_ramps.is_empty()
                && self.out_ramps.is_empty())
        {
            return 0;
        }
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        if cw <= 0 || ch <= 0 {
            return 0;
        }
        let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        let mut guard = std::mem::take(&mut self.letter_guard);
        #[cfg(any(test, debug_assertions))]
        let no_room = guard.no_room;
        #[cfg(not(any(test, debug_assertions)))]
        let no_room = false;
        let ledger = &mut self.ink_ledger;
        ledger.grid = (ox as f32, oy as f32, cw as f32, ch as f32);
        ledger.dims.clear();
        let text = &self.frame_text;
        let (under, halos) = (self.under_out.as_slice(), self.halo_out.as_slice());
        let over_q = over.as_slice();
        // Each stream's lit extent down the page, its ramps' boxes counted:
        // a text row none of its lights reach skips that stream.
        #[inline]
        const fn y_bounds(qs: &[GlowQuad], ramps: &GlowRamps) -> (i32, i32) {
            let (mut y0, mut y1) = (i32::MAX, i32::MIN);
            let mut i = 0;
            while i < qs.len() {
                let q = &qs[i];
                if lit(q) {
                    let qy = q.y as i32;
                    let qy1 = qy + q.h as i32;
                    if qy < y0 {
                        y0 = qy;
                    }
                    if qy1 > y1 {
                        y1 = qy1;
                    }
                }
                i += 1;
            }
            let items = ramps.items.as_slice();
            let mut j = 0;
            while j < items.len() {
                let r = &items[j];
                if r.w > 0 && r.h > 0 {
                    let ry = r.y as i32;
                    let ry1 = ry + r.h as i32;
                    if ry < y0 {
                        y0 = ry;
                    }
                    if ry1 > y1 {
                        y1 = ry1;
                    }
                }
                j += 1;
            }
            (y0, y1)
        }
        let (u_y0, u_y1) = y_bounds(under, &self.under_ramps);
        let (o_y0, o_y1) = y_bounds(over_q, &self.out_ramps);
        let (mut ha_y0, mut ha_y1) = (i32::MAX, i32::MIN);
        let (mut ho_y0, mut ho_y1) = (i32::MAX, i32::MIN);
        let mut hi = 0;
        while hi < halos.len() {
            let h = &halos[hi];
            if h.w > 0 && h.h > 0 && h.color & 0x00FF_FFFF != 0 {
                let hy0 = i32::from(h.y);
                let hy1 = hy0 + i32::from(h.h);
                if h.mode == HaloMode::Add {
                    ha_y0 = ha_y0.min(hy0);
                    ha_y1 = ha_y1.max(hy1);
                } else {
                    ho_y0 = ho_y0.min(hy0);
                    ho_y1 = ho_y1.max(hy1);
                }
            }
            hi += 1;
        }
        let all_y0 = u_y0.min(o_y0).min(ha_y0).min(ho_y0);
        let all_y1 = u_y1.max(o_y1).max(ha_y1).max(ho_y1);
        if all_y0 >= all_y1 {
            self.letter_guard = guard;
            return 0;
        }
        let mut ramp_rows = std::mem::take(&mut guard.ramp_rows);
        let mut print = 0u64;
        let mut solid = None;
        #[inline]
        const fn meets_y(y: u16, h: u16, ry0: i32, ry1: i32) -> bool {
            let y0 = y as i32;
            y0 < ry1 && y0 + h as i32 > ry0
        }
        let mut i = 0;
        while i < text.rows.len() {
            let Some((first, end)) = text.rows[i] else {
                i += 1;
                continue;
            };
            let row = text.lo + i as i32;
            let (ry0, ry1) = (oy + row * ch - RING, oy + (row + 1) * ch + RING);
            if ry1 <= all_y0 || ry0 >= all_y1 {
                i += 1;
                continue;
            }
            guard.row.clear();
            ramp_rows.under.clear();
            ramp_rows.over.clear();
            let mut row_bounds = (i32::MAX, i32::MIN);
            // Each stream's quads, then where it has ramps its quads and
            // ramps in draw order ([`row_lights`]); a stream of quads alone
            // reads as the guard read it before ramps.
            if ry1 > u_y0 && ry0 < u_y1 {
                if self.under_ramps.is_empty() {
                    let mut foot = 0usize;
                    let mut k = 0;
                    while k < under.len() {
                        let q = &under[k];
                        if lit(q)
                            && meets_y(q.y, q.h, ry0, ry1)
                            && !ledger.in_foot_rail(k, &mut foot)
                        {
                            push_row_light(
                                &mut guard.row,
                                &mut row_bounds,
                                Stream::Under,
                                k,
                                (q.x, q.w),
                                0,
                            );
                        }
                        k += 1;
                    }
                } else {
                    row_lights(
                        (under, &self.under_ramps),
                        (Stream::Under, Stream::UnderRow),
                        (ry0, ry1),
                        &mut ramp_rows.under,
                        (&mut guard.row, &mut row_bounds),
                        Some(&*ledger),
                    );
                }
            }
            if ry1 > o_y0 && ry0 < o_y1 {
                if self.out_ramps.is_empty() {
                    let mut k = 0;
                    while k < over_q.len() {
                        let q = &over_q[k];
                        if lit(q) && meets_y(q.y, q.h, ry0, ry1) {
                            push_row_light(
                                &mut guard.row,
                                &mut row_bounds,
                                Stream::Over,
                                k,
                                (q.x, q.w),
                                0,
                            );
                        }
                        k += 1;
                    }
                } else {
                    row_lights(
                        (over_q, &self.out_ramps),
                        (Stream::Over, Stream::OverRow),
                        (ry0, ry1),
                        &mut ramp_rows.over,
                        (&mut guard.row, &mut row_bounds),
                        None,
                    );
                }
            }
            if ry1 > ha_y0 && ry0 < ha_y1 {
                let mut k = 0;
                while k < halos.len() {
                    let h = &halos[k];
                    if h.mode == HaloMode::Add
                        && h.w > 0
                        && h.h > 0
                        && h.color & 0x00FF_FFFF != 0
                        && meets_y(h.y, h.h, ry0, ry1)
                    {
                        push_row_light(
                            &mut guard.row,
                            &mut row_bounds,
                            Stream::HaloAdd,
                            k,
                            (h.x, h.w),
                            0,
                        );
                    }
                    k += 1;
                }
            }
            if ry1 > ho_y0 && ry0 < ho_y1 {
                let mut k = 0;
                while k < halos.len() {
                    let h = &halos[k];
                    if h.mode == HaloMode::Over
                        && h.w > 0
                        && h.h > 0
                        && h.color & 0x00FF_FFFF != 0
                        && meets_y(h.y, h.h, ry0, ry1)
                    {
                        push_row_light(
                            &mut guard.row,
                            &mut row_bounds,
                            Stream::HaloOver,
                            k,
                            (h.x, h.w),
                            0,
                        );
                    }
                    k += 1;
                }
            }
            if guard.row.is_empty() {
                i += 1;
                continue;
            }
            let (min_x, max_x) = row_bounds;
            let skip = match caret {
                Some((r, c)) if i32::from(r) == row => Some(i32::from(c)),
                _ => None,
            };
            let row_from = ledger.dims.len();
            guard.cuts.clear();
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
                guard.cell.clear();
                let mut li = 0;
                while li < guard.row.len() {
                    let light = guard.row[li];
                    if light.x0 < cx1 && light.x1 > cx0 {
                        if light.rows == 0 {
                            guard.cell.push((light.stream, light.idx));
                        } else {
                            let mut k = light.idx;
                            let k_end = light.idx + light.rows;
                            while k < k_end {
                                guard.cell.push((light.stream, k));
                                k += 1;
                            }
                        }
                    }
                    li += 1;
                }
                if guard.cell.is_empty() {
                    col += 1;
                    continue;
                }
                let streams = Streams {
                    quads: [under, &ramp_rows.under, over_q, &ramp_rows.over],
                    halos,
                };
                // A cell whose ring box, text and lights are the last
                // frame's keeps the last frame's verdict.
                let key = guard.cell_key((cx0, ry0, cx1, ry1), (ink, ground), no_room, streams);
                let share = if let Some(share) = guard.verdict(key) {
                    share
                } else {
                    let share = guard.judge(
                        (cx0, ry0, cx1, ry1),
                        (ink, ground),
                        &mut solid,
                        no_room,
                        streams,
                    );
                    guard.fresh.push((key, share));
                    share
                };
                if share == NEAR_WHOLE {
                    guard.near_whole_fresh.push(box_at(cx0, ry0));
                }
                if share < 1.0 {
                    ledger.dims.push(Dim {
                        x0: cx0 as f32,
                        y0: ry0 as f32,
                        x1: cx1 as f32,
                        y1: ry1 as f32,
                        share,
                    });
                    guard.cuts.push((col, share));
                    print = aterm_hash::fnv_mix_u64(
                        print | 1,
                        ((row as u64) << 48) ^ ((col as u64) << 32) ^ u64::from(share.to_bits()),
                    );
                }
                col += 1;
            }
            #[cfg(any(test, debug_assertions))]
            let comb = guard.comb;
            #[cfg(not(any(test, debug_assertions)))]
            let comb = false;
            if comb || guard.cuts.is_empty() {
                i += 1;
                continue;
            }
            #[cfg(any(test, debug_assertions))]
            let climb = guard.gaps_climb;
            #[cfg(not(any(test, debug_assertions)))]
            let climb = false;
            let cuts = std::mem::take(&mut guard.cuts);
            ramp_row(
                &cuts,
                skip,
                climb,
                |col| matches!(text.pair(row, col), Some((Some(_), _))),
                |col, share| {
                    match find_cut(&cuts, col) {
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
            guard.cuts = cuts;
            i += 1;
        }
        guard.ramp_rows = ramp_rows;
        guard.settle_verdicts();
        if !ledger.dims.is_empty() {
            ledger.guard_mask_stream(&mut self.under_out, &mut self.under_ramps, true);
            ledger.guard_mask_stream(over, &mut self.out_ramps, false);
            ledger.guard_mask_halos(&mut self.halo_out);
        }
        ledger.dims.clear();
        self.letter_guard = guard;
        print
    }

    /// On a dim theme, veil the pet's body with the page under every letter
    /// it lies beneath (looks6 N11). The renderer's knock-out rings each
    /// glyph in a pixel of page, which keeps a bright theme's letters; a dim
    /// theme's text over the coat read at its cell well under its own
    /// reading (`dim-grey`'s `o` of `echo` at 3.94:1 under the body, its
    /// bar [`letter_bar`] 4.6:1). Each lettered cell the body's box
    /// ([`Self::note_pet_body`], padded [`PET_VEIL_PAD_PX`]) reaches, but
    /// the caret's, takes the page at the least opacity that keeps the
    /// letter at its bar over the worst coat the pet may wear (the letter's
    /// own colour), as one run per row of lettered
    /// cells, its ends eased over [`PET_VEIL_EASE_PX`] outside the lettered
    /// cells (inside them, a word's first and last letters' outer pixels
    /// read under the bar, looks6 review): the letters' row
    /// reads as lying in front of the pet, which keeps its outline above
    /// and below the row. A cell whose text reads past
    /// [`PET_VEIL_THEME_BARE`] draws none. It runs after the guard (whose
    /// cuts would thin it) into the under stream, under the glyphs and over
    /// the coat, and is the guard's: the guard's negative control drops it.
    /// The frame's print of what it drew.
    ///
    /// It is drawn by the body the pet's tick drew this frame
    /// ([`Self::veil_pet_letters`], after the host's pet tick), into the
    /// published under stream: drawn in the trail's step it read the body
    /// the pet drew a frame before, so a moving pet's veil lay a frame
    /// behind it, page over the letters it had left and its new letters
    /// bare (looks7 M10). Given the body's `sprite`, each run keeps to the
    /// rows its art draws, each to that row's drawn span padded
    /// [`PET_VEIL_PAD_PX`]: the box's transparent margin (the 16 px under a
    /// standing cat's feet, the air over its back) takes no page, which on
    /// a translucent window read as a box of opaque page around the coat and
    /// on any window cut the trail's light under the letters beside it.
    #[inline]
    pub(crate) fn veil_letters_on_pet(
        &mut self,
        (out, ramps): (&mut Vec<GlowQuad>, &mut GlowRamps),
        caret: Option<(u16, u16)>,
        geom: Geom,
        sprite: Option<&aterm_render::sprites::FreeSprite>,
    ) -> u64 {
        #[cfg(any(test, debug_assertions))]
        if self.letter_guard.off || self.pet_veil_off {
            return 0;
        }
        let Some((bx0, bx1, by0, by1)) = self.pet_body else {
            return 0;
        };
        let (cw, ch) = (geom.cw as i32, geom.ch as i32);
        if cw <= 0 || ch <= 0 || self.veil_text().rows.is_empty() {
            return 0;
        }
        let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
        let pad = PET_VEIL_PAD_PX;
        let (bx0, bx1, by0, by1) = (bx0 - pad, bx1 + pad, by0 - pad, by1 + pad);
        // The silhouette's rows, each its span padded; the padded box whole
        // without the body's sprite.
        let mut bands = std::mem::take(&mut self.icon_scratch);
        bands.clear();
        match sprite {
            Some(s) if !self.pet_veil_boxed() => {
                super::super::icon_shade::sprite_rows(
                    s,
                    (bx0, bx1, by0, by1),
                    (ox, oy),
                    &mut bands,
                );
                let mut bi = 0;
                while bi < bands.len() {
                    let b = &mut bands[bi];
                    (b.0, b.1) = ((b.0 - pad).max(bx0), (b.1 + pad).min(bx1));
                    bi += 1;
                }
            }
            _ => bands.push((bx0, bx1, by0, by1)),
        }
        // The least page opacity over the worst coat that keeps `ink` at
        // its bar on `ground`; `None` where the text needs no veil.
        #[inline]
        const fn solve_veil(ink: u32, ground: u32) -> Option<u8> {
            let table = crate::color_math::relative_luminance_table();
            let solid = solid_ink(ink, ground);
            let la = crate::color_math::relative_luminance_with(table, solid);
            #[inline]
            const fn ratio_ink(
                table: &crate::color_math::RelativeLuminanceTable,
                la: f32,
                b: u32,
            ) -> f32 {
                let lb = crate::color_math::relative_luminance_with(table, b);
                (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
            }
            let bare = ratio_ink(table, la, ground);
            if bare >= PET_VEIL_THEME_BARE {
                return None;
            }
            let bar = (letter_bar(bare, false) + PET_VEIL_GUARD).min(bare);
            // The worst coat is the letter's own colour (a coat may wear any).
            let coat = solid;
            let mut prev_g = coat;
            let mut found = u8::MAX;
            let mut a = 1u8;
            loop {
                let g = aterm_render::over_premul(coat, premul_rgb(ground, a), a) & 0x00FF_FFFF;
                if g != prev_g {
                    prev_g = g;
                    if ratio_ink(table, la, g) >= bar {
                        found = a;
                        break;
                    }
                }
                if a == u8::MAX {
                    break;
                }
                a += 1;
            }
            Some(found)
        }
        #[inline]
        const fn solve_veil_memo(
            memo: &mut Option<((u32, u32), Option<u8>)>,
            ink: u32,
            ground: u32,
        ) -> Option<u8> {
            if let Some((prev, ans)) = *memo
                && prev.0 == ink
                && prev.1 == ground
            {
                return ans;
            }
            let ans = solve_veil(ink, ground);
            *memo = Some(((ink, ground), ans));
            ans
        }
        let mut veil_memo: Option<((u32, u32), Option<u8>)> = None;
        let ease = PET_VEIL_EASE_PX;
        #[cfg(any(test, debug_assertions))]
        let risen = !self.pet_veil_sliced;
        #[cfg(not(any(test, debug_assertions)))]
        let risen = true;
        let rise = if risen {
            ((ch as f32 * PET_VEIL_RISE_SHARE).round() as i32).max(1)
        } else {
            0
        };
        #[cfg(any(test, debug_assertions))]
        let deep = self.pet_veil_deep;
        #[cfg(not(any(test, debug_assertions)))]
        let deep = false;
        let text = self.veil_text();
        let under = out;
        let from = under.len();
        let mut print = 0u64;
        // One run: the lettered span `s0..s1` at full opacity, its eases laid
        // outside it, so every lettered pixel takes the whole veil. Where the
        // body's box cuts the span, the ease lies in the box's pad, off the
        // coat; beside the caret's cell (`wall`) the span ends full, and the
        // caret's cell takes none.
        // Above and below the row (`edges`: where no letter of the row beside
        // it lies under the body) the run eases in over `rise` px, one pixel
        // row at a time, and no deeper than the coat reaches into the row
        // from that edge: cut at the row's edge, the page sliced the coat
        // across (the burst on Solarized Dark, looks7 round 2).
        let mut push = |under: &mut Vec<GlowQuad>,
                        (s0, wall0): (i32, bool),
                        (s1, wall1): (i32, bool),
                        (y0, y1): (i32, i32),
                        edges: (bool, bool),
                        page: u32,
                        a: u8| {
            let (l0, mut f0) = if s0 <= bx0 {
                (bx0, bx0 + ease)
            } else if wall0 {
                (s0, s0)
            } else {
                ((s0 - ease).max(bx0), s0)
            };
            let (mut f1, r1) = if s1 >= bx1 {
                (bx1 - ease, bx1)
            } else if wall1 {
                (s1, s1)
            } else {
                (s1, (s1 + ease).min(bx1))
            };
            if f1 < f0 {
                let mid = (f0 + f1) / 2;
                (f0, f1) = (mid.max(l0), mid.min(r1));
            }
            // Each ramp, clipped to each silhouette row's span the band
            // reaches, its opacity read at the cut.
            #[inline]
            fn push_veil_ramp(
                under: &mut Vec<GlowQuad>,
                bands: &[(i32, i32, i32, i32)],
                (geom, page): (Geom, u32),
                (x0, x1): (i32, i32),
                (y0, y1): (i32, i32),
                (a0, a1): (u8, u8),
                (k0, k1): (i32, i32),
            ) {
                let mut bi = 0;
                while bi < bands.len() {
                    let (cx0, cx1, cy0, cy1) = bands[bi];
                    let (c0, c1) = (x0.max(cx0).max(k0), x1.min(cx1).min(k1));
                    let (t0, t1) = (y0.max(cy0), y1.min(cy1));
                    if c1 <= c0 || t1 <= t0 {
                        bi += 1;
                        continue;
                    }
                    let dx = (x1 - x0).max(1);
                    let (ia0, da) = (i32::from(a0), i32::from(a1) - i32::from(a0));
                    let s = (ia0 + da * (c0 - x0) / dx).clamp(0, 255) as u8;
                    let e = (ia0 + da * (c1 - x0) / dx).clamp(0, 255) as u8;
                    crate::effect_util::push_fx_ramp(
                        under,
                        geom,
                        c0,
                        t0,
                        c1 - c0,
                        t1 - t0,
                        (premul_rgb(page, s), s),
                        (premul_rgb(page, e), e),
                    );
                    bi += 1;
                }
            }
            #[inline]
            fn lay_veil_band(
                under: &mut Vec<GlowQuad>,
                bands: &[(i32, i32, i32, i32)],
                gp: (Geom, u32),
                (l0, f0, f1, r1): (i32, i32, i32, i32),
                ys: (i32, i32),
                a: u8,
                clip: (i32, i32),
            ) {
                push_veil_ramp(under, bands, gp, (l0, f0), ys, (1, a), clip);
                push_veil_ramp(under, bands, gp, (f0, f1), ys, (a, a), clip);
                push_veil_ramp(under, bands, gp, (f1, r1), ys, (a, 1), clip);
            }
            let gp = (geom, page);
            let hspan = (l0, f0, f1, r1);
            lay_veil_band(under, &bands, gp, hspan, (y0, y1), a, (i32::MIN, i32::MAX));
            // A rise keeps to the coat the row's own edge pixel row veils:
            // where the silhouette leaves the row bare, it lays nothing.
            #[inline]
            const fn veil_edge_span(
                bands: &[(i32, i32, i32, i32)],
                l0: i32,
                r1: i32,
                y: i32,
            ) -> Option<(i32, i32)> {
                let mut span: Option<(i32, i32)> = None;
                let mut bi = 0;
                while bi < bands.len() {
                    let b = bands[bi];
                    if b.2 <= y && y < b.3 {
                        let a0 = if b.0 > l0 { b.0 } else { l0 };
                        let b0 = if b.1 < r1 { b.1 } else { r1 };
                        if b0 > a0 {
                            span = Some(match span {
                                Some((sa, sb)) => {
                                    (if sa < a0 { sa } else { a0 }, if sb > b0 { sb } else { b0 })
                                }
                                None => (a0, b0),
                            });
                        }
                    }
                    bi += 1;
                }
                span
            }
            // The coat's depth into the row from an edge pixel row `y`,
            // stepping `dy`, up to `rise`.
            #[inline]
            const fn veil_coat_depth(
                bands: &[(i32, i32, i32, i32)],
                (l0, r1): (i32, i32),
                (y0, y1): (i32, i32),
                rise: i32,
                y: i32,
                dy: i32,
            ) -> i32 {
                let mut d = 0;
                while d < rise {
                    let yk = y + dy * d;
                    if yk < y0 || yk >= y1 || veil_edge_span(bands, l0, r1, yk).is_none() {
                        break;
                    }
                    d += 1;
                }
                d
            }
            let feathers = [(edges.0, y0, 1, -1, y0 - 1), (edges.1, y1 - 1, -1, 1, y1)];
            let mut fi = 0;
            while fi < feathers.len() {
                let (open, edge, inward, outward, first) = feathers[fi];
                fi += 1;
                if !open {
                    continue;
                }
                let Some(clip) = veil_edge_span(&bands, l0, r1, edge) else {
                    continue;
                };
                let r = if deep {
                    rise
                } else {
                    veil_coat_depth(&bands, (l0, r1), (y0, y1), rise, edge, inward)
                };
                let inv_r1 = 1.0 / (r + 1) as f32;
                let mut k = 1;
                while k <= r {
                    let u = (r + 1 - k) as f32 * inv_r1;
                    let ak = (f32::from(a) * u * u * (3.0 - 2.0 * u)).round() as u8;
                    let y = first + outward * (k - 1);
                    if ak != 0 && y >= by0 && y < by1 {
                        lay_veil_band(under, &bands, gp, hspan, (y, y + 1), ak, clip);
                    }
                    k += 1;
                }
            }
            print = aterm_hash::fnv_mix_u64(
                print | 1,
                ((l0 as u64) << 48) ^ ((y0 as u64) << 32) ^ ((r1 as u64) << 16) ^ u64::from(a),
            );
        };
        #[inline]
        const fn row_is_lettered(
            rows: &[Option<(i32, i32)>],
            (ox, cw, bx0, bx1): (i32, i32, i32, i32),
            j: Option<usize>,
        ) -> bool {
            let Some(j) = j else {
                return false;
            };
            if j >= rows.len() {
                return false;
            };
            let Some((f, e)) = rows[j] else {
                return false;
            };
            ox + e * cw > bx0 && ox + f * cw < bx1
        }
        let row_params = (ox, cw, bx0, bx1);
        let mut i = 0;
        while i < text.rows.len() {
            let Some((first, end)) = text.rows[i] else {
                i += 1;
                continue;
            };
            if ox + end * cw <= bx0 || ox + first * cw >= bx1 {
                i += 1;
                continue;
            }
            let row = text.lo + i as i32;
            let (top, bottom) = (oy + row * ch, oy + (row + 1) * ch);
            let (y0, y1) = (top.max(by0), bottom.min(by1));
            if y1 <= y0 {
                i += 1;
                continue;
            }
            let edges = (
                y0 == top && !row_is_lettered(&text.rows, row_params, i.checked_sub(1)),
                y1 == bottom && !row_is_lettered(&text.rows, row_params, i.checked_add(1)),
            );
            let caret_col = match caret {
                Some((r, c)) if i32::from(r) == row => Some(i32::from(c)),
                _ => None,
            };
            // `(first column, page, opacity)` of the open run.
            let mut run: Option<(i32, u32, u8)> = None;
            let mut col = first;
            while col <= end {
                let at = if col < end
                    && caret_col != Some(col)
                    && ox + (col + 1) * cw > bx0
                    && ox + col * cw < bx1
                    && let Some((Some(ink), ground)) = text.pair(row, col)
                    && let Some(b) =
                        solve_veil_memo(&mut veil_memo, ink & 0x00FF_FFFF, ground & 0x00FF_FFFF)
                {
                    Some((col, ground & 0x00FF_FFFF, b))
                } else {
                    None
                };
                match (run, at) {
                    (Some((c0, page, a)), Some((_, g, b))) if g == page => {
                        run = Some((c0, page, a.max(b)));
                    }
                    (open, next) => {
                        if let Some((c0, page, a)) = open {
                            let (s0, s1) = (ox + c0 * cw, ox + col * cw);
                            if s1.min(bx1) > s0.max(bx0) {
                                push(
                                    under,
                                    (s0, caret_col == Some(c0 - 1)),
                                    (s1, caret_col == Some(col)),
                                    (y0, y1),
                                    edges,
                                    page,
                                    a,
                                );
                            }
                        }
                        run = next;
                    }
                }
                col += 1;
            }
            i += 1;
        }
        self.icon_scratch = bands;
        // The veil lies on the pet's coat, beneath every light: laid over
        // them, it cut a dim page's bed (its hue at the page's own
        // luminance) back to the page along the silhouette's rows, a hard
        // edge across the letters it veiled, where the judge's ring read
        // each channel's median of page and bed, brighter than either (the
        // flat rainbow's burst on Solarized Dark: `f` at 3.78:1 of its own
        // 4.06, `#1e2b55` of `#002b36` and `#1e1b55`).
        #[cfg(any(test, debug_assertions))]
        let beneath = !self.pet_veil_over;
        #[cfg(not(any(test, debug_assertions)))]
        let beneath = true;
        if beneath {
            let laid = under.len() - from;
            under.rotate_right(laid);
            // The stream's ramps keep their places among the quads they
            // were drawn before, the veil beneath them all.
            let mut ri = 0;
            while ri < ramps.items.len() {
                ramps.items[ri].at += laid as u32;
                ri += 1;
            }
        }
        print
    }

    /// The text the pet's veil reads: the rows the body lay on last frame
    /// ([`Self::pet_text`]), or the trail's own read where the body was
    /// off glass then. A test's negative control reads the trail's.
    #[inline]
    const fn veil_text(&self) -> &super::super::FrameText {
        #[cfg(any(test, debug_assertions))]
        if self.pet_veil_caret_rows {
            return &self.frame_text;
        }
        if self.pet_text.rows.is_empty() {
            &self.frame_text
        } else {
            &self.pet_text
        }
    }

    /// A test's negative control: the veil reads only the trail's own rows
    /// of text, about the caret ([`Self::veil_text`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_veil_caret_rows(&mut self, on: bool) {
        self.pet_veil_caret_rows = on;
    }

    /// A test's negative control: the veil laid over the body's whole box,
    /// not its silhouette ([`Self::veil_letters_on_pet`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_veil_boxed(&mut self, boxed: bool) {
        self.pet_veil_boxed = boxed;
    }

    /// A test's negative control: the veil cut at its lettered row's edges,
    /// with no rise above or below it ([`Self::veil_letters_on_pet`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_veil_risen(&mut self, on: bool) {
        self.pet_veil_sliced = !on;
    }

    /// A test's negative control: the rise above or below a lettered row
    /// feathers its whole height, however shallow the coat's cut at that
    /// edge ([`Self::veil_letters_on_pet`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_veil_feather_deep(&mut self, on: bool) {
        self.pet_veil_deep = on;
    }

    #[inline]
    const fn pet_veil_boxed(&self) -> bool {
        #[cfg(any(test, debug_assertions))]
        {
            self.pet_veil_boxed
        }
        #[cfg(not(any(test, debug_assertions)))]
        {
            false
        }
    }

    /// The caret and geometry of the frame the step drew, for the pet's veil
    /// the host draws after its pet's tick ([`Self::veil_pet_letters`]).
    #[inline]
    pub(crate) const fn note_veil_frame(&mut self, caret: Option<(u16, u16)>, geom: Geom) {
        self.veil_frame = Some((caret, geom));
    }

    /// The pet's page veil ([`Self::veil_letters_on_pet`]) at the body the
    /// pet's tick drew this frame ([`Self::note_pet_body`]), laid in the
    /// frame's published under stream `out` (its quads and their ramps),
    /// kept to the silhouette of the body's sprite among the frame's
    /// `sprites` (grid pixels; the one whose box is the body's). The host calls it after the pet's tick, its
    /// word pass and the trail's publish; the print of what it drew. What it
    /// drew is kept ([`Self::pet_veil`]) for the frames that project the
    /// trail's streams again before the next pet tick.
    #[inline]
    pub fn veil_pet_letters(
        &mut self,
        (out, ramps): (&mut Vec<GlowQuad>, &mut GlowRamps),
        sprites: &[aterm_render::sprites::FreeSprite],
    ) -> u64 {
        let from = out.len();
        let print = match self.veil_frame {
            Some((caret, geom)) => {
                let (ox, oy) = (i32::from(geom.origin_x), i32::from(geom.origin_y));
                let mut sprite = None;
                if let Some(body) = self.pet_body {
                    let mut si = 0;
                    while si < sprites.len() {
                        let s = &sprites[si];
                        let (x, y) = (s.x + ox, s.y + oy);
                        if (x, x + i32::from(s.w), y, y + i32::from(s.h)) == body {
                            sprite = Some(s);
                            break;
                        }
                        si += 1;
                    }
                }
                self.veil_letters_on_pet((&mut *out, ramps), caret, geom, sprite)
            }
            None => 0,
        };
        self.pet_veil.clear();
        if let Some(slice) = out.get(from..) {
            self.pet_veil.extend_from_slice(slice);
        }
        print
    }

    /// The veil the last [`Self::veil_pet_letters`] laid, window pixels: a
    /// host that projects the trail's under stream again without a pet tick
    /// (a split pane's cursor-only frame, whose pet sprite stays published)
    /// lays it with the stream, or the letters under the pet lose their
    /// veil on that frame and get it back on the next full one (a blink).
    #[inline]
    #[must_use]
    pub const fn pet_veil(&self) -> &[GlowQuad] {
        self.pet_veil.as_slice()
    }

    /// A test's negative control: the guard on, but no page veil under the
    /// letters the pet lies beneath ([`Self::veil_letters_on_pet`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_veil(&mut self, on: bool) {
        self.pet_veil_off = !on;
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

    /// A test's negative control: Classic's spectrum face passes the guard by
    /// on paper too ([`LetterGuard::classic_paper_off`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_classic_paper(&mut self, on: bool) {
        self.letter_guard.classic_paper_off = !on;
    }

    /// A test's negative control: a word gap climbs the ramp's slope from
    /// each of its letters ([`WORD_GAP_COLS`]), as it did.
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_word_gaps(&mut self, on: bool) {
        self.letter_guard.gaps_climb = !on;
    }

    /// A test's negative control: a dim letter's bar at its bare share
    /// ([`OWN_SHARE`]), the guard keeping no room over it ([`OWN_ROOM`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_room(&mut self, on: bool) {
        self.letter_guard.no_room = !on;
    }

    /// A test's negative control: the pet's veil laid over every light,
    /// not beneath them ([`Self::veil_letters_on_pet`]).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_pet_veil_beneath(&mut self, on: bool) {
        self.pet_veil_over = !on;
    }

    /// A test's negative control: the guard's cut pieces rounded to the
    /// nearest byte, laying light it never weighed (`guard_mask_quads`).
    #[cfg(any(test, debug_assertions))]
    #[inline]
    pub const fn set_letter_guard_cuts_rounded(&mut self, on: bool) {
        self.ink_ledger.round_cuts = on;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramped(cuts: &[(i32, f32)], skip: Option<i32>) -> Vec<(i32, f32)> {
        climbed(cuts, skip, false)
    }

    fn climbed(cuts: &[(i32, f32)], skip: Option<i32>, climb: bool) -> Vec<(i32, f32)> {
        lettered_climbed(cuts, &[], skip, climb)
    }

    /// The ramp over `cuts`, the letters being the cut columns and `whole`
    /// (letters left uncut).
    fn lettered_climbed(
        cuts: &[(i32, f32)],
        whole: &[i32],
        skip: Option<i32>,
        climb: bool,
    ) -> Vec<(i32, f32)> {
        let mut out = cuts.to_vec();
        let lettered = |col: i32| whole.contains(&col) || cuts.iter().any(|c| c.0 == col);
        ramp_row(cuts, skip, climb, lettered, |col, share| {
            match out.iter_mut().find(|c| c.0 == col) {
                Some(c) => c.1 = share,
                None => out.push((col, share)),
            }
        });
        out.sort_by_key(|c| c.0);
        out
    }

    /// A word gap between two cut letters keeps no more light than they do
    /// beyond one slope step, and every share only falls.
    #[test]
    fn a_row_s_cuts_ease_as_one_ramp_through_its_word_gaps() {
        // "as the": columns 0-1 cut, 2 a gap, 3-5 cut; 4 left whole by its own cut.
        let cuts = [(0, 0.20), (1, 0.30), (3, 0.25), (5, 0.60)];
        let out = ramped(&cuts, None);
        let at = |col: i32| out.iter().find(|c| c.0 == col).map_or(1.0, |c| c.1);
        assert!(
            at(2) <= at(1).min(at(3)) + RAMP_SLOPE + 1e-6,
            "the gap keeps {} against its letters' {} and {}",
            at(2),
            at(1),
            at(3)
        );
        for w in out.windows(2) {
            assert!(
                (w[1].1 - w[0].1).abs() <= RAMP_SLOPE * (w[1].0 - w[0].0) as f32 + 1e-6,
                "a step of {} between columns {} and {}",
                (w[1].1 - w[0].1).abs(),
                w[0].0,
                w[1].0
            );
        }
        for &(col, s) in &cuts {
            assert!(at(col) <= s, "column {col} rose from {s} to {}", at(col));
        }
        assert!(at(4) < 1.0, "the letter between two cuts eases with them");
        // The caret's cell is the caret's law, and nothing past the run is touched.
        let skipped = ramped(&cuts, Some(2));
        assert!(skipped.iter().all(|c| c.0 != 2));
        assert!(out.iter().all(|c| (0..=5).contains(&c.0)));
    }

    /// looks8 round 2: the dark landing ring's rim runs on under the text. A
    /// one-pixel slab across `ab  cd` keeps [`RIM_UNDER_LETTERS`] of its light
    /// on each lettered cell and the judge's [`RING`] round it, its whole
    /// light elsewhere, and no pixel of its span goes dark. Negative control:
    /// cut off the letters (a share of 0, as the rim was cut), the span breaks
    /// into the pieces that read as stray glyphs.
    #[test]
    fn the_dark_rim_runs_on_under_the_letters_at_a_fifth_of_its_light() {
        let g = Geom {
            cw: 8,
            ch: 16,
            rows: 6,
            cols: 40,
            origin_x: 0,
            origin_y: 0,
            win_w: 320,
            win_h: 96,
            head: 0,
        };
        let row = 2u16;
        let letters = [(4, 'a'), (5, 'b'), (8, 'c'), (9, 'd')];
        let mut cells = vec![vec![aterm_core::terminal::RenderCell::default(); g.cols]; g.rows];
        for (col, ch) in letters {
            cells[usize::from(row)][col].ch = ch;
        }
        let lettered = |x: i32| {
            letters
                .iter()
                .any(|&(c, _)| x >= c as i32 * 8 - RING && x < (c as i32 + 1) * 8 + RING)
        };
        let mut glow = CursorGlow::default();
        glow.observe_frame_text(Some((row, 20)), &cells, Instant::now());
        let slab = GlowQuad::flat(row, 0, row * 16 + 5, 120, 1, 0x0040_C080, 0);
        let lay = |share: f32| {
            let mut q = vec![GlowQuad::default(), slab];
            glow.pass_under_letters(&mut q, 1, g, share);
            q
        };
        let light = |q: &[GlowQuad], x: i32| {
            q[1..]
                .iter()
                .find(|p| x >= i32::from(p.x) && x < i32::from(p.x) + i32::from(p.w))
                .map(|p| (p.color, p.is_flat()))
        };
        let q = lay(RIM_UNDER_LETTERS);
        assert_eq!(
            q[0],
            GlowQuad::default(),
            "a quad before `from` is left alone"
        );
        let span: u32 = q[1..].iter().map(|p| u32::from(p.w)).sum();
        assert_eq!(span, 120, "the pieces tile the slab once: {q:?}");
        for x in 0..120 {
            let want = if lettered(x) {
                scale_rgb(slab.color, RIM_UNDER_LETTERS)
            } else {
                slab.color
            };
            assert_eq!(light(&q, x), Some((want, true)), "pixel {x}: {q:?}");
        }
        let cut = lay(0.0);
        assert!(
            (0..120).any(|x| light(&cut, x).is_none()),
            "control: the rim cut off the letters keeps its whole span: {cut:?}"
        );
    }

    /// looks7 WIDEN: a page whose text has no room for the GPU's bloom holds
    /// it, and one whose text clears the floor keeps it. Solarized Dark's
    /// text (4.75:1, its solid ink 4.04:1), a 5.0:1 grey on the dark page
    /// and a 4.95:1 grey on paper hold it; the cold dark page's text and
    /// GitHub Light's ink keep it: the negative control, which a hold on
    /// every page (or on none) fails.
    #[test]
    fn a_dim_page_holds_the_bloom_and_a_bright_one_keeps_it() {
        let mut glow = CursorGlow::default();
        for (ink, ground) in [
            (0x0083_9496, 0x0000_2B36),
            (0x0085_8585, 0x0011_1318),
            (0x0070_7070, 0x00FF_FFFF),
        ] {
            assert!(
                glow.holds_bloom(ink, ground),
                "{ink:06x} on {ground:06x} keeps the bloom"
            );
        }
        for (ink, ground) in [(0x00C8_D3F5, 0x0011_1318), (0x001F_2328, 0x00FF_FFFF)] {
            assert!(
                !glow.holds_bloom(ink, ground),
                "{ink:06x} on {ground:06x} holds the bloom"
            );
        }
        // The memo answers for its own pair only.
        assert!(glow.holds_bloom(0x0083_9496, 0x0000_2B36));
        assert!(!glow.holds_bloom(0x00C8_D3F5, 0x0011_1318));
    }

    /// looks7 M4: a word gap between two letters cut deep keeps no more than
    /// the straight line between their shares: Phaser's dark bar stood a
    /// stripe in each gap, a share and a half of its letters' light. Past the
    /// run's last cut the gap kept its whole light beside a letter cut to
    /// two fifths (the gallery's `the quick`, `e` cut and `q` whole): it
    /// keeps the line to the uncut letter's whole. Climbing the slope from
    /// each letter (the negative control), it peaks, and past the run stays
    /// whole. A long run of blanks still climbs the slope back toward its
    /// whole light.
    #[test]
    fn a_word_gap_keeps_the_line_between_its_letters() {
        // "the quick": 'e' at 2, the gap at 3, 'q' at 4; a long blank run
        // from 6 to 17 between 'u' at 5 and a cut at 18.
        let cuts = [(1, 0.18), (2, 0.15), (4, 0.20), (5, 0.22), (18, 0.2)];
        let line =
            |out: &[(i32, f32)], col: i32| out.iter().find(|c| c.0 == col).map_or(1.0, |c| c.1);
        let out = ramped(&cuts, None);
        let gap = line(&out, 3);
        assert!(
            gap <= 0.5 * (0.15 + 0.20) + 1e-6,
            "the gap keeps {gap} between its letters' 0.15 and 0.20"
        );
        assert!(
            line(&out, 11) > 0.22 + 2.0 * RAMP_SLOPE,
            "the long blank run keeps only {} in its middle",
            line(&out, 11)
        );
        let climbed_gap = line(&climbed(&cuts, None, true), 3);
        assert!(
            climbed_gap > 0.20 + 0.01,
            "control: climbing the slope the gap keeps only {climbed_gap}, no more than its letters"
        );
        // "the quick", `e` cut at 2 and `q`, `u` at 4 and 5 left whole.
        let edge = [(1, 0.18), (2, 0.4)];
        let gap = line(&lettered_climbed(&edge, &[4, 5], None, false), 3);
        assert!(
            gap <= 0.5 * (0.4 + 1.0) + 1e-6,
            "past the run the gap keeps {gap} between its letters' 0.4 and whole"
        );
        let past = line(&lettered_climbed(&edge, &[4, 5], None, true), 3);
        assert!(
            past >= 1.0,
            "control: climbing, the gap past the run keeps {past}, under its whole light"
        );
    }

    /// Lane B5: the guard cuts a stream's ramps as it cuts the quads their
    /// rows stand for, a rail laid clear of the ring included
    /// ([`InkLedger::note_rail_laid`]): a rail's row, or its run's one
    /// quad, is cut only where it meets a dimmed cell's box, and every cut
    /// piece is truncated as the quad's is. A light across a dimmed row and
    /// a rail below it, laid once as quads and once as ramps (every run a
    /// ramp), are cut alike, light for light. Premise: the rail noted, its
    /// quads keep light the feather would cut. Negative controls: the
    /// rail's ramps not noted (its quads alone, [`InkLedger::note_rail`]),
    /// or the ramps' cut pieces rounded, the ramps are cut otherwise.
    #[test]
    fn a_stream_s_ramps_are_cut_as_their_quads_rails_included() {
        use aterm_render::{BeamRows, RampCost, RampSink};
        // Cells 10 x 20 px; columns 4..7 of the row at y 40..60 dimmed.
        let dimmed = || {
            let mut ledger = InkLedger::EMPTY;
            ledger.grid = (0.0, 0.0, 10.0, 20.0);
            for (col, share) in [(4, 0.3), (5, 0.25), (6, 0.4)] {
                ledger.dims.push(Dim {
                    x0: col as f32 * 10.0,
                    y0: 40.0,
                    x1: (col + 1) as f32 * 10.0,
                    y1: 60.0,
                    share,
                });
            }
            ledger
        };
        // A row's ends: additive, every third source-over.
        fn ends(y: u16) -> ((u32, u8), (u32, u8)) {
            let k = u32::from(y % 5 + 1);
            if y.is_multiple_of(3) {
                ((k * 0x000E_0804, 0x50), (k * 0x0004_080C, 0x48))
            } else {
                ((k * 0x0018_1008, 0), (k * 0x0006_0C14, 0))
            }
        }
        // The light across the row (y 30..56), then the rail below it: rows
        // 56..60 in the dimmed box, a run's tall light at 60..64 and rows
        // to 70, in the feather's reach and past it. The rail's marks.
        fn lay<S: BeamRows>(out: &mut S) -> (S::Mark, S::Mark) {
            out.slab();
            for y in 30..56 {
                let (a, b) = ends(y);
                out.row(i32::from(y / 20), y / 20, (30, y, 50), a, b);
            }
            let from = out.mark();
            out.slab();
            for y in 56..60 {
                let (a, b) = ends(y);
                out.row(3, 3, (30, y, 50), a, b);
            }
            out.rect(3, 3, (30, 60, 50, 4), (0x0020_1810, 0));
            for y in 64..70 {
                let (a, b) = ends(y);
                out.row(3, 3, (30, y, 50), a, b);
            }
            (from, out.mark())
        }
        let lead = GlowQuad::flat(1, 0, 10, 4, 4, 0x0010_1010, 0);
        let legacy = |rails: bool| {
            let mut quads = vec![lead];
            let (from, to) = lay(&mut quads);
            let mut ledger = dimmed();
            if rails {
                ledger.note_rail(from, to);
            }
            ledger.guard_mask_stream(&mut quads, &mut GlowRamps::default(), true);
            quads
        };
        let ramped = |(rails, rounded): (bool, bool)| {
            let (mut quads, mut ramps) = (vec![lead], GlowRamps::default());
            let (from, to) = {
                let mut sink = RampSink::new(&mut quads, &mut ramps).costed(RampCost::ALWAYS);
                lay(&mut sink)
            };
            assert!(
                to.1.items() > from.1.items(),
                "premise: the rail rides ramps"
            );
            let mut ledger = dimmed();
            if rails {
                ledger.note_rail_laid(from, to);
            } else {
                ledger.note_rail(from.0, to.0);
            }
            ledger.round_cuts = rounded;
            ledger.guard_mask_stream(&mut quads, &mut ramps, true);
            let mut got = Vec::new();
            ramps.expand_into(&quads, &mut got);
            (got, ramps.laid(quads.len()))
        };
        let key = |q: &GlowQuad| (q.y, q.x, q.w, q.h, q.color, q.alpha, q.color2, q.alpha2);
        let sorted = |mut v: Vec<GlowQuad>| {
            v.sort_by_key(key);
            v
        };
        let want = legacy(true);
        assert_ne!(
            sorted(want.clone()),
            sorted(legacy(false)),
            "premise: the rail keeps light the feather would cut"
        );
        let (got, laid) = ramped((true, false));
        assert_eq!(
            sorted(got),
            sorted(want.clone()),
            "the ramps are cut as their quads"
        );
        assert_eq!(laid, want.len(), "the stream counts alike");
        assert_ne!(
            sorted(ramped((false, false)).0),
            sorted(want.clone()),
            "control: the rail's ramps not noted, they are cut as the rail's quads are"
        );
        assert_ne!(
            sorted(ramped((true, true)).0),
            sorted(want),
            "control: rounded, the ramps' cut pieces are their quads' truncated"
        );
    }

    /// The property tests' seeded randomness: a splitmix64 stream.
    struct Mix(u64);

    impl Mix {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: u32) -> u32 {
            (self.next() % u64::from(n.max(1))) as u32
        }

        fn chance(&mut self, in_ten: u32) -> bool {
            self.below(10) < in_ten
        }
    }

    /// What a reference reading leaves out, for the property test's
    /// negative controls.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Lapse {
        None,
        /// Each pixel under its first quad light alone.
        FirstLightOnly,
        /// No halo.
        NoHalos,
    }

    /// The share the guard's first form weighed: at each share the
    /// bisection tries, every pixel of the ring box composited light by
    /// light in the cell's order (an additive cell's whole light summed and
    /// scaled), every ink pixel read against the least and greatest ground
    /// of its whole ring.
    fn share_by_every_pixel(
        j: Judge,
        bar: f32,
        cell: &[(Stream, u32)],
        streams: Streams<'_>,
        lapse: Lapse,
    ) -> f32 {
        let lum = crate::color_math::relative_luminance;
        let halos = streams.halos;
        let cell: Vec<(Stream, u32)> = cell
            .iter()
            .copied()
            .filter(|&(st, _)| {
                lapse != Lapse::NoHalos || !matches!(st, Stream::HaloAdd | Stream::HaloOver)
            })
            .collect();
        let additive = cell.iter().all(|&(st, i)| match st {
            Stream::HaloAdd => true,
            Stream::HaloOver => false,
            _ => {
                let q = streams.quad(st, i as usize);
                q.alpha | q.alpha2 == 0
            }
        });
        let has_over = cell.iter().any(|&(st, _)| !st.on_ground());
        let covers = |st: Stream, i: u32, x: i32, y: i32| -> bool {
            let (bx, by, bw, bh) = match st {
                Stream::HaloAdd | Stream::HaloOver => {
                    let h = &halos[i as usize];
                    (h.x, h.y, h.w, h.h)
                }
                _ => {
                    let q = streams.quad(st, i as usize);
                    (q.x, q.y, q.w, q.h)
                }
            };
            x >= i32::from(bx)
                && x < i32::from(bx) + i32::from(bw)
                && y >= i32::from(by)
                && y < i32::from(by) + i32::from(bh)
        };
        // A pixel's composite over `base` at share `s`, the ground's lights
        // (`ground`) or the over-text ones alone.
        let px = |base: u32, x: i32, y: i32, ground: bool, s: f32| -> u32 {
            if additive {
                let mut lift = [0u16; 3];
                for &(st, i) in &cell {
                    if !covers(st, i, x, y) || (st.on_ground() && !ground) {
                        continue;
                    }
                    let c = match st {
                        Stream::HaloAdd => {
                            let h = &halos[i as usize];
                            aterm_render::premul_rgb(h.color & 0x00FF_FFFF, halo_wt(h, x, y))
                        }
                        _ => quad_at(streams.quad(st, i as usize), x, 1.0).0,
                    };
                    for (k, sh) in [16u32, 8, 0].into_iter().enumerate() {
                        lift[k] = lift[k].saturating_add(((c >> sh) & 0xff) as u16);
                    }
                }
                let ch =
                    |sh: u32, l: u16| (((base >> sh) & 0xff) + (f32::from(l) * s) as u32).min(255);
                return (ch(16, lift[0]) << 16) | (ch(8, lift[1]) << 8) | ch(0, lift[2]);
            }
            let mut p = base;
            let mut quads_laid = 0;
            for &(st, i) in &cell {
                if !covers(st, i, x, y) {
                    continue;
                }
                match st {
                    Stream::HaloAdd => {
                        let h = &halos[i as usize];
                        let wt = halo_wt(h, x, y);
                        if wt > 0 {
                            let c = if s >= 1.0 {
                                h.color & 0x00FF_FFFF
                            } else {
                                scale_rgb(h.color & 0x00FF_FFFF, s)
                            };
                            p = aterm_render::add_sat(p, aterm_render::premul_rgb(c, wt));
                        }
                    }
                    Stream::HaloOver => {
                        let h = &halos[i as usize];
                        let wt = halo_wt(h, x, y);
                        if let Some(cap) = over_cap(h.color, s)
                            && wt > 0
                        {
                            p = aterm_render::over_rgb(p, h.color & 0x00FF_FFFF, wt.min(cap));
                        }
                    }
                    _ => {
                        if st.on_ground() && !ground {
                            continue;
                        }
                        let (c, a) = quad_at(streams.quad(st, i as usize), x, s);
                        if (c, a) == (0, 0) {
                            continue;
                        }
                        quads_laid += 1;
                        if lapse == Lapse::FirstLightOnly && quads_laid > 1 {
                            continue;
                        }
                        p = aterm_render::over_premul(p, c, a);
                    }
                }
            }
            p
        };
        let holds = |s: f32| -> bool {
            let r = RING;
            if j.x1 - j.x0 <= 2 * r || j.y1 - j.y0 <= 2 * r {
                return true;
            }
            let ratio = |a: f32, b: f32| (a.max(b) + 0.05) / (a.min(b) + 0.05);
            for y in j.y0 + r..j.y1 - r {
                for x in j.x0 + r..j.x1 - r {
                    let li = if has_over {
                        lum(px(j.ink, x, y, false, s))
                    } else {
                        j.l_ink
                    };
                    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
                    for yy in y - r..=y + r {
                        for xx in x - r..=x + r {
                            let l = lum(px(j.ground, xx, yy, true, s));
                            lo = lo.min(l);
                            hi = hi.max(l);
                        }
                    }
                    if ratio(li, lo) < bar || ratio(li, hi) < bar {
                        return false;
                    }
                }
            }
            true
        };
        if holds(1.0) {
            return 1.0;
        }
        let (mut lo, mut hi) = (0.0f32, 1.0f32);
        for _ in 0..STEPS {
            let mid = 0.5 * (lo + hi);
            if holds(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// A cell's ring box, bar, lights by stream, halos and lights in order.
    type RandomCell = (
        Judge,
        f32,
        [Vec<GlowQuad>; 4],
        Vec<RainHalo>,
        Vec<(Stream, u32)>,
    );

    /// A random cell's ring box, text and lights: quads and one-pixel
    /// rows on the ground and over the ink, gradients and flat ones, adding
    /// and source-over, and halos of both modes, laid in the guard's order.
    fn random_cell(mix: &mut Mix) -> RandomCell {
        let (x0, y0) = (16 + mix.below(400) as i32, 16 + mix.below(300) as i32);
        let (w, h) = if mix.chance(8) {
            (18 + 2 * RING, 38 + 2 * RING)
        } else {
            (3 + mix.below(28) as i32, 3 + mix.below(46) as i32)
        };
        let (x1, y1) = (x0 + w, y0 + h);
        let rgb = |mix: &mut Mix| (mix.next() as u32) & 0x00FF_FFFF;
        let (ink, ground) = (rgb(mix), rgb(mix));
        let additive = mix.chance(3);
        let premul = |mix: &mut Mix, a: u8| -> u32 {
            if a == 0 {
                return (mix.next() as u32) & 0x003F_3F3F;
            }
            let ch = |mix: &mut Mix| mix.below(u32::from(a) + 1);
            (ch(mix) << 16) | (ch(mix) << 8) | ch(mix)
        };
        let quad = |mix: &mut Mix, row: bool| -> GlowQuad {
            let a = if additive { 0 } else { mix.below(256) as u8 };
            let a2 = if additive || mix.chance(5) {
                a
            } else {
                mix.below(256) as u8
            };
            let color = premul(mix, a);
            let color2 = if mix.chance(3) {
                color
            } else {
                premul(mix, a2)
            };
            GlowQuad {
                row: 0,
                x: (x0 - 8 + mix.below(w as u32 + 16) as i32) as u16,
                y: (y0 - 4 + mix.below(h as u32 + 8) as i32) as u16,
                w: 1 + mix.below(if row { 9 } else { 30 }) as u16,
                h: if row { 1 } else { 1 + mix.below(12) as u16 },
                color,
                alpha: a,
                color2,
                alpha2: a2,
            }
        };
        let mut quads: [Vec<GlowQuad>; 4] = Default::default();
        let mut cell = Vec::new();
        // The under stream's quads and rows interleaved, then the over
        // stream's, as `row_lights` lays them.
        for (stream, row_stream, from) in [
            (Stream::Under, Stream::UnderRow, 0),
            (Stream::Over, Stream::OverRow, 2),
        ] {
            let n = mix.below(if from == 0 { 60 } else { 12 });
            for _ in 0..n {
                let row = mix.chance(6);
                let (st, at) = if row {
                    (row_stream, from + 1)
                } else {
                    (stream, from)
                };
                quads[at].push(quad(mix, row));
                cell.push((st, (quads[at].len() - 1) as u32));
            }
        }
        let mut halos = Vec::new();
        for mode in [HaloMode::Add, HaloMode::Over] {
            if (mode == HaloMode::Over && additive) || !mix.chance(3) {
                continue;
            }
            for _ in 0..1 + mix.below(2) {
                let (cx, cy) = (
                    x0 + mix.below(w as u32) as i32,
                    y0 + mix.below(h as u32) as i32,
                );
                let (rx, ry) = (2 + mix.below(24) as i32, 2 + mix.below(24) as i32);
                let color = if mode == HaloMode::Add {
                    premul(mix, 0)
                } else {
                    rgb(mix) | 0x8000_0000
                };
                halos.push(RainHalo {
                    row: 0,
                    x: (cx - rx).max(0) as u16,
                    y: (cy - ry).max(0) as u16,
                    w: (2 * rx + 1) as u16,
                    h: (2 * ry + 1) as u16,
                    color,
                    cx: cx as u16,
                    cy: cy as u16,
                    rx: rx as u16,
                    ry: ry as u16,
                    mode,
                });
                let st = if mode == HaloMode::Add {
                    Stream::HaloAdd
                } else {
                    Stream::HaloOver
                };
                cell.push((st, (halos.len() - 1) as u32));
            }
        }
        let solid = solid_ink(ink, ground);
        let lum = crate::color_math::relative_luminance;
        let (l_ink, l_ground) = (lum(solid), lum(ground));
        let bar = letter_bar(
            (l_ink.max(l_ground) + 0.05) / (l_ink.min(l_ground) + 0.05),
            mix.chance(2),
        );
        let j = Judge {
            x0,
            y0,
            x1,
            y1,
            ink: solid,
            ground,
            l_ink,
            l_ground,
        };
        (j, bar, quads, halos, cell)
    }

    /// The guard's share for a cell, laid once and read row by row at each
    /// share, is the share the first form weighed composing every pixel
    /// of the ring box light by light, over randomized cells; each negative
    /// control (a reading that drops the stacked lights, or the halos)
    /// differs from it on some cell, so the cells exercise both.
    #[test]
    fn the_guard_s_share_is_the_share_of_every_pixel_composited() {
        let mut mix = Mix(0x6C65_7474_6572);
        let mut guard = LetterGuard::default();
        let (mut cut, mut lapses) = (0, [0usize; 2]);
        let cases = 1500;
        for case in 0..cases {
            let (j, bar, quads, halos, cell) = random_cell(&mut mix);
            let streams = Streams {
                quads: [&quads[0], &quads[1], &quads[2], &quads[3]],
                halos: &halos,
            };
            guard.cell.clear();
            guard.cell.extend_from_slice(&cell);
            let got = guard.share(j, bar, streams);
            let want = share_by_every_pixel(j, bar, &cell, streams, Lapse::None);
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "case {case}: the guard's share {got} against every pixel's {want}"
            );
            cut += usize::from(got < 1.0);
            for (k, lapse) in [Lapse::FirstLightOnly, Lapse::NoHalos]
                .into_iter()
                .enumerate()
            {
                let off = share_by_every_pixel(j, bar, &cell, streams, lapse);
                lapses[k] += usize::from(off.to_bits() != want.to_bits());
            }
        }
        assert!(
            cut > cases / 10,
            "the cells cut too seldom to weigh the bisection: {cut}"
        );
        assert!(
            lapses[0] > 0,
            "control: no cell's share read its stacked lights"
        );
        assert!(lapses[1] > 0, "control: no cell's share read its halos");
    }

    /// A quad's light read with no division ([`quad_light`]) is the
    /// gradient law's ([`quad_at`]): at every column of every width up to
    /// [`RECIP_W`] for the channel pairs that make the largest numerators,
    /// and on randomized quads of widths past it too. The control, a
    /// reciprocal rounded down, misreads some column.
    #[test]
    fn a_quad_s_light_read_with_no_division_is_the_gradient_law_s() {
        let quad = |w: u16, (c, a): (u32, u8), (c2, a2): (u32, u8)| GlowQuad {
            row: 0,
            x: 40,
            y: 0,
            w,
            h: 1,
            color: c,
            alpha: a,
            color2: c2,
            alpha2: a2,
        };
        let law = |q: &GlowQuad, x: i32| {
            let (c, a) = quad_at(q, x, 1.0);
            pack_light(c, a)
        };
        let ends = [
            (0x00FF_FFFF, 255),
            (0, 0),
            (0x00FF_00FF, 255),
            (0x0000_FF00, 1),
        ];
        let mut short = 0usize;
        for w in 1..=RECIP_W as u16 {
            for &a in &ends {
                for &b in &ends {
                    let q = quad(w, a, b);
                    for x in 40..40 + i32::from(w) {
                        assert_eq!(quad_light(&q, x), law(&q, x), "width {w} at {x}");
                    }
                    // The control: `⌊2^36 / 2w⌋` in place of the ceiling.
                    let d = 2 * u64::from(w);
                    let down = (1u64 << 36) / d;
                    for i in 0..u32::from(w) {
                        let m = 2 * i + 1;
                        let n = u64::from(255 * (2 * u32::from(w) - m) + u32::from(w));
                        short += usize::from((n * down) >> 36 != n / d);
                    }
                }
            }
        }
        assert!(
            short > 0,
            "control: a reciprocal rounded down read every column"
        );
        let mut mix = Mix(0x7175_6164);
        for case in 0..200_000 {
            let w = 1 + mix.below(if case % 4 == 0 { 3_000 } else { 64 }) as u16;
            let end = |mix: &mut Mix| (mix.next() as u32 & 0x00FF_FFFF, mix.below(256) as u8);
            let (a, b) = (end(&mut mix), end(&mut mix));
            let b = if mix.chance(3) { a } else { b };
            let q = quad(w, a, b);
            let x = 32 + mix.below(u32::from(w) + 16) as i32;
            assert_eq!(
                quad_light(&q, x),
                law(&q, x),
                "case {case}: width {w} at {x}"
            );
        }
    }

    /// A ground reads under one ink's bar (over `1.0`) exactly where
    /// [`LetterGuard::clear_of`] puts it between its clear readings, as the
    /// ring test of a cell with no over-text light reads it: over randomized
    /// inks, bars and grounds, each bound and its neighbours among them, the
    /// threshold test is the quotient's. The control, a dark bound one step
    /// under, misreads one.
    #[test]
    fn a_ground_under_the_bar_reads_between_the_clear_readings() {
        let ratio = |a: f32, b: f32| (a.max(b) + 0.05) / (a.min(b) + 0.05);
        let mut guard = LetterGuard::default();
        let mut mix = Mix(0x7269_6E67);
        let unit = |mix: &mut Mix| (mix.next() >> 40) as f32 / (1u64 << 24) as f32;
        let mut short = 0usize;
        for case in 0..20_000 {
            let li = unit(&mut mix);
            let bar = 1.0f32.next_up() + 6.5 * unit(&mut mix);
            let (dark, light) = guard.clear_of(li, li, bar);
            let mut grounds: Vec<f32> = (0..16).map(|_| unit(&mut mix)).collect();
            for b in [dark, light] {
                if b.is_finite() {
                    grounds.extend([b.next_down(), b, b.next_up()]);
                }
            }
            for g in grounds.into_iter().filter(|g| (0.0..=1.0).contains(g)) {
                assert_eq!(
                    g > dark && g < light,
                    ratio(li, g) < bar,
                    "case {case}: ink {li}, bar {bar}, ground {g}, bounds {dark}..{light}"
                );
                let under = dark.next_down();
                short += usize::from((g > under && g < light) != (ratio(li, g) < bar));
            }
        }
        assert!(
            short > 0,
            "control: a dark bound one step under read every ground alike"
        );
    }

    /// A share of `k / 128` scales a byte exactly as the float product
    /// does, for every byte and every `k` the bisection weighs; a share off
    /// that grid (the control) keeps the float product.
    #[test]
    fn a_bisection_share_scales_a_byte_as_its_float_product() {
        for k in 1..128u32 {
            let s = k as f32 / 128.0;
            let share = Share::of(s);
            assert!(matches!(share, Share::Of128(kk) if kk == k), "k {k}");
            for c in 0..=255u32 {
                assert_eq!(share.scale(c), (c as f32 * s) as u32, "k {k}, byte {c}");
            }
        }
        assert!(matches!(Share::of(1.0), Share::Whole));
        assert!(
            matches!(Share::of(0.3), Share::Any(_)),
            "control: an off-grid share"
        );
    }

    /// A pixel one quad light lies on reads, at every share under one the
    /// bisection weighs, at most [`LetterGuard::vouch`]'s bound from its
    /// composite there, `max(d + 1, o' + 2)` per channel: for every pair of
    /// shares `k < k'` on the grid over grounds, colours and opacities at
    /// their extremes and at random. The controls, each bound a step
    /// tighter (`o' + 1`, or the bare `d`), are crossed by some pixel.
    #[test]
    fn a_lesser_share_keeps_a_lone_light_s_pixel_under_its_vouched_bound() {
        let composite = |d: u32, c: u32, a: u32, k: u32| {
            over_packed(
                d,
                pack_light(c, a as u8),
                if k >= 128 {
                    Share::Whole
                } else {
                    Share::Of128(k)
                },
            )
        };
        let mut mix = Mix(0x0076_6F75_6368);
        let ends = [0u32, 1, 2, 127, 128, 253, 254, 255];
        let mut triples = Vec::new();
        for &d in &ends {
            for &c in &ends {
                for &a in &ends {
                    triples.push((d, c, a));
                }
            }
        }
        for _ in 0..1500 {
            triples.push((mix.below(256), mix.below(256), mix.below(256)));
        }
        let (mut tight_o, mut tight_d) = (0usize, 0usize);
        for &(d, c, a) in &triples {
            let outs: Vec<u32> = (0..=128).map(|k| composite(d, c, a, k)).collect();
            for kk in 1..=128usize {
                let o2 = outs[kk];
                for (k, &o) in outs.iter().enumerate().take(kk).skip(1) {
                    assert!(
                        o <= (d + 1).max(o2 + 2).min(255),
                        "ground {d}, colour {c}, opacity {a}: share {k} reads {o}, over the bound from {kk}'s {o2}"
                    );
                    tight_o += usize::from(o > (d + 1).max(o2 + 1).min(255));
                    tight_d += usize::from(o > d.max(o2 + 2).min(255));
                }
            }
        }
        assert!(tight_o > 0, "control: `o' + 1` bounded every lesser share");
        assert!(
            tight_d > 0,
            "control: the bare ground bounded every lesser share"
        );
    }

    /// Cells the guard weighs near the whole first: no over-text light, no
    /// halo, source-over quads scaled until the cell sits on its bar (the
    /// faintest scale whose whole light it cuts). With the last frame's
    /// [`NEAR_WHOLE`] noted for every cell, the guard's share is the share
    /// it weighs with none noted and the share of every pixel composited;
    /// some cells it vouches for at once ([`LetterGuard::vouch`]) and some
    /// it does not, so both arms run.
    #[test]
    fn a_cell_weighed_near_the_whole_first_keeps_the_bisection_s_share() {
        let mut mix = Mix(0x6E65_6172);
        let mut guard = LetterGuard::default();
        let (mut vouched, mut weighed, mut trusting, mut cells) = (0usize, 0usize, 0usize, 0usize);
        while cells < 300 {
            let (j, bar, quads, _, cell) = random_cell(&mut mix);
            let cell: Vec<(Stream, u32)> =
                cell.into_iter().filter(|&(st, _)| st.on_ground()).collect();
            if cell.is_empty()
                || quads[0]
                    .iter()
                    .chain(&quads[1])
                    .all(|q| q.alpha | q.alpha2 == 0)
            {
                continue;
            }
            let scaled = |f: u32| -> [Vec<GlowQuad>; 4] {
                let by = |v: u32| v * f / 256;
                let sc = |c: u32| {
                    (by((c >> 16) & 0xff) << 16) | (by((c >> 8) & 0xff) << 8) | by(c & 0xff)
                };
                let mut out: [Vec<GlowQuad>; 4] = Default::default();
                for (o, qs) in out.iter_mut().zip(&quads) {
                    o.extend(qs.iter().map(|q| GlowQuad {
                        color: sc(q.color),
                        color2: sc(q.color2),
                        alpha: by(u32::from(q.alpha).max(1)) as u8,
                        alpha2: by(u32::from(q.alpha2).max(1)) as u8,
                        ..*q
                    }));
                }
                out
            };
            let weigh = |guard: &mut LetterGuard, qs: &[Vec<GlowQuad>; 4], noted: bool| {
                let streams = Streams {
                    quads: [&qs[0], &qs[1], &qs[2], &qs[3]],
                    halos: &[],
                };
                guard.cell.clear();
                guard.cell.extend_from_slice(&cell);
                guard.near_whole.clear();
                if noted {
                    guard.near_whole.push(box_at(j.x0, j.y0));
                }
                guard.share(j, bar, streams)
            };
            // The faintest scale (of 256) whose whole light the cell cuts.
            if weigh(&mut guard, &scaled(256), false) >= 1.0 {
                continue;
            }
            let (mut lo, mut hi) = (0u32, 256u32);
            while hi - lo > 1 {
                let mid = (lo + hi) / 2;
                if weigh(&mut guard, &scaled(mid), false) >= 1.0 {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let qs = scaled(hi);
            let plain = weigh(&mut guard, &qs, false);
            let near = weigh(&mut guard, &qs, true);
            let streams = Streams {
                quads: [&qs[0], &qs[1], &qs[2], &qs[3]],
                halos: &[],
            };
            let want = share_by_every_pixel(j, bar, &cell, streams, Lapse::None);
            assert_eq!(
                plain.to_bits(),
                want.to_bits(),
                "cell {cells}: share {plain} against every pixel's {want}"
            );
            assert_eq!(
                near.to_bits(),
                want.to_bits(),
                "cell {cells}: weighed near the whole first {near}, every pixel {want}"
            );
            // Whether the near-whole share holds by its composite alone.
            let flags = (guard.additive(streams), false);
            if flags.0 {
                continue;
            }
            cells += 1;
            guard.cell.clear();
            guard.cell.extend_from_slice(&cell);
            guard.lay_lights(j, streams, false);
            if guard.holds(j, NEAR_WHOLE, bar, flags) {
                weighed += 1;
                vouched += usize::from(guard.vouch(j, NEAR_WHOLE, bar));
                // The control: a probe that took the near-whole share's own
                // verdict for every lesser one.
                trusting += usize::from(want != NEAR_WHOLE);
            }
        }
        assert!(
            vouched > 0,
            "no cell held near the whole by its composite alone"
        );
        assert!(
            vouched < weighed,
            "every cell that held near the whole held by its composite alone: {vouched}"
        );
        assert!(
            trusting > 0,
            "control: every cell that held near the whole kept that share"
        );
    }

    /// The guard's ramp cut reads its dims as a fresh reading does
    /// ([`InkLedger::knots`]): over randomized dims across several text
    /// rows and a stream of rows and tall pieces over them, laid once as
    /// quads (each sliced by dims gathered afresh) and once as ramps (a row
    /// whose one-row box gathers the dims the row above gathered reusing
    /// them, and a row every near dim spans reusing the row above's masks),
    /// the two are cut alike, light for light. At every point a slice reads
    /// of a randomized box, sliced by one ledger box after box as a fresh
    /// ledger slices it, the mask over the dims `knots` kept near is the
    /// mask over every dim, and a box `knots` finds no dim near reads the
    /// whole light at its corners. Premise: the ramps gathered their near
    /// dims fewer times than their quads did, so the reuse ran.
    #[test]
    fn a_ramp_cut_reuses_its_near_dims_as_a_fresh_reading_reads_them() {
        use aterm_render::{BeamRows, RampCost, RampSink};
        #[derive(Clone, Copy)]
        enum Piece {
            Row(u16, u16, u16, (u32, u8), (u32, u8)),
            Tall(u16, u16, u16, u16, (u32, u8)),
        }
        fn lay<S: BeamRows>(out: &mut S, slabs: &[Vec<Piece>], (oy, ch): (u16, u16)) {
            for slab in slabs {
                out.slab();
                for &p in slab {
                    match p {
                        Piece::Row(x, y, w, a, b) => {
                            let band = (y - oy) / ch;
                            out.row(i32::from(band), band, (x, y, w), a, b);
                        }
                        Piece::Tall(x, y, w, h, end) => {
                            let band = (y - oy) / ch;
                            out.rect(i32::from(band), band, (x, y, w, h), end);
                        }
                    }
                }
            }
            out.mark();
        }
        let key = |q: &GlowQuad| (q.y, q.x, q.w, q.h, q.color, q.alpha, q.color2, q.alpha2);
        let sorted = |mut v: Vec<GlowQuad>| {
            v.sort_by_key(key);
            v
        };
        let mut mix = Mix(0x6E65_6172_6469_6D73);
        let (o, rows) = (8i32, 4i32);
        let (mut gathered_ramps, mut gathered_quads) = (0u64, 0u64);
        for case in 0..300 {
            let (cw, ch) = (6 + mix.below(12) as i32, 12 + mix.below(24) as i32);
            let ledger_with = |dims: &[Dim]| {
                let mut ledger = InkLedger::EMPTY;
                ledger.grid = (o as f32, o as f32, cw as f32, ch as f32);
                ledger.dims.extend_from_slice(dims);
                ledger
            };
            // Ring boxes of cut cells, as the guard lays its dims.
            let mut dims = Vec::new();
            for _ in 0..1 + mix.below(12) {
                let (col, row) = (mix.below(12) as i32, mix.below(rows as u32) as i32);
                dims.push(Dim {
                    x0: (o + col * cw - RING) as f32,
                    y0: (o + row * ch - RING) as f32,
                    x1: (o + (col + 1) * cw + RING) as f32,
                    y1: (o + (row + 1) * ch + RING) as f32,
                    share: mix.below(128) as f32 / 128.0,
                });
            }
            let end = |mix: &mut Mix| -> (u32, u8) {
                let a = if mix.chance(3) { mix.below(256) } else { 0 };
                let ch = |mix: &mut Mix| mix.below(if a == 0 { 64 } else { a + 1 });
                ((ch(mix) << 16) | (ch(mix) << 8) | ch(mix), a as u8)
            };
            let mut slabs = Vec::new();
            for _ in 0..1 + mix.below(4) {
                let x = (o + mix.below((12 * cw) as u32) as i32) as u16;
                let w = 1 + mix.below((4 * cw) as u32) as u16;
                let y0 = o + mix.below((rows * ch) as u32) as i32;
                let y1 = (y0 + 1 + mix.below((2 * ch) as u32) as i32).min(o + rows * ch);
                let mut slab = Vec::new();
                let mut y = y0;
                while y < y1 {
                    if mix.below(12) == 0 {
                        let h = (2 + mix.below(8) as i32).min(y1 - y);
                        slab.push(Piece::Tall(x, y as u16, w, h as u16, end(&mut mix)));
                        y += h;
                    } else {
                        slab.push(Piece::Row(x, y as u16, w, end(&mut mix), end(&mut mix)));
                        y += 1;
                    }
                }
                slabs.push(slab);
            }
            let grid = (o as u16, ch as u16);
            let mut quads = Vec::new();
            lay(&mut quads, &slabs, grid);
            let mut fresh = ledger_with(&dims);
            fresh.guard_mask_stream(&mut quads, &mut GlowRamps::default(), false);
            gathered_quads += fresh.near_gen;
            let (mut rq, mut ramps) = (Vec::new(), GlowRamps::default());
            {
                let mut sink = RampSink::new(&mut rq, &mut ramps).costed(RampCost::ALWAYS);
                lay(&mut sink, &slabs, grid);
            }
            let mut reused = ledger_with(&dims);
            reused.guard_mask_stream(&mut rq, &mut ramps, false);
            gathered_ramps += reused.near_gen;
            let mut got = Vec::new();
            ramps.expand_into(&rq, &mut got);
            assert_eq!(
                sorted(got),
                sorted(quads),
                "case {case}: the ramps are cut as their quads"
            );
            // One ledger slices box after box, half of them across the box
            // before's columns, as a fresh ledger slices each; the mask over
            // the near dims alone is the mask over every dim.
            let mut ledger = ledger_with(&dims);
            let feather = ledger.feather();
            let mut across = None;
            for _ in 0..24 {
                let (x0, x1) = match across {
                    Some(xs) if mix.chance(5) => xs,
                    _ => {
                        let x0 = mix.below((14 * cw) as u32) as i32;
                        (x0, x0 + 1 + mix.below((4 * cw) as u32) as i32)
                    }
                };
                across = Some((x0, x1));
                let y0 = mix.below(((rows + 1) * ch) as u32) as i32;
                let y1 = y0 + 1 + mix.below(ch as u32) as i32;
                let met = ledger.knots((x0, y0, x1, y1));
                let mut once = ledger_with(&dims);
                assert_eq!(
                    met,
                    once.knots((x0, y0, x1, y1)),
                    "case {case}: a box's dims met"
                );
                if met {
                    assert_eq!(
                        (&ledger.knots_x, &ledger.knots_y),
                        (&once.knots_x, &once.knots_y),
                        "case {case}: box ({x0}, {y0})..({x1}, {y1}) sliced afresh"
                    );
                }
                let points: Vec<(f32, f32)> = if met {
                    let ys: Vec<f32> = ledger
                        .knots_y
                        .windows(2)
                        .map(|k| 0.5 * (k[0] + k[1]) as f32)
                        .collect();
                    let xs: Vec<f32> = ledger
                        .knots_x
                        .iter()
                        .map(|&x| x as f32)
                        .chain(
                            ledger
                                .knots_x
                                .windows(2)
                                .map(|k| 0.5 * (k[0] + k[1]) as f32),
                        )
                        .collect();
                    ys.iter()
                        .flat_map(|&y| xs.iter().map(move |&x| (x, y)))
                        .collect()
                } else {
                    let ym = 0.5 * (y0 + y1) as f32;
                    vec![(x0 as f32, ym), (x1 as f32, ym)]
                };
                for (x, y) in points {
                    let kept = if met {
                        ledger.mask_at_with(x, y, feather)
                    } else {
                        1.0
                    };
                    let near = std::mem::replace(&mut ledger.near, ledger.dims.clone());
                    let every = ledger.mask_at_with(x, y, feather);
                    ledger.near = near;
                    assert_eq!(
                        kept.to_bits(),
                        every.to_bits(),
                        "case {case}: box ({x0}, {y0})..({x1}, {y1}) at ({x}, {y})"
                    );
                }
            }
        }
        assert!(
            gathered_ramps < gathered_quads,
            "premise: the ramps gathered their near dims {gathered_ramps} times, their quads {gathered_quads}"
        );
    }
}
