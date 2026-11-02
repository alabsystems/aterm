// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The yield (`docs/design/PRISM-GLASS.md` §4, §15.2 1.9): app colour wins over the light glass.
//!
//! The glass is solved so the theme fg keeps its bar at every alpha; an app's own ink is not.
//! After the frame's word pass (`EffectsPipeline::yield_glass`, through
//! [`CursorGlow::yield_glass`]), each word of the glass's rows takes a cap `k`: the least,
//! over its inks and the glass laid under it, of the largest alpha at which the ink keeps
//! [`YIELD_BAR`] at every lower alpha ([`cap`]); a cap under [`K_MIN`] is `0`. The glass in the
//! word's cells is scaled by `k / 255`, so it lies at whole strength, at a strength the ink
//! reads over, or not at all. The rules, cell by cell of each row the glass lies on:
//! 1. A ground other than the page (a cell's own bg, a selection, a default bg span off the
//!    page, an image, a wallpaper, a line of another size, an under-text sprite) takes `0`.
//! 2. A theme-fg glyph or a blank takes `255`.
//! 3. A word, a maximal run of other glyphs, takes its cap.
//! 4. An animating ink (a word-pass `InkCell`) takes `0` on every frame it animates: its life.
//! 5. A blank run between two yielded cells is yielded; (5b) so is a run of blanks and theme-fg
//!    glyphs shorter than [`ORPHAN_CELLS`] between two yielded words, or between a yielded word
//!    and the caret, the caret's cell with it. A rule-1 ground is no word and bounds no such run.
//! 6. A blank beside a yielded cell (and no other) eases from the cap back to whole over its
//!    cell ([`RAMP_KNOT_PX`] knots).
//! 7. Only the glass is cut: the record [`GlassLaid`] names it, and no other light (a lip, a
//!    meteor, a star) is touched.
//!
//! A scaled end whose alpha rounds to `0` carries no colour ([`scale_end`]): it would draw as an
//! additive light.
//!
//! That is the colour law ([`GlassYield::apply`]), every glyph's ink judged solid; the hosts'
//! yield keeps it while the glow holds no glyph table. Once a host has observed one
//! ([`CursorGlow::observe_glyph_ink`]) the yield is strict: each glyph, theme-fg ones too, is
//! judged by its raster in the frame's own evidence ([`GlyphInk::for_frame`]), its solid pixels
//! and core keeping their bars as the renderer blends text, and a word holding a glyph no
//! evidence vouches for takes `0`. A measured theme-fg glyph stays plain for rule 5b, and one
//! under `255` bounds an orphan run as a word does. A strict cap is then stepped down until the
//! gradient the lay publishes at it, which rounds apart from the scaled composites, keeps every
//! judged glyph's bars.
//!
//! [`CursorGlow::yield_glass`]: super::CursorGlow::yield_glass
//! [`CursorGlow::observe_glyph_ink`]: super::CursorGlow::observe_glyph_ink

use aterm_core::render::{GlowEnd, GlowQuad, GlowRamp, GlowRamps, Laid, RenderInput};
use aterm_core::terminal::UnderlineStyle;
use aterm_hash::FxHashMap;
use aterm_render::glyph_ink::{FrameGlyphInk, GlyphInkEpoch};
use aterm_render::sprites::{FreeSprite, FreeZ};
use aterm_render::{GlyphCoverage, GlyphInk, GlyphKey};
use aterm_time::Instant;

/// The bar an app's ink keeps over the glass: the glass's own ([`GLASS_BAR`], 5.25:1, on the
/// glyph as it is seen), the `bar` of §4's seam and §15.2 1.9's rules.
///
/// [`GLASS_BAR`]: crate::rainbow_kitty::prism_glass::GLASS_BAR
pub const YIELD_BAR: f64 = crate::rainbow_kitty::prism_glass::GLASS_BAR;

/// The least cap a word keeps: under it the glass leaves the word (§4 rule 3).
pub const K_MIN: u8 = 200;

/// A glass run shorter than this many cells between two yielded words, or a yielded word and
/// the caret, is yielded (§4 rule 5b).
pub const ORPHAN_CELLS: usize = 3;

/// The knots, px, of a blank's ease back to whole beside a yielded cell (§4 rule 6).
pub const RAMP_KNOT_PX: i32 = 2;

/// The grid a glass is laid over: origin x and y, cell width and height, window px.
pub type Grid = (i32, i32, i32, i32);

/// Where a glass was solved and laid: the page `(theme fg, theme bg)`, the [`Grid`], the caret
/// cell and the columns `(first, count)` of the pane the step drew in (`None`: the window's one
/// pane).
pub type Place = ((u32, u32), Grid, Option<(u16, u16)>, Option<(u16, u16)>);

/// The glass a frame laid (§15.2 1.9): its quads and ramps in draw order, as the cursor step
/// left them, and what the yield reads them by.
#[derive(Clone, Debug, Default)]
pub struct GlassLaid {
    /// The cursor step's instant.
    at: Option<Instant>,
    /// The glass's quads.
    quads: Vec<GlowQuad>,
    /// The glass's ramps, each drawn before the quad of [`Self::quads`] its `at` names.
    ramps: GlowRamps,
    /// The page the glass was solved on: `(theme fg, theme bg)`.
    page: (u32, u32),
    /// The grid: origin x and y, cell width and height, window px.
    grid: (i32, i32, i32, i32),
    /// The caret cell.
    caret: Option<(u16, u16)>,
    /// The columns `(first, count)`, window cells, of the pane the step drew in: in a composed
    /// frame the yield reads no cell outside them, another session's. `None`: the whole row.
    pane: Option<(u16, u16)>,
}

impl GlassLaid {
    /// The glass laid at `at`, its `quads` and `ramps` (each ramp's `at` among `quads`), on
    /// `page` `(theme fg, theme bg)`, over `grid` `(origin x, origin y, cell w, cell h)`, the
    /// caret at `caret`.
    #[must_use]
    pub fn new(
        at: Instant,
        (quads, ramps): (Vec<GlowQuad>, GlowRamps),
        page: (u32, u32),
        grid: (i32, i32, i32, i32),
        caret: Option<(u16, u16)>,
    ) -> Self {
        Self {
            at: Some(at),
            quads,
            ramps,
            page: (page.0 & 0x00FF_FFFF, page.1 & 0x00FF_FFFF),
            grid,
            caret,
            pane: None,
        }
    }

    /// This glass laid by a step in the pane whose columns are `pane` (`(first, count)`, window
    /// cells; `None`: the whole row).
    #[must_use]
    pub const fn in_pane(mut self, pane: Option<(u16, u16)>) -> Self {
        self.pane = pane;
        self
    }

    /// Forget the glass: the frame laid none.
    #[inline]
    pub fn clear(&mut self) {
        self.at = None;
        self.quads.clear();
        self.ramps.clear();
        self.caret = None;
        self.pane = None;
    }

    /// The instant of the step that laid it.
    #[inline]
    #[must_use]
    pub const fn at(&self) -> Option<Instant> {
        self.at
    }

    /// Whether it holds no light.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.quads.is_empty() && self.ramps.items.is_empty()
    }

    /// Record the glass of a stream: its quads `[q0, q1)` and ramps `[r0, r1)` of `quads` and
    /// `ramps`, laid at `at` on `page` over `grid`, the caret at `caret`, by a step in `pane`.
    pub(crate) fn record(
        &mut self,
        (quads, ramps): (&[GlowQuad], &GlowRamps),
        (q0, q1, r0, r1): (usize, usize, usize, usize),
        at: Instant,
        (page, grid, caret, pane): Place,
    ) {
        self.clear();
        let q1 = q1.min(quads.len());
        let q0 = q0.min(q1);
        let r1 = r1.min(ramps.items.len());
        let r0 = r0.min(r1);
        self.quads.extend_from_slice(&quads[q0..q1]);
        for r in &ramps.items[r0..r1] {
            let place = (r.at as usize).clamp(q0, q1) - q0;
            self.ramps.copy_rows(r, &ramps.ends, place, (0, r.h));
        }
        if self.is_empty() {
            return;
        }
        self.at = Some(at);
        self.page = (page.0 & 0x00FF_FFFF, page.1 & 0x00FF_FFFF);
        self.grid = grid;
        self.caret = caret;
        self.pane = pane;
    }
}

/// Remove only `glass` from its actual pre-yield publication, preserving every other light.
/// The temporary all-cell selection is restored before the returned input can be rendered.
///
/// # Panics
/// Panics if the temporary selection does not cover every materialized cell and grid column.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn without_body_glass_for_test(input: &RenderInput, glass: &GlassLaid) -> RenderInput {
    use aterm_core::selection::{SelectionSide, SelectionType};

    let mut twin = input.clone();
    let saved = (
        std::mem::take(&mut twin.selection),
        twin.selection_clip.take(),
        twin.selection_bg,
        twin.selection_fg,
        std::mem::take(&mut twin.selections),
    );
    twin.selection
        .start_selection(i32::MIN, 0, SelectionSide::Left, SelectionType::Simple);
    twin.selection
        .update_selection(i32::MAX, u16::MAX, SelectionSide::Right);
    twin.selection.complete_selection();
    assert!(
        twin.cells.iter().enumerate().all(|(row, cells)| {
            (0..cells.len().max(twin.cols))
                .all(|col| twin.selection_hit(row, col, false, false).is_some())
        }),
        "the ablation selects every possible body-glass cell"
    );
    let _ = GlassYield::default().apply(&mut twin, glass, &[]);
    (
        twin.selection,
        twin.selection_clip,
        twin.selection_bg,
        twin.selection_fg,
        twin.selections,
    ) = saved;
    twin
}

/// What a yield cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct YieldReport {
    /// It rewrote the under stream's quads.
    pub quads: bool,
    /// It rewrote the under stream's ramps.
    pub ramps: bool,
    /// The cells whose glass it scaled under whole.
    pub cells: usize,
}

impl YieldReport {
    /// Whether it cut anything.
    #[inline]
    #[must_use]
    pub const fn cut(&self) -> bool {
        self.quads || self.ramps
    }
}

/// A cell's kind, for the rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// Rule 1: a ground the glass never lies on.
    Forced,
    /// A blank cell.
    Blank,
    /// A theme-fg glyph.
    Fg,
    /// A glyph in another ink.
    Ink(u32),
    /// A glyph whose ink the word pass animates.
    Animated,
    /// A cell outside the step's pane (a composed frame's other session): the glass never lies
    /// there, and it is neither a word nor a blank, so it bounds no run and eases no blank.
    Outside,
}

impl Class {
    /// Strict observation measures theme ink too, while retaining its plain-run
    /// classification for rule 5b after that measurement.
    const fn needs_cap(self, strict: bool) -> bool {
        matches!(self, Self::Ink(_)) || strict && matches!(self, Self::Fg)
    }

    /// Whether the cell is a word's glyph the yield caps (rules 3 and 4): another ink, or one
    /// the word pass animates. A rule-1 ground (`Forced`) is no word: rule 5b's orphan runs are
    /// bounded by words and the caret alone.
    const fn worded(self) -> bool {
        matches!(self, Self::Ink(_) | Self::Animated)
    }

    /// Whether the cell is a blank or a theme-fg glyph.
    const fn plain(self) -> bool {
        matches!(self, Self::Blank | Self::Fg)
    }
}

/// A cell's scale: flat at `k`, or (rule 6) easing from `k` at its yielded side to whole.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Scale {
    k: f32,
    /// `0` flat, `1` easing up to the right (yielded on the left), `-1` easing up to the left.
    dir: i8,
}

impl Scale {
    const WHOLE: Self = Self { k: 1.0, dir: 0 };

    /// The scale at `u` across the cell (`0` its left edge, `1` its right).
    fn at(self, u: f32) -> f32 {
        let smooth = |t: f32| {
            let t = t.clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        match self.dir {
            0 => self.k,
            d if d > 0 => self.k + (1.0 - self.k) * smooth(u),
            _ => self.k + (1.0 - self.k) * smooth(1.0 - u),
        }
    }

    fn whole(self) -> bool {
        self.dir == 0 && self.k >= 1.0
    }
}

/// One piece of a stream in draw order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Piece {
    Quad(usize),
    Ramp(usize),
}

/// The yield's scratch and its memo of caps ([`cap`]).
#[derive(Clone, Debug, Default)]
pub struct GlassYield {
    /// Caps by `(ink, glass composite)` on [`Self::memo_page`].
    memo: FxHashMap<(u32, u32), u8>,
    memo_page: u32,
    memo_fg: u32,
    observed_caps: FxHashMap<(GlyphKey, u32, u32, u8), u8>,
    judges: FxHashMap<(GlyphKey, u32), Option<GlyphJudge>>,
    /// The judges' levels, shared across glyphs and kept across glyph epochs.
    level_tables: LevelTables,
    observed_epoch: Option<GlyphInkEpoch>,
    seq_in: Vec<Piece>,
    seq_rec: Vec<Piece>,
    /// Whether each piece of the input stream (by its place in [`Self::seq_in`]) is glass,
    /// and its text row.
    glass: Vec<Option<usize>>,
    /// The rows the glass lies on, ascending, each with its first scale in [`Self::scales`] and
    /// its columns.
    rows: Vec<(usize, usize, usize)>,
    scales: Vec<Scale>,
    classes: Vec<Class>,
    ks: Vec<u8>,
    inks: Vec<u32>,
    sprites: Vec<(i32, i32, i32, i32)>,
    composites: Vec<(u32, u8)>,
    /// A ramp's rows' distinct ends, for [`Self::word_cap`].
    row_ends: Vec<(GlowEnd, GlowEnd)>,
    knots: Vec<i32>,
    out_quads: Vec<GlowQuad>,
    out_ramps: GlowRamps,
}

/// The actual raster pixels that the two existing judges read, with their own
/// clean-page bars. Alpha bytes are retained so every candidate is blended by
/// the renderer's exact text operation rather than an assumed full ink.
///
/// Each level is held as its channel tables ([`aterm_render::TextLevel::channel_tables`]): what
/// the renderer lays over a ground, by the ground's channel. [`Self::cap`] reads every solid
/// level over up to 255 grounds, and reads each distinct ground once. Blending each level
/// afresh for each ground (three sRGB transfers, two of them `powf`, in every `blend_text`)
/// slowed the Rainbow Kitty's glass on GitHub Light. In composed final's gallery the pet's typed
/// line ended 2415 ms after its first key, against 1441 ms before the strict judge, and
/// `yield_glass` took 1910 of a captured instance's 3181 main-thread samples. Every cap is the
/// one reading each alpha in turn through `blend_text` gives
/// (`a_judge_caps_as_every_alpha_read_in_turn`).
///
/// A cap is mostly settled without reading a single alpha ([`Self::settled_cap`]): the glass is
/// animated, its colour and alpha new on almost every frame, so a memo keyed by them missed, and
/// on GitHub Light every frame of the typed line read about 2,300 caps of up to 255 grounds each
/// (some 435,000 ground readings, 12 ms a frame at the median and 47 at the 95th percentile in a
/// debug build, the frames a person saw jitter). The judge's [`Margins`], worked out once per
/// glyph, ink and page, bound every level's reading over a whole box of grounds, and the grounds
/// a glass lays at its alphas lie in the box between the page and its whole composite.
#[derive(Clone, Debug)]
struct GlyphJudge {
    page: u32,
    solid: Vec<LevelChannels>,
    core: LevelChannels,
    solid_bar: f64,
    core_bar: f64,
    /// Whether the bare page keeps every bar: a page that does not caps every glass at `0`.
    page_kept: bool,
    margins: Margins,
}

#[cfg(test)]
std::thread_local! {
    /// The grounds the thread's judges read level by level ([`GlyphJudge::keeps`] past its
    /// margins), for the tests that count them.
    static LEVEL_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// [`LEVEL_READS`], zeroed.
#[cfg(test)]
fn take_level_reads() -> u64 {
    LEVEL_READS.with(|n| n.replace(0))
}

/// Per channel and channel level `v` of a ground, the least over a judge's levels of what that
/// channel adds to the room its level's reading keeps over its bar, taken over every level
/// between `v` and the page's own level in that channel ([`margins`]), rounded down to an `f32`
/// (3 KB a judge). A ground whose three margins sum over [`MARGIN_ROOM`] keeps every bar, and so
/// does every ground in a box of channel levels that holds the page, by the margins of its two
/// corners.
type Margins = std::sync::Arc<[[f32; 256]; 3]>;

/// The most judges a yield holds before it forgets them all: each holds its [`Margins`], 3 KB, and
/// the glyphs a glass lies on near the caret are far fewer.
const JUDGES_HELD: usize = 1 << 12;

/// The room over `0` a sum of [`Margins`] must leave for the judge to take it as kept: over every
/// rounding of the luminance's sums, which are of order `1` in `f64`.
const MARGIN_ROOM: f64 = 1e-9;

/// The [`Margins`] of `levels` (each a level's tables and the bar its reading keeps) on `page`.
///
/// A level's reading of a ground keeps its bar where the ground is the lighter
/// (`(y_g + 0.05) / (y_ink + 0.05) >= bar`), which is
/// `Σ_c w_c (lin(g_c) − bar · lin(t_c(g_c))) >= 0.05 (bar − 1)`: a sum of one term per channel,
/// as WCAG's luminance is. Each channel's least term over the levels, less a third of its level's
/// constant, sums to no more than any one level's whole sum, so a sum of the least terms over
/// `MARGIN_ROOM` keeps every level. A ground darker than its ink is never kept by them: it falls
/// to the levels' own readings.
fn margins(levels: &[(&LevelChannels, f64)], page: u32) -> Margins {
    let lin = crate::rainbow_kitty::prism_glass::colour::linear_levels();
    let weights = [0.2126, 0.7152, 0.0722];
    let mut out = [[f64::INFINITY; 256]; 3];
    for (c, row) in out.iter_mut().enumerate() {
        let w = weights[c];
        for (v, slot) in row.iter_mut().enumerate() {
            for &(tables, bar) in levels {
                let term =
                    w * (lin[v] - bar * lin[usize::from(tables[c][v])]) - 0.05 * (bar - 1.0) / 3.0;
                *slot = slot.min(term);
            }
        }
        // Each level the least over every level between it and the page's.
        let p = ((page >> (16 - 8 * c)) & 0xff) as usize;
        for v in (0..p).rev() {
            row[v] = row[v].min(row[v + 1]);
        }
        for v in p + 1..256 {
            row[v] = row[v].min(row[v - 1]);
        }
    }
    // Rounded down, so a sum of them is never over the one in `f64`.
    let down = |v: f64| {
        let f = v as f32;
        if f64::from(f) > v { f.next_down() } else { f }
    };
    std::sync::Arc::new(out.map(|row| row.map(down)))
}

/// One judged level's channel tables ([`aterm_render::TextLevel::channel_tables`]), shared by
/// every judge of its ink, page and level.
type LevelChannels = std::sync::Arc<[[u8; 256]; 3]>;

/// The judges' channel tables by `(ink, page, level, corrected)`: a level's tables do not depend
/// on the glyph, so the judges of one ink on one page share at most 256 levels' worth.
#[derive(Clone, Debug, Default)]
struct LevelTables(FxHashMap<(u32, u32, u8, bool), LevelChannels>);

impl LevelTables {
    /// The tables of level `t` of `ink` on `page`.
    fn of(&mut self, ink: u32, page: u32, t: u8, corrected: bool) -> LevelChannels {
        if self.0.len() >= 1 << 12 {
            self.0.clear();
        }
        std::sync::Arc::clone(self.0.entry((ink, page, t, corrected)).or_insert_with(|| {
            std::sync::Arc::new(
                aterm_render::TextLevel::new(ink, page, t, corrected).channel_tables(),
            )
        }))
    }
}

impl GlyphJudge {
    #[cfg(test)]
    fn new(
        coverage: &GlyphCoverage,
        ink: u32,
        page: u32,
        theme_fg: u32,
        corrected: bool,
    ) -> Option<Self> {
        Self::new_in(
            &mut LevelTables::default(),
            coverage,
            ink,
            page,
            theme_fg,
            corrected,
        )
    }

    /// The judge of `coverage` in `ink` on `page`, its levels' tables from `tables`.
    fn new_in(
        tables: &mut LevelTables,
        coverage: &GlyphCoverage,
        ink: u32,
        page: u32,
        theme_fg: u32,
        corrected: bool,
    ) -> Option<Self> {
        use crate::rainbow_kitty::prism_glass::colour::{SOLID_SHARE, contrast, luminance};
        let pixel = |a| aterm_render::blend_text(page, ink, page, a, corrected);
        let core_level =
            coverage.core_level(luminance(ink) as f32, luminance(page) as f32, corrected)?;
        let solid_floor = SOLID_SHARE * contrast(pixel(coverage.max), page);
        let solid: Vec<_> = (1..=coverage.max)
            .filter(|&a| {
                coverage.levels[usize::from(a)] > 0 && contrast(pixel(a), page) >= solid_floor
            })
            .collect();
        let own = solid
            .iter()
            .map(|&a| contrast(pixel(a), page))
            .reduce(f64::min)?;
        let solid_bar = if ink == theme_fg {
            YIELD_BAR.min(crate::rainbow_kitty::prism_glass::PAGE_SHARE * own)
        } else {
            YIELD_BAR
        };
        let solid: Vec<_> = solid
            .iter()
            .map(|&a| tables.of(ink, page, a, corrected))
            .collect();
        let core = tables.of(ink, page, core_level, corrected);
        let core_bar = 4.5_f64.min(0.97 * contrast(pixel(core_level), page));
        let levels: Vec<_> = std::iter::once((&core, core_bar))
            .chain(solid.iter().map(|t| (t, solid_bar)))
            .collect();
        let margins = margins(&levels, page);
        let mut judge = Self {
            page,
            solid,
            core,
            solid_bar,
            core_bar,
            page_kept: false,
            margins,
        };
        judge.page_kept = judge.keeps(page);
        Some(judge)
    }

    /// Whether every ground in the box of channel levels `lo ..= hi`, which holds the page's,
    /// keeps every bar by the judge's [`Margins`] alone. Not settled is no verdict: the levels'
    /// own readings give it.
    fn box_kept(&self, lo: [u8; 3], hi: [u8; 3]) -> bool {
        let m = &*self.margins;
        let mut sum = 0.0_f64;
        for c in 0..3 {
            sum += f64::from(m[c][usize::from(lo[c])].min(m[c][usize::from(hi[c])]));
        }
        sum > MARGIN_ROOM
    }

    /// Whether the glyph keeps every bar over `ground`.
    fn keeps(&self, ground: u32) -> bool {
        let at = [16_u32, 8, 0].map(|sh| ((ground >> sh) & 0xff) as u8);
        if self.box_kept(at, at) {
            return true;
        }
        #[cfg(test)]
        LEVEL_READS.with(|n| n.set(n.get() + 1));
        self.keeps_by_levels(ground)
    }

    /// [`Self::keeps`] by each level's own reading of `ground`.
    fn keeps_by_levels(&self, ground: u32) -> bool {
        use crate::rainbow_kitty::prism_glass::colour::{contrast_y, luminance, luminance_rgb};
        let y = luminance(ground);
        let [r, g, b] = [16_u32, 8, 0].map(|sh| ((ground >> sh) & 0xff) as usize);
        // `luminance(blend_text(ground, ink, page, level, corrected))`, read off the tables.
        let reading = |t: &LevelChannels| contrast_y(luminance_rgb([t[0][r], t[1][g], t[2][b]]), y);
        reading(&self.core) >= self.core_bar
            && self.solid.iter().all(|t| reading(t) >= self.solid_bar)
    }

    /// The cap of the glass `(premultiplied colour, alpha)` where it is settled without reading
    /// its alphas: `0` on a page the glyph does not keep its bars on, `255` where the box of
    /// grounds the glass can lay at any alpha keeps them by the judge's margins.
    ///
    /// At alpha `a` the glass lays `over_premul(page, scale_end(glass, a / 255))`: per channel
    /// `C' + round(P · (255 − A') / 255)`, its colour `C' = round(C · a / 255)` and alpha
    /// `A' = round(α · a / 255)` each within half a level of the line from the page `P` to the
    /// whole composite, the composite within half a level of the line's end, and the sum within
    /// half a level more: within two levels of the box between the page and the composite. A
    /// glass whose colour outruns its alpha in a channel (not premultiplied) is clamped there
    /// ([`scale_end`]) and left to the reading.
    fn settled_cap(&self, (color, alpha): (u32, u8)) -> Option<u8> {
        if !self.page_kept {
            return Some(0);
        }
        let ch = |c: u32, sh: u32| ((c >> sh) & 0xff) as u8;
        if [16, 8, 0].iter().any(|&sh| ch(color, sh) > alpha) {
            return None;
        }
        let whole = aterm_render::over_premul(self.page, color & 0x00FF_FFFF, alpha);
        let (mut lo, mut hi) = ([0_u8; 3], [0_u8; 3]);
        for (c, sh) in [16_u32, 8, 0].into_iter().enumerate() {
            let (p, g) = (ch(self.page, sh), ch(whole, sh));
            lo[c] = p.min(g).saturating_sub(2);
            hi[c] = p.max(g).saturating_add(2);
        }
        self.box_kept(lo, hi).then_some(255)
    }

    fn cap(&self, glass: (u32, u8)) -> u8 {
        if let Some(k) = self.settled_cap(glass) {
            return k;
        }
        // Neighbouring alphas round to one ground where the light is faint, and a ground kept
        // once is kept again: each distinct ground is read once.
        let mut kept = self.page;
        for alpha in 1..=255_u16 {
            let (color, coverage) = scale_end(glass.0, glass.1, f32::from(alpha) / 255.0);
            let ground = aterm_render::over_premul(self.page, color, coverage) & 0x00FF_FFFF;
            if ground == kept {
                continue;
            }
            if !self.keeps(ground) {
                return (alpha - 1) as u8;
            }
            kept = ground;
        }
        255
    }
}

/// Push the actual premultiplied color and alpha of each column under the word, so
/// a cap scales source-over light rather than its already-composited RGB.
fn composites_under(
    out: &mut Vec<(u32, u8)>,
    (wx0, wx1): (i32, i32),
    (x, w): (u16, u16),
    (e0, e1): ((u32, u8), (u32, u8)),
) {
    let (x0, x1) = (i32::from(x), i32::from(x) + i32::from(w));
    for px in x0.max(wx0)..x1.min(wx1) {
        let i = (px - x0) as u32;
        let col = aterm_render::glow_lerp_rgb(e0.0, e1.0, i, u32::from(w));
        let alpha = aterm_render::glow_lerp(u32::from(e0.1), u32::from(e1.1), i, u32::from(w));
        let alpha = alpha.min(255) as u8;
        if alpha == 0 && col == 0 {
            continue;
        }
        out.push((col, alpha));
    }
}

/// WCAG's linear light of a channel level `v` in `0 ..= 255`, unrounded.
fn lin(v: f64) -> f64 {
    let c = v / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The WCAG luminance of `rgb`, unrounded channels.
fn luminance(rgb: [f64; 3]) -> f64 {
    0.2126 * lin(rgb[0]) + 0.7152 * lin(rgb[1]) + 0.0722 * lin(rgb[2])
}

/// `c`'s channels.
fn channels(c: u32) -> [f64; 3] {
    [
        f64::from((c >> 16) & 0xff),
        f64::from((c >> 8) & 0xff),
        f64::from(c & 0xff),
    ]
}

/// The cap of `ink` over a glass whose whole composite on `page` is `glass` (§4 rule 3): the
/// largest alpha `k` such that `ink`'s glyph keeps `bar` over the glass at every alpha `1 ..= k`,
/// as it is seen (`prism_glass::colour::seen`: its dimmest solid ink against the ground, as the
/// glass's own bar is read), the glass at alpha `a` laying `page + (glass − page)·a/255`; `0`
/// where the page itself is under the bar, or the ink is no darker than the page (no ink of a
/// light page's app reads lighter). The seen reading rises with the ground's luminance, so the
/// bar is one luminance.
///
/// The luminance along the lerp is convex (each channel is linear in `a` and WCAG's curve is
/// convex), so it falls to its least and rises after: the cap is whole where its least clears
/// the bar, and otherwise the first crossing, found by bisection where it falls.
#[must_use]
pub fn cap(ink: u32, glass: u32, page: u32, bar: f64) -> u8 {
    let (yi, p, g) = (luminance(channels(ink)), channels(page), channels(glass));
    let yp = luminance(p);
    if yi >= yp {
        return 0;
    }
    let y_bar = crate::rainbow_kitty::prism_glass::colour::seen_floor_y(yi, yp, bar);
    if yp < y_bar {
        return 0;
    }
    let at = |t: f64| {
        luminance([
            p[0] + (g[0] - p[0]) * t,
            p[1] + (g[1] - p[1]) * t,
            p[2] + (g[2] - p[2]) * t,
        ])
    };
    // No composite on the way is darker in any channel than the two ends' least.
    if luminance([p[0].min(g[0]), p[1].min(g[1]), p[2].min(g[2])]) >= y_bar {
        return 255;
    }
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..48 {
        let m1 = lo + (hi - lo) / 3.0;
        let m2 = hi - (hi - lo) / 3.0;
        if at(m1) < at(m2) {
            hi = m2;
        } else {
            lo = m1;
        }
    }
    let least = 0.5 * (lo + hi);
    if at(least) >= y_bar && at(1.0) >= y_bar {
        return 255;
    }
    let (mut a, mut b) = (0.0_f64, least);
    for _ in 0..40 {
        let m = 0.5 * (a + b);
        if at(m) >= y_bar {
            a = m;
        } else {
            b = m;
        }
    }
    // The largest alpha whose share is still at or before the crossing.
    let k = (a * 255.0).floor();
    if k >= 255.0 { 255 } else { k as u8 }
}

/// An end `(colour, alpha)` of a premultiplied light scaled by `s` (§4: "a vertex whose scaled
/// alpha rounds to 0 also gets zero colour"): each channel and the alpha rounded, no channel
/// over the alpha, and an alpha that rounds to `0` carrying no colour, which would otherwise
/// draw as an additive light.
#[must_use]
pub fn scale_end(color: u32, alpha: u8, s: f32) -> (u32, u8) {
    let s = s.clamp(0.0, 1.0);
    if s >= 1.0 {
        return (color & 0x00FF_FFFF, alpha);
    }
    let round = |v: u32| (v as f32 * s + 0.5).floor() as u32;
    let a = round(u32::from(alpha)).min(255);
    if a == 0 {
        return (0, 0);
    }
    let ch = |sh: u32| round((color >> sh) & 0xff).min(a);
    ((ch(16) << 16) | (ch(8) << 8) | ch(0), a as u8)
}

/// The end of a horizontal gradient from `(c0, a0)` to `(c1, a1)` across `w` px at `k` px in,
/// as a piece cut there continues it (`icon_shade`'s `push_pieces`).
fn edge_end((c0, a0): (u32, u8), (c1, a1): (u32, u8), k: i32, w: i32) -> (u32, u8) {
    if (c0, a0) == (c1, a1) || k <= 0 {
        return (c0, a0);
    }
    if k >= w {
        return (c1, a1);
    }
    let lerp = |v0: u32, v1: u32| {
        let (v0, v1) = (v0 as i32, v1 as i32);
        ((v0 * (w - k) + v1 * k + w / 2) / w).clamp(0, 255) as u32
    };
    let ch = |sh: u32| lerp((c0 >> sh) & 0xff, (c1 >> sh) & 0xff);
    (
        (ch(16) << 16) | (ch(8) << 8) | ch(0),
        lerp(u32::from(a0), u32::from(a1)) as u8,
    )
}

/// Whether the input quad `a` is the record's `b`, kept whole or narrowed by a clip.
fn quad_of(a: &GlowQuad, b: &GlowQuad) -> bool {
    a.row == b.row
        && (a.color, a.alpha, a.color2, a.alpha2) == (b.color, b.alpha, b.color2, b.alpha2)
        && a.w > 0
        && a.h > 0
        && a.x >= b.x
        && u32::from(a.x) + u32::from(a.w) <= u32::from(b.x) + u32::from(b.w)
        && a.y >= b.y
        && u32::from(a.y) + u32::from(a.h) <= u32::from(b.y) + u32::from(b.h)
}

/// Whether the input ramp `a` (ends in `ae`) is the record's `b` (ends in `be`), kept whole or
/// narrowed by a clip: within its box, each of its rows the record's row there.
fn ramp_of(a: &GlowRamp, ae: &[GlowEnd], b: &GlowRamp, be: &[GlowEnd]) -> bool {
    if a.row != b.row
        || a.w == 0
        || a.h == 0
        || a.x < b.x
        || u32::from(a.x) + u32::from(a.w) > u32::from(b.x) + u32::from(b.w)
        || a.y < b.y
        || u32::from(a.y) + u32::from(a.h) > u32::from(b.y) + u32::from(b.h)
    {
        return false;
    }
    let dy = a.y - b.y;
    (0..a.h).all(|r| a.row_ends(ae, r) == b.row_ends(be, r + dy))
}

/// A stream's pieces in draw order.
fn pieces(quads: usize, ramps: &GlowRamps, out: &mut Vec<Piece>) {
    out.clear();
    ramps.in_order(quads, |laid| match laid {
        Laid::Quads(r) => out.extend(r.map(Piece::Quad)),
        Laid::Ramp(k) => out.push(Piece::Ramp(k)),
    });
}

impl GlassYield {
    /// Yield `glass` in `input`'s under stream to the frame's words (the module's rules), the
    /// frame's free `sprites` beside them; what it cut.
    pub fn apply(
        &mut self,
        input: &mut RenderInput,
        glass: &GlassLaid,
        sprites: &[FreeSprite],
    ) -> YieldReport {
        self.apply_inner(input, glass, sprites, None, false)
    }

    fn apply_observed(
        &mut self,
        input: &mut RenderInput,
        glass: &GlassLaid,
        sprites: &[FreeSprite],
        ink: Option<&GlyphInk>,
        strict: bool,
    ) -> YieldReport {
        self.apply_inner(input, glass, sprites, ink, strict)
    }

    fn apply_inner(
        &mut self,
        input: &mut RenderInput,
        glass: &GlassLaid,
        sprites: &[FreeSprite],
        ink: Option<&GlyphInk>,
        strict: bool,
    ) -> YieldReport {
        let (_, _, cw, ch) = glass.grid;
        if glass.is_empty() || cw <= 0 || ch <= 0 {
            return YieldReport::default();
        }
        if !self.find(input, glass) {
            return YieldReport::default();
        }
        let observed = ink.and_then(|ink| ink.for_frame(input, (cw as usize, ch as usize)));
        let cells = self.solve(input, glass, sprites, observed, strict);
        if cells == 0 {
            return YieldReport::default();
        }
        let (quads, ramps) = self.lay(input, glass);
        YieldReport {
            quads,
            ramps,
            cells,
        }
    }

    /// Mark the input stream's glass ([`Self::glass`]): the record's pieces, each kept whole or
    /// narrowed, met in the input's draw order as one run. Whether any was met.
    fn find(&mut self, input: &RenderInput, glass: &GlassLaid) -> bool {
        let (_, oy, _, ch) = glass.grid;
        let rows = input.cells.len();
        pieces(
            input.glow_under.len(),
            &input.glow_under_ramps,
            &mut self.seq_in,
        );
        pieces(glass.quads.len(), &glass.ramps, &mut self.seq_rec);
        self.glass.clear();
        self.glass.resize(self.seq_in.len(), None);
        let (iq, ir, ie) = (
            &input.glow_under,
            &input.glow_under_ramps.items,
            &input.glow_under_ramps.ends,
        );
        let (gq, gr, ge) = (&glass.quads, &glass.ramps.items, &glass.ramps.ends);
        let row_of = |y: u16| {
            let y = i32::from(y);
            (y >= oy)
                .then(|| ((y - oy) / ch) as usize)
                .filter(|&r| r < rows)
        };
        let (mut next, mut met) = (0usize, false);
        for (pos, piece) in self.seq_in.iter().enumerate() {
            if next >= self.seq_rec.len() {
                break;
            }
            // A piece of no light draws nothing: it neither meets the glass nor ends it.
            let unlit = match *piece {
                Piece::Quad(a) => {
                    let q = &iq[a];
                    q.w == 0 || q.h == 0 || (q.color | q.color2) == 0 && (q.alpha | q.alpha2) == 0
                }
                Piece::Ramp(a) => ir[a].lights(ie) == 0,
            };
            if unlit {
                continue;
            }
            let found = self.seq_rec[next..]
                .iter()
                .position(|rec| match (*piece, *rec) {
                    (Piece::Quad(a), Piece::Quad(b)) => quad_of(&iq[a], &gq[b]),
                    (Piece::Ramp(a), Piece::Ramp(b)) => ramp_of(&ir[a], ie, &gr[b], ge),
                    _ => false,
                });
            match found {
                Some(j) => {
                    next += j + 1;
                    met = true;
                    // The glass stands on its row's bottom: its last pixel row names the row.
                    let (y, h) = match *piece {
                        Piece::Quad(a) => (iq[a].y, iq[a].h),
                        Piece::Ramp(a) => (ir[a].y, ir[a].h),
                    };
                    self.glass[pos] = row_of(y.saturating_add(h.max(1) - 1));
                }
                None if met => break,
                None => {}
            }
        }
        self.glass.iter().any(Option::is_some)
    }

    /// Each glass row's scales ([`Self::rows`], [`Self::scales`]) by the rules; the cells cut.
    fn solve(
        &mut self,
        input: &RenderInput,
        glass: &GlassLaid,
        sprites: &[FreeSprite],
        observed: Option<&FrameGlyphInk>,
        strict: bool,
    ) -> usize {
        let (fg, bg) = glass.page;
        if self.memo_page != bg || self.memo_fg != fg || self.memo.len() > 1 << 14 {
            self.memo.clear();
            self.memo_page = bg;
            self.memo_fg = fg;
            self.observed_caps.clear();
            self.judges.clear();
        }
        let epoch = observed.map(FrameGlyphInk::epoch);
        if self.observed_epoch != epoch
            || self.observed_caps.len() > 1 << 14
            || self.judges.len() > JUDGES_HELD
        {
            self.observed_caps.clear();
            self.judges.clear();
            self.observed_epoch = epoch;
        }
        self.rows.clear();
        self.scales.clear();
        let mut rows: Vec<usize> = self.glass.iter().filter_map(|r| *r).collect();
        rows.sort_unstable();
        rows.dedup();
        let (ox, oy, cw, ch) = glass.grid;
        self.sprites.clear();
        for s in sprites.iter().filter(|s| s.z == FreeZ::UnderText) {
            let x0 = s.x.max(s.clip.x0);
            let y0 = s.y.max(s.clip.y0);
            let x1 = (s.x + i32::from(s.w)).min(s.clip.x1);
            let y1 = (s.y + i32::from(s.h)).min(s.clip.y1);
            if x1 > x0 && y1 > y0 {
                self.sprites.push((x0, y0, x1, y1));
            }
        }
        let mut cut = 0usize;
        for &r in &rows {
            // A row's cells may stop short of the grid: the rest are blanks on the page.
            let cols = input.cells[r].len().max(input.cols);
            let from = self.scales.len();
            self.rows.push((r, from, cols));
            self.classify(input, glass, r, cols);
            // Rule 3: each word's cap, over the glass laid under it.
            self.ks.clear();
            self.ks.resize(cols, 255);
            let mut c = 0;
            while c < cols {
                match self.classes[c] {
                    Class::Forced | Class::Animated => {
                        self.ks[c] = 0;
                        c += 1;
                    }
                    class if class.needs_cap(strict) => {
                        let mut e = c;
                        while e + 1 < cols && self.classes[e + 1].needs_cap(strict) {
                            e += 1;
                        }
                        let k = self.word_cap(input, glass, (r, c, e), (fg, bg), observed, strict);
                        for kk in &mut self.ks[c..=e] {
                            *kk = k;
                        }
                        c = e + 1;
                    }
                    _ => c += 1,
                }
            }
            self.bridge(cols, glass.caret.filter(|&(cr, _)| usize::from(cr) == r));
            // Rule 6: a blank beside one yielded cell eases back to whole across it.
            for c in 0..cols {
                let k = self.ks[c];
                let scale = if k < 255 {
                    Scale {
                        k: f32::from(k) / 255.0,
                        dir: 0,
                    }
                } else if self.classes[c] == Class::Blank {
                    let left = c.checked_sub(1).map(|l| self.ks[l]).filter(|&k| k < 255);
                    let right = self.ks.get(c + 1).copied().filter(|&k| k < 255);
                    match (left, right) {
                        (Some(l), None) => Scale {
                            k: f32::from(l) / 255.0,
                            dir: 1,
                        },
                        (None, Some(rk)) => Scale {
                            k: f32::from(rk) / 255.0,
                            dir: -1,
                        },
                        (Some(l), Some(rk)) => Scale {
                            k: f32::from(l.min(rk)) / 255.0,
                            dir: 0,
                        },
                        (None, None) => Scale::WHOLE,
                    }
                } else {
                    Scale::WHOLE
                };
                if !scale.whole() && self.glass_meets(input, r, c, (ox, oy, cw, ch)) {
                    cut += 1;
                }
                self.scales.push(scale);
            }
        }
        cut
    }

    /// Whether any glass piece of row `r` meets cell `c`.
    fn glass_meets(
        &self,
        input: &RenderInput,
        r: usize,
        c: usize,
        (ox, _, cw, _): (i32, i32, i32, i32),
    ) -> bool {
        let (x0, x1) = (ox + c as i32 * cw, ox + (c as i32 + 1) * cw);
        self.seq_in
            .iter()
            .zip(&self.glass)
            .filter(|(_, g)| **g == Some(r))
            .any(|(p, _)| {
                let (x, w) = match *p {
                    Piece::Quad(a) => (input.glow_under[a].x, input.glow_under[a].w),
                    Piece::Ramp(a) => (
                        input.glow_under_ramps.items[a].x,
                        input.glow_under_ramps.items[a].w,
                    ),
                };
                i32::from(x) < x1 && i32::from(x) + i32::from(w) > x0
            })
    }

    /// Row `r`'s cells' kinds over its `cols` columns ([`Self::classes`]): rules 1, 2 and 4, and
    /// every cell outside the step's pane [`Class::Outside`].
    fn classify(&mut self, input: &RenderInput, glass: &GlassLaid, r: usize, cols: usize) {
        let cells = &input.cells[r];
        let (fg, bg) = glass.page;
        let (_, _, cw, ch) = glass.grid;
        self.classes.clear();
        self.inks.clear();
        self.inks.resize(cols, u32::MAX);
        for ink in input.ink.iter().filter(|i| usize::from(i.row) == r) {
            if let Some(slot) = self.inks.get_mut(usize::from(ink.col)) {
                let [cr, cg, cb] = ink.color;
                *slot = u32::from(cr) << 16 | u32::from(cg) << 8 | u32::from(cb);
            }
        }
        let wallpaper = input.wallpaper.is_some();
        let sized = input
            .line_sizes
            .get(r)
            .is_some_and(|l| *l != Default::default());
        let selected = input.any_selection();
        let images = input.images.get(r);
        let (ry0, ry1) = (r as i32 * ch, (r as i32 + 1) * ch);
        let rgb = |v: [u8; 3]| u32::from(v[0]) << 16 | u32::from(v[1]) << 8 | u32::from(v[2]);
        let pane = glass
            .pane
            .map(|(first, n)| (usize::from(first), usize::from(first) + usize::from(n)));
        for c in 0..cols {
            if pane.is_some_and(|(first, end)| c < first || c >= end) {
                self.classes.push(Class::Outside);
                continue;
            }
            let cell = cells.get(c);
            let page = match input.default_bg_at(r, c) {
                aterm_core::render::COLOR_UNSET => bg,
                live => live & 0x00FF_FFFF,
            };
            let wide = cell.is_some_and(|cell| cell.wide);
            let (cx0, cx1) = (c as i32 * cw, (c as i32 + 1) * cw);
            let forced = wallpaper
                || sized
                || page != bg
                || cell.is_some_and(|cell| rgb(cell.bg) != page)
                || (selected
                    && input
                        .selection_hit(r, c, cells.get(c + 1).is_some_and(|n| n.wide), wide)
                        .is_some())
                || images
                    .is_some_and(|row| !aterm_core::render::images_in_row_at(row, c).is_empty())
                || self
                    .sprites
                    .iter()
                    .any(|&(x0, y0, x1, y1)| x0 < cx1 && x1 > cx0 && y0 < ry1 && y1 > ry0);
            let class = match cell {
                _ if forced => Class::Forced,
                // A wide glyph's right half is its lead's.
                Some(cell) if cell.wide && c > 0 => self.classes[c - 1],
                None => Class::Blank,
                Some(cell) if blank(cell) => Class::Blank,
                Some(_) if self.inks[c] != u32::MAX => Class::Animated,
                Some(cell) if rgb(cell.fg) == fg => Class::Fg,
                Some(cell) => Class::Ink(rgb(cell.fg)),
            };
            self.classes.push(class);
        }
    }

    /// Rules 5 and 5b over [`Self::ks`]: blank runs between yielded cells, and short plain runs
    /// between yielded words or a yielded word and the `caret` column, yielded.
    fn bridge(&mut self, cols: usize, caret: Option<(u16, u16)>) {
        let caret = caret.map(|(_, c)| usize::from(c)).filter(|&c| c < cols);
        let yielded =
            |ks: &[u8], c: Option<usize>| c.and_then(|c| ks.get(c)).copied().filter(|&k| k < 255);
        // Rule 5: blank runs.
        let mut c = 0;
        while c < cols {
            if self.classes[c] != Class::Blank || self.ks[c] < 255 {
                c += 1;
                continue;
            }
            let mut e = c;
            while e + 1 < cols && self.classes[e + 1] == Class::Blank && self.ks[e + 1] == 255 {
                e += 1;
            }
            if let (Some(l), Some(r)) = (
                yielded(&self.ks, c.checked_sub(1)),
                yielded(&self.ks, Some(e + 1)),
            ) {
                for k in &mut self.ks[c..=e] {
                    *k = l.min(r);
                }
            }
            c = e + 1;
        }
        // Rule 5b: short plain runs, bounded by yielded words or a word and the caret.
        let mut c = 0;
        while c < cols {
            if !self.classes[c].plain() || self.ks[c] < 255 {
                c += 1;
                continue;
            }
            let mut e = c;
            while e + 1 < cols && self.classes[e + 1].plain() && self.ks[e + 1] == 255 {
                e += 1;
            }
            let word = |ks: &[u8], classes: &[Class], at: Option<usize>| {
                at.filter(|&a| {
                    a < cols && (classes[a].worded() || classes[a] == Class::Fg && ks[a] < 255)
                })
                .and_then(|a| yielded(ks, Some(a)))
            };
            let (left, right) = (
                word(&self.ks, &self.classes, c.checked_sub(1)),
                word(&self.ks, &self.classes, Some(e + 1)),
            );
            match caret {
                // The caret inside the run: each side of it is bounded by it.
                Some(at) if (c..=e).contains(&at) => {
                    if let Some(l) = left
                        && at - c < ORPHAN_CELLS
                    {
                        for k in &mut self.ks[c..=at] {
                            *k = l;
                        }
                    }
                    if let Some(r) = right
                        && e - at < ORPHAN_CELLS
                    {
                        for k in &mut self.ks[at..=e] {
                            *k = (*k).min(r);
                        }
                    }
                }
                _ => {
                    let run = e - c + 1;
                    let caret_left = caret.is_some_and(|at| at + 1 == c);
                    let caret_right = caret.is_some_and(|at| at == e + 1);
                    let bound = match (left, right) {
                        (Some(l), Some(r)) => Some(l.min(r)),
                        (Some(l), None) if caret_right => Some(l),
                        (None, Some(r)) if caret_left => Some(r),
                        _ => None,
                    };
                    if let Some(k) = bound
                        && run < ORPHAN_CELLS
                    {
                        for kk in &mut self.ks[c..=e] {
                            *kk = k;
                        }
                        // The caret's own cell goes with the run it bounds.
                        if let Some(at) = caret.filter(|_| caret_left || caret_right)
                            && self.classes[at].plain()
                        {
                            self.ks[at] = self.ks[at].min(k);
                        }
                    }
                }
            }
            c = e + 1;
        }
    }

    /// The cap of the word at columns `c ..= e` of row `r` over the glass laid under it: the
    /// least [`cap`] of its inks over each column of each glass piece there, each row of a ramp
    /// by its own ends; `0` under [`K_MIN`].
    fn word_cap(
        &mut self,
        input: &RenderInput,
        glass: &GlassLaid,
        (r, c, e): (usize, usize, usize),
        (fg, bg): (u32, u32),
        observed: Option<&FrameGlyphInk>,
        strict: bool,
    ) -> u8 {
        let (ox, _, cw, _) = glass.grid;
        let (wx0, wx1) = (ox + c as i32 * cw, ox + (e as i32 + 1) * cw);
        self.composites.clear();
        // The source spans under the word, kept beside their per-column composites so a
        // strict cap can be re-judged against the gradient `lay` publishes (below).
        type SourceSpan = (i32, u16, (u32, u8), (u32, u8));
        let mut spans: Vec<SourceSpan> = Vec::new();
        for (p, g) in self.seq_in.iter().zip(&self.glass) {
            if *g != Some(r) {
                continue;
            }
            match *p {
                Piece::Quad(a) => {
                    let q = &input.glow_under[a];
                    spans.push((
                        i32::from(q.x),
                        q.w,
                        (q.color, q.alpha),
                        (q.color2, q.alpha2),
                    ));
                    composites_under(
                        &mut self.composites,
                        (wx0, wx1),
                        (q.x, q.w),
                        ((q.color, q.alpha), (q.color2, q.alpha2)),
                    );
                }
                // Every row's own ends: a wedge row carries its own colours, so a dimmer row of
                // another hue may stand lower against the ink than the ramp's brightest.
                Piece::Ramp(a) => {
                    let ramp = &input.glow_under_ramps.items[a];
                    let ends = &input.glow_under_ramps.ends;
                    self.row_ends.clear();
                    self.row_ends
                        .extend((0..ramp.h).map(|row| ramp.row_ends(ends, row)));
                    self.row_ends.sort_unstable();
                    self.row_ends.dedup();
                    for &(l, rt) in &self.row_ends {
                        let end = |e: GlowEnd| {
                            (
                                aterm_core::render::end_color(e),
                                aterm_core::render::end_alpha(e),
                            )
                        };
                        spans.push((i32::from(ramp.x), ramp.w, end(l), end(rt)));
                        composites_under(
                            &mut self.composites,
                            (wx0, wx1),
                            (ramp.x, ramp.w),
                            (end(l), end(rt)),
                        );
                    }
                }
            }
        }
        self.composites.sort_unstable();
        self.composites.dedup();
        let mut k = 255u8;
        let mut judged: Vec<GlyphJudge> = Vec::new();
        'inks: for col in c..=e {
            let ink = match self.classes[col] {
                Class::Ink(ink) => ink,
                Class::Fg if strict => fg,
                _ => continue,
            };
            if !strict && col > c && self.classes[col - 1] == Class::Ink(ink) {
                continue;
            }
            if !strict {
                for &(color, alpha) in &self.composites {
                    let g = aterm_render::over_premul(bg, color, alpha) & 0x00FF_FFFF;
                    let kk = *self
                        .memo
                        .entry((ink, g))
                        .or_insert_with(|| cap(ink, g, bg, YIELD_BAR));
                    k = k.min(kk);
                    if k < K_MIN {
                        break 'inks;
                    }
                }
                continue;
            }
            if self.composites.is_empty() {
                continue;
            }
            let Some(frame) = observed else {
                return 0;
            };
            let Some((key, coverage)) = frame.coverage(r, col) else {
                return 0;
            };
            if self.observed_caps.len() >= 1 << 14 {
                self.observed_caps.clear();
            }
            if self.judges.len() >= JUDGES_HELD {
                self.judges.clear();
            }
            let tables = &mut self.level_tables;
            let judge = self.judges.entry((key, ink)).or_insert_with(|| {
                GlyphJudge::new_in(tables, coverage, ink, bg, fg, frame.corrected())
            });
            let Some(judge) = judge else {
                return 0;
            };
            judged.push(judge.clone());
            for &(color, alpha) in &self.composites {
                // Settled by the judge's margins as a rule; the memo keeps the caps it reads
                // alpha by alpha, for a glass that holds still.
                let kk = match judge.settled_cap((color, alpha)) {
                    Some(kk) => kk,
                    None => *self
                        .observed_caps
                        .entry((key, ink, color, alpha))
                        .or_insert_with(|| judge.cap((color, alpha))),
                };
                k = k.min(kk);
                if k < K_MIN {
                    break 'inks;
                }
            }
        }
        if strict && k >= K_MIN && !judged.is_empty() {
            // The cap above judges each source column scaled on its own; what `lay`
            // publishes is a gradient between scaled ends, whose interior rounding can
            // land a channel step past the per-column prediction. Re-judge the emitted
            // columns (sub-spans cut at the word's cell edges, as `lay` cuts them) and
            // step the cap down until the published gradient itself keeps every bar.
            let emitted_keeps = |k: u8| {
                let s = f32::from(k) / 255.0;
                spans.iter().all(|&(x, w, e0, e1)| {
                    let (a, b) = (wx0.max(x), wx1.min(x + i32::from(w)));
                    let mut cuts: Vec<i32> = vec![a];
                    cuts.extend(
                        (c..=e + 1)
                            .map(|col| ox + col as i32 * cw)
                            .filter(|&edge| edge > a && edge < b),
                    );
                    cuts.push(b);
                    cuts.windows(2).all(|pair| {
                        let (xa, xb) = (pair[0], pair[1]);
                        let ea = edge_end(e0, e1, xa - x, i32::from(w));
                        let eb = edge_end(e0, e1, xb - x, i32::from(w));
                        let sa = scale_end(ea.0, ea.1, s);
                        let sb = scale_end(eb.0, eb.1, s);
                        let pw = (xb - xa) as u32;
                        (xa..xb).all(|px| {
                            let i = (px - xa) as u32;
                            let ground = aterm_render::over_premul(
                                bg,
                                aterm_render::glow_lerp_rgb(sa.0, sb.0, i, pw),
                                aterm_render::glow_lerp(u32::from(sa.1), u32::from(sb.1), i, pw)
                                    as u8,
                            ) & 0x00FF_FFFF;
                            judged.iter().all(|judge| judge.keeps(ground))
                        })
                    })
                })
            };
            while k >= K_MIN && !emitted_keeps(k) {
                k -= 1;
            }
        }
        if k < K_MIN { 0 } else { k }
    }

    /// The scale at the two ends of `[xa, xb)` by the scales of the row from `row_from`: the
    /// scale of `xa`'s cell, which a span over more than one cell holds whole.
    fn span_scale(
        &self,
        row_from: usize,
        cols: usize,
        grid: (i32, i32, i32, i32),
        (xa, xb): (i32, i32),
    ) -> (f32, f32) {
        let (ox, _, cw, _) = grid;
        if xa < ox {
            return (1.0, 1.0);
        }
        let c = ((xa - ox) / cw) as usize;
        if c >= cols {
            return (1.0, 1.0);
        }
        let s = self.scales[row_from + c];
        let cx0 = ox + c as i32 * cw;
        let u = |x: i32| (x - cx0) as f32 / cw as f32;
        (s.at(u(xa)), s.at(u(xb)))
    }

    /// The knots of a piece over `[x0, x1)` of a row: its ends, each cell edge inside beside a
    /// cut cell, and each easing cell's [`RAMP_KNOT_PX`] knots ([`Self::knots`]); whether any of
    /// its spans is cut.
    fn knot(
        &mut self,
        row_from: usize,
        cols: usize,
        grid: (i32, i32, i32, i32),
        (x0, x1): (i32, i32),
    ) -> bool {
        let (ox, _, cw, _) = grid;
        self.knots.clear();
        self.knots.push(x0);
        if x0 < ox && x1 > ox {
            self.knots.push(ox);
        }
        let mut cut = false;
        let first = if x0 <= ox {
            0
        } else {
            ((x0 - ox) / cw) as usize
        };
        let mut c = first;
        while c < cols {
            let (cx0, cx1) = (ox + c as i32 * cw, ox + (c as i32 + 1) * cw);
            if cx0 >= x1 {
                break;
            }
            if cx1 > x0 {
                let s = self.scales[row_from + c];
                if !s.whole() {
                    cut = true;
                }
                if s.dir != 0 {
                    let mut k = cx0 + RAMP_KNOT_PX;
                    while k < cx1 {
                        if k > x0 && k < x1 {
                            self.knots.push(k);
                        }
                        k += RAMP_KNOT_PX;
                    }
                }
                // A cell edge between two whole cells cuts nothing: the light runs on across it.
                let next_cut = c + 1 < cols && !self.scales[row_from + c + 1].whole();
                if cx1 > x0 && cx1 < x1 && (!s.whole() || next_cut) {
                    self.knots.push(cx1);
                }
            }
            c += 1;
        }
        self.knots.push(x1);
        self.knots.sort_unstable();
        self.knots.dedup();
        cut
    }

    /// Drop the memo of caps and the scratch: the light left (§10.1).
    pub(super) fn forget(&mut self) {
        *self = Self::default();
    }

    /// What the memo holds: its caps, and the judges that settle a kept glass's cap with none.
    #[cfg(any(test, debug_assertions))]
    pub(super) fn memo_len(&self) -> usize {
        self.memo.len() + self.observed_caps.len() + self.judges.len()
    }

    /// Lay the input's under stream again with its glass yielded; whether its quads and its
    /// ramps changed.
    fn lay(&mut self, input: &mut RenderInput, glass: &GlassLaid) -> (bool, bool) {
        let grid = glass.grid;
        let mut out_q = std::mem::take(&mut self.out_quads);
        let mut out_r = std::mem::take(&mut self.out_ramps);
        out_q.clear();
        out_r.clear();
        let (mut quads_cut, mut ramps_cut) = (false, false);
        let seq = std::mem::take(&mut self.seq_in);
        let marks = std::mem::take(&mut self.glass);
        for (p, g) in seq.iter().zip(&marks) {
            let row = g.and_then(|r| {
                self.rows
                    .iter()
                    .find(|(rr, _, _)| *rr == r)
                    .map(|&(_, from, cols)| (from, cols))
            });
            match *p {
                Piece::Quad(a) => {
                    let q = input.glow_under[a];
                    let (x0, x1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
                    match row {
                        Some((from, cols)) if self.knot(from, cols, grid, (x0, x1)) => {
                            quads_cut = true;
                            for i in 0..self.knots.len() - 1 {
                                let (xa, xb) = (self.knots[i], self.knots[i + 1]);
                                let (sa, sb) = self.span_scale(from, cols, grid, (xa, xb));
                                let w = i32::from(q.w);
                                let ea =
                                    edge_end((q.color, q.alpha), (q.color2, q.alpha2), xa - x0, w);
                                let eb =
                                    edge_end((q.color, q.alpha), (q.color2, q.alpha2), xb - x0, w);
                                let (ca, aa) = scale_end(ea.0, ea.1, sa);
                                let (cb, ab) = scale_end(eb.0, eb.1, sb);
                                if (ca, aa, cb, ab) == (0, 0, 0, 0) {
                                    continue;
                                }
                                out_q.push(GlowQuad {
                                    x: xa as u16,
                                    w: (xb - xa) as u16,
                                    color: ca,
                                    alpha: aa,
                                    color2: cb,
                                    alpha2: ab,
                                    ..q
                                });
                            }
                        }
                        _ => out_q.push(q),
                    }
                }
                Piece::Ramp(a) => {
                    let ramp = input.glow_under_ramps.items[a];
                    let ends = &input.glow_under_ramps.ends;
                    let (x0, x1) = (i32::from(ramp.x), i32::from(ramp.x) + i32::from(ramp.w));
                    match row {
                        Some((from, cols)) if self.knot(from, cols, grid, (x0, x1)) => {
                            ramps_cut = true;
                            let kept = ramp.kept_ends(ends);
                            for i in 0..self.knots.len() - 1 {
                                let (xa, xb) = (self.knots[i], self.knots[i + 1]);
                                let (sa, sb) = self.span_scale(from, cols, grid, (xa, xb));
                                let w = i32::from(ramp.w);
                                let ink = out_r.ends.len();
                                for &[l, rt] in kept.as_chunks::<2>().0 {
                                    let e0 = (
                                        aterm_core::render::end_color(l),
                                        aterm_core::render::end_alpha(l),
                                    );
                                    let e1 = (
                                        aterm_core::render::end_color(rt),
                                        aterm_core::render::end_alpha(rt),
                                    );
                                    let ea = edge_end(e0, e1, xa - x0, w);
                                    let eb = edge_end(e0, e1, xb - x0, w);
                                    let (ca, aa) = scale_end(ea.0, ea.1, sa);
                                    let (cb, ab) = scale_end(eb.0, eb.1, sb);
                                    out_r.ends.push(aterm_core::render::glow_end(ca, aa));
                                    out_r.ends.push(aterm_core::render::glow_end(cb, ab));
                                }
                                let piece = GlowRamp {
                                    at: out_q.len() as u32,
                                    x: xa as u16,
                                    w: (xb - xa) as u16,
                                    ink: ink as u32,
                                    ..ramp
                                };
                                let lights = piece.lights(&out_r.ends);
                                if lights == 0 {
                                    out_r.ends.truncate(ink);
                                    continue;
                                }
                                out_r.items.push(piece);
                            }
                        }
                        _ => {
                            let ink = out_r.ends.len();
                            out_r.ends.extend_from_slice(ramp.kept_ends(ends));
                            let piece = GlowRamp {
                                at: out_q.len() as u32,
                                ink: ink as u32,
                                ..ramp
                            };
                            out_r.items.push(piece);
                        }
                    }
                }
            }
        }
        out_r.lit = out_r.count_lit();
        self.seq_in = seq;
        self.glass = marks;
        if quads_cut || ramps_cut {
            std::mem::swap(&mut input.glow_under, &mut out_q);
            std::mem::swap(&mut input.glow_under_ramps, &mut out_r);
        }
        self.out_quads = out_q;
        self.out_ramps = out_r;
        (quads_cut, ramps_cut)
    }
}

/// Whether `cell` draws nothing: a space with no line through, under or over it.
fn blank(cell: &aterm_core::terminal::RenderCell) -> bool {
    matches!(cell.ch, ' ' | '\0')
        && cell.underline == UnderlineStyle::None
        && !cell.strikethrough
        && !cell.overline
}

impl super::CursorGlow {
    /// Record the glass the frame laid ([`GlassLaid::record`]) from its light-owned spans of the
    /// under stream (`InkLedger::note_light_owned_laid`), as the tick leaves them: the letter
    /// guard keeps them whole, so the record is the glass the frame publishes.
    pub(super) fn record_glass(
        &mut self,
        now: Instant,
        cfg: &super::GlowConfig,
        geom: super::Geom,
        cur: Option<(u16, u16)>,
    ) {
        let (quads, ramps) = self.ink_ledger.light_owned();
        if quads.is_empty() && ramps.is_empty() {
            self.forget_glass();
            return;
        }
        let span = |spans: &[(u32, u32)]| {
            spans
                .iter()
                .fold(None, |acc: Option<(usize, usize)>, &(a, b)| {
                    let (a, b) = (a as usize, b as usize);
                    Some(acc.map_or((a, b), |(x, y)| (x.min(a), y.max(b))))
                })
        };
        let (q0, q1) = span(quads).unwrap_or((0, 0));
        let (r0, r1) = span(ramps).unwrap_or((0, 0));
        let grid = (
            i32::from(geom.origin_x),
            i32::from(geom.origin_y),
            geom.cw.min(i32::MAX as usize) as i32,
            geom.ch.min(i32::MAX as usize) as i32,
        );
        self.glass_laid.record(
            (&self.under_out, &self.under_ramps),
            (q0, q1, r0, r1),
            now,
            ((cfg.theme_fg, cfg.theme_bg), grid, cur, self.pane_columns),
        );
    }

    /// Yield the glass the step at `at` laid in `input`'s under stream to the frame's words
    /// (§15.2 1.9, [`GlassYield::apply`]), the frame's free `sprites` beside them: what it cut.
    /// A frame whose step laid no glass is left as it is, and so is the stream this step's last
    /// cut left (a host's second word pass over its own frame), whose cut pieces the record no
    /// longer names. Every other stream holding the step's glass is cut: a copy of the uncut
    /// stream the glow keeps, which a retained projection (a windowed composed capture) and the
    /// heterogeneous route lay, yields as the frame's own did.
    pub fn yield_glass(
        &mut self,
        input: &mut RenderInput,
        sprites: &[FreeSprite],
        at: Instant,
    ) -> YieldReport {
        if self.glass_laid.at() != Some(at) {
            return YieldReport::default();
        }
        if self.yield_spent == Some(at)
            && input.glow_under == self.yield_left.0
            && input.glow_under_ramps == self.yield_left.1
        {
            return YieldReport::default();
        }
        // Strict (actual-raster) caps apply only once a glyph table has been observed:
        // with none ever said, every glyph's ink is judged solid and the colour-only
        // law decides. A table retained while its frame evidence is unavailable stays
        // strict, so its words fail closed rather than borrow the solid reading.
        let report = self.glass_yield.apply_observed(
            input,
            &self.glass_laid,
            sprites,
            self.glyph_frame_available
                .then_some(self.glyph_ink.as_deref())
                .flatten(),
            self.glyph_ink.is_some(),
        );
        if report.cut() {
            self.yield_spent = Some(at);
            self.yield_left.0.clone_from(&input.glow_under);
            self.yield_left.1.clone_from(&input.glow_under_ramps);
            self.yielded = Some((at, report));
        }
        report
    }

    /// Forget the glass the last step laid and what the yield did with it: the record, the last
    /// cut's report and the step whose glass a publish spent (§10.1: light state goes with the
    /// light). Every tick starts by it, so a step that lays no glass, and every dark step, holds
    /// none of them.
    pub(super) fn forget_glass(&mut self) {
        self.glass_laid.clear();
        self.yielded = None;
        self.yield_spent = None;
        self.yield_left.0.clear();
        self.yield_left.1.clear();
    }

    /// The step whose glass a word publish cut ([`Self::yield_glass`]) and what the yield's memo
    /// holds (its caps and judges): `(None, 0)` once the light leaves. Debug and test builds only.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn yield_held(&self) -> (Option<Instant>, usize) {
        (self.yield_spent, self.glass_yield.memo_len())
    }

    /// The last yield that cut: its step's instant and what it cut. `None` until a frame's glass
    /// yielded to its words (§15.2 1.9), and again from the next step that lays no glass, so
    /// `None` on every dark frame, the first after a light one included.
    #[inline]
    #[must_use]
    pub const fn last_yield(&self) -> Option<(Instant, YieldReport)> {
        self.yielded
    }

    /// What the yield cut on the frame whose step ran at `at`, if it cut anything.
    #[inline]
    #[must_use]
    pub fn yielded_at(&self, at: Instant) -> Option<YieldReport> {
        self.yielded.filter(|(when, _)| *when == at).map(|(_, r)| r)
    }

    /// The glass the last step laid ([`GlassLaid`]): empty unless a light arm drew.
    #[inline]
    #[must_use]
    pub const fn glass_laid(&self) -> &GlassLaid {
        &self.glass_laid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_caps_keep_synthetic_ink_readable_after_gradient_rounding() {
        const WIDTH: usize = 15;
        let mut renderer =
            aterm_render::Renderer::from_system(40.0, aterm_render::Theme::default()).unwrap();
        renderer.set_font_subpixel("off");
        renderer.debug_block_on_lazy_fallbacks();
        let (cw, ch) = renderer.cell_size();
        assert!(cw >= WIDTH, "the gradient fits wholly inside one word cell");
        let mut term = Terminal::new(1, 1);
        term.process(b"\x1b[?25l\x1b]10;rgb:2b/2d/33\x07\x1b]11;rgb:f6/f7/f9\x07M");
        let base = term.cell_frame(1, 1);
        let clean = renderer.render_input(&base);
        assert_eq!((clean.width, clean.height), (cw, ch));
        let ink = renderer.glyph_ink_for(&base);
        let frame = ink
            .for_frame(&base, (cw, ch))
            .expect("stable actual glyph key");
        let (key, _) = frame
            .coverage(0, 0)
            .expect("the carrier has an observed raster");
        let (epoch, corrected) = (frame.epoch(), frame.corrected());
        assert!(corrected, "the witness uses corrected text blending");
        let baseline = renderer.baseline();
        let image = renderer.glyph_image(key);
        let ink_top = baseline - image.height() as i32 - image.ymin();
        let ink_bottom = baseline - image.ymin();
        let y = (0..ch)
            .find(|&y| {
                !(ink_top..ink_bottom).contains(&(y as i32))
                    && (0..WIDTH).all(|x| clean.pixels[y * clean.width + x] & 0x00FF_FFFF == BG)
            })
            .expect("a row outside the actual glyph raster exposes only the published ground");
        let palette = crate::rainbow_kitty::prism_glass::Palette::shipped(FG, BG).unwrap();
        let mut quad = GlowQuad::flat(0, 0, 0, WIDTH as u16, ch as u16, palette.glass[34], 255);
        quad.color2 = palette.glass[42];

        for level in [255, 175] {
            // Only the profile is synthetic. Reuse a real key/epoch in this test's
            // private memo so production word-cap, publication and lay run unchanged.
            // This does not claim that the carrier font has this uniform coverage.
            let judge = GlyphJudge::new(&GlyphCoverage::of(&[level; 32]), FG, BG, FG, corrected)
                .expect("nonempty synthetic coverage");
            assert!(
                judge.keeps(BG),
                "the synthetic ink keeps its clean-page bar"
            );
            let at = Instant::now();
            let mut effects = crate::pipeline::EffectsPipeline::for_window();
            let glow = &mut effects.cursor_mut().glow;
            glow.observe_glyph_ink(Some(std::sync::Arc::clone(&ink)));
            glow.under_out = vec![quad];
            glow.glass_laid = GlassLaid::new(
                at,
                (vec![quad], GlowRamps::default()),
                (FG, BG),
                (0, 0, cw as i32, ch as i32),
                None,
            );
            glow.glass_yield.memo_fg = FG;
            glow.glass_yield.memo_page = BG;
            glow.glass_yield.observed_epoch = Some(epoch.clone());
            glow.glass_yield
                .judges
                .insert((key, FG), Some(judge.clone()));
            let mut input = base.clone();
            effects.publish_cursor(&mut input);
            assert_eq!(
                input.glow_under,
                [quad],
                "the actual publisher carries the source gradient"
            );
            let report = effects.cursor_mut().glow.yield_glass(&mut input, &[], at);
            let cap = effects.cursor().glow.glass_yield.ks[0];
            if level == 255 {
                assert_eq!(cap, 255, "the ordinary full-coverage control keeps glass");
                assert!(!report.cut());
            } else {
                assert!(
                    (K_MIN..255).contains(&cap),
                    "the thin profile chooses a finite retained cap"
                );
                assert!(
                    report.cut(),
                    "the actual lay must rewrite the capped gradient"
                );
            }

            // Check the cap's own predictions through its existing sampling and
            // scaling operations, rather than a second interpolation formula.
            let mut predicted = Vec::new();
            composites_under(
                &mut predicted,
                (0, WIDTH as i32),
                (quad.x, quad.w),
                ((quad.color, quad.alpha), (quad.color2, quad.alpha2)),
            );
            for (color, alpha) in predicted {
                let (color, alpha) = scale_end(color, alpha, f32::from(cap) / 255.0);
                assert!(
                    judge.keeps(aterm_render::over_premul(BG, color, alpha)),
                    "the selected cap's predictions must pass before judging its emitted stream"
                );
            }
            let actual = renderer.render_input(&input);
            assert_eq!((actual.width, actual.height), (clean.width, clean.height));
            assert!(
                (0..WIDTH).any(|x| actual.pixels[y * actual.width + x] & 0x00FF_FFFF != BG),
                "the witness must rasterize retained glass"
            );
            for x in 0..WIDTH {
                let ground = actual.pixels[y * actual.width + x] & 0x00FF_FFFF;
                assert!(
                    judge.keeps(ground),
                    "coverage {level}, cap {cap}, x {x}: emitted ground {ground:06x} must keep the unchanged glyph bar"
                );
            }
        }
    }

    #[test]
    fn actual_raster_coverage_keeps_safe_glass_and_caps_thin_ink() {
        let (fg, page) = (0x002B_2D33, 0x00F6_F7F9);
        let glass = crate::rainbow_kitty::prism_glass::Palette::shipped(fg, page)
            .unwrap()
            .glass[0];
        let source = (aterm_render::premul_rgb(glass, 255), 255);
        let full = GlyphJudge::new(&GlyphCoverage::of(&[255; 32]), fg, page, fg, true).unwrap();
        let thin = GlyphJudge::new(&GlyphCoverage::of(&[160; 32]), fg, page, fg, true).unwrap();
        assert_eq!(
            full.cap(source),
            255,
            "safe observed ink retains actual glass"
        );
        assert_eq!(
            cap(fg, glass, page, YIELD_BAR),
            255,
            "the calibrated color-only law accepts it"
        );
        assert!(
            thin.cap(source) < 255,
            "the real thin raster cannot borrow full-ink coverage"
        );
        assert!(GlyphJudge::new(&GlyphCoverage::default(), fg, page, fg, true).is_none());
    }

    /// A judge reads each distinct ground of a glass's alphas once, and each level's corrected
    /// alpha is worked out once ([`aterm_render::TextLevel`]): its cap is still the last alpha
    /// before the first whose ground the glyph does not keep, as reading every alpha in turn
    /// gives. This holds over the glass of Default Light and GitHub Light at every eighth
    /// colour, whole and faint, for a full, a thin and an antialiased glyph, corrected and not.
    #[test]
    fn a_judge_caps_as_every_alpha_read_in_turn() {
        let ramp: Vec<u8> = (0..32_u8).map(|i| i * 8 + 7).collect();
        let (mut read, mut capped) = (0, 0);
        for (fg, page) in [(0x002B_2D33, 0x00F6_F7F9), (0x001F_2328, 0x00FF_FFFF)] {
            let palette = crate::rainbow_kitty::prism_glass::Palette::shipped(fg, page).unwrap();
            for corrected in [true, false] {
                for levels in [&[255_u8; 32][..], &[160; 32], &ramp] {
                    let Some(judge) =
                        GlyphJudge::new(&GlyphCoverage::of(levels), fg, page, fg, corrected)
                    else {
                        continue;
                    };
                    for &glass in palette.glass.iter().step_by(8) {
                        for alpha in [255_u8, 160, 64, 12] {
                            let source = (aterm_render::premul_rgb(glass, alpha), alpha);
                            let ground = |a: u16| {
                                let (c, k) = scale_end(source.0, source.1, f32::from(a) / 255.0);
                                aterm_render::over_premul(page, c, k) & 0x00FF_FFFF
                            };
                            let every = if judge.keeps(page) {
                                (1..=255_u16)
                                    .find(|&a| !judge.keeps(ground(a)))
                                    .map_or(255, |a| (a - 1) as u8)
                            } else {
                                0
                            };
                            assert_eq!(
                                judge.cap(source),
                                every,
                                "#{glass:06x} at {alpha} over {levels:?} (corrected {corrected})"
                            );
                            read += 1;
                            capped += usize::from(every < 255);
                        }
                    }
                }
            }
        }
        assert!(
            read > 100 && capped > 0,
            "fixture: {read} glasses read, {capped} capped"
        );
    }

    /// r25 K8-1, THE LIGHT GLASS'S CAPS COST NO FRAME: the glass is animated, its colour and
    /// alpha new on almost every frame, so the caps' memo missed and every frame of a line typed
    /// on GitHub Light read about 2,300 caps of up to 255 grounds each, level by level (12 ms a
    /// frame at the median, 47 at the 95th percentile, in a debug build: frames a person sees
    /// jitter). A glass a full glyph keeps at every alpha (the solid ink of every glyph's core)
    /// is capped by the judge's margins without one ground read level by level: every glass of
    /// Default Light's and GitHub Light's at each alpha here, in the theme's ink and in an app's,
    /// corrected and not. Every cap, over an antialiased glyph too, is still the one reading
    /// every alpha in turn gives, and a glass the glyph does not keep is read to its first
    /// ground under a bar.
    #[test]
    fn a_kept_glass_is_capped_without_reading_its_alphas() {
        let ramp: Vec<u8> = (0..32_u8).map(|i| i * 8 + 7).collect();
        let (mut settled, mut capped) = (0, 0);
        for (fg, page) in [(0x002B_2D33, 0x00F6_F7F9), (0x001F_2328, 0x00FF_FFFF)] {
            let palette = crate::rainbow_kitty::prism_glass::Palette::shipped(fg, page).unwrap();
            for ink in [fg, 0x0011_1111] {
                for corrected in [true, false] {
                    for levels in [&[255_u8; 32][..], &ramp] {
                        let judge =
                            GlyphJudge::new(&GlyphCoverage::of(levels), ink, page, fg, corrected)
                                .expect("an inked glyph");
                        assert!(judge.page_kept, "fixture: the bare page keeps the glyph");
                        for &glass in &palette.glass {
                            for alpha in [255_u8, 192, 128, 64, 16] {
                                let source = (aterm_render::premul_rgb(glass, alpha), alpha);
                                let every = (1..=255_u16)
                                    .find(|&a| {
                                        let (c, k) =
                                            scale_end(source.0, source.1, f32::from(a) / 255.0);
                                        !judge.keeps_by_levels(
                                            aterm_render::over_premul(page, c, k) & 0x00FF_FFFF,
                                        )
                                    })
                                    .map_or(255, |a| (a - 1) as u8);
                                let _ = take_level_reads();
                                let cap = judge.cap(source);
                                let reads = take_level_reads();
                                assert_eq!(cap, every, "#{glass:06x} at {alpha} over {levels:?}");
                                capped += usize::from(every < 255);
                                if every == 255 && levels[0] == 255 {
                                    assert_eq!(
                                        reads, 0,
                                        "#{glass:06x} at {alpha} (ink #{ink:06x}, corrected \
                                         {corrected}): a kept glass read {reads} grounds level \
                                         by level"
                                    );
                                    settled += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(
            settled > 1000 && capped > 0,
            "fixture: {settled} kept glasses settled, {capped} capped"
        );
    }

    /// r25 K8-1 through the yield: a line of the face's own glyphs under a glass whose colour
    /// and alpha move every frame, as the trail's do, reads no ground level by level on a frame
    /// whose every word keeps the whole glass, on Default Light and GitHub Light.
    #[test]
    fn an_animated_glass_over_a_kept_line_reads_no_alpha() {
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                .expect("the bundled face");
        let (cw, ch) = renderer.cell_size();
        let mut kept_frames = 0;
        for (fg, bg, colours) in [
            (
                0x002B_2D33,
                0x00F6_F7F9,
                b"\x1b]10;rgb:2b/2d/33\x07\x1b]11;rgb:f6/f7/f9\x07",
            ),
            (
                0x001F_2328,
                0x00FF_FFFF,
                b"\x1b]10;rgb:1f/23/28\x07\x1b]11;rgb:ff/ff/ff\x07",
            ),
        ] {
            let mut term = Terminal::new(1, 24);
            term.process(colours);
            term.process(b"echo the quick brown fox");
            let mut base = term.cell_frame(1, 24);
            base.cursor_visible = false;
            let ink = renderer.glyph_ink_for(&base);
            let glasses = crate::rainbow_kitty::prism_glass::Palette::shipped(fg, bg)
                .unwrap()
                .glass;
            let mut yield_ = GlassYield::default();
            for frame in 0..24_usize {
                let alpha = [64_u8, 96, 128][frame % 3];
                let colour = |i: usize| aterm_render::premul_rgb(glasses[i % glasses.len()], alpha);
                let mut quad = GlowQuad::flat(
                    0,
                    0,
                    0,
                    (24 * cw) as u16,
                    ch as u16,
                    colour(frame * 5),
                    alpha,
                );
                quad.color2 = colour(frame * 5 + 40);
                let mut input = base.clone();
                input.glow_under = vec![quad];
                let glass = GlassLaid::new(
                    Instant::now(),
                    (vec![quad], GlowRamps::default()),
                    (fg, bg),
                    (0, 0, cw as i32, ch as i32),
                    None,
                );
                let _ = take_level_reads();
                let _ = yield_.apply_observed(&mut input, &glass, &[], Some(&ink), true);
                let reads = take_level_reads();
                if yield_.ks.iter().all(|&k| k == 255) {
                    assert_eq!(
                        reads, 0,
                        "#{bg:06x} frame {frame}: every word keeps the glass, yet {reads} grounds \
                         were read level by level"
                    );
                    kept_frames += 1;
                }
            }
        }
        assert!(
            kept_frames >= 40,
            "fixture: {kept_frames} frames kept the glass"
        );
    }

    /// Strict observation measures a theme glyph and keeps it plain for rule 5b: a safe one,
    /// which its own raster keeps under the whole glass, is still yielded where rule 5b yields a
    /// run, between two yielded words or between a yielded word and the caret, the caret's cell
    /// with it. A theme glyph touching an app's ink is measured with that ink's word (rule 3
    /// reads a run of glyphs of any ink), so the words that bound a one-cell run here are ones
    /// the word pass animates (rule 4), which no cap measures. The negative control is a run of
    /// [`ORPHAN_CELLS`] cells between two yielded words, ` M `, which is no orphan: the safe glyph
    /// keeps its glass.
    #[test]
    fn observed_safe_theme_ink_still_bridges_between_yielded_words_and_caret() {
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                .expect("the bundled face");
        let red = |cols: std::ops::Range<u16>| {
            cols.map(|col| InkCell {
                row: 0,
                col,
                color: [196, 38, 46],
            })
            .collect::<Vec<_>>()
        };
        // (text, the caret, the word pass's animated cells, the theme glyph's column, the
        // run's columns, whether rule 5b yields it)
        for (text, caret, animated, safe_col, run, bridged) in [
            (
                b"\x1b[38;2;196;38;46mred\x1b[39mM\x1b[38;2;196;38;46mred".as_slice(),
                None,
                [red(0..3), red(4..7)].concat(),
                3,
                3..4,
                true,
            ),
            (
                b"\x1b[38;2;196;38;46mred\x1b[39m M".as_slice(),
                Some((0, 5)),
                Vec::new(),
                4,
                3..5,
                true,
            ),
            (
                b"\x1b[38;2;196;38;46mred\x1b[39m M \x1b[38;2;196;38;46mred".as_slice(),
                None,
                Vec::new(),
                4,
                3..6,
                false,
            ),
        ] {
            let mut input = page(text);
            input.ink = animated;
            let (cw, ch) = renderer.cell_size();
            let ink = renderer.glyph_ink_for(&input);
            let frame = ink
                .for_frame(&input, (cw, ch))
                .expect("actual current glyph evidence");
            let (_, coverage) = frame
                .coverage(0, safe_col)
                .expect("the theme glyph's actual raster");
            assert_eq!(
                GlyphJudge::new(coverage, FG, BG, FG, frame.corrected())
                    .unwrap()
                    .cap(GLASS),
                255,
                "the theme glyph is safe before rule5b bridges it"
            );
            let quad = GlowQuad::flat(0, 0, 0, (24 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
            input.glow_under = vec![quad];
            let glass = GlassLaid::new(
                Instant::now(),
                (vec![quad], GlowRamps::default()),
                (FG, BG),
                (0, 0, cw as i32, ch as i32),
                caret,
            );
            let mut yield_ = GlassYield::default();
            assert!(
                yield_
                    .apply_observed(&mut input, &glass, &[], Some(&ink), true)
                    .cut()
            );
            assert_eq!(
                yield_.classes[safe_col],
                Class::Fg,
                "measurement preserves plain classification"
            );
            assert!(
                yield_.ks[run.start - 1] < 255
                    && yield_.classes[run.start - 1].worded()
                    && (caret.is_some() || yield_.ks[run.end] < 255),
                "premise: a yielded word bounds the run {run:?} on the left, and one or the caret \
                 on the right: {:?}",
                &yield_.ks[..8]
            );
            if bridged {
                assert!(run.len() < ORPHAN_CELLS, "premise: the run is an orphan");
                assert_eq!(
                    yield_.ks[safe_col], 0,
                    "rule5b bridges the safe theme glyph in the run {run:?}"
                );
            } else {
                assert_eq!(run.len(), ORPHAN_CELLS, "premise: the run is no orphan");
                assert_eq!(
                    &yield_.ks[run.clone()],
                    [255; ORPHAN_CELLS],
                    "control: a run of ORPHAN_CELLS keeps its glass, the safe glyph's too"
                );
            }
            if let Some((_, caret_col)) = caret {
                assert_eq!(
                    yield_.ks[usize::from(caret_col)],
                    0,
                    "the orphan's caret yields too"
                );
            }
        }
    }

    #[test]
    fn unavailable_frame_evidence_keeps_the_guard_table_and_yields_glass() {
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default()).unwrap();
        let mut input = page(b"M");
        let (cw, ch) = renderer.cell_size();
        let ink = renderer.glyph_ink_for(&input);
        assert!(
            ink.for_frame(&input, (cw, ch))
                .unwrap()
                .coverage(0, 0)
                .is_some()
        );
        let quad = GlowQuad::flat(0, 0, 0, (24 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
        input.glow_under = vec![quad];
        let at = Instant::now();
        let glass = GlassLaid::new(
            at,
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, cw as i32, ch as i32),
            None,
        );
        let mut known = input.clone();
        assert!(
            !GlassYield::default()
                .apply_observed(&mut known, &glass, &[], Some(&ink), true)
                .cut(),
            "actual safe ink keeps its glass before the renderer becomes unavailable"
        );
        let mut glow = super::super::CursorGlow {
            glass_laid: glass,
            ..Default::default()
        };
        glow.observe_glyph_ink(Some(std::sync::Arc::clone(&ink)));
        glow.observe_glyph_ink_unavailable();
        assert!(
            std::sync::Arc::ptr_eq(glow.glyph_ink.as_ref().unwrap(), &ink),
            "the existing non-glass guard retains its table"
        );
        assert!(
            glow.yield_glass(&mut input, &[], at).cut(),
            "retaining the guard does not confer fresh frame evidence"
        );
        assert_eq!(glow.glass_yield.ks[0], 0);
    }
    use aterm_core::render::InkCell;
    use aterm_core::terminal::Terminal;

    /// Default Light's page.
    const FG: u32 = 0x002B_2D33;
    const BG: u32 = 0x00F6_F7F9;
    /// A warm glass light over the page: premultiplied, `(colour, alpha)`.
    const GLASS: (u32, u8) = (0x005A_4B00, 90);
    /// The cell, px.
    const CW: i32 = 10;
    const CH: i32 = 20;

    /// A frame of `text` on Default Light's page, 3 rows of 24 columns.
    fn page(text: &[u8]) -> RenderInput {
        let mut term = Terminal::new(3, 24);
        term.process(b"\x1b]10;rgb:2b/2d/33\x07\x1b]11;rgb:f6/f7/f9\x07");
        term.process(text);
        let mut input = term.cell_frame(3, 24);
        input.cursor_visible = false;
        assert_eq!(
            input.default_bg & 0x00FF_FFFF,
            BG,
            "premise: the page is Default Light's"
        );
        assert_eq!(
            input.cells[0][0].bg,
            [0xF6, 0xF7, 0xF9],
            "premise: a cell's ground is the page"
        );
        input
    }

    /// The glass over `row`, columns `0 .. cols`: one flat quad, laid as the record names it.
    fn glass_over(
        input: &mut RenderInput,
        row: u16,
        cols: i32,
        at: Instant,
        caret: Option<(u16, u16)>,
    ) -> GlassLaid {
        let quad = GlowQuad::flat(
            row,
            0,
            row * CH as u16,
            (cols * CW) as u16,
            CH as u16,
            GLASS.0,
            GLASS.1,
        );
        input.glow_under = vec![quad];
        input.glow_under_ramps.clear();
        GlassLaid::new(
            at,
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, CW, CH),
            caret,
        )
    }

    /// The glass's alpha at pixel `x` of the stream's quads on `row`, the most of them.
    fn alpha_at(input: &RenderInput, row: u16, x: i32) -> u32 {
        let y = i32::from(row) * CH + CH / 2;
        input
            .glow_under
            .iter()
            .filter(|q| {
                (i32::from(q.x)..i32::from(q.x) + i32::from(q.w)).contains(&x)
                    && (i32::from(q.y)..i32::from(q.y) + i32::from(q.h)).contains(&y)
            })
            .map(|q| {
                aterm_render::glow_lerp(
                    u32::from(q.alpha),
                    u32::from(q.alpha2),
                    (x - i32::from(q.x)) as u32,
                    u32::from(q.w),
                )
            })
            .max()
            .unwrap_or(0)
    }

    /// The alpha at pixel `(x, y)` of the stream's quads, the most of them.
    fn alpha_at_y(input: &RenderInput, x: i32, y: i32) -> u32 {
        input
            .glow_under
            .iter()
            .filter(|q| {
                (i32::from(q.x)..i32::from(q.x) + i32::from(q.w)).contains(&x)
                    && (i32::from(q.y)..i32::from(q.y) + i32::from(q.h)).contains(&y)
            })
            .map(|q| {
                aterm_render::glow_lerp(
                    u32::from(q.alpha),
                    u32::from(q.alpha2),
                    (x - i32::from(q.x)) as u32,
                    u32::from(q.w),
                )
            })
            .max()
            .unwrap_or(0)
    }

    /// The glass's alpha across cell `col` of `row`: its least and its most.
    fn cell_alpha(input: &RenderInput, row: u16, col: i32) -> (u32, u32) {
        (col * CW..(col + 1) * CW).fold((u32::MAX, 0), |(lo, hi), x| {
            let a = alpha_at(input, row, x);
            (lo.min(a), hi.max(a))
        })
    }

    /// Every end the stream lays: a zero alpha carries no colour.
    fn no_coloured_zero(input: &RenderInput) {
        for q in &input.glow_under {
            for (c, a) in [(q.color, q.alpha), (q.color2, q.alpha2)] {
                assert!(a > 0 || c == 0, "an end of alpha 0 carries {c:06x}: {q:?}");
            }
        }
    }

    /// The measured caps (§4 rule 3 and its Measured table, as `docs/design/PRISM-GLASS.md`
    /// §15.8 amendment 49 amends it: each ink's glyph is read as it is seen), over Default
    /// Light's shipped glass at its anchors, whole (alpha 255), each a word's k (a cap under
    /// [`K_MIN`] is 0): the theme's ink `#2b2d33` keeps every anchor whole (the glass is solved
    /// for it); the grey `#474d57` keeps the yellow, the green and the cyan whole and loses the
    /// rest; `#c4262e`, `#0b5cc4`, `#a2269e`, `#5c6370`, `#1a7f37`, `#8a6100`, `#0f7684` and
    /// `#6b7280`, whose glyphs read under the bar as seen on the page itself (`#c4262e` is
    /// 5.34:1 there, 4.41 seen), take 0 at every alpha. An ink lighter than the page, or a page
    /// under the bar itself, keeps no glass either.
    #[test]
    fn the_caps_are_the_measured_ones() {
        use crate::rainbow_kitty::prism_glass::{ANCHOR_AT, Palette};
        let shipped = Palette::shipped(FG, BG).expect("Default Light's tables ship");
        // Red, orange, yellow, green, cyan, azure, lilac, magenta.
        let anchors = ANCHOR_AT.map(|at| shipped.glass_arc[at]);
        let k = |ink: u32, glass: u32| match cap(ink, glass, BG, YIELD_BAR) {
            c if c < K_MIN => 0,
            c => c,
        };
        for (i, &glass) in anchors.iter().enumerate() {
            assert_eq!(k(FG, glass), 255, "#2b2d33 on anchor {i} #{glass:06x}");
            let grey = if (2..=4).contains(&i) { 255 } else { 0 };
            assert_eq!(
                k(0x0047_4D57, glass),
                grey,
                "#474d57 on anchor {i} #{glass:06x}"
            );
            for ink in [
                0x00C4_262E,
                0x000B_5CC4,
                0x00A2_269E,
                0x005C_6370,
                0x001A_7F37,
                0x008A_6100,
                0x000F_7684,
                0x006B_7280,
            ] {
                assert_eq!(
                    cap(ink, glass, BG, YIELD_BAR),
                    0,
                    "#{ink:06x}, under the bar on the page, on anchor {i}"
                );
            }
            assert_eq!(cap(0x00FF_FFFF, glass, BG, YIELD_BAR), 0, "white ink");
        }
        assert_eq!(
            cap(0x00F0_F0F0, BG, BG, YIELD_BAR),
            0,
            "a page under the bar"
        );
        // The bar is §4's 5.25:1 on the glyph as it is seen: the grey #5a5a5a is 6.43:1 on the
        // page itself and 5.27 seen, between the bar and 5.35 (the bar with the glass's lerp
        // guard over it), so a glass that leaves the page no darker keeps it whole, where 5.35
        // would cut it at every alpha; read bare, 5.34:1 #c4262e kept it.
        let seen = |ink: u32| crate::rainbow_kitty::prism_glass::colour::seen(ink, BG, BG);
        assert!(
            (5.25..5.35).contains(&seen(0x005A_5A5A)),
            "{}",
            seen(0x005A_5A5A)
        );
        let pale = aterm_render::over_premul(BG, 0x0005_0505, 3) & 0x00FF_FFFF;
        assert_eq!(
            cap(0x005A_5A5A, pale, BG, YIELD_BAR),
            255,
            "#5a5a5a over #{pale:06x}"
        );
        assert_eq!(cap(0x00C4_262E, pale, BG, YIELD_BAR), 0, "#c4262e as seen");
    }

    /// §4 rules 1-3 and 6: an app's red word takes no glass, the grey word beside it and the
    /// theme's letters keep it whole, and each blank beside the red word eases back to whole
    /// across its cell. Negative control: the same row in the theme's ink keeps its whole glass.
    #[test]
    fn app_colour_wins_over_the_light_glass() {
        let at = Instant::now();
        // "ab red grey cd": a, b at 0-1, red at 3-5, grey at 7-10, c, d at 12-13.
        let mut input =
            page(b"ab \x1b[38;2;196;38;46mred\x1b[39m \x1b[38;2;71;77;87mgrey\x1b[39m cd");
        let glass = glass_over(&mut input, 0, 16, at, None);
        let report = GlassYield::default().apply(&mut input, &glass, &[]);
        assert!(report.quads && !report.ramps, "{report:?}");
        let whole = u32::from(GLASS.1);
        for col in 3..=5 {
            assert_eq!(
                cell_alpha(&input, 0, col),
                (0, 0),
                "the red word's cell {col}"
            );
        }
        for col in [0, 1, 7, 8, 9, 10, 12, 13] {
            assert_eq!(
                cell_alpha(&input, 0, col),
                (whole, whole),
                "cell {col} keeps its glass"
            );
        }
        // The blanks beside the red word ease from it back to whole.
        // Each piece is read at its pixels' centres, so the ease starts a step under whole.
        let (lo, hi) = cell_alpha(&input, 0, 2);
        assert!(
            lo < whole / 4 && hi > whole * 9 / 10,
            "cell 2 eases {lo}..{hi}"
        );
        assert!(alpha_at(&input, 0, 2 * CW) >= alpha_at(&input, 0, 3 * CW - 1));
        let (lo, hi) = cell_alpha(&input, 0, 6);
        assert!(
            lo < whole / 4 && hi > whole * 9 / 10,
            "cell 6 eases {lo}..{hi}"
        );
        assert!(alpha_at(&input, 0, 6 * CW) <= alpha_at(&input, 0, 7 * CW - 1));
        no_coloured_zero(&input);
        // Control: the row in the theme's ink.
        let mut plain = page(b"ab red grey cd");
        let glass = glass_over(&mut plain, 0, 16, at, None);
        let before = plain.glow_under.clone();
        assert_eq!(
            GlassYield::default().apply(&mut plain, &glass, &[]),
            YieldReport::default()
        );
        assert_eq!(
            plain.glow_under, before,
            "control: the theme's row keeps its glass"
        );
    }

    /// §4 rule 5b: `shit.` with `shit` in an app's red and the caret just past the `.`: the
    /// glass between the yielded word and the caret, the caret's cell with it, is yielded, and
    /// the blank past the caret eases back. Negative control: the caret on another row, the
    /// `.` keeps its glass.
    #[test]
    fn an_orphan_glass_run_beside_the_caret_is_yielded() {
        let at = Instant::now();
        let text: &[u8] = b"\x1b[38;2;196;38;46mshit\x1b[39m.";
        let mut input = page(text);
        let glass = glass_over(&mut input, 0, 12, at, Some((0, 5)));
        let report = GlassYield::default().apply(&mut input, &glass, &[]);
        assert!(report.cut(), "{report:?}");
        for col in 0..=5 {
            assert_eq!(
                cell_alpha(&input, 0, col),
                (0, 0),
                "cell {col} through the caret"
            );
        }
        let whole = u32::from(GLASS.1);
        let (lo, hi) = cell_alpha(&input, 0, 6);
        assert!(
            lo < whole / 4 && hi > whole * 9 / 10,
            "cell 6 eases {lo}..{hi}"
        );
        for col in 7..12 {
            assert_eq!(cell_alpha(&input, 0, col), (whole, whole), "cell {col}");
        }
        no_coloured_zero(&input);
        let mut away = page(text);
        let glass = glass_over(&mut away, 0, 12, at, Some((2, 5)));
        let _ = GlassYield::default().apply(&mut away, &glass, &[]);
        assert_eq!(
            cell_alpha(&away, 0, 3),
            (0, 0),
            "control: the word still yields"
        );
        assert_eq!(
            cell_alpha(&away, 0, 4),
            (whole, whole),
            "control: the caret elsewhere, the `.` keeps its glass"
        );
    }

    /// §4 rule 5b is bounded by words and the caret alone: `a two.` with `two` on its own
    /// ground (a rule-1 ground, which takes no glass) and the caret just past the `.` leaves the
    /// `.` its glass, where the same `two` in an app's red (a yielded word) takes the `.` and the
    /// caret's cell with it (the negative control).
    #[test]
    fn a_ground_off_the_page_bounds_no_orphan_run() {
        let at = Instant::now();
        let whole = u32::from(GLASS.1);
        let run = |text: &[u8]| {
            let mut input = page(text);
            let glass = glass_over(&mut input, 0, 12, at, Some((0, 6)));
            let _ = GlassYield::default().apply(&mut input, &glass, &[]);
            input
        };
        let ground = run(b"a \x1b[48;2;220;230;255mtwo\x1b[49m.");
        for col in 2..5 {
            assert_eq!(
                cell_alpha(&ground, 0, col),
                (0, 0),
                "the ground's cell {col}"
            );
        }
        assert_eq!(
            cell_alpha(&ground, 0, 5),
            (whole, whole),
            "the `.` beside a ground keeps its glass"
        );
        let word = run(b"a \x1b[38;2;196;38;46mtwo\x1b[39m.");
        for col in 2..=6 {
            assert_eq!(
                cell_alpha(&word, 0, col),
                (0, 0),
                "control: beside a red word, cell {col} through the caret yields"
            );
        }
    }

    /// A composed row's cells outside the step's pane (another session's) bound nothing: rules 5
    /// and 6 read the pane's own columns. Two blanks at the pane's left edge, between a
    /// neighbour pane on a dark page and an app's red word, keep their glass by the edge, the
    /// one beside the word easing back to whole; the same row recorded with no pane (the whole
    /// row, the negative control) takes the neighbour's off-page cells as yielded and cuts the
    /// two blanks between them and the word.
    #[test]
    fn a_neighbour_pane_bounds_nothing() {
        let at = Instant::now();
        let whole = u32::from(GLASS.1);
        let run = |pane: Option<(u16, u16)>| {
            let mut input = page(b"              \x1b[38;2;196;38;46mtwo\x1b[39m");
            // The neighbour pane's twelve cells, on its own dark ground.
            for cell in &mut input.cells[0][..12] {
                cell.bg = [26, 27, 38];
            }
            let quad = GlowQuad::flat(
                0,
                (12 * CW) as u16,
                0,
                (12 * CW) as u16,
                CH as u16,
                GLASS.0,
                GLASS.1,
            );
            input.glow_under = vec![quad];
            input.glow_under_ramps.clear();
            let glass = GlassLaid::new(
                at,
                (vec![quad], GlowRamps::default()),
                (FG, BG),
                (0, 0, CW, CH),
                None,
            )
            .in_pane(pane);
            let _ = GlassYield::default().apply(&mut input, &glass, &[]);
            input
        };
        let paned = run(Some((12, 12)));
        assert_eq!(
            cell_alpha(&paned, 0, 12),
            (whole, whole),
            "the pane's edge blank keeps its glass"
        );
        let (lo, hi) = cell_alpha(&paned, 0, 13);
        assert!(
            lo < hi,
            "the blank beside the word eases back to whole: {lo} ..= {hi}"
        );
        for col in 14..17 {
            assert_eq!(
                cell_alpha(&paned, 0, col),
                (0, 0),
                "the red word's cell {col}"
            );
        }
        let whole_row = run(None);
        for col in 12..14 {
            assert_eq!(
                cell_alpha(&whole_row, 0, col),
                (0, 0),
                "control: the neighbour's cells bound cell {col}"
            );
        }
    }

    /// §4 rule 3 over a ramp: a word's cap is the least over every row of each ramp under it,
    /// each by its own ends. A navy word over one ramp whose lower rows are a bright yellow and
    /// whose upper rows a blue the navy loses its bar over takes no glass, though the ramp's
    /// brightest rows (the yellow, the most alpha) would keep it whole; the negative control is
    /// the same ramp all yellow, which the word keeps whole.
    #[test]
    fn every_ramp_row_under_a_word_bounds_its_cap() {
        use aterm_render::{BeamRows, RampCost, RampSink};
        const NAVY: u32 = 0x0010_104A;
        const YELLOW: (u32, u8) = (0x00FF_FF00, 255);
        const BLUE: (u32, u8) = (0x0000_00FA, 250);
        let at = Instant::now();
        let lay = |blue_rows: i32| {
            let mut input = page(b"\x1b[38;2;16;16;74mnavy\x1b[39m ok");
            let (mut quads, mut ramps) = (Vec::new(), GlowRamps::default());
            {
                let mut sink = RampSink::new(&mut quads, &mut ramps).costed(RampCost::ALWAYS);
                sink.slab();
                for y in 0..CH {
                    let end = if y < blue_rows { BLUE } else { YELLOW };
                    sink.row(0, 0, (0, y as u16, (12 * CW) as u16), end, end);
                }
            }
            assert!(
                quads.is_empty() && ramps.items.len() == 1,
                "fixture: one ramp"
            );
            input.glow_under.clone_from(&quads);
            input.glow_under_ramps.clone_from(&ramps);
            let glass = GlassLaid::new(at, (quads, ramps), (FG, BG), (0, 0, CW, CH), None);
            let report = GlassYield::default().apply(&mut input, &glass, &[]);
            let mut laid = Vec::new();
            input
                .glow_under_ramps
                .expand_into(&input.glow_under, &mut laid);
            input.glow_under = laid;
            input.glow_under_ramps.clear();
            (input, report)
        };
        let over = |(c, a): (u32, u8)| aterm_render::over_premul(BG, c, a) & 0x00FF_FFFF;
        assert!(
            cap(NAVY, over(YELLOW), BG, YIELD_BAR) == 255
                && cap(NAVY, over(BLUE), BG, YIELD_BAR) < K_MIN,
            "premise: the navy keeps its bar over the yellow and not the blue"
        );
        let (two, report) = lay(CH / 2);
        assert!(report.ramps, "the blue rows cut the ramp: {report:?}");
        for col in 0..4 {
            for row_y in [2, CH - 3] {
                let x = col * CW + CW / 2;
                assert_eq!(
                    alpha_at_y(&two, x, row_y),
                    0,
                    "the navy's cell {col} at y {row_y}"
                );
            }
        }
        let (yellow, report) = lay(0);
        assert_eq!(
            report,
            YieldReport::default(),
            "control: the all-yellow ramp is kept whole"
        );
        assert_eq!(alpha_at_y(&yellow, CW / 2, CH - 3), u32::from(YELLOW.1));
    }

    /// §4: a scaled end whose alpha rounds to 0 carries no colour. A dim premultiplied end
    /// (`#050505` at alpha 3) scaled to a tenth rounds its alpha to 0 and its colour to 1, an
    /// additive light where nothing should be; [`scale_end`] lays it as no light. Each channel
    /// stays under the alpha, and a whole scale leaves the end as it is.
    #[test]
    fn a_zero_scaled_alpha_carries_no_colour() {
        let naive = |c: u32, a: u8, s: f32| {
            let r = |v: u32| (v as f32 * s + 0.5).floor() as u32;
            (
                (r(c >> 16 & 0xff) << 16) | (r(c >> 8 & 0xff) << 8) | r(c & 0xff),
                r(u32::from(a)),
            )
        };
        assert_eq!(naive(0x0005_0505, 3, 0.1), (0x0001_0101, 0), "premise");
        assert_eq!(scale_end(0x0005_0505, 3, 0.1), (0, 0));
        assert_eq!(scale_end(0x0014_1414, 20, 0.5), (0x000A_0A0A, 10));
        assert_eq!(
            scale_end(0x0009_0000, 4, 0.5),
            (0x0002_0000, 2),
            "a channel under its alpha"
        );
        assert_eq!(scale_end(0x0012_3456, 0x80, 1.0), (0x0012_3456, 0x80));
        // Through the yield: a dim glass beside a word eases from 0 without a coloured zero.
        // The word's green `#1a7f37` is under the bar on the page itself (§4 Measured), so it
        // takes 0 over any glass, however dim.
        let at = Instant::now();
        let mut input = page(b"ab \x1b[38;2;26;127;55mgrn\x1b[39m cd");
        let quad = GlowQuad::flat(0, 0, 0, 120, CH as u16, 0x0005_0505, 3);
        input.glow_under = vec![quad];
        let glass = GlassLaid::new(
            at,
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, CW, CH),
            None,
        );
        let report = GlassYield::default().apply(&mut input, &glass, &[]);
        assert!(report.cut(), "premise: the yield cut the dim glass");
        assert!(
            input
                .glow_under
                .iter()
                .any(|q| q.alpha == 0 || q.alpha2 == 0),
            "premise: the word scaled an end of the glass to alpha 0: {:?}",
            input.glow_under
        );
        no_coloured_zero(&input);
    }

    /// §4 rule 4: the word pass's first candy frame over the glass (its cells' ink animated,
    /// [`InkCell`]) takes no glass, so the frame's candy ink stands on the page's own ground at
    /// its bar, whatever its colour is that frame; the theme's word beside it keeps its glass.
    /// Its crown twin waits for 3.10 (§15.2 3.10).
    #[test]
    fn the_first_candy_frame_over_glass_meets_its_bar() {
        let at = Instant::now();
        let mut input = page(b"cat sat");
        let glass = glass_over(&mut input, 0, 10, at, None);
        // The candy's first frame: the word `cat` inked pink, which on the glass is under its bar.
        input.ink = (0..3)
            .map(|col| InkCell {
                row: 0,
                col,
                color: [0xFF, 0x5C, 0xA8],
            })
            .collect();
        let pink_glass = aterm_render::over_premul(BG, GLASS.0, GLASS.1) & 0x00FF_FFFF;
        assert!(
            cap(0x00FF_5CA8, pink_glass, BG, YIELD_BAR) < K_MIN,
            "premise: the frame's candy ink is under its bar on the glass"
        );
        let _ = GlassYield::default().apply(&mut input, &glass, &[]);
        for col in 0..3 {
            assert_eq!(cell_alpha(&input, 0, col), (0, 0), "the candy's cell {col}");
        }
        let whole = u32::from(GLASS.1);
        for col in 4..7 {
            assert_eq!(
                cell_alpha(&input, 0, col),
                (whole, whole),
                "the theme's cell {col}"
            );
        }
        // Control: the same word not animated, in the theme's ink, keeps its glass.
        let mut still = page(b"cat sat");
        let glass = glass_over(&mut still, 0, 10, at, None);
        let _ = GlassYield::default().apply(&mut still, &glass, &[]);
        assert_eq!(cell_alpha(&still, 0, 1), (whole, whole), "control");
    }

    /// §4 rule 1: a selected word, and a cell whose own ground is not the page, takes no glass.
    #[test]
    fn a_ground_off_the_page_takes_no_glass() {
        use aterm_core::selection::{SelectionSide, SelectionType};
        let at = Instant::now();
        let mut term = Terminal::new(3, 24);
        term.process(b"\x1b]10;rgb:2b/2d/33\x07\x1b]11;rgb:f6/f7/f9\x07");
        term.process(b"one \x1b[48;2;220;230;255mtwo\x1b[49m three");
        let selection = term.text_selection_mut();
        selection.start_selection(0, 8, SelectionSide::Left, SelectionType::Simple);
        selection.update_selection(0, 12, SelectionSide::Right);
        selection.complete_selection();
        let mut input = term.cell_frame(3, 24);
        let glass = glass_over(&mut input, 0, 16, at, None);
        let _ = GlassYield::default().apply(&mut input, &glass, &[]);
        let whole = u32::from(GLASS.1);
        for col in [4, 5, 6, 8, 9, 10, 11, 12] {
            assert_eq!(cell_alpha(&input, 0, col), (0, 0), "cell {col}");
        }
        for col in [0, 1, 2, 14, 15] {
            assert_eq!(cell_alpha(&input, 0, col), (whole, whole), "cell {col}");
        }
    }

    /// The yield finds the glass by the record (§4 rule 7): a light the record does not name,
    /// laid on the row after the glass, is never cut.
    #[test]
    fn only_the_glass_yields() {
        let at = Instant::now();
        let mut input = page(b"\x1b[38;2;196;38;46mred\x1b[39m");
        let glass = glass_over(&mut input, 0, 8, at, None);
        let lip = GlowQuad::flat(0, 0, 18, 80, 2, 0x0030_2010, 0x40);
        input.glow_under.push(lip);
        let _ = GlassYield::default().apply(&mut input, &glass, &[]);
        assert_eq!(input.glow_under.last(), Some(&lip), "the lip is laid whole");
        assert!(
            !input.glow_under[..input.glow_under.len() - 1]
                .iter()
                .any(|q| q.x < 30 && q.alpha > 0),
            "the glass under the red word is yielded: {:?}",
            input.glow_under
        );
    }

    #[test]
    fn a_body_only_twin_restores_selections_and_keeps_unrecorded_lights() {
        use aterm_core::render::{PaneSelection, SelectionClip};
        use aterm_core::selection::{SelectionSide, SelectionType};

        let mut input = page(b"ordinary words");
        input
            .selection
            .start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
        input.selection.update_selection(0, 3, SelectionSide::Right);
        input.selection.complete_selection();
        input.selection_clip = Some(SelectionClip::new(0, 1, 1, 4));
        input.selection_bg = 0x0012_3456;
        input.selection_fg = 0x0065_4321;
        input.selections.push(PaneSelection {
            selection: input.selection.clone(),
            clip: SelectionClip::new(0, 1, 8, 12),
            bg: 0x0023_4567,
            fg: 0x0076_5432,
            inactive: true,
        });
        let glass = glass_over(&mut input, 0, 16, Instant::now(), None);
        let lip = GlowQuad::flat(0, 0, 18, 160, 2, 0x0030_2010, 0x40);
        input.glow_under.push(lip);
        input.cursor_glow_add.push(lip);
        assert_eq!(
            without_body_glass_for_test(&input, &GlassLaid::default()),
            input,
            "an absent body record removes no light or selection"
        );
        let twin = without_body_glass_for_test(&input, &glass);
        let mut expected = input.clone();
        expected.glow_under = vec![lip];
        assert_eq!(
            twin, expected,
            "only the recorded body leaves; all five selection fields and other lights survive"
        );
    }
}
