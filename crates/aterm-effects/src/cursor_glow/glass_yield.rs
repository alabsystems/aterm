// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The yield (`docs/design/PRISM-GLASS.md` §4, §15.2 1.9): app colour wins over the light glass.
//!
//! The glass is solved so the theme fg keeps its bar at every alpha; an app's own ink is not.
//! After the frame's word pass (`EffectsPipeline::yield_glass`, through
//! [`CursorGlow::yield_glass`]), each word of the glass's rows takes a cap `k`: the least,
//! over its inks and the glass laid under it, of the largest alpha at which the ink keeps its
//! letter bar at every lower alpha ([`cap`], [`solid_ink_bar`]). The glass in the word's cells is
//! scaled by `k / 255`: a word that holds its bar on the glass at whole strength takes no cut, and
//! one that does not takes the least wash its letters read over, nothing outlining them. The rules,
//! cell by cell of each row the glass lies on:
//! 1. A ground other than the page (a cell's own bg, a selection, a default bg span off the
//!    page, an image, a wallpaper, a line of another size) takes `0`. An under-text sprite (a
//!    companion's body, a word icon's peeking face) takes `0` only where its art draws, pixel by
//!    pixel (`icon_shade::sprite_rows`): it is drawn over the light, cut where its
//!    letters lie, so the glass runs on through its box's clear margin and under its word.
//! 2. A theme-fg glyph or a blank takes `255`.
//! 3. A word, a maximal run of other glyphs, takes its cap flat across its cells. Where the frame's
//!    glyph evidence shows its letters ([`FrameGlyphInk::ink_rows`]), the glass stays whole on the
//!    word's foot rail, the pixel rows past the ring of the line's feet (a round letter's, which
//!    overshoots the baseline), so the run goes on under the word at one hue phase whatever its
//!    cap. A letter whose ink reaches the rail (a descender) takes the cap there within
//!    [`LETTER_RING_PX`] of its ink, easing back to whole over [`LETTER_FEATHER_PX`]
//!    ([`letter_field`]). On the rail, such a word bounds no run of rules 5 and 6.
//! 4. An animating ink (a word-pass `InkCell`) is a word's ink, judged on each frame by what it
//!    reads as that frame ([`animated_reading`]): it holds its bar like any other or takes its
//!    cap, so a word pops neither to a wash nor from one as its animation ends.
//! 5. A blank run between two yielded cells is yielded, at their lesser cap; (5b) so is a run of
//!    blanks and theme-fg glyphs shorter than [`ORPHAN_CELLS`] between two yielded words, or between
//!    a yielded word and the caret, the caret's cell with it. A rule-1 ground is no word and bounds
//!    no such run.
//! 6. A blank eases across its cell between what stands on its two sides ([`RAMP_KNOT_PX`] knots),
//!    holding a side's cap for [`LETTER_RING_PX`], the ring that side's letters are read in.
//! 7. Only the glass is cut: the record [`GlassLaid`] names it, and no other light (a lip, a
//!    meteor, a star) is touched.
//!
//! A scaled end whose alpha rounds to `0` carries no colour ([`scale_end`]): it would draw as an
//! additive light.
//!
//! That is the colour law ([`GlassYield::apply`]), every glyph's ink judged solid; the hosts' yield
//! keeps it while the glow holds no glyph table. Once a host has observed one
//! ([`CursorGlow::observe_glyph_ink`]) the yield is strict: each glyph, theme-fg ones too, is
//! judged by its raster in the frame's own evidence ([`GlyphInk::for_frame`]), its solid pixels and
//! core keeping their bars as the renderer blends text (an app's ink, each solid level the letter
//! bar of its own reading, as the judges read a letter by its solid pixels' median), and a word
//! holding a glyph no evidence vouches for takes `0`. A measured theme-fg glyph stays plain for
//! rule 5b, and one under `255` bounds an orphan run as a word does. A strict cap is then stepped
//! down until the gradient the lay publishes at it, which rounds apart from the scaled composites,
//! keeps every judged glyph's bars.
//!
//! [`CursorGlow::yield_glass`]: super::CursorGlow::yield_glass
//! [`CursorGlow::observe_glyph_ink`]: super::CursorGlow::observe_glyph_ink

use aterm_core::render::{GlowEnd, GlowQuad, GlowRamp, GlowRamps, Laid, RenderInput};
use aterm_core::terminal::UnderlineStyle;
use aterm_hash::FxHashMap;
use aterm_render::glyph_ink::{FrameGlyphInk, GlyphInkEpoch, InkRows};
use aterm_render::sprites::{FreeSprite, FreeZ};
use aterm_render::{GlyphCoverage, GlyphInk, GlyphKey};
use aterm_time::Instant;

/// The bar the theme's ink keeps over the glass: the glass's own ([`GLASS_BAR`], 5.25:1, on the
/// glyph as it is seen), no more than [`PAGE_SHARE`] of what it reads on the page. An app's ink
/// keeps its letter bar ([`solid_ink_bar`]).
///
/// [`PAGE_SHARE`]: crate::rainbow_kitty::prism_glass::PAGE_SHARE
/// [`GLASS_BAR`]: crate::rainbow_kitty::prism_glass::GLASS_BAR
pub const YIELD_BAR: f64 = crate::rainbow_kitty::prism_glass::GLASS_BAR;

/// A glass run shorter than this many cells between two yielded words, or a yielded word and
/// the caret, is yielded (§4 rule 5b).
pub const ORPHAN_CELLS: usize = 3;

/// The knots, px, of a blank's ease back to whole beside a yielded cell (§4 rule 6).
pub const RAMP_KNOT_PX: i32 = 2;

/// How far past a yielded letter's ink, px, the glass keeps the letter's cap (§4 rule 3): the
/// ring a letter's ground is read in, the letter guard's and `trail-contrast`'s
/// ([`crate::cursor_glow::LETTER_GUARD_RING`]). A blank's ease holds its cap this far from its
/// word, and a word's foot rail starts past it.
pub const LETTER_RING_PX: f32 = crate::cursor_glow::LETTER_GUARD_RING as f32;

/// Over how many px past [`LETTER_RING_PX`] the rail eases from a descender's cap back to whole
/// (smoothstep).
pub const LETTER_FEATHER_PX: f32 = 2.0;

/// The step of the grey lattice an animating ink is judged on ([`animated_reading`]).
const ANIMATED_LATTICE: usize = 4;

/// What marks a judge's ink as an animating ink's reading ([`animated_reading`]): the theme's ink
/// has a bar of its own, and a lattice grey that happens to equal it must not take it.
const ANIMATED_READING: u32 = 1 << 24;

#[cfg(any(test, debug_assertions))]
thread_local! {
    static WHOLE_WORDS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static SPRITE_RECTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static NO_LIGHT_BUDGET: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LEAST_LEVEL_BAR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A test's negative control: `f` run with every solid level of an app's ink keeping the letter
/// bar of the glyph's dimmest solid level, as amendment 64 held it, and not each the bar of its own
/// reading. Debug and test builds only.
#[cfg(any(test, debug_assertions))]
pub fn with_least_level_bar<R>(f: impl FnOnce() -> R) -> R {
    LEAST_LEVEL_BAR.with(|c| c.set(true));
    let out = f();
    LEAST_LEVEL_BAR.with(|c| c.set(false));
    out
}

/// Whether each solid level of an app's ink keeps the letter bar of its own reading
/// ([`with_least_level_bar`]).
#[inline]
fn level_bars() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        !LEAST_LEVEL_BAR.with(std::cell::Cell::get)
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        true
    }
}

/// A test's negative control: `f` run with rule 1 taking every cell an under-text sprite's box
/// reaches, as Prism Glass 1.12 did, and no sprite-shaped cut. Debug and test builds only.
#[cfg(any(test, debug_assertions))]
pub fn with_sprite_rect_cut<R>(f: impl FnOnce() -> R) -> R {
    SPRITE_RECTS.with(|c| c.set(true));
    let out = f();
    SPRITE_RECTS.with(|c| c.set(false));
    out
}

/// Whether rule 1 cuts an under-text sprite's glass where its art draws (and not its box).
#[inline]
fn sprite_shaped() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        !SPRITE_RECTS.with(std::cell::Cell::get)
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        true
    }
}

/// A test's negative control: `f` run with a yielded word's cap across every pixel row of its
/// cells, as Prism Glass 1.12 cut it, and no foot rail: the run breaks at the word. Debug and test
/// builds only.
#[cfg(any(test, debug_assertions))]
pub fn with_whole_word_cut<R>(f: impl FnOnce() -> R) -> R {
    WHOLE_WORDS.with(|c| c.set(true));
    let out = f();
    WHOLE_WORDS.with(|c| c.set(false));
    out
}

/// The most lights the yield publishes for a frame, which the step that laid its glass kept to
/// ([`super::CursorGlow::MAX_QUADS`]). On a word's foot rail, every row a descender's ring and
/// feather cross is cut, so a frame's lights grow with its cells' pixels and nothing else bound
/// them.
const YIELD_LIGHT_BUDGET: usize = super::CursorGlow::MAX_QUADS;

/// How long a frame past [`YIELD_LIGHT_BUDGET`] holds the yield to whole words. No railed lay is
/// tried while it holds, so the hold ends up to this long past the last frame over the budget.
pub const WHOLE_WORDS_HOLD: std::time::Duration = std::time::Duration::from_secs(1);

/// What [`GlassYield::lay`] does with lights past [`YIELD_LIGHT_BUDGET`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Budget {
    /// Publishes nothing, for a lay with lights to spare: the whole-word cut.
    Refuse,
    /// Drops the lights past it in draw order, as the step's own lay does.
    Trim,
    /// Publishes them: a test's negative control ([`with_no_light_budget`]).
    Publish,
}

/// A test's negative control: `f` run with the yield publishing however many lights it lays.
/// Debug and test builds only.
#[cfg(any(test, debug_assertions))]
pub fn with_no_light_budget<R>(f: impl FnOnce() -> R) -> R {
    NO_LIGHT_BUDGET.with(|c| c.set(true));
    let out = f();
    NO_LIGHT_BUDGET.with(|c| c.set(false));
    out
}

/// Whether a test has lifted [`YIELD_LIGHT_BUDGET`].
#[inline]
fn unbudgeted() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        NO_LIGHT_BUDGET.with(std::cell::Cell::get)
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        false
    }
}

/// Whether a yielded word whose letters the frame's evidence shows keeps its foot rail.
#[inline]
fn foot_railed() -> bool {
    #[cfg(any(test, debug_assertions))]
    {
        !WHOLE_WORDS.with(std::cell::Cell::get)
    }
    #[cfg(not(any(test, debug_assertions)))]
    {
        true
    }
}

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
        matches!(self, Self::Ink(_) | Self::Animated) || strict && matches!(self, Self::Fg)
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

/// A cell's scale: from `left` at its left edge to `right` at its right, flat where they agree.
/// A cell with a word on a side (rule 6) holds that word's cap for [`LETTER_RING_PX`], the ring
/// its letters are read in, and eases between.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Scale {
    left: f32,
    right: f32,
}

impl Scale {
    #[cfg(test)]
    const WHOLE: Self = Self::flat(1.0);

    const fn flat(k: f32) -> Self {
        Self { left: k, right: k }
    }

    /// The scale at `u` across the cell (`0` its left edge, `1` its right), `hold` of it at each
    /// side held at that side's scale.
    fn at(self, u: f32, hold: f32) -> f32 {
        if self.left == self.right {
            return self.left;
        }
        let t = ((u - hold) / (1.0 - 2.0 * hold).max(f32::EPSILON)).clamp(0.0, 1.0);
        self.left + (self.right - self.left) * (t * t * (3.0 - 2.0 * t))
    }

    fn whole(self) -> bool {
        self.left >= 1.0 && self.right >= 1.0
    }

    fn eases(self) -> bool {
        self.left != self.right
    }
}

/// One piece of a stream in draw order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Piece {
    Quad(usize),
    Ramp(usize),
}

/// A row the glass lies on, as [`GlassYield::lay`] reads its scales.
#[derive(Clone, Copy, Debug)]
struct GlassRow {
    /// The text row.
    row: usize,
    /// Its cells' scales' first in [`GlassYield::scales`].
    from: usize,
    /// The first of the scales of its pixel rows from [`Self::foot_at`] down: its yielded words'
    /// foot rail. [`Self::from`] where it has none.
    foot_from: usize,
    /// The window y its yielded words' foot rail starts at; `i32::MAX` where it has none.
    foot_at: i32,
    /// Its columns.
    cols: usize,
}

impl GlassRow {
    /// `(first scale, columns)` of pixel row `y`.
    const fn cells_at(&self, y: i32) -> (usize, usize) {
        (
            if y >= self.foot_at {
                self.foot_from
            } else {
                self.from
            },
            self.cols,
        )
    }
}

/// A yielded letter that reaches its word's foot rail: the cap its word took, and where its raster
/// lays ink (§4 rule 3).
#[derive(Clone, Debug)]
struct LetterHole {
    /// The cap, as a scale (`k / 255`).
    k: f32,
    /// The letter's cell's top left, window px.
    at: (i32, i32),
    /// Its ink, row by row in the cell.
    ink: InkRows,
}

/// The scale a text row's yielded letters lay over the glass, pixel by pixel over the box their
/// ink and its ring and feather reach ([`letter_field`]): `1` whole, the letter's cap within
/// [`LETTER_RING_PX`] of its ink.
#[derive(Clone, Debug, Default)]
struct LetterField {
    /// The text row.
    row: usize,
    /// The box's left and top, window px.
    x0: i32,
    y0: i32,
    /// Its width and height, px.
    w: i32,
    h: i32,
    /// The scale, row-major.
    s: Vec<f32>,
    /// Whether each pixel lies in a letter's ring or under a sprite, row-major: a pixel whose
    /// scale the yield's law sets, where the others only ease it back to whole.
    ring: Vec<bool>,
}

impl LetterField {
    /// Pixel row `y` of the field, if the field reaches it.
    fn row_at(&self, y: i32) -> Option<FieldRow<'_>> {
        if y < self.y0 || y >= self.y0 + self.h {
            return None;
        }
        let w = self.w as usize;
        let i = (y - self.y0) as usize * w;
        Some(FieldRow {
            s: &self.s[i..i + w],
            ring: &self.ring[i..i + w],
            x0: self.x0,
        })
    }

    /// Whether pixel row `y` scales any of `[x0, x1)`.
    fn scales(&self, y: i32, xs: (i32, i32)) -> bool {
        #[cfg(test)]
        if lay_mutant() == LayMutant::BandRows {
            return self.row_at(y).is_some() && xs.0 < self.x0 + self.w && xs.1 > self.x0;
        }
        self.row_at(y).is_some_and(|row| row.scales(xs))
    }
}

/// One pixel row of a [`LetterField`].
#[derive(Clone, Copy)]
struct FieldRow<'a> {
    /// The scales, the first at window x [`Self::x0`].
    s: &'a [f32],
    /// [`LetterField::ring`]'s row.
    ring: &'a [bool],
    x0: i32,
}

impl FieldRow<'_> {
    /// The index of window x `x` in the row.
    fn at(&self, x: i32) -> Option<usize> {
        usize::try_from(x - self.x0)
            .ok()
            .filter(|&i| i < self.s.len())
    }

    /// The scale at window x `x`; whole past the row.
    fn scale(&self, x: i32) -> f32 {
        self.at(x).map_or(1.0, |i| self.s[i])
    }

    /// Whether window x `x` only eases back to whole: under `1`, lit, in no ring.
    fn eases(&self, x: i32) -> bool {
        #[cfg(test)]
        if lay_mutant() == LayMutant::PixelSpans {
            return false;
        }
        self.at(x)
            .is_some_and(|i| !self.ring[i] && self.s[i] > 0.0 && self.s[i] < 1.0)
    }

    /// The scales a span of the easing pixels `[xa, xb)` starts and ends on, so that its first and
    /// last pixels lay their own ([`aterm_render::glow_lerp`] reads a span at pixel centres), held
    /// to `0 ..= 1`.
    fn eased_ends(&self, xa: i32, xb: i32) -> (f32, f32) {
        let (a, z, pw) = (self.scale(xa), self.scale(xb - 1), (xb - xa) as f32);
        if pw <= 1.0 {
            return (a, a);
        }
        let half = (z - a) / (pw - 1.0) / 2.0;
        ((a - half).clamp(0.0, 1.0), (z + half).clamp(0.0, 1.0))
    }

    /// Whether each pixel of `[xa, xb)` lies within [`FEATHER_SPAN_SLACK`] of its span's line.
    fn runs_straight(&self, xa: i32, xb: i32) -> bool {
        let ((a, z), pw) = (self.eased_ends(xa, xb), (xb - xa) as f32);
        (xa..xb).all(|x| {
            let line = a + (z - a) * ((x - xa) as f32 + 0.5) / pw;
            (line - self.scale(x)).abs() <= FEATHER_SPAN_SLACK
        })
    }

    /// Whether the row scales any of `[x0, x1)`.
    fn scales(&self, (x0, x1): (i32, i32)) -> bool {
        let w = self.s.len() as i32;
        let (a, b) = ((x0 - self.x0).max(0), (x1 - self.x0).min(w));
        a < b && self.s[a as usize..b as usize].iter().any(|&v| v < 1.0)
    }
}

/// How far past a letter's ink, px, its field reaches.
fn letter_reach() -> i32 {
    (LETTER_RING_PX + LETTER_FEATHER_PX).ceil() as i32
}

/// How far, as a scale, a feather pixel may lie from its own scale in a span with its neighbours
/// ([`FieldRow::runs_straight`]): past a letter's ring, where no judge reads, and a fraction of
/// the feather's own step between two pixels.
const FEATHER_SPAN_SLACK: f32 = 0.125;

/// The ease back to whole round one glyph's ink, which every letter of that glyph lays over its
/// own cap: `0` within [`LETTER_RING_PX`] of its ink, `1` from [`LETTER_RING_PX`] +
/// [`LETTER_FEATHER_PX`] of it (smoothstep of the distance), over the box they reach.
#[derive(Clone, Debug)]
struct LetterStamp {
    /// The ink it was read from, held so its address names it while the memo holds it.
    _ink: InkRows,
    /// The box's left and top from the letter's cell, px.
    x0: i32,
    y0: i32,
    /// Its width and height, px.
    w: i32,
    h: i32,
    /// The ease, row-major.
    ease: Vec<f32>,
}

impl LetterStamp {
    /// The stamp of `ink`, its distances read once.
    fn of(ink: &InkRows) -> Self {
        let reach = letter_reach();
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &[y, a, z] in ink.iter() {
            x0 = x0.min(i32::from(a) - reach);
            x1 = x1.max(i32::from(z) + reach);
            y0 = y0.min(i32::from(y) - reach);
            y1 = y1.max(i32::from(y) + 1 + reach);
        }
        let (w, h) = ((x1 - x0).max(0), (y1 - y0).max(0));
        let mut ease = vec![1.0_f32; (w * h) as usize];
        for &[iy, ia, iz] in ink.iter() {
            let (iy, xa, xz) = (i32::from(iy), i32::from(ia), i32::from(iz) - 1);
            for y in (iy - reach).max(y0)..(iy + reach + 1).min(y1) {
                let dy = (y - iy) as f32;
                let at = ((y - y0) * w) as usize;
                for x in (xa - reach).max(x0)..(xz + reach + 1).min(x1) {
                    let dx = (xa - x).max(x - xz).max(0) as f32;
                    let t = (((dx * dx + dy * dy).sqrt() - LETTER_RING_PX) / LETTER_FEATHER_PX)
                        .clamp(0.0, 1.0);
                    let e = &mut ease[at + (x - x0) as usize];
                    *e = e.min(t * t * (3.0 - 2.0 * t));
                }
            }
        }
        Self {
            _ink: std::sync::Arc::clone(ink),
            x0,
            y0,
            w,
            h,
            ease,
        }
    }
}

/// The most glyphs' stamps [`GlassYield`] holds ([`LetterStamp`]).
const LETTER_STAMPS_HELD: usize = 1 << 10;

/// The field of a text row's yielded `letters` on their word's foot rail, the row's pixels from
/// window y `foot_at` down, and its under-text sprites' drawn pixels (`sprites`, each
/// `(x0, x1, y0, y1)` in window px), on the row's pixel band `[ry0, ry1)`: each rail pixel the
/// least, over the letters, of the cap eased back to whole from [`LETTER_RING_PX`] to
/// [`LETTER_RING_PX`] + [`LETTER_FEATHER_PX`] of the letter's ink (each glyph's [`LetterStamp`],
/// read once in `stamps`), and `0` where a sprite draws (rule 1, sprite-shaped: the body drawn
/// over the light keeps its own edge).
fn letter_field(
    letters: &[LetterHole],
    sprites: &[(i32, i32, i32, i32)],
    row: usize,
    ((ry0, ry1), foot_at): ((i32, i32), i32),
    scales: &mut Vec<(Vec<f32>, Vec<bool>)>,
    stamps: &mut FxHashMap<usize, LetterStamp>,
) -> Option<LetterField> {
    let (mut bx0, mut by0, mut bx1, mut by1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for l in letters {
        let key = std::sync::Arc::as_ptr(&l.ink).cast::<[i16; 3]>() as usize;
        if !stamps.contains_key(&key) && stamps.len() >= LETTER_STAMPS_HELD {
            stamps.clear();
        }
        let stamp = stamps.entry(key).or_insert_with(|| LetterStamp::of(&l.ink));
        bx0 = bx0.min(l.at.0 + stamp.x0);
        bx1 = bx1.max(l.at.0 + stamp.x0 + stamp.w);
        by0 = by0.min((l.at.1 + stamp.y0).max(foot_at));
        by1 = by1.max(l.at.1 + stamp.y0 + stamp.h);
    }
    for &(sx0, sx1, sy0, sy1) in sprites {
        bx0 = bx0.min(sx0);
        bx1 = bx1.max(sx1);
        by0 = by0.min(sy0);
        by1 = by1.max(sy1);
    }
    if bx0 >= bx1 {
        return None;
    }
    let (x0, x1) = (bx0, bx1);
    let (y0, y1) = (by0.max(ry0), by1.min(ry1));
    if y0 >= y1 {
        return None;
    }
    let (w, h) = (x1 - x0, y1 - y0);
    let (mut s, mut ring) = scales.pop().unwrap_or_default();
    s.clear();
    s.resize((w * h) as usize, 1.0);
    ring.clear();
    ring.resize((w * h) as usize, false);
    for l in letters {
        let key = std::sync::Arc::as_ptr(&l.ink).cast::<[i16; 3]>() as usize;
        let Some(stamp) = stamps.get(&key) else {
            continue;
        };
        let (bx, by) = (l.at.0 + stamp.x0, l.at.1 + stamp.y0);
        for y in by.max(y0).max(foot_at)..(by + stamp.h).min(y1) {
            let from = ((y - by) * stamp.w) as usize;
            let line = &stamp.ease[from..from + stamp.w as usize];
            let at = ((y - y0) * w + (bx - x0)) as usize;
            for (i, &e) in line.iter().enumerate() {
                if e < 1.0 {
                    let px = at + i;
                    s[px] = s[px].min(l.k + (1.0 - l.k) * e);
                    ring[px] |= e <= 0.0;
                }
            }
        }
    }
    for &(sx0, sx1, sy0, sy1) in sprites {
        for y in sy0.max(y0)..sy1.min(y1) {
            let at = ((y - y0) * w) as usize;
            for x in sx0.max(x0)..sx1.min(x1) {
                let i = at + (x - x0) as usize;
                s[i] = 0.0;
                ring[i] = true;
            }
        }
    }
    Some(LetterField {
        row,
        x0,
        y0,
        w,
        h,
        s,
        ring,
    })
}

/// The yield's scratch and its memo of caps ([`cap`]).
#[derive(Clone, Debug, Default)]
pub struct GlassYield {
    /// Caps by `(ink, glass composite)` on [`Self::memo_page`].
    memo: FxHashMap<(u32, u32), u8>,
    memo_page: u32,
    memo_fg: u32,
    /// Caps by `(word, glass composite)`, a word by its id in [`Self::word_ids`], each as far as
    /// it was read ([`CapRead`]).
    observed_caps: FxHashMap<(u32, u32, u8), CapRead>,
    judges: FxHashMap<(GlyphKey, u32), Option<std::sync::Arc<GlyphJudge>>>,
    /// The words [`Self::observed_caps`] holds caps of, by their glyphs' `(key, ink)`.
    word_ids: FxHashMap<Vec<(GlyphKey, u32)>, u32>,
    /// The word [`Self::strict_word_cap`] judges: its glyphs, and their judges read as one.
    word_keys: Vec<(GlyphKey, u32)>,
    word: WordJudge,
    /// What each word read of each ground, by its id in [`Self::word_ids`]
    /// ([`WordJudge::keeps_once`]): a function of the word and the ground alone, so it holds across
    /// frames, which a word at its cap, its grounds at its bars, would read every one of.
    kept: FxHashMap<(u32, u32), bool>,
    /// The judges' levels, shared across glyphs and kept across glyph epochs.
    level_tables: LevelTables,
    observed_epoch: Option<GlyphInkEpoch>,
    seq_in: Vec<Piece>,
    seq_rec: Vec<Piece>,
    /// Whether each piece of the input stream (by its place in [`Self::seq_in`]) is glass,
    /// and its text row.
    glass: Vec<Option<usize>>,
    /// The rows the glass lies on, ascending ([`GlassRow`]).
    rows: Vec<GlassRow>,
    scales: Vec<Scale>,
    classes: Vec<Class>,
    /// Whether each cell is a wide glyph's right half, whose glyph is its lead's.
    tails: Vec<bool>,
    ks: Vec<u8>,
    /// What a word's foot rail keeps of [`Self::ks`]: a rule-1 ground and a word no evidence
    /// shows the letters of.
    foot_ks: Vec<u8>,
    inks: Vec<u32>,
    /// The boxes `(x0, y0, x1, y1)`, grid px, of the frame's under-text sprites, under
    /// [`with_sprite_rect_cut`] only: rule 1 as Prism Glass 1.12 read it.
    sprites: Vec<(i32, i32, i32, i32)>,
    /// The rows the frame's under-text sprites draw (rule 1, sprite-shaped), each its drawn
    /// texels' span `(x0, x1, y0, y1)`, grid px ([`super::icon_shade::sprite_rows`]).
    sprite_rows: Vec<(i32, i32, i32, i32)>,
    /// One sprite's rows, as [`super::icon_shade::sprite_rows`] reads them.
    sprite_scratch: Vec<(i32, i32, i32, i32)>,
    /// A text row's share of [`Self::sprite_rows`], window px.
    holes: Vec<(i32, i32, i32, i32)>,
    composites: Vec<(u32, u8)>,
    /// A ramp's rows' distinct ends, for [`Self::word_cap`].
    row_ends: Vec<(GlowEnd, GlowEnd)>,
    /// The source spans under the word [`Self::word_cap`] caps.
    under_spans: Vec<SourceSpan>,
    /// The rows [`Self::solve`] walks.
    glass_rows: Vec<usize>,
    knots: Vec<i32>,
    /// The spans of a piece's row between [`Self::knots`]: `(xa, xb, scale at xa, at xb)`.
    spans: Vec<(i32, i32, f32, f32)>,
    /// The frame's yielded letters that reach their word's foot rail, by text row (§4 rule 3).
    letters: Vec<LetterHole>,
    /// The fields of [`Self::letters`], one a text row that holds any.
    fields: Vec<LetterField>,
    /// The last fields' scales, to lay the next in.
    field_scales: Vec<(Vec<f32>, Vec<bool>)>,
    /// Each glyph's stamp by its ink's address ([`LetterStamp`]).
    letter_stamps: FxHashMap<usize, LetterStamp>,
    /// The time of the last frame a railed lay ran past [`YIELD_LIGHT_BUDGET`], which holds
    /// the yield to whole words for [`WHOLE_WORDS_HOLD`].
    whole_words_since: Option<Instant>,
    out_quads: Vec<GlowQuad>,
    out_ramps: GlowRamps,
}

/// The actual raster pixels that the two existing judges read, with their own
/// clean-page bars. Alpha bytes are retained so every candidate is blended by
/// the renderer's exact text operation rather than an assumed full ink.
///
/// Each level is held as its channel tables ([`aterm_render::TextLevel::channel_tables`]): what
/// the renderer lays over a ground, by the ground's channel. A cap ([`WordJudge::cap`]) reads every
/// solid level over up to 255 grounds, and reads each distinct ground once. Blending each level
/// afresh for each ground (three sRGB transfers, two of them `powf`, in every `blend_text`)
/// slowed the Rainbow Kitty's glass on GitHub Light. In composed final's gallery the pet's typed
/// line ended 2415 ms after its first key, against 1441 ms before the strict judge, and
/// `yield_glass` took 1910 of a captured instance's 3181 main-thread samples. Every cap is the
/// one reading each alpha in turn through `blend_text` gives
/// (`a_judge_caps_as_every_alpha_read_in_turn`).
///
/// A cap is mostly settled without reading a single alpha ([`WordJudge::settled`]): the glass is
/// animated, its colour and alpha new on almost every frame, so a memo keyed by them missed, and
/// on GitHub Light every frame of the typed line read about 2,300 caps of up to 255 grounds each
/// (some 435,000 ground readings, 12 ms a frame at the median and 47 at the 95th percentile in a
/// debug build, the frames a person saw jitter). The judge's [`Margins`], worked out once per
/// glyph, ink and page, bound every level's reading over a whole box of grounds, and the grounds
/// a glass lays at its alphas lie in the box between the page and its whole composite.
#[derive(Clone, Debug)]
struct GlyphJudge {
    #[cfg(test)]
    page: u32,
    /// Each solid level's tables and the bar its reading keeps.
    solid: Vec<(LevelChannels, f64)>,
    core: LevelChannels,
    core_bar: f64,
    /// Whether the bare page keeps every bar: a page that does not caps every glass at `0`.
    page_kept: bool,
    margins: Margins,
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
        let animated = ink & ANIMATED_READING != 0;
        let ink = ink & 0x00FF_FFFF;
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
        let solid_bars: Vec<f64> = if ink == theme_fg && !animated {
            let bar = YIELD_BAR.min(crate::rainbow_kitty::prism_glass::PAGE_SHARE * own);
            vec![bar; solid.len()]
        } else {
            // An app's ink: each solid level keeps the letter bar of its own reading. The judges
            // read a letter by the lower median of its solid pixels in a row, so a bar from its
            // dimmest level alone let a letter they read at AA fall under it (after-land round 3,
            // W1: `w` #b43fb4 on Default Light read 4.40 of its own 4.50 under a word's wash).
            // A level that reads 4.495 to 4.563 on the page (AA as the judges read it, under its
            // level in hand) keeps its bar at no glass, so a glyph holding one lies on the page
            // under every glass however high its full reading: its solid levels reach down to
            // `SOLID_SHARE` of that, so inks up to about 5.37 (GitHub Light's #0969da at 5.19
            // loses 9 of a to z at 24 px). The judge holds levels, not rows, so any solid level
            // may be the median a row reads (PRISM-GLASS amendment 65, the land review's A-A1).
            let darkest = animated_darkest(ink);
            let reading = |a: u8| {
                if animated {
                    contrast(
                        aterm_render::blend_text(page, darkest, page, a, corrected),
                        page,
                    )
                } else {
                    contrast(pixel(a), page)
                }
            };
            let least = solid
                .iter()
                .map(|&a| reading(a))
                .fold(f64::INFINITY, f64::min);
            solid
                .iter()
                .map(|&a| {
                    let read = if level_bars() { reading(a) } else { least };
                    let bar = f64::from(crate::cursor_glow::letter_bar(read as f32, false));
                    #[cfg(test)]
                    let bar = if law_mutant() == LawMutant::FlatBar {
                        YIELD_BAR
                    } else {
                        bar
                    };
                    bar
                })
                .collect()
        };
        let solid: Vec<_> = solid
            .iter()
            .zip(solid_bars)
            .map(|(&a, bar)| (tables.of(ink, page, a, corrected), bar))
            .collect();
        let core = tables.of(ink, page, core_level, corrected);
        // The core keeps the letter bar of its own reading too, as the letter guard holds a
        // core: the judges read a letter by the lower of its core and its strokes.
        let core_bar = f64::from(crate::cursor_glow::letter_bar(
            contrast(pixel(core_level), page) as f32,
            false,
        ));
        let levels: Vec<_> = std::iter::once((&core, core_bar))
            .chain(solid.iter().map(|(t, bar)| (t, *bar)))
            .collect();
        let margins = margins(&levels, page);
        let mut judge = Self {
            #[cfg(test)]
            page,
            solid,
            core,
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

    /// Whether the glyph keeps every bar over `ground`: by its margins, or each level's reading.
    /// The yield reads a glyph as its word's ([`WordJudge::keeps`]).
    fn keeps(&self, ground: u32) -> bool {
        self.keeps_by_margins(ground) || self.keeps_by_levels(ground)
    }

    /// [`Self::keeps`] by the judge's margins alone. Not kept is no verdict.
    #[inline]
    fn keeps_by_margins(&self, ground: u32) -> bool {
        let at = [16_u32, 8, 0].map(|sh| ((ground >> sh) & 0xff) as u8);
        self.box_kept(at, at)
    }

    /// [`Self::keeps`] by each level's own reading of `ground`.
    fn keeps_by_levels(&self, ground: u32) -> bool {
        use crate::rainbow_kitty::prism_glass::colour::{contrast_y, luminance, luminance_rgb};
        let y = luminance(ground);
        let [r, g, b] = [16_u32, 8, 0].map(|sh| ((ground >> sh) & 0xff) as usize);
        // `luminance(blend_text(ground, ink, page, level, corrected))`, read off the tables.
        let reading = |t: &LevelChannels| {
            count_level_read();
            contrast_y(luminance_rgb([t[0][r], t[1][g], t[2][b]]), y)
        };
        reading(&self.core) >= self.core_bar && self.solid.iter().all(|(t, bar)| reading(t) >= *bar)
    }

    /// The cap of the glass `(premultiplied colour, alpha)` where it is settled without reading
    /// its alphas: `0` on a page the glyph does not keep its bars on, `255` where the box of
    /// grounds the glass can lay at any alpha ([`glass_box`]) keeps them by the judge's margins.
    #[cfg(test)]
    fn settled_cap(&self, glass: (u32, u8)) -> Option<u8> {
        if !self.page_kept {
            return Some(0);
        }
        let (lo, hi) = glass_box(self.page, glass)?;
        self.box_kept(lo, hi).then_some(255)
    }

    /// The glyph's cap alone: the yield reads a word's as one ([`WordJudge::cap`]).
    #[cfg(test)]
    fn cap(&self, glass: (u32, u8)) -> u8 {
        if let Some(k) = self.settled_cap(glass) {
            return k;
        }
        cap_along(self.page, glass, (0, 255), |ground| self.keeps(ground))
    }
}

/// `k`, stepped down until the gradient `lay` publishes over the word at columns `c ..= e` keeps
/// what `keeps` keeps. A cap judges each source column scaled on its own;
/// what `lay` publishes is a gradient between scaled ends, whose interior rounding can land a
/// channel step past the per-column prediction, so the emitted columns are re-judged (sub-spans
/// cut at the word's cell edges, as `lay` cuts them).
fn published_cap(
    mut k: u8,
    spans: &[SourceSpan],
    (ox, _, cw, _): Grid,
    (c, e): (usize, usize),
    bg: u32,
    mut keeps: impl FnMut(u32) -> bool,
) -> u8 {
    let (wx0, wx1) = (ox + c as i32 * cw, ox + (e as i32 + 1) * cw);
    let mut emitted_keeps = |k: u8| {
        let s = f32::from(k) / 255.0;
        spans.iter().all(|&(x, w, e0, e1)| {
            let (a, b) = (wx0.max(x), wx1.min(x + i32::from(w)));
            let mut xa = a;
            (c..=e + 1)
                .map(|col| ox + col as i32 * cw)
                .filter(|&edge| edge > a && edge < b)
                .chain(std::iter::once(b))
                .all(|xb| {
                    let ea = edge_end(e0, e1, xa - x, i32::from(w));
                    let eb = edge_end(e0, e1, xb - x, i32::from(w));
                    let sa = scale_end(ea.0, ea.1, s);
                    let sb = scale_end(eb.0, eb.1, s);
                    let pw = (xb - xa) as u32;
                    // A faint gradient lays one ground over many pixels: it is read once.
                    let mut read = None;
                    let kept = (xa..xb).all(|px| {
                        let i = (px - xa) as u32;
                        let ground = aterm_render::over_premul(
                            bg,
                            aterm_render::glow_lerp_rgb(sa.0, sb.0, i, pw),
                            aterm_render::glow_lerp(u32::from(sa.1), u32::from(sb.1), i, pw) as u8,
                        ) & 0x00FF_FFFF;
                        read == Some(ground) || {
                            read = Some(ground);
                            keeps(ground)
                        }
                    });
                    xa = xb;
                    kept
                })
        })
    };
    while k > 0 && !emitted_keeps(k) {
        k -= 1;
    }
    k
}

/// The box `(lo, hi)` of channel levels that holds every ground the glass `(premultiplied colour,
/// alpha)` lays over `page` at any alpha; `None` for a glass whose colour outruns its alpha in a
/// channel (not premultiplied), which [`scale_end`] clamps, and which is left to the reading.
///
/// At alpha `a` the glass lays `over_premul(page, scale_end(glass, a / 255))`: per channel
/// `C' + round(P · (255 − A') / 255)`, its colour `C' = round(C · a / 255)` and alpha
/// `A' = round(α · a / 255)` each within half a level of the line from the page `P` to the
/// whole composite, the composite within half a level of the line's end, and the sum within
/// half a level more: within two levels of the box between the page and the composite.
fn glass_box(page: u32, (color, alpha): (u32, u8)) -> Option<([u8; 3], [u8; 3])> {
    let ch = |c: u32, sh: u32| ((c >> sh) & 0xff) as u8;
    if [16, 8, 0].iter().any(|&sh| ch(color, sh) > alpha) {
        return None;
    }
    let whole = aterm_render::over_premul(page, color & 0x00FF_FFFF, alpha);
    let (mut lo, mut hi) = ([0_u8; 3], [0_u8; 3]);
    for (c, sh) in [16_u32, 8, 0].into_iter().enumerate() {
        let (p, g) = (ch(page, sh), ch(whole, sh));
        lo[c] = p.min(g).saturating_sub(2);
        hi[c] = p.max(g).saturating_add(2);
    }
    Some((lo, hi))
}

/// The cap of a glass whose composite over `page` is `glass`, by what `keeps` keeps, up to
/// `bound`: the largest alpha `k <= bound` whose every ground over `page` at alpha `1 ..= k` it
/// keeps; `0` where it does not keep the page itself. The alphas `1 ..= settled` are known to keep,
/// and none past `bound` is read.
fn cap_along(
    page: u32,
    glass: (u32, u8),
    (settled, bound): (u16, u8),
    mut keeps: impl FnMut(u32) -> bool,
) -> u8 {
    if !keeps(page) {
        return 0;
    }
    // Neighbouring alphas round to one ground where the light is faint, and a ground kept once
    // is kept again: each distinct ground is read once.
    let mut kept = page;
    for alpha in settled + 1..=u16::from(bound) {
        let (color, coverage) = scale_end(glass.0, glass.1, f32::from(alpha) / 255.0);
        let ground = aterm_render::over_premul(page, color, coverage) & 0x00FF_FFFF;
        if ground == kept {
            continue;
        }
        if !keeps(ground) {
            return (alpha - 1) as u8;
        }
        kept = ground;
    }
    bound
}

/// The source spans under a word, `(x, w, left end, right end)`, kept beside their per-column
/// composites so a strict cap is re-judged against the gradient `lay` publishes.
type SourceSpan = (i32, u16, (u32, u8), (u32, u8));

/// A word's glyph judges read as one (after-land R0, on r25 K8-1's margins): each distinct glyph
/// once, each composite's box once, and each ground once.
///
/// A glass every glyph's [`Margins`] keep at all its alphas is settled in a few lookups a glyph
/// ([`Self::settled`]), which is most of what a line under the glass needs. Past the margins (a
/// glass near or over a glyph's bar) a ground is read by the levels of the glyphs whose margins do
/// not keep it, and what a ground read is held for the word's other composites and frames
/// ([`Self::keeps_once`]): the word's composites are its columns' and its pieces' rows', and their
/// grounds coincide where the light is faint and between neighbouring columns.
///
/// Judged glyph by glyph with no margins, each glyph scanned every composite's grounds on its own
/// and read every level of its own: in the profile of a GitHub Light gallery instance,
/// `yield_glass` held 1467 of the 1519 main-thread samples its captures took, 1419 of them in
/// `GlyphJudge::cap`, and the paper ribbons' frames ran 150 to 300 ms late.
#[derive(Clone, Debug, Default)]
struct WordJudge {
    /// The judges of the word's distinct glyphs.
    glyphs: Vec<std::sync::Arc<GlyphJudge>>,
}

impl WordJudge {
    fn clear(&mut self) {
        self.glyphs.clear();
    }

    /// Judge `judge`'s glyph with the word's.
    fn add(&mut self, judge: &std::sync::Arc<GlyphJudge>) {
        #[cfg(test)]
        if mutant() == WordMutant::FirstGlyph && !self.glyphs.is_empty() {
            return;
        }
        self.glyphs.push(std::sync::Arc::clone(judge));
    }

    /// Whether every glyph keeps every bar on the bare page: a word one does not caps every glass
    /// at `0`.
    fn page_kept(&self) -> bool {
        self.glyphs.iter().all(|judge| judge.page_kept)
    }

    /// Whether every glyph's margins keep `glass` over `page` at all its alphas ([`glass_box`]).
    /// Not settled is no verdict: [`Self::cap`] gives it.
    fn settled(&self, page: u32, glass: (u32, u8)) -> bool {
        glass_box(page, glass)
            .is_some_and(|(lo, hi)| self.glyphs.iter().all(|judge| judge.box_kept(lo, hi)))
    }

    /// Whether every glyph's margins keep every composite between `a` and `b` over `page` at all
    /// its alphas, as [`Self::settled`] keeps each: the composites a gradient lays between two of
    /// its columns. Each channel and the alpha of such a composite lie between the two ends' (the
    /// gradient law rounds each as the same weighted mean, [`aterm_render::glow_lerp`]), so its
    /// colour is premultiplied where both ends' are, and the ground it lays at its whole alpha
    /// lies, per channel, between the least channel over the most alpha and the most channel over
    /// the least. The box from the page over both, widened as [`glass_box`] widens its own, holds
    /// each composite's box, and margins kept at a box's corners keep every box inside it that
    /// holds the page ([`Margins`]). Not settled is no verdict.
    fn settles_between(&self, page: u32, a: (u32, u8), b: (u32, u8)) -> bool {
        let ch = |c: u32, sh: u32| (c >> sh) & 0xff;
        if [a, b]
            .iter()
            .any(|&(c, alpha)| [16, 8, 0].iter().any(|&sh| ch(c, sh) > u32::from(alpha)))
        {
            return false;
        }
        let (least, most) = (u32::from(a.1.min(b.1)), u32::from(a.1.max(b.1)));
        let (mut lo, mut hi) = ([0_u8; 3], [0_u8; 3]);
        for (i, sh) in [16_u32, 8, 0].into_iter().enumerate() {
            let p = ch(page, sh);
            // `over_premul`'s page term at an alpha: the page itself at `0`.
            let under = |alpha: u32| (p * (255 - alpha) + 127) / 255;
            let dark = (ch(a.0, sh).min(ch(b.0, sh)) + under(most)).min(255);
            let light = (ch(a.0, sh).max(ch(b.0, sh)) + under(least)).min(255);
            lo[i] = (p.min(dark) as u8).saturating_sub(2);
            hi[i] = (p.max(light) as u8).saturating_add(2);
        }
        self.glyphs.iter().all(|judge| judge.box_kept(lo, hi))
    }

    /// Whether every glyph's margins keep `glass` over `page` at all alphas up to `k`: a composite
    /// that leaves a cap the word already took as it is, in a few lookups a glyph.
    fn settled_to(&self, page: u32, (color, alpha): (u32, u8), k: u8) -> bool {
        let (color, alpha) = scale_end(color, alpha, f32::from(k) / 255.0);
        self.settled(page, (color, alpha))
    }

    /// Whether `ground` keeps every glyph of the word ([`GlyphJudge::keeps`]).
    fn keeps(&self, ground: u32) -> bool {
        self.glyphs.iter().all(|judge| judge.keeps(ground))
    }

    /// Whether every glyph's margins keep `ground`. Not kept is no verdict.
    fn margins_keep(&self, ground: u32) -> bool {
        self.glyphs
            .iter()
            .all(|judge| judge.keeps_by_margins(ground))
    }

    /// [`Self::keeps`] of the word `id`, read once: what `kept` holds of a ground, and only a
    /// ground the word's margins do not keep looked for there.
    fn keeps_once(&self, id: u32, ground: u32, kept: &mut FxHashMap<(u32, u32), bool>) -> bool {
        #[cfg(test)]
        let id = if mutant() == WordMutant::KeptAcrossWords {
            0
        } else {
            id
        };
        if self.margins_keep(ground) {
            return true;
        }
        if let Some(&read) = kept.get(&(id, ground)) {
            return read;
        }
        let keeps = self.keeps(ground);
        #[cfg(test)]
        if mutant() == WordMutant::KeptAlways {
            kept.insert((id, ground), true);
            return keeps;
        }
        kept.insert((id, ground), keeps);
        keeps
    }

    /// The word `id`'s cap over `glass` on `page` up to `bound` ([`cap_along`]), each ground read
    /// once ([`Self::keeps_once`]), the alphas `1 ..= known` known to keep. The least of its
    /// glyphs' caps: the first ground one fails. A box up to a lower alpha lies in the box up to a
    /// higher one, so the alphas every glyph's margins settle are found in a few reads of the box,
    /// and the grounds past them up to `bound` are the ones read.
    fn cap(
        &self,
        id: u32,
        page: u32,
        glass: (u32, u8),
        (known, bound): (u8, u8),
        kept: &mut FxHashMap<(u32, u32), bool>,
    ) -> u8 {
        let (mut settled, mut over) = (u16::from(known), u16::from(bound) + 1);
        while over - settled > 1 {
            let mid = (settled + over) / 2;
            if self.settled_to(page, glass, mid as u8) {
                settled = mid;
            } else {
                over = mid;
            }
        }
        cap_along(page, glass, (settled, bound), |ground: u32| {
            self.keeps_once(id, ground, kept)
        })
    }
}

/// The id in `ids` of the word of glyphs `keys`, a new one where it holds none; `caps` and `kept`,
/// which hold what is read by word id, go with `ids` when they fill.
fn word_id_of(
    ids: &mut FxHashMap<Vec<(GlyphKey, u32)>, u32>,
    caps: &mut FxHashMap<(u32, u32, u8), CapRead>,
    kept: &mut FxHashMap<(u32, u32), bool>,
    keys: &[(GlyphKey, u32)],
) -> u32 {
    if let Some(&id) = ids.get(keys) {
        return id;
    }
    if ids.len() >= 1 << 14 || kept.len() >= KEPT_HELD {
        ids.clear();
        caps.clear();
        kept.clear();
    }
    let id = ids.len() as u32;
    ids.insert(keys.to_vec(), id);
    id
}

/// The most readings of a ground [`GlassYield`] holds.
const KEPT_HELD: usize = 1 << 16;

/// A word's cap over a composite, as far as [`GlassYield::memo_word_cap`] read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CapRead {
    /// The cap itself: an alpha a ground fails past, or `255`.
    Whole(u8),
    /// Every alpha up to this one keeps, and none past it was read.
    To(u8),
}

/// The word judge's negative controls, each a law of [`WordJudge`] broken: a word read as its
/// first glyph, a ground read once taken as kept whether it kept or not, a ground one word kept
/// taken as kept for the words after it; and a saving of it undone, each composite read to its own
/// cap.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WordMutant {
    None,
    FirstGlyph,
    KeptAlways,
    KeptAcrossWords,
    /// Each composite read to its own cap, past the cap the word already took.
    Unbounded,
    /// A cap read up to a bound taken as the cap past it, unread ([`CapRead::To`]).
    ReadOnSkipped,
}

/// The railed lay's negative controls, each a saving of [`GlassYield::lay`] undone.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LayMutant {
    None,
    /// Each pixel a letter's feather eases is a span of its own.
    PixelSpans,
    /// Every pixel row of a piece a field's box lies on is cut, whatever it scales.
    BandRows,
    /// A letter no glass on the rail comes near is fielded too.
    AllLetters,
}

/// The yield's laws, each undone for a test's negative control.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LawMutant {
    None,
    /// An animating ink takes `0` on each frame it animates, whatever it reads.
    AnimatedTakesNothing,
    /// A cap under 200 takes `0`.
    Floor,
    /// A yielded word's rail starts at the top of its cells: every letter, ringed.
    RailFromTop,
    /// A blank with a cap on each side lies flat at the lesser.
    FlatBlanks,
    /// An app's ink keeps a flat [`YIELD_BAR`], and not its letter bar.
    FlatBar,
    /// An animating ink is read as the grey on the lattice on the darker side of it.
    DarkerGrey,
}

/// How the word judge's equivalence test reads a word's glyphs in turn, in place of as one
/// ([`GlassYield::per_glyph_cap`]).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlyphReading {
    /// Each glyph's margins, then its levels.
    Margins,
    /// Every level of each glyph, each ground and composite read for each: the oracle.
    Levels,
}

#[cfg(test)]
thread_local! {
    static MUTANT: std::cell::Cell<WordMutant> = const { std::cell::Cell::new(WordMutant::None) };
    static GLYPH_READING: std::cell::Cell<Option<GlyphReading>> =
        const { std::cell::Cell::new(None) };
    static LEVEL_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static LAY_MUTANT: std::cell::Cell<LayMutant> = const { std::cell::Cell::new(LayMutant::None) };
    static LAW_MUTANT: std::cell::Cell<LawMutant> = const { std::cell::Cell::new(LawMutant::None) };
}

#[cfg(test)]
fn mutant() -> WordMutant {
    MUTANT.with(std::cell::Cell::get)
}

#[cfg(test)]
fn lay_mutant() -> LayMutant {
    LAY_MUTANT.with(std::cell::Cell::get)
}

/// `f` run with the lay's control `mutant` on.
#[cfg(test)]
fn with_lay_mutant<R>(mutant: LayMutant, f: impl FnOnce() -> R) -> R {
    LAY_MUTANT.with(|c| c.set(mutant));
    let out = f();
    LAY_MUTANT.with(|c| c.set(LayMutant::None));
    out
}

#[cfg(test)]
fn law_mutant() -> LawMutant {
    LAW_MUTANT.with(std::cell::Cell::get)
}

/// `f` run with the law's control `mutant` on.
#[cfg(test)]
fn with_law_mutant<R>(mutant: LawMutant, f: impl FnOnce() -> R) -> R {
    LAW_MUTANT.with(|c| c.set(mutant));
    let out = f();
    LAW_MUTANT.with(|c| c.set(LawMutant::None));
    out
}

/// The levels a judge has read on this thread, past its margins.
#[cfg(test)]
fn level_reads() -> u64 {
    LEVEL_READS.with(std::cell::Cell::get)
}

/// [`level_reads`], zeroed.
#[cfg(test)]
fn take_level_reads() -> u64 {
    LEVEL_READS.with(|n| n.replace(0))
}

/// Count one level read ([`level_reads`]).
#[inline]
fn count_level_read() {
    #[cfg(test)]
    LEVEL_READS.with(|n| n.set(n.get() + 1));
}

/// Push the actual premultiplied color and alpha of each column under the word, so
/// a cap scales source-over light rather than its already-composited RGB; a run of columns that
/// lays one composite pushes it once.
fn composites_under(
    out: &mut Vec<(u32, u8)>,
    (wx0, wx1): (i32, i32),
    (x, w): (i32, u16),
    (e0, e1): ((u32, u8), (u32, u8)),
) {
    let (x0, x1) = (x, x + i32::from(w));
    for px in x0.max(wx0)..x1.min(wx1) {
        let (col, alpha) = composite_at((x, w), (e0, e1), px);
        // A faint gradient lays one composite over many pixels: it is kept once, not once a
        // pixel to be sorted out again.
        if alpha == 0 && col == 0 || out.last() == Some(&(col, alpha)) {
            continue;
        }
        out.push((col, alpha));
    }
}

/// The premultiplied colour and alpha the gradient from `e0` to `e1` over `(x, w)` lays at pixel
/// column `px` ([`composites_under`]).
fn composite_at((x, w): (i32, u16), (e0, e1): ((u32, u8), (u32, u8)), px: i32) -> (u32, u8) {
    let i = (px - x) as u32;
    let col = aterm_render::glow_lerp_rgb(e0.0, e1.0, i, u32::from(w));
    let alpha = aterm_render::glow_lerp(u32::from(e0.1), u32::from(e1.1), i, u32::from(w));
    (col, alpha.min(255) as u8)
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

/// What an animating ink is judged as (§4 rule 4): the grey of its luminance, on a lattice of
/// greys, rounded to the lighter. The word pass moves such an ink on almost every frame, and a
/// judge is its levels' tables and its margins (one ink's costs thousands of blends), so a judge
/// per colour would read a new ink every frame of its life. A glyph's reading of a ground is by
/// luminance, and the renderer lays an edge as the ink and its ground mixed in linear light, whose
/// luminance is the two's mixed: the grey reads as the ink does, to a level's rounding, and a
/// lighter grey reads less on the paper the glass lies on. Its bar is the bar of the darkest ink
/// its lattice step stands for ([`animated_darkest`]), since a letter's bar rises with what it
/// reads on the page.
fn animated_reading(ink: u32) -> u32 {
    let lin = crate::rainbow_kitty::prism_glass::colour::linear_levels();
    let channel = |sh: u32| lin[((ink >> sh) & 0xff) as usize];
    let y = 0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0);
    let grey = lin.partition_point(|&v| v < y);
    let grey = grey.div_ceil(ANIMATED_LATTICE) * ANIMATED_LATTICE;
    #[cfg(test)]
    let grey = if law_mutant() == LawMutant::DarkerGrey {
        grey.saturating_sub(ANIMATED_LATTICE)
    } else {
        grey
    };
    (grey.min(255) as u32) * 0x0001_0101
}

/// The darkest ink the lattice's `grey` stands for ([`animated_reading`]).
const fn animated_darkest(grey: u32) -> u32 {
    let level = (grey & 0xff) as usize;
    (level.saturating_sub(ANIMATED_LATTICE) as u32) * 0x0001_0101
}

/// The bar an ink read as solid keeps on `page`: the letter guard's on its reading there.
fn solid_ink_bar(ink: u32, page: u32) -> f64 {
    use crate::rainbow_kitty::prism_glass::colour::seen;
    #[cfg(test)]
    if law_mutant() == LawMutant::FlatBar {
        return YIELD_BAR;
    }
    f64::from(crate::cursor_glow::letter_bar(
        seen(ink, page, page) as f32,
        true,
    ))
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
        // Railed while the frame's lights fit, and whole words a second on from the last frame
        // whose did not: each flip between the two moves the glass at the words, so they are held
        // apart, and the try that does not fit publishes nothing.
        let at = glass.at();
        let held = match (at, self.whole_words_since) {
            (Some(at), Some(since)) => at.saturating_duration_since(since) < WHOLE_WORDS_HOLD,
            _ => false,
        };
        let rail = foot_railed() && !held;
        // A word's foot rail starts past the ring of the line's feet: a round letter's, which
        // overshoots the baseline, and no descender's.
        let foot = ink
            .and_then(|ink| ink.cell_box(GlyphInk::index('o', false)))
            .map(|[_, _, _, y1]| i32::from(y1) + LETTER_RING_PX as i32);
        let mut cells = self.solve(input, glass, sprites, observed, (strict, rail), foot);
        if cells == 0 {
            return YieldReport::default();
        }
        let budget = if unbudgeted() {
            Budget::Publish
        } else if rail {
            Budget::Refuse
        } else {
            Budget::Trim
        };
        let laid = match self.lay(input, glass, budget) {
            Some(laid) => laid,
            None => {
                self.whole_words_since = at;
                cells = self.solve(input, glass, sprites, observed, (strict, false), foot);
                if cells == 0 {
                    return YieldReport::default();
                }
                self.lay(input, glass, Budget::Trim).unwrap_or_default()
            }
        };
        let (quads, ramps) = laid;
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
        (strict, rail): (bool, bool),
        foot: Option<i32>,
    ) -> usize {
        let (fg, bg) = glass.page;
        if self.memo_page != bg || self.memo_fg != fg || self.memo.len() > 1 << 14 {
            self.memo.clear();
            self.memo_page = bg;
            self.memo_fg = fg;
            self.observed_caps.clear();
            self.judges.clear();
            self.word_ids.clear();
            self.kept.clear();
        }
        let epoch = observed.map(FrameGlyphInk::epoch);
        if self.observed_epoch != epoch
            || self.observed_caps.len() > 1 << 14
            || self.judges.len() > JUDGES_HELD
        {
            self.observed_caps.clear();
            self.judges.clear();
            self.word_ids.clear();
            self.kept.clear();
            self.observed_epoch = epoch;
        }
        self.rows.clear();
        self.scales.clear();
        self.letters.clear();
        let spent = self.fields.drain(..).map(|field| (field.s, field.ring));
        self.field_scales.extend(spent);
        let mut rows = std::mem::take(&mut self.glass_rows);
        rows.clear();
        rows.extend(self.glass.iter().filter_map(|r| *r));
        rows.sort_unstable();
        rows.dedup();
        let (ox, oy, cw, ch) = glass.grid;
        self.sprites.clear();
        self.sprite_rows.clear();
        // Rule 1's under-text sprites. The companions' bodies are drawn over the light, cut
        // where their letters lie, so the glass under one is the page only where its art draws:
        // that is cut, pixel by pixel, and the glass runs on through its box's clear margin.
        for s in sprites.iter().filter(|s| s.z == FreeZ::UnderText) {
            if sprite_shaped() {
                if s.is_blank() {
                    continue;
                }
                let v = s.visible();
                super::icon_shade::sprite_rows(
                    s,
                    (v.x0, v.x1, v.y0, v.y1),
                    (0, 0),
                    &mut self.sprite_scratch,
                );
                self.sprite_rows.extend_from_slice(&self.sprite_scratch);
                continue;
            }
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
            self.classify(input, glass, r, cols);
            // Rule 3: each word's cap, over the glass laid under it.
            self.ks.clear();
            self.ks.resize(cols, 255);
            let mut c = 0;
            while c < cols {
                match self.classes[c] {
                    Class::Forced => {
                        self.ks[c] = 0;
                        c += 1;
                    }
                    #[cfg(test)]
                    Class::Animated if law_mutant() == LawMutant::AnimatedTakesNothing => {
                        self.ks[c] = 0;
                        c += 1;
                    }
                    class if class.needs_cap(strict) => {
                        let mut e = c;
                        while e + 1 < cols && self.classes[e + 1].needs_cap(strict) {
                            e += 1;
                        }
                        let k = self.word_cap(input, glass, (r, c, e), (fg, bg), observed, strict);
                        #[cfg(test)]
                        let k = if law_mutant() == LawMutant::Floor && k < 200 {
                            0
                        } else {
                            k
                        };
                        for kk in &mut self.ks[c..=e] {
                            *kk = k;
                        }
                        c = e + 1;
                    }
                    _ => c += 1,
                }
            }
            // Rule 3's foot rail: under a yielded word whose letters the frame's evidence shows, the
            // glass stays whole from past the ring of the line's feet down, but round a letter
            // that reaches it.
            let letters_from = self.letters.len();
            let cy = oy + r as i32 * ch;
            let foot_at = foot.map_or(i32::MAX, |foot| cy + foot);
            #[cfg(test)]
            let foot_at = if law_mutant() == LawMutant::RailFromTop {
                cy
            } else {
                foot_at
            };
            let railed = observed.filter(|_| rail && foot_at < cy + ch);
            let mut any_rail = false;
            self.foot_ks.clear();
            self.foot_ks.extend_from_slice(&self.ks);
            if let Some(frame) = railed {
                // Whether the cell before was a letter on the rail.
                let mut lead_railed = false;
                for c in 0..cols {
                    let k = self.ks[c];
                    let lead = std::mem::take(&mut lead_railed);
                    if k == 255
                        || !matches!(self.classes[c], Class::Ink(_) | Class::Animated | Class::Fg)
                    {
                        continue;
                    }
                    // A wide glyph's right half: its ink is its lead's letter, laid over both.
                    if self.tails[c] {
                        if lead {
                            self.foot_ks[c] = 255;
                            any_rail = true;
                        }
                        continue;
                    }
                    let Some(ink) = frame.ink_rows(r, c) else {
                        continue;
                    };
                    lead_railed = true;
                    any_rail = true;
                    self.foot_ks[c] = 255;
                    // Past the line's feet by the ring: a letter that does not is clear of the rail.
                    let reaches = ink.last().is_some_and(|&[y, ..]| {
                        cy + i32::from(y) + 1 + LETTER_RING_PX as i32 > foot_at
                    });
                    let cell = ox + c as i32 * cw;
                    let reach = letter_reach();
                    let near = reaches
                        && self.glass_meets_px(
                            input,
                            r,
                            (cell - reach, cell + cw + reach),
                            (foot_at, cy + ch),
                        );
                    #[cfg(test)]
                    let near = near || lay_mutant() == LayMutant::AllLetters && !ink.is_empty();
                    if near {
                        self.letters.push(LetterHole {
                            k: f32::from(k) / 255.0,
                            at: (cell, cy),
                            ink: std::sync::Arc::clone(ink),
                        });
                    }
                }
            }
            // Rule 1, sprite-shaped: an under-text sprite's drawn pixels on the row take `0`.
            self.holes.clear();
            for &(x0, x1, y0, y1) in &self.sprite_rows {
                let (a, b) = ((y0 + oy).max(cy), (y1 + oy).min(cy + ch));
                if a < b && x0 < x1 {
                    self.holes.push((x0 + ox, x1 + ox, a, b));
                }
            }
            for c in 0..cols {
                let (cx0, cx1) = (ox + c as i32 * cw, ox + (c as i32 + 1) * cw);
                if self.ks[c] == 255
                    && self.holes.iter().any(|&(x0, x1, y0, y1)| {
                        x0 < cx1
                            && x1 > cx0
                            && self.glass_meets_px(input, r, (cx0.max(x0), cx1.min(x1)), (y0, y1))
                    })
                {
                    cut += 1;
                }
            }
            if let Some(field) = letter_field(
                &self.letters[letters_from..],
                &self.holes,
                r,
                ((cy, cy + ch), foot_at),
                &mut self.field_scales,
                &mut self.letter_stamps,
            ) {
                self.fields.push(field);
            }
            let caret = glass.caret.filter(|&(cr, _)| usize::from(cr) == r);
            self.bridge(cols, caret);
            let from = self.scales.len();
            cut += self.push_scales(input, r, cols, glass.grid);
            let foot_from = if any_rail {
                std::mem::swap(&mut self.ks, &mut self.foot_ks);
                self.bridge(cols, caret);
                let foot_from = self.scales.len();
                // Whatever the rail cuts its word's scales already counted.
                let _ = self.push_scales(input, r, cols, glass.grid);
                std::mem::swap(&mut self.ks, &mut self.foot_ks);
                foot_from
            } else {
                from
            };
            self.rows.push(GlassRow {
                row: r,
                from,
                foot_from,
                foot_at: if any_rail { foot_at } else { i32::MAX },
                cols,
            });
        }
        self.glass_rows = rows;
        cut
    }

    /// Row `r`'s scales by rule 6 over [`Self::ks`], pushed on [`Self::scales`]; the cells it cuts.
    /// A blank's edge beside a glyph or a rule-1 ground takes that cell's cap, its edge beside
    /// another blank the lesser of the two: so a blank eases from a word's cap to whole, or from
    /// one word's cap to the next's, with no step, and the cell it stands for keeps its own.
    fn push_scales(
        &mut self,
        input: &RenderInput,
        r: usize,
        cols: usize,
        grid: (i32, i32, i32, i32),
    ) -> usize {
        let mut cut = 0;
        for c in 0..cols {
            let k = self.ks[c];
            let scale = if self.classes[c] == Class::Blank {
                let edge = |n: Option<usize>| match n.filter(|&n| n < cols) {
                    None => k,
                    Some(n) if matches!(self.classes[n], Class::Blank | Class::Outside) => {
                        k.min(self.ks[n])
                    }
                    Some(n) => self.ks[n],
                };
                let (left, right) = (edge(c.checked_sub(1)), edge(Some(c + 1)));
                #[cfg(test)]
                let (left, right) =
                    if law_mutant() == LawMutant::FlatBlanks && left < 255 && right < 255 {
                        (left.min(right), left.min(right))
                    } else {
                        (left, right)
                    };
                Scale {
                    left: f32::from(left) / 255.0,
                    right: f32::from(right) / 255.0,
                }
            } else {
                Scale::flat(f32::from(k) / 255.0)
            };
            if !scale.whole() && self.glass_meets(input, r, c, grid) {
                cut += 1;
            }
            self.scales.push(scale);
        }
        cut
    }

    /// Whether any glass piece of row `r` meets the window px box `[x0, x1) × [y0, y1)`.
    fn glass_meets_px(
        &self,
        input: &RenderInput,
        r: usize,
        (x0, x1): (i32, i32),
        (y0, y1): (i32, i32),
    ) -> bool {
        self.seq_in
            .iter()
            .zip(&self.glass)
            .filter(|(_, g)| **g == Some(r))
            .any(|(p, _)| {
                let (x, w, y, h) = match *p {
                    Piece::Quad(a) => {
                        let q = &input.glow_under[a];
                        (q.x, q.w, q.y, q.h)
                    }
                    Piece::Ramp(a) => {
                        let q = &input.glow_under_ramps.items[a];
                        (q.x, q.w, q.y, q.h)
                    }
                };
                let (x, w, y, h) = (i32::from(x), i32::from(w), i32::from(y), i32::from(h));
                x < x1 && x + w > x0 && y < y1 && y + h > y0
            })
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
        self.tails.clear();
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
            self.tails
                .push(c > 0 && cells.get(c).is_some_and(|cell| cell.wide));
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
    /// by its own ends.
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
        // The source spans under the word, kept beside their per-column composites so a
        // strict cap can be re-judged against the gradient `lay` publishes.
        let mut spans = std::mem::take(&mut self.under_spans);
        spans.clear();
        // A piece of the row that does not reach the word's columns lays no composite under it
        // and no ground the published re-judge reads there, so it is passed by: in a flood, every
        // word of a row worked each of the row's pieces' wedge rows out and sorted them.
        let reaches = |x: u16, w: u16| i32::from(x) < wx1 && i32::from(x) + i32::from(w) > wx0;
        for (p, g) in self.seq_in.iter().zip(&self.glass) {
            if *g != Some(r) {
                continue;
            }
            match *p {
                Piece::Quad(a) if !reaches(input.glow_under[a].x, input.glow_under[a].w) => {}
                Piece::Ramp(a)
                    if !reaches(
                        input.glow_under_ramps.items[a].x,
                        input.glow_under_ramps.items[a].w,
                    ) => {}
                Piece::Quad(a) => {
                    let q = &input.glow_under[a];
                    spans.push((
                        i32::from(q.x),
                        q.w,
                        (q.color, q.alpha),
                        (q.color2, q.alpha2),
                    ));
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
                    }
                }
            }
        }
        // A word no piece reaches lays no composite and takes `255`, judged or not: on a typed
        // line the glass reaches the few words by the caret, and the rest are not judged.
        if spans.is_empty() {
            self.under_spans = spans;
            self.composites.clear();
            return 255;
        }
        // A strict cap judges the word's glyphs before its composites: a piece between two of
        // whose columns the word's margins settle every composite lays none that can lower its
        // cap, and is passed by ([`WordJudge::settles_between`]). The flood's trail is mostly such
        // faint glass.
        let judged = strict.then(|| self.judge_word((r, c, e), (fg, bg), observed));
        #[cfg(test)]
        let oracle = GLYPH_READING.with(std::cell::Cell::get).is_some();
        #[cfg(not(test))]
        let oracle = false;
        let settle = judged == Some(None) && !oracle && self.word.page_kept();
        let mut composites = std::mem::take(&mut self.composites);
        composites.clear();
        // Whether a piece passed by laid a composite at all: a word with none takes `255`.
        let mut passed = false;
        for &(x, w, e0, e1) in &spans {
            if settle {
                let (xa, xb) = (x.max(wx0), (x + i32::from(w)).min(wx1));
                let (a, b) = (
                    composite_at((x, w), (e0, e1), xa),
                    composite_at((x, w), (e0, e1), xb - 1),
                );
                if self.word.settles_between(bg, a, b) {
                    passed |= a != (0, 0) || b != (0, 0);
                    continue;
                }
            }
            composites_under(&mut composites, (wx0, wx1), (x, w), (e0, e1));
        }
        composites.sort_unstable();
        composites.dedup();
        let laid = passed || !composites.is_empty();
        self.composites = composites;
        if let Some(judged) = judged {
            let k = self.strict_word_cap(glass, (c, e), bg, (laid, judged), &spans);
            self.under_spans = spans;
            return k;
        }
        self.under_spans = spans;
        let mut k = 255u8;
        'inks: for col in c..=e {
            let (ink, reads_as) = match self.classes[col] {
                Class::Ink(ink) => (ink, ink),
                Class::Animated => {
                    let grey = animated_reading(self.inks[col]);
                    (grey | ANIMATED_READING, animated_darkest(grey))
                }
                _ => continue,
            };
            if col > c && self.classes[col - 1] == Class::Ink(ink) {
                continue;
            }
            for &(color, alpha) in &self.composites {
                let g = aterm_render::over_premul(bg, color, alpha) & 0x00FF_FFFF;
                let kk = *self
                    .memo
                    .entry((ink, g))
                    .or_insert_with(|| cap(ink & 0x00FF_FFFF, g, bg, solid_ink_bar(reads_as, bg)));
                k = k.min(kk);
                if k == 0 {
                    break 'inks;
                }
            }
        }
        k
    }

    /// Judge the glyphs of the word over columns `c ..= e` of row `r` as one ([`Self::word`],
    /// [`Self::word_keys`]): `None` where they are judged, else the word's cap without its
    /// composites, `0` where a glyph of it has no judge and `255` where it holds none to judge.
    fn judge_word(
        &mut self,
        (r, c, e): (usize, usize, usize),
        (fg, bg): (u32, u32),
        observed: Option<&FrameGlyphInk>,
    ) -> Option<u8> {
        self.word_keys.clear();
        self.word.clear();
        for col in c..=e {
            let ink = match self.classes[col] {
                Class::Ink(ink) => ink,
                Class::Fg => fg,
                Class::Animated => animated_reading(self.inks[col]) | ANIMATED_READING,
                _ => continue,
            };
            // A wide glyph's right half is judged with its lead, over both its cells.
            if self.tails[col] && col > c {
                continue;
            }
            let Some(frame) = observed else {
                return Some(0);
            };
            let Some((key, coverage)) = frame.coverage(r, col) else {
                return Some(0);
            };
            if self.judges.len() >= JUDGES_HELD {
                self.judges.clear();
            }
            let tables = &mut self.level_tables;
            let judge = self.judges.entry((key, ink)).or_insert_with(|| {
                GlyphJudge::new_in(tables, coverage, ink, bg, fg, frame.corrected())
                    .map(std::sync::Arc::new)
            });
            let Some(judge) = judge else {
                return Some(0);
            };
            if !self.word_keys.contains(&(key, ink)) {
                self.word_keys.push((key, ink));
                self.word.add(judge);
            }
        }
        if self.word_keys.is_empty() {
            return Some(255);
        }
        None
    }

    /// [`Self::word_cap`] judged by the frame's rasters: the least cap of the word's glyphs over
    /// [`Self::composites`], each glyph by its judge, the word's judges read as one
    /// ([`WordJudge`]), and stepped down until the gradient `lay` publishes over `spans` keeps
    /// every judge; `255` where the glass under it `laid` no composite, and `judged`'s cap where
    /// [`Self::judge_word`] gave one (`0` where a glyph of the word has no judge). An animating
    /// glyph is judged by the frame's ink, as it reads ([`animated_reading`]).
    fn strict_word_cap(
        &mut self,
        glass: &GlassLaid,
        (c, e): (usize, usize),
        bg: u32,
        (laid, judged): (bool, Option<u8>),
        spans: &[SourceSpan],
    ) -> u8 {
        if !laid {
            return 255;
        }
        if let Some(k) = judged {
            return k;
        }
        #[cfg(test)]
        if let Some(reading) = GLYPH_READING.with(std::cell::Cell::get) {
            return self.per_glyph_cap(glass, (c, e), bg, spans, reading);
        }
        if !self.word.page_kept() {
            return 0;
        }
        let mut k = 255u8;
        // The word's caps past the margins, memoised for a glass that holds still, by the word
        // and the composite: a word's judge is its glyphs' (each a function of its key and ink in
        // one glyph epoch on one page, which clear the memo). Where the margins do not settle a
        // composite, the one that darkens the page most is read first: it decides almost all of a
        // cap the word falls to, and leaves the rest settled at that cap in a few lookups a glyph.
        let mut word_id = None;
        let first = self
            .composites
            .iter()
            .position(|&composite| !self.word.settled(bg, composite));
        if let Some(first) = first {
            let darkest = self.composites[first..]
                .iter()
                .copied()
                .min_by_key(|&(color, alpha)| {
                    let ground = aterm_render::over_premul(bg, color, alpha);
                    2126 * ((ground >> 16) & 0xff)
                        + 7152 * ((ground >> 8) & 0xff)
                        + 722 * (ground & 0xff)
                })
                .expect("the first unsettled composite");
            let id = word_id_of(
                &mut self.word_ids,
                &mut self.observed_caps,
                &mut self.kept,
                &self.word_keys,
            );
            word_id = Some(id);
            k = self.memo_word_cap(id, bg, darkest, 255);
            for i in first..self.composites.len() {
                if k == 0 {
                    return 0;
                }
                let composite = self.composites[i];
                if composite != darkest && !self.word.settled_to(bg, composite, k) {
                    k = self.memo_word_cap(id, bg, composite, k);
                }
            }
            if k == 0 {
                return 0;
            }
        }
        let (word, kept) = (&self.word, &mut self.kept);
        let (ids, caps, keys) = (&mut self.word_ids, &mut self.observed_caps, &self.word_keys);
        published_cap(k, spans, glass.grid, (c, e), bg, |ground| {
            word.margins_keep(ground) || {
                let id = match word_id {
                    Some(id) => id,
                    None => {
                        let id = word_id_of(ids, &mut *caps, &mut *kept, keys);
                        word_id = Some(id);
                        id
                    }
                };
                word.keeps_once(id, ground, kept)
            }
        })
    }

    /// The word `id`'s cap over `composite` on `page` up to `bound`, the least of the two, each
    /// alpha read once ([`Self::observed_caps`]): a cap the word took from another composite needs
    /// only this one's alphas up to it, and a cap read up to a bound is read on from there.
    fn memo_word_cap(&mut self, id: u32, page: u32, composite: (u32, u8), bound: u8) -> u8 {
        if self.observed_caps.len() >= 1 << 14 {
            self.observed_caps.clear();
        }
        let asked = bound;
        #[cfg(test)]
        let bound = if mutant() == WordMutant::Unbounded {
            255
        } else {
            bound
        };
        let key = (id, composite.0, composite.1);
        let known = match self.observed_caps.get(&key) {
            Some(&CapRead::Whole(k)) => return k.min(asked),
            Some(&CapRead::To(k)) if k >= bound => return asked,
            #[cfg(test)]
            Some(&CapRead::To(_)) if mutant() == WordMutant::ReadOnSkipped => return asked,
            Some(&CapRead::To(k)) => k,
            None => 0,
        };
        let k = self
            .word
            .cap(id, page, composite, (known, bound), &mut self.kept);
        let read = if k < bound || bound == u8::MAX {
            CapRead::Whole(k)
        } else {
            CapRead::To(k)
        };
        self.observed_caps.insert(key, read);
        k.min(asked)
    }

    /// The word's glyphs read in turn, as [`WordJudge`] does not read them, its equivalence
    /// test's readings: each glyph caps each composite on its own ([`cap_along`]), the least cap
    /// stepped down by every glyph's judge.
    #[cfg(test)]
    fn per_glyph_cap(
        &self,
        glass: &GlassLaid,
        (c, e): (usize, usize),
        bg: u32,
        spans: &[SourceSpan],
        reading: GlyphReading,
    ) -> u8 {
        let judges = &self.word.glyphs;
        let keeps = |judge: &GlyphJudge, ground: u32| match reading {
            GlyphReading::Margins => judge.keeps(ground),
            GlyphReading::Levels => judge.keeps_by_levels(ground),
        };
        let mut k = 255u8;
        for judge in judges {
            for &glass in &self.composites {
                let cap = match reading {
                    GlyphReading::Margins => judge.cap(glass),
                    GlyphReading::Levels => {
                        cap_along(bg, glass, (0, 255), |ground| keeps(judge, ground))
                    }
                };
                k = k.min(cap);
                if k == 0 {
                    return 0;
                }
            }
        }
        published_cap(k, spans, glass.grid, (c, e), bg, |ground| {
            judges.iter().all(|judge| keeps(judge, ground))
        })
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
        let hold = LETTER_RING_PX / cw as f32;
        (s.at(u(xa), hold), s.at(u(xb), hold))
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
                if s.eases() {
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

    /// The spans of a piece's row over `[x0, x1)` ([`Self::spans`]): between its knots
    /// ([`Self::knot`]), and, where a letter field's `field` row lies, cut as it scales, each
    /// span's ends scaled by its cells' scales times the field's; whether any span is cut.
    fn cut_spans(
        &mut self,
        row: (usize, usize),
        grid: (i32, i32, i32, i32),
        (x0, x1): (i32, i32),
        field: Option<FieldRow<'_>>,
    ) -> bool {
        let (from, cols) = row;
        let mut cut = self.knot(from, cols, grid, (x0, x1));
        if let Some(f) = field {
            let (a, b) = (x0.max(f.x0), x1.min(f.x0 + f.s.len() as i32));
            let glass_knots = self.knots.len();
            let mut x = a;
            while x < b {
                let v = f.scale(x);
                if v >= 1.0 {
                    x += 1;
                } else if v <= 0.0 {
                    // A run of pixels it takes whole away lays nothing, one span.
                    cut = true;
                    if f.scale(x - 1) > 0.0 {
                        self.knots.push(x);
                    }
                    if f.scale(x + 1) > 0.0 {
                        self.knots.push(x + 1);
                    }
                    x += 1;
                } else if f.eases(x) {
                    // A run that only eases back to whole is one span while its pixels lie along
                    // one line: one a pixel, a flood's letters laid every light over again (22
                    // thousand lights at 18x40 cells against the 16 thousand a frame may lay, the
                    // after-land review).
                    cut = true;
                    let start = x;
                    let next = self.knots[..glass_knots].partition_point(|&k| k <= start);
                    let stop = self.knots[..glass_knots].get(next).map_or(b, |&k| k.min(b));
                    self.knots.push(start);
                    x += 1;
                    while x < stop && f.eases(x) && f.runs_straight(start, x + 1) {
                        x += 1;
                    }
                    self.knots.push(x);
                } else {
                    // Every pixel of a letter's ring is a span of its own, so each lays exactly
                    // the scaled composite of its own column, which the word's cap judged.
                    cut = true;
                    self.knots.push(x);
                    self.knots.push(x + 1);
                    x += 1;
                }
            }
            self.knots.sort_unstable();
            self.knots.dedup();
        }
        self.spans.clear();
        if !cut {
            return false;
        }
        for i in 0..self.knots.len() - 1 {
            let (xa, xb) = (self.knots[i], self.knots[i + 1]);
            let (sa, sb) = self.span_scale(from, cols, grid, (xa, xb));
            let (fa, fb) = field.map_or((1.0, 1.0), |f| {
                if f.eases(xa) {
                    f.eased_ends(xa, xb)
                } else {
                    (f.scale(xa), f.scale(xa))
                }
            });
            self.spans.push((xa, xb, sa * fa, sb * fb));
        }
        true
    }

    /// Push quad `q` (every pixel row of it alike), its rows cut by [`Self::cut_spans`]; whether
    /// it was cut.
    fn lay_quad(
        &mut self,
        out: &mut Vec<GlowQuad>,
        q: GlowQuad,
        row: (usize, usize),
        grid: (i32, i32, i32, i32),
        field: Option<FieldRow<'_>>,
    ) -> bool {
        let (x0, x1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
        if !self.cut_spans(row, grid, (x0, x1), field) {
            out.push(q);
            return false;
        }
        let w = i32::from(q.w);
        for &(xa, xb, sa, sb) in &self.spans {
            let ea = edge_end((q.color, q.alpha), (q.color2, q.alpha2), xa - x0, w);
            let eb = edge_end((q.color, q.alpha), (q.color2, q.alpha2), xb - x0, w);
            let (ca, aa) = scale_end(ea.0, ea.1, sa);
            let (cb, ab) = scale_end(eb.0, eb.1, sb);
            if (ca, aa, cb, ab) == (0, 0, 0, 0) {
                continue;
            }
            out.push(GlowQuad {
                x: xa as u16,
                w: (xb - xa) as u16,
                color: ca,
                alpha: aa,
                color2: cb,
                alpha2: ab,
                ..q
            });
        }
        true
    }

    /// Push ramp `ramp` (its ends in `ends`) before quad `at`, its rows cut by
    /// [`Self::cut_spans`]; whether it was cut.
    fn lay_ramp(
        &mut self,
        out: &mut GlowRamps,
        (ramp, ends, at): (GlowRamp, &[GlowEnd], u32),
        row: (usize, usize),
        grid: (i32, i32, i32, i32),
        field: Option<FieldRow<'_>>,
    ) -> bool {
        let (x0, x1) = (i32::from(ramp.x), i32::from(ramp.x) + i32::from(ramp.w));
        let kept = ramp.kept_ends(ends);
        if !self.cut_spans(row, grid, (x0, x1), field) {
            let ink = out.ends.len();
            out.ends.extend_from_slice(kept);
            out.items.push(GlowRamp {
                at,
                ink: ink as u32,
                ..ramp
            });
            return false;
        }
        let w = i32::from(ramp.w);
        for &(xa, xb, sa, sb) in &self.spans {
            let ink = out.ends.len();
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
                out.ends.push(aterm_core::render::glow_end(ca, aa));
                out.ends.push(aterm_core::render::glow_end(cb, ab));
            }
            let piece = GlowRamp {
                at,
                x: xa as u16,
                w: (xb - xa) as u16,
                ink: ink as u32,
                ..ramp
            };
            if piece.lights(&out.ends) == 0 {
                out.ends.truncate(ink);
                continue;
            }
            out.items.push(piece);
        }
        true
    }

    /// Lay the input's under stream again with its glass yielded; whether its quads and its
    /// ramps changed, or `None` with the stream left as it was where it would lay more lights
    /// than [`YIELD_LIGHT_BUDGET`] and `over` is [`Budget::Refuse`]. A pixel row of a piece that a
    /// letter field scales is laid alone, and the rows between them whole.
    fn lay(
        &mut self,
        input: &mut RenderInput,
        glass: &GlassLaid,
        over: Budget,
    ) -> Option<(bool, bool)> {
        let grid = glass.grid;
        let mut out_q = std::mem::take(&mut self.out_quads);
        let mut out_r = std::mem::take(&mut self.out_ramps);
        out_q.clear();
        out_r.clear();
        let (mut quads_cut, mut ramps_cut) = (false, false);
        let seq = std::mem::take(&mut self.seq_in);
        let marks = std::mem::take(&mut self.glass);
        let fields = std::mem::take(&mut self.fields);
        for (p, g) in seq.iter().zip(&marks) {
            let row = g.and_then(|r| self.rows.iter().find(|row| row.row == r).copied());
            let field = g.and_then(|r| fields.iter().find(|f| f.row == r));
            match *p {
                Piece::Quad(a) => {
                    let q = input.glow_under[a];
                    let Some(row) = row else {
                        out_q.push(q);
                        continue;
                    };
                    let xs = (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
                    let (y0, y1) = (i32::from(q.y), i32::from(q.y) + i32::from(q.h));
                    let scaled = |y: i32| field.is_some_and(|f| f.scales(y, xs));
                    let mut y = y0;
                    while y < y1 {
                        let from = y;
                        y += 1;
                        let line = if scaled(from) {
                            field.and_then(|f| f.row_at(from))
                        } else {
                            while y < y1 && !scaled(y) && y != row.foot_at {
                                y += 1;
                            }
                            None
                        };
                        let part = GlowQuad {
                            y: from as u16,
                            h: (y - from) as u16,
                            ..q
                        };
                        let cells = row.cells_at(from);
                        quads_cut |= self.lay_quad(&mut out_q, part, cells, grid, line);
                    }
                }
                Piece::Ramp(a) => {
                    let ramp = input.glow_under_ramps.items[a];
                    let ends = &input.glow_under_ramps.ends;
                    let at = out_q.len() as u32;
                    let Some(row) = row else {
                        let ink = out_r.ends.len();
                        out_r.ends.extend_from_slice(ramp.kept_ends(ends));
                        out_r.items.push(GlowRamp {
                            at,
                            ink: ink as u32,
                            ..ramp
                        });
                        continue;
                    };
                    let xs = (i32::from(ramp.x), i32::from(ramp.x) + i32::from(ramp.w));
                    let (y0, y1) = (i32::from(ramp.y), i32::from(ramp.y) + i32::from(ramp.h));
                    let scaled = |y: i32| field.is_some_and(|f| f.scales(y, xs));
                    let mut y = y0;
                    while y < y1 {
                        let from = y;
                        y += 1;
                        let line = if scaled(from) {
                            field.and_then(|f| f.row_at(from))
                        } else {
                            while y < y1 && !scaled(y) && y != row.foot_at {
                                y += 1;
                            }
                            None
                        };
                        let mut part = ramp;
                        part.keep_rows((from - y0) as u16, (y - from) as u16);
                        let cells = row.cells_at(from);
                        ramps_cut |= self.lay_ramp(&mut out_r, (part, ends, at), cells, grid, line);
                    }
                }
            }
        }
        out_r.lit = out_r.count_lit();
        self.seq_in = seq;
        self.glass = marks;
        self.fields = fields;
        let laid = out_r.laid(out_q.len());
        let fits = laid <= YIELD_LIGHT_BUDGET;
        if !fits && over == Budget::Refuse {
            self.out_quads = out_q;
            self.out_ramps = out_r;
            return None;
        }
        if !fits && over == Budget::Trim {
            out_r.truncate_laid(&mut out_q, YIELD_LIGHT_BUDGET);
        }
        if quads_cut || ramps_cut {
            std::mem::swap(&mut input.glow_under, &mut out_q);
            std::mem::swap(&mut input.glow_under_ramps, &mut out_r);
        }
        self.out_quads = out_q;
        self.out_ramps = out_r;
        Some((quads_cut, ramps_cut))
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

    /// The whole-cell cut's gradient rounding, which a word's wash and a word the frame's evidence
    /// shows no letters for both lay: it runs under [`with_whole_word_cut`], since a descender on
    /// the rail lays each pixel of its ring as a span of its own
    /// (`a_letter_s_ring_lays_each_pixel_and_its_feather_one_span`).
    #[test]
    fn published_caps_keep_synthetic_ink_readable_after_gradient_rounding() {
        with_whole_word_cut(
            published_caps_keep_synthetic_ink_readable_after_gradient_rounding_whole,
        );
    }

    fn published_caps_keep_synthetic_ink_readable_after_gradient_rounding_whole() {
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
                .insert((key, FG), Some(std::sync::Arc::new(judge.clone())));
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
                    (1..255).contains(&cap),
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
                (i32::from(quad.x), quad.w),
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

    /// After-land R0: a word's judge ([`WordJudge`]) keeps exactly the grounds each of its glyphs'
    /// judges keeps, and caps a composite at the least of their caps: on every shipped paper, for
    /// words of the synthetic profiles (a full ink, a thin one, an antialiased ramp, each with bars
    /// of its own), in the page's ink and an app's. The negative control reads the word as its
    /// first glyph ([`WordMutant::FirstGlyph`]), and keeps a ground a later glyph fails.
    #[test]
    fn a_word_keeps_where_each_of_its_glyphs_keeps() {
        let ramp: Vec<u8> = (0..32_u8).map(|i| i * 8 + 7).collect();
        let profiles = [
            GlyphCoverage::of(&[255; 32]),
            GlyphCoverage::of(&[160; 32]),
            GlyphCoverage::of(&ramp),
        ];
        let words: [&[usize]; 4] = [&[0, 2], &[2, 0], &[0, 1, 2], &[1, 2]];
        let (mut grounds, mut capped, mut wrong) = (0_u64, 0_u64, 0_u64);
        for palette in &crate::rainbow_kitty::prism_glass::tables::SHIPPED {
            let (fg, page) = (palette.fg, palette.bg);
            for ink in [fg, 0x00C4_262E] {
                let mut tables = LevelTables::default();
                let judges: Vec<std::sync::Arc<GlyphJudge>> = profiles
                    .iter()
                    .filter_map(|p| GlyphJudge::new_in(&mut tables, p, ink, page, fg, true))
                    .map(std::sync::Arc::new)
                    .collect();
                assert_eq!(
                    judges.len(),
                    3,
                    "fixture: every profile judged on #{page:06x}"
                );
                for word in words {
                    let glyphs: Vec<&std::sync::Arc<GlyphJudge>> =
                        word.iter().map(|&i| &judges[i]).collect();
                    let as_one = |mutant: WordMutant| {
                        MUTANT.with(|c| c.set(mutant));
                        let mut judge = WordJudge::default();
                        for glyph in &glyphs {
                            judge.add(glyph);
                        }
                        MUTANT.with(|c| c.set(WordMutant::None));
                        judge
                    };
                    let (judge, first) = (as_one(WordMutant::None), as_one(WordMutant::FirstGlyph));
                    let mut kept = FxHashMap::default();
                    for &glass in palette.glass.iter().step_by(16) {
                        for alpha in [255_u8, 160, 64, 12] {
                            let source = (aterm_render::premul_rgb(glass, alpha), alpha);
                            for a in 0..=255_u16 {
                                let (c, k) = scale_end(source.0, source.1, f32::from(a) / 255.0);
                                let ground = aterm_render::over_premul(page, c, k) & 0x00FF_FFFF;
                                let each = glyphs.iter().all(|glyph| glyph.keeps(ground));
                                assert_eq!(
                                    judge.keeps(ground),
                                    each,
                                    "#{ground:06x} on #{page:06x}"
                                );
                                wrong += u64::from(first.keeps(ground) != each);
                                grounds += 1;
                            }
                            let least_cap =
                                glyphs.iter().map(|glyph| glyph.cap(source)).min().unwrap();
                            assert_eq!(
                                judge.cap(0, page, source, (0, 255), &mut kept),
                                least_cap,
                                "#{glass:06x} at {alpha}"
                            );
                            capped += u64::from(least_cap < 255);
                        }
                    }
                }
            }
        }
        assert!(
            grounds > 100_000 && capped > 0,
            "fixture: {grounds} grounds, {capped} capped"
        );
        assert!(
            wrong > 0,
            "control: a word read as its first glyph keeps what a later glyph fails"
        );
    }

    /// A CAP READ UP TO A BOUND IS READ ON PAST IT ([`CapRead::To`]): a word asked for its cap over
    /// a composite up to a bound under that cap, and then up to 255, takes the cap a word read whole
    /// takes, on every shipped paper, for words of the synthetic profiles in the page's ink and an
    /// app's. The equivalence test of the word pass reaches this read only twice. The negative
    /// control takes the cap read up to the first bound as the cap past it, unread
    /// ([`WordMutant::ReadOnSkipped`]), and publishes a cap over the word's.
    #[test]
    fn a_cap_read_up_to_a_bound_is_read_on_past_it() {
        let ramp: Vec<u8> = (0..32_u8).map(|i| i * 8 + 7).collect();
        let profiles = [
            GlyphCoverage::of(&[255; 32]),
            GlyphCoverage::of(&[160; 32]),
            GlyphCoverage::of(&ramp),
        ];
        let words: [&[usize]; 3] = [&[0, 2], &[0, 1, 2], &[1, 2]];
        let (mut read_on, mut over) = (0_u64, 0_u64);
        for palette in &crate::rainbow_kitty::prism_glass::tables::SHIPPED {
            let (fg, page) = (palette.fg, palette.bg);
            for ink in [fg, 0x00C4_262E] {
                let mut tables = LevelTables::default();
                let judges: Vec<std::sync::Arc<GlyphJudge>> = profiles
                    .iter()
                    .filter_map(|p| GlyphJudge::new_in(&mut tables, p, ink, page, fg, true))
                    .map(std::sync::Arc::new)
                    .collect();
                for word in words {
                    let mut judge = WordJudge::default();
                    for &i in word {
                        judge.add(&judges[i]);
                    }
                    for &glass in palette.glass.iter().step_by(16) {
                        for alpha in [255_u8, 160, 64, 12] {
                            let source = (aterm_render::premul_rgb(glass, alpha), alpha);
                            let whole =
                                judge.cap(0, page, source, (0, 255), &mut FxHashMap::default());
                            let first = whole / 2;
                            if first == 0 || whole == 255 {
                                continue;
                            }
                            for mutant in [WordMutant::None, WordMutant::ReadOnSkipped] {
                                let mut glass_yield = GlassYield {
                                    word: judge.clone(),
                                    ..GlassYield::default()
                                };
                                MUTANT.with(|c| c.set(mutant));
                                let to = glass_yield.memo_word_cap(0, page, source, first);
                                let on = glass_yield.memo_word_cap(0, page, source, 255);
                                MUTANT.with(|c| c.set(WordMutant::None));
                                assert_eq!(to, first, "#{glass:06x} at {alpha} up to {first}");
                                if mutant == WordMutant::None {
                                    assert_eq!(
                                        on, whole,
                                        "#{glass:06x} at {alpha} on #{page:06x}: read on past {first}"
                                    );
                                    read_on += 1;
                                } else {
                                    over += u64::from(on > whole);
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(read_on > 0, "fixture: no cap read on past a bound");
        assert!(
            over > 0,
            "control: a cap read up to a bound taken as the cap past it published none over a word's"
        );
    }

    /// After-land R0: the yield reads a word's glyphs as one ([`WordJudge`]) and publishes
    /// exactly what judging each glyph on its own did (`per_glyph_cap`, the reading before: every
    /// glyph scanning every composite's grounds and reading every level of its own), the cells'
    /// scales with it: on GitHub Light and Default Light, in the page's ink and two app inks, over
    /// the bundled face's rasters at 1x and 2x, under gradient quads and wedge ramps of the shipped
    /// spectrum and of three deep lights, at full to faint alpha. The work is counted, not timed:
    /// past their margins the word reads fewer levels than its glyphs read in turn by their margins
    /// (r25 K8-1's reading), and under a fifth of what they read with none (63 thousand, 4.0
    /// million and 408 million on after-land round 3's bars); and reading each composite only up
    /// to the cap the word already took, fewer than reading each to its own cap
    /// ([`WordMutant::Unbounded`], 165 thousand), which publishes the same. One-colour glass, a
    /// pinned colour's, lays every word the same composites, so what one word kept is what the
    /// next word reads, and a yield held across the frames reads its memo: both publish the same.
    /// The negative controls read a word as its first glyph, take a ground read once as kept
    /// whether it kept or not, and take a ground one word kept as kept for the next
    /// ([`WordMutant`]): each publishes what the glyphs judged in turn do not.
    #[test]
    fn a_word_judged_as_one_yields_as_its_glyphs_judged_in_turn() {
        use crate::rainbow_kitty::prism_glass::{GLASS_LEN, Palette};
        const TEXT: &[u8] = b"The quick brown fox jumps over\r\n\x1b[38;2;196;38;46mred\x1b[39m \
            words \x1b[38;2;5;80;174mblue\x1b[39m %@{} mm\r\nlazy dog 0123 ,;:\"' wwiill";
        const DEEP: [u32; 3] = [0x0080_40C0, 0x00C0_6020, 0x0020_8080];
        let (mut frames, mut cut, mut differ) = (0, 0, [0; 3]);
        let (mut word_reads, mut glyph_reads, mut margin_reads) = (0_u64, 0_u64, 0_u64);
        let mut unbounded_reads = 0_u64;
        for (fg, page) in [
            (0x001F_2328_u32, 0x00FF_FFFF_u32),
            (0x002B_2D33, 0x00F6_F7F9),
        ] {
            let palette = Palette::shipped(fg, page).unwrap();
            let light = |i: usize| {
                if i % 7 == 6 {
                    DEEP[i % 3]
                } else {
                    palette.glass[i % GLASS_LEN]
                }
            };
            for px in [14.0, 28.0] {
                let mut renderer =
                    aterm_render::Renderer::from_system(px, aterm_render::Theme::default())
                        .unwrap();
                renderer.debug_block_on_lazy_fallbacks();
                let (cw, ch) = renderer.cell_size();
                let mut term = Terminal::new(3, 40);
                let rgb = |c: u32| {
                    format!(
                        "rgb:{:02x}/{:02x}/{:02x}",
                        c >> 16,
                        (c >> 8) & 0xff,
                        c & 0xff
                    )
                };
                term.process(
                    format!("\x1b]10;{}\x07\x1b]11;{}\x07", rgb(fg), rgb(page)).as_bytes(),
                );
                term.process(TEXT);
                let mut base = term.cell_frame(3, 40);
                base.cursor_visible = false;
                let _ = renderer.render_input(&base);
                let ink = renderer.glyph_ink_for(&base);
                assert!(
                    ink.for_frame(&base, (cw, ch)).is_some(),
                    "stable actual glyph keys"
                );
                let (cw, ch) = (cw as i32, ch as i32);
                let mut held = GlassYield::default();
                // The first fourteen a gradient and a wedge, the rest one colour.
                for v in 0..26_usize {
                    let at = Instant::now();
                    let flat = v >= 14;
                    let alpha = if flat {
                        [255_u8, 240, 220, 200, 180, 160, 128, 96, 64, 40, 24, 12][v - 14]
                    } else {
                        [255_u8, 220, 160, 96, 40, 12, 255][v % 7]
                    };
                    let fade = [255_u8, 200, 120][v % 3];
                    let mut quads = Vec::new();
                    let mut ramps = GlowRamps::default();
                    for row in 0..3_u16 {
                        let i0 = v * 37 + usize::from(row) * 61;
                        let i1 = i0 + 9 + v;
                        let x0 = ((v * 3 + usize::from(row) * 5) % 9) as i32 * cw;
                        let (x, y, w) = (
                            x0 as u16,
                            (i32::from(row) * ch) as u16,
                            (30 * cw - x0) as u16,
                        );
                        let end = |i: usize, a: u8| {
                            aterm_core::render::glow_end(aterm_render::premul_rgb(light(i), a), a)
                        };
                        if flat {
                            quads.push(GlowQuad::flat(
                                row,
                                x,
                                y,
                                w,
                                ch as u16,
                                aterm_render::premul_rgb(light(3), alpha),
                                alpha,
                            ));
                        } else if row == 1 && v % 2 == 1 {
                            // A wedge: every row its own ends.
                            ramps.open(
                                quads.len(),
                                row,
                                (x, y, w),
                                (end(i0, alpha), end(i1, fade)),
                            );
                            for k in 1..ch as usize {
                                ramps.extend_last(0, (end(i0 + k, alpha), end(i1 + k / 2, fade)));
                            }
                        } else {
                            let mut q = GlowQuad::flat(
                                row,
                                x,
                                y,
                                w,
                                ch as u16,
                                aterm_render::premul_rgb(light(i0), alpha),
                                alpha,
                            );
                            (q.color2, q.alpha2) =
                                (aterm_render::premul_rgb(light(i1), fade), fade);
                            quads.push(q);
                        }
                    }
                    let run_on = |glass_yield: &mut GlassYield,
                                  reading: Option<GlyphReading>,
                                  mutant: WordMutant| {
                        GLYPH_READING.with(|c| c.set(reading));
                        MUTANT.with(|c| c.set(mutant));
                        let mut input = base.clone();
                        input.glow_under.clone_from(&quads);
                        input.glow_under_ramps.clone_from(&ramps);
                        let laid = GlassLaid::new(
                            at,
                            (quads.clone(), ramps.clone()),
                            (fg, page),
                            (0, 0, cw, ch),
                            None,
                        );
                        let from = level_reads();
                        let report =
                            glass_yield.apply_observed(&mut input, &laid, &[], Some(&ink), true);
                        let reads = level_reads() - from;
                        GLYPH_READING.with(|c| c.set(None));
                        MUTANT.with(|c| c.set(WordMutant::None));
                        (
                            (
                                input.glow_under,
                                input.glow_under_ramps,
                                glass_yield.scales.clone(),
                                report,
                            ),
                            reads,
                        )
                    };
                    let run = |reading: Option<GlyphReading>, mutant: WordMutant| {
                        run_on(&mut GlassYield::default(), reading, mutant)
                    };
                    let (word, reads) = run(None, WordMutant::None);
                    let (glyphs, glyph_read) = run(Some(GlyphReading::Levels), WordMutant::None);
                    let (kept, margin_read) = run(Some(GlyphReading::Margins), WordMutant::None);
                    let (unbounded, unbounded_read) = run(None, WordMutant::Unbounded);
                    assert_eq!(word, glyphs, "{px} px on #{page:06x}, variation {v}");
                    assert_eq!(kept, glyphs, "{px} px on #{page:06x}, variation {v}");
                    assert_eq!(unbounded, glyphs, "{px} px on #{page:06x}, variation {v}");
                    unbounded_reads += unbounded_read;
                    let (read_on, _) = run_on(&mut held, None, WordMutant::None);
                    assert_eq!(
                        read_on, glyphs,
                        "{px} px on #{page:06x}, variation {v}, held"
                    );
                    word_reads += reads;
                    glyph_reads += glyph_read;
                    margin_reads += margin_read;
                    for (n, mutant) in [
                        WordMutant::FirstGlyph,
                        WordMutant::KeptAlways,
                        WordMutant::KeptAcrossWords,
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        differ[n] += usize::from(run(None, mutant).0 != glyphs);
                    }
                    cut += usize::from(word.3.cut());
                    frames += 1;
                }
            }
        }
        assert!(
            frames >= 50 && cut * 2 > frames,
            "fixture: {cut} of {frames} frames cut"
        );
        assert!(
            word_reads * 5 < glyph_reads,
            "the word read {word_reads} levels where its glyphs read in turn read {glyph_reads}"
        );
        assert!(
            word_reads < margin_reads,
            "the word read {word_reads} levels where its glyphs read in turn by their margins read \
             {margin_reads}"
        );
        assert!(
            word_reads < unbounded_reads,
            "the word read {word_reads} levels where it read {unbounded_reads} with each composite \
             read to its own cap"
        );
        assert!(
            differ.iter().all(|&n| n > 0),
            "controls: a word read as its first glyph, a ground taken as kept once read, and one \
             taken as kept for the next word, yield otherwise in {differ:?} frames"
        );
    }

    /// Strict observation measures a theme glyph and keeps it plain for rule 5b: a safe one,
    /// which its own raster keeps under the whole glass, is still yielded where rule 5b yields a
    /// run, between a yielded word and the caret, the caret's cell with it. A theme glyph touching
    /// an app's ink, or an animating one, is measured with that ink's word (rule 3 reads a run of
    /// glyphs of any ink) and takes its cap. The negative control is a run of [`ORPHAN_CELLS`]
    /// cells between two yielded words, ` M `, which is no orphan: the safe glyph keeps its glass.
    /// On the foot rail, a word bounds no run, so this runs under [`with_whole_word_cut`] to read
    /// every row one way.
    #[test]
    fn observed_safe_theme_ink_still_bridges_between_yielded_words_and_caret() {
        with_whole_word_cut(
            observed_safe_theme_ink_still_bridges_between_yielded_words_and_caret_whole,
        );
    }

    fn observed_safe_theme_ink_still_bridges_between_yielded_words_and_caret_whole() {
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
            let word = yield_.ks[run.start - 1];
            if bridged {
                assert!(run.len() < ORPHAN_CELLS, "premise: the run is an orphan");
                assert_eq!(
                    yield_.ks[safe_col], word,
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
                    word,
                    "the orphan's caret yields too"
                );
            }
        }
    }

    /// §4 rule 3 (after-land A7, R2's "one run, one hue phase, not sticker-outlined"): a yielded
    /// word takes its cap flat across its cells, so nothing outlines its letters, and the glass runs
    /// on whole under it on the foot rail, the rows past the ring of the line's feet, and through
    /// the blanks there. A descender is read on the rail as any letter: the cap within
    /// [`LETTER_RING_PX`] of its ink, and past that the rail goes on. Negative controls:
    /// [`LawMutant::RailFromTop`] lays the rail from the cells' top, every letter ringed, and
    /// [`with_whole_word_cut`] lays the cap across the whole cells, and the run breaks at the word.
    #[test]
    fn a_yielded_word_lies_flat_at_its_cap_and_the_glass_runs_on_under_it() {
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                .expect("the bundled face");
        // `gap` in the red, a descender either side of a letter that stops at the line's feet.
        let text = b"ab \x1b[38;2;196;38;46mgap\x1b[39m cd".as_slice();
        let (cw, ch) = renderer.cell_size();
        let (cwi, chi) = (cw as i32, ch as i32);
        let foot = rail_from(&mut renderer, &page(text));
        assert!(
            foot < chi,
            "premise: the cell holds a rail: {foot} of {chi}"
        );
        let mut lay = || {
            let mut input = page(text);
            let ink = renderer.glyph_ink_for(&input);
            let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
            input.glow_under = vec![quad];
            let glass = GlassLaid::new(
                Instant::now(),
                (vec![quad], GlowRamps::default()),
                (FG, BG),
                (0, 0, cwi, chi),
                None,
            );
            let mut glass_yield = GlassYield::default();
            let report = glass_yield.apply_observed(&mut input, &glass, &[], Some(&ink), true);
            assert!(report.cut(), "premise: the red word yields");
            no_coloured_zero(&input);
            let k = glass_yield.ks[3];
            (input, ink, k)
        };
        let (laid, ink, k) = lay();
        assert!(k < 255, "premise: the word takes a cap");
        let capped = u32::from(scale_end(GLASS.0, GLASS.1, f32::from(k) / 255.0).1);
        let whole = u32::from(GLASS.1);
        let flat = |input: &RenderInput| {
            (0..foot).all(|y| (3 * cwi..6 * cwi).all(|x| alpha_at_y(input, x, y) == capped))
        };
        assert!(
            flat(&laid),
            "the word's cells lie flat at its cap above the rail"
        );
        // What each letter inks on the rail is at the cap, its ring with it.
        let frame = ink
            .for_frame(&page(text), (cw, ch))
            .expect("current evidence");
        let ring = LETTER_RING_PX as i32;
        let mut rail_ink = 0;
        for c in 3..6_usize {
            let cx = c as i32 * cwi;
            let rows = frame.ink_rows(0, c).expect("each letter's raster");
            for &[y, a, z] in rows.iter().filter(|r| i32::from(r[0]) >= foot) {
                for x in cx + i32::from(a) - ring..cx + i32::from(z) + ring {
                    rail_ink += 1;
                    assert_eq!(alpha_at_y(&laid, x, i32::from(y)), capped, "({x}, {y})");
                }
            }
        }
        assert!(rail_ink > 0, "premise: the g and the p reach the rail");
        // The run: the rail's first row is whole across the line, but where a descender skips it.
        let runs_on = |input: &RenderInput| {
            // The `a` stops at the line's feet: the rail under it is the source's own, and the
            // blanks either side are too.
            let under_a = 4 * cwi + cwi / 2;
            [2 * cwi + cwi / 2, under_a, 6 * cwi + cwi / 2]
                .iter()
                .all(|&x| (foot..chi).all(|y| alpha_at_y(input, x, y) == whole))
        };
        assert!(runs_on(&laid), "the rail runs on whole under the word");
        let (ringed, _, _) = with_law_mutant(LawMutant::RailFromTop, &mut lay);
        assert!(
            !flat(&ringed),
            "control: a rail from the cells' top leaves every letter in a ring"
        );
        let (cut, _, _) = with_whole_word_cut(&mut lay);
        assert!(
            !runs_on(&cut),
            "control: cut across its whole cells, the word breaks the run"
        );
    }

    /// §4 rule 3: a word whose letters all stop at the line's feet fields nothing on the rail, so
    /// the rail is one piece of glass under it. Negative control ([`LawMutant::RailFromTop`]): read
    /// from the cells' top, each letter is fielded.
    #[test]
    fn a_word_with_no_descender_fields_nothing() {
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                .expect("the bundled face");
        let text = b"\x1b[38;2;196;38;46mwar\x1b[39m".as_slice();
        let (cw, ch) = renderer.cell_size();
        let mut letters = || {
            let mut input = page(text);
            let ink = renderer.glyph_ink_for(&input);
            let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
            input.glow_under = vec![quad];
            let glass = GlassLaid::new(
                Instant::now(),
                (vec![quad], GlowRamps::default()),
                (FG, BG),
                (0, 0, cw as i32, ch as i32),
                None,
            );
            let mut glass_yield = GlassYield::default();
            let report = glass_yield.apply_observed(&mut input, &glass, &[], Some(&ink), true);
            assert!(report.cut(), "premise: the red word yields");
            (glass_yield.letters.len(), glass_yield.fields.len())
        };
        assert_eq!(letters(), (0, 0), "no letter of the word meets the rail");
        assert_eq!(
            with_law_mutant(LawMutant::RailFromTop, &mut letters),
            (3, 1),
            "control: from the cells' top, each letter meets it"
        );
    }

    /// An app's ink keeps its letter bar on the strict path: 4.6; where it reads under 4.6 on the
    /// bare page and AA or more, AA with a level in hand; under AA, 97% of what it reads there; and
    /// no more. There are glasses a grey word reads over 4.6 and under 5.25 on, and it keeps them
    /// whole. Negative control ([`LawMutant::FlatBar`]): at the flat 5.25 it is cut.
    #[test]
    fn an_app_ink_keeps_its_letter_bar_and_no_more() {
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                .expect("the bundled face");
        let (cw, ch) = renderer.cell_size();
        let text = b"\x1b[38;2;71;77;87mgrey\x1b[39m".as_slice();
        // A grey glass, which darkens the page by its alpha.
        let mut lay = |alpha: u8| {
            let mut input = page(text);
            let ink = renderer.glyph_ink_for(&input);
            let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, 0, alpha);
            input.glow_under = vec![quad];
            let glass = GlassLaid::new(
                Instant::now(),
                (vec![quad], GlowRamps::default()),
                (FG, BG),
                (0, 0, cw as i32, ch as i32),
                None,
            );
            let mut glass_yield = GlassYield::default();
            let _ = glass_yield.apply_observed(&mut input, &glass, &[], Some(&ink), true);
            glass_yield.ks[0]
        };
        let glasses = (1..=120_u8)
            .filter(|&alpha| {
                lay(alpha) == 255 && with_law_mutant(LawMutant::FlatBar, || lay(alpha)) < 255
            })
            .count();
        assert!(
            glasses > 0,
            "no glass the word keeps at its letter bar and loses at a flat 5.25"
        );
        let whole_at_flat = with_law_mutant(LawMutant::FlatBar, || {
            (1..=120).filter(|&a| lay(a) == 255).count()
        });
        let whole_at_letter = (1..=120).filter(|&a| lay(a) == 255).count();
        assert!(
            whole_at_letter > whole_at_flat,
            "control: {whole_at_letter} whole glasses at its letter bar, {whole_at_flat} at 5.25"
        );
    }

    /// The caps of `wow` in `ink` (`r;g;b`) on Default Light under a grey glass at each alpha from
    /// 1 to 60, which darkens the page by it, on the strict path at 24 px.
    fn grey_glass_caps(ink: &str) -> Vec<u8> {
        grey_glass_word_caps("wow", ink)
    }

    /// [`grey_glass_caps`] of `word`.
    fn grey_glass_word_caps(word: &str, ink: &str) -> Vec<u8> {
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                .expect("the bundled face");
        let (cw, ch) = renderer.cell_size();
        let text = format!("\x1b[38;2;{ink}m{word}\x1b[39m");
        (1..=60_u8)
            .map(|alpha| {
                let mut input = page(text.as_bytes());
                let glyphs = renderer.glyph_ink_for(&input);
                let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, 0, alpha);
                input.glow_under = vec![quad];
                let glass = GlassLaid::new(
                    Instant::now(),
                    (vec![quad], GlowRamps::default()),
                    (FG, BG),
                    (0, 0, cw as i32, ch as i32),
                    None,
                );
                let mut glass_yield = GlassYield::default();
                let _ = glass_yield.apply_observed(&mut input, &glass, &[], Some(&glyphs), true);
                glass_yield.ks[0]
            })
            .collect()
    }

    /// After-land round 3 (W1): each solid level of an app's ink keeps the letter bar of its own
    /// reading. The judges read a letter by the lower median of its solid pixels in a row, and
    /// `w` in #b43fb4 on Default Light, 4.545 at its full ink, reads AA by them while its
    /// antialiased solid levels read under it: held by its dimmest level's bar, a word's wash
    /// lowered its full ink to that level's share, and the coloured line's `w` read 4.40 of its
    /// own 4.50. Its full ink reads under AA's level in hand, so it takes no glass that lowers it.
    /// Negative control ([`with_least_level_bar`]): every level at its dimmest level's bar, the
    /// word takes a wash. The word is the face's [`uncored_word`]: a glyph whose core holds its
    /// word from every glass holds it under the control too, which could not bite through it.
    #[test]
    fn each_solid_level_of_an_app_ink_keeps_the_bar_of_its_own_reading() {
        let word = uncored_word("180;63;180");
        let caps = grey_glass_word_caps(word, "180;63;180");
        assert!(
            caps.iter().all(|&k| k == 0),
            "`{word}` in #b43fb4 under a grey glass takes {caps:?}"
        );
        let least = with_least_level_bar(|| grey_glass_word_caps(word, "180;63;180"));
        assert!(
            least.iter().any(|&k| k > 0),
            "control: at its dimmest level's bar `{word}` in #b43fb4 takes no wash ({least:?})"
        );
    }

    /// The first of `wow` and `xxx` in `ink` (`r;g;b`) on Default Light at 24 px no glyph of which
    /// holds the word from every glass by its core alone: a core whose letter bar lies over its
    /// own reading, as one reading AA as the judges read it and under AA's level in hand does,
    /// keeps the word whole whatever its solid levels' bars. Under macOS's SF Mono, where
    /// amendment 65's control was measured, #b43fb4's is `wow`: that control's caps, [255, 255,
    /// 255, 223, 178, 148, ...], were `wow`'s, and a core holding it would have left them 0.
    /// Under Linux's bundled JetBrains Mono (14 x 28 px cells) `o`'s core (coverage 253) reads
    /// 4.4976, `x`'s (248) 4.32, and the word is `xxx`, whose caps under the control are those
    /// (Linux arm64, 2026-11-07).
    fn uncored_word(ink: &str) -> &'static str {
        use crate::rainbow_kitty::prism_glass::colour::{contrast, luminance};
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                .expect("the bundled face");
        let (cw, ch) = renderer.cell_size();
        let rgb = ink
            .split(';')
            .map(|c| c.parse::<u32>().expect("a channel"))
            .fold(0, |rgb, c| rgb << 8 | c);
        ["wow", "xxx"]
            .into_iter()
            .find(|word| {
                let input = page(format!("\x1b[38;2;{ink}m{word}\x1b[39m").as_bytes());
                let glyphs = renderer.glyph_ink_for(&input);
                let frame = glyphs
                    .for_frame(&input, (cw, ch))
                    .expect("the frame's glyph evidence");
                let corrected = frame.corrected();
                (0..word.len()).all(|col| {
                    let (_, coverage) = frame.coverage(0, col).expect("the glyph's raster");
                    let core = coverage
                        .core_level(luminance(rgb) as f32, luminance(BG) as f32, corrected)
                        .expect("an inked glyph");
                    let read =
                        contrast(aterm_render::blend_text(BG, rgb, BG, core, corrected), BG) as f32;
                    crate::cursor_glow::letter_bar(read, false) <= read
                })
            })
            .expect("a word no core holds")
    }

    /// After-land round 3: a letter the judges read at AA keeps AA. They give a reading to two
    /// decimals, so #b441b4, 4.4976 on Default Light, reads 4.50 there, and held at the share a
    /// letter under AA keeps, it took a wash that read 4.47 (the flood's `o` in #c1479f on GitHub
    /// Light, 4.4973). It takes no glass that lowers it. Negative control
    /// ([`with_aa_unrounded`](crate::cursor_glow::with_aa_unrounded)): AA held from 4.5 itself,
    /// the word takes a wash.
    #[test]
    fn a_letter_the_judges_read_at_aa_keeps_aa() {
        let caps = grey_glass_caps("180;65;180");
        assert!(
            caps.iter().all(|&k| k == 0),
            "#b441b4 under a grey glass takes {caps:?}"
        );
        let unrounded = crate::cursor_glow::with_aa_unrounded(|| grey_glass_caps("180;65;180"));
        assert!(
            unrounded.iter().any(|&k| k > 0),
            "control: with AA held from 4.5 #b441b4 takes no wash ({unrounded:?})"
        );
    }

    /// The lay of one pixel row a letter's field scales: the whole pixels either side stay one
    /// span each, and the letter's ring, whose scale the yield's law sets and the cap judged column
    /// by column, a span a pixel; the feather between, which only eases back to whole, one span
    /// along the line through its first and last pixels. A run the ring takes whole away lays one
    /// span of nothing. The control ([`LayMutant::PixelSpans`]) lays a span a feather pixel.
    #[test]
    fn a_letter_s_ring_lays_each_pixel_and_its_feather_one_span() {
        const S: [f32; 15] = [
            1.0, 1.0, 0.9, 0.85, 0.82, 0.8, 0.8, 0.8, 0.85, 0.95, 1.0, 0.5, 0.0, 0.0, 0.5,
        ];
        const R: [bool; 15] = [
            false, false, false, false, false, true, true, true, false, false, false, false, true,
            true, false,
        ];
        let spans = || {
            let mut y = GlassYield {
                scales: vec![Scale::WHOLE; 2],
                ..GlassYield::default()
            };
            let row = FieldRow {
                s: &S,
                ring: &R,
                x0: 0,
            };
            assert!(y.cut_spans((0, 2), (0, 0, 10, 20), (0, 15), Some(row)));
            y.spans
        };
        let laid = spans();
        let want = [
            (0, 2, 1.0, 1.0),
            (2, 5, 0.92, 0.8),
            (5, 6, 0.8, 0.8),
            (6, 7, 0.8, 0.8),
            (7, 8, 0.8, 0.8),
            (8, 10, 0.8, 1.0),
            (10, 11, 1.0, 1.0),
            (11, 12, 0.5, 0.5),
            (12, 14, 0.0, 0.0),
            (14, 15, 0.5, 0.5),
        ];
        assert_eq!(laid.len(), want.len(), "{laid:?}");
        for (got, want) in laid.iter().zip(want) {
            assert!(
                got.0 == want.0
                    && got.1 == want.1
                    && (got.2 - want.2).abs() < 1e-5
                    && (got.3 - want.3).abs() < 1e-5,
                "{got:?} against {want:?}"
            );
        }
        let control = with_lay_mutant(LayMutant::PixelSpans, spans);
        assert!(
            control.len() >= 13 && control.iter().all(|&(a, b, ..)| b - a <= 2),
            "control: a span a feather pixel: {control:?}"
        );
    }

    /// The red word over a whole glass at 24 px, its descenders' feathers on the rail one span
    /// each: fewer lights than a span a pixel ([`LayMutant::PixelSpans`]), on the same whole pixels
    /// and the same ring, and no feather pixel far from what a span a pixel laid it.
    #[test]
    fn a_word_s_feather_spans_spare_lights_and_keep_its_ring() {
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                .expect("the bundled face");
        let text = b"ab \x1b[38;2;196;38;46mgypq\x1b[39m cd".as_slice();
        let (cw, ch) = renderer.cell_size();
        let (cwi, chi) = (cw as i32, ch as i32);
        let mut lay = || {
            let mut input = page(text);
            let ink = renderer.glyph_ink_for(&input);
            let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
            input.glow_under = vec![quad];
            let glass = GlassLaid::new(
                Instant::now(),
                (vec![quad], GlowRamps::default()),
                (FG, BG),
                (0, 0, cwi, chi),
                None,
            );
            let mut glass_yield = GlassYield::default();
            let report = glass_yield.apply_observed(&mut input, &glass, &[], Some(&ink), true);
            assert!(report.cut(), "premise: the red word yields");
            no_coloured_zero(&input);
            (input, glass_yield)
        };
        let (spans, shaped) = lay();
        let (pixels, _) = with_lay_mutant(LayMutant::PixelSpans, &mut lay);
        assert!(
            spans.glow_under.len() < pixels.glow_under.len(),
            "{} lights against {} a span a pixel",
            spans.glow_under.len(),
            pixels.glow_under.len()
        );
        let whole = u32::from(GLASS.1);
        let field = shaped.fields.first().expect("the word's field");
        let mut feather = 0;
        for y in 0..chi {
            for x in 0..10 * cwi {
                let (a, b) = (alpha_at_y(&spans, x, y), alpha_at_y(&pixels, x, y));
                let row = field.row_at(y);
                if row.is_some_and(|row| row.eases(x)) {
                    feather += 1;
                    assert!(
                        a.abs_diff(b) as f32 <= FEATHER_SPAN_SLACK * whole as f32 + 2.0,
                        "({x}, {y}) in the feather: {a} against {b}"
                    );
                } else {
                    assert_eq!(a, b, "({x}, {y}) off the feather");
                }
            }
        }
        assert!(feather > 0, "premise: the word has a feather");
    }

    /// A piece of glass on the rail between two descenders, its pixel rows inside the box the row's
    /// field spans but scaled nowhere, stays one piece, and a word's letters no glass comes near are
    /// not fielded at all. The controls cut every row of the field's box ([`LayMutant::BandRows`])
    /// and field every letter of the word ([`LayMutant::AllLetters`]).
    #[test]
    fn glass_no_letter_reaches_is_laid_whole_and_fields_nothing() {
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                .expect("the bundled face");
        // Two descenders in the red, with glass over them and between them, on the rail.
        let red = "\x1b[38;2;196;38;46m";
        let text = format!("{red}g\x1b[39m{}{red}p\x1b[39m", " ".repeat(9));
        let (cw, ch) = renderer.cell_size();
        let (cwi, chi) = (cw as i32, ch as i32);
        let foot = rail_from(&mut renderer, &page(text.as_bytes()));
        let between = GlowQuad::flat(
            0,
            (4 * cw) as u16,
            foot as u16,
            (3 * cw) as u16,
            (chi - foot) as u16,
            GLASS.0,
            GLASS.1,
        );
        let mut lay = || {
            let mut input = page(text.as_bytes());
            let ink = renderer.glyph_ink_for(&input);
            let over = |col: usize| {
                GlowQuad::flat(
                    0,
                    (col * cw) as u16,
                    0,
                    cw as u16,
                    ch as u16,
                    GLASS.0,
                    GLASS.1,
                )
            };
            let quads = vec![over(0), between, over(10)];
            input.glow_under = quads.clone();
            let glass = GlassLaid::new(
                Instant::now(),
                (quads, GlowRamps::default()),
                (FG, BG),
                (0, 0, cwi, chi),
                None,
            );
            let mut glass_yield = GlassYield::default();
            let report = glass_yield.apply_observed(&mut input, &glass, &[], Some(&ink), true);
            assert!(report.cut(), "premise: the red letters yield");
            (input, glass_yield)
        };
        let pieces = |input: &RenderInput| {
            input
                .glow_under
                .iter()
                .filter(|q| q.x == between.x && q.w == between.w)
                .count()
        };
        let (input, shaped) = lay();
        assert_eq!(
            pieces(&input),
            1,
            "the glass between the letters stays whole"
        );
        assert!(
            input.glow_under.contains(&between),
            "the piece between the letters is the source's own"
        );
        assert_eq!(shaped.letters.len(), 2, "premise: both letters yield");
        let (control, _) = with_lay_mutant(LayMutant::BandRows, &mut lay);
        assert!(
            pieces(&control) > 1,
            "control: every row of the field's box cut, the piece is cut too"
        );
        // A twelve-letter word with glass over its first two cells.
        let mut fielded = || {
            let mut input = page(format!("{red}gpqgpqgpqgpq\x1b[39m").as_bytes());
            let ink = renderer.glyph_ink_for(&input);
            let quad = GlowQuad::flat(0, 0, 0, (2 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
            input.glow_under = vec![quad];
            let glass = GlassLaid::new(
                Instant::now(),
                (vec![quad], GlowRamps::default()),
                (FG, BG),
                (0, 0, cwi, chi),
                None,
            );
            let mut glass_yield = GlassYield::default();
            let report = glass_yield.apply_observed(&mut input, &glass, &[], Some(&ink), true);
            assert!(report.cut(), "premise: the red word yields");
            glass_yield.letters.len()
        };
        let near = fielded();
        assert!(near <= 3, "{near} letters of the word fielded");
        assert_eq!(
            with_lay_mutant(LayMutant::AllLetters, &mut fielded),
            12,
            "control: every letter of the word fielded"
        );
    }

    /// A letter's ring and feather are read off its glyph once ([`LetterStamp`]): a word's repeated
    /// letters share one stamp, a frame after it reads none anew, and what it lays is what a yield
    /// with nothing held lays.
    #[test]
    fn a_glyph_s_stamp_is_read_once() {
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                .expect("the bundled face");
        let text = b"\x1b[38;2;196;38;46mgpq gpq\x1b[39m".as_slice();
        let (cw, ch) = renderer.cell_size();
        let mut input = page(text);
        let ink = renderer.glyph_ink_for(&input);
        let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
        let glass = GlassLaid::new(
            Instant::now(),
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, cw as i32, ch as i32),
            None,
        );
        let mut held = GlassYield::default();
        let frame = |y: &mut GlassYield, input: &mut RenderInput| {
            input.glow_under = vec![quad];
            input.glow_under_ramps.clear();
            assert!(y.apply_observed(input, &glass, &[], Some(&ink), true).cut());
            input.glow_under.clone()
        };
        let first = frame(&mut held, &mut input);
        assert_eq!(held.letters.len(), 6, "premise: six letters yield");
        assert_eq!(held.letter_stamps.len(), 3, "three glyphs, three stamps");
        let second = frame(&mut held, &mut input);
        assert_eq!(held.letter_stamps.len(), 3, "a later frame reads none anew");
        assert_eq!(first, second);
        assert_eq!(first, frame(&mut GlassYield::default(), &mut input));
    }

    /// A word no piece of the glass reaches takes `255` unjudged: on a typed line the glass reaches
    /// the few words by the caret, and judging the rest each frame was a cost the typing path
    /// paid for nothing. Here the glass lies under `gpq` alone and `xyz` builds no glyph judge.
    #[test]
    fn a_word_the_glass_does_not_reach_is_not_judged() {
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                .expect("the bundled face");
        let text = b"\x1b[38;2;196;38;46mgpq xyz\x1b[39m".as_slice();
        let (cw, ch) = renderer.cell_size();
        let mut input = page(text);
        let ink = renderer.glyph_ink_for(&input);
        let quad = GlowQuad::flat(0, 0, 0, (3 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
        let glass = GlassLaid::new(
            Instant::now(),
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, cw as i32, ch as i32),
            None,
        );
        let mut y = GlassYield::default();
        input.glow_under = vec![quad];
        assert!(
            y.apply_observed(&mut input, &glass, &[], Some(&ink), true)
                .cut()
        );
        assert_eq!(y.judges.len(), 3, "the glyphs of `gpq`, and none of `xyz`");
    }

    /// The frame's lights keep to [`YIELD_LIGHT_BUDGET`], which nothing else bounds past the step
    /// that laid the glass: a frame whose railed lay would run past it, here a flood of descenders,
    /// takes whole words, every yielded word at its cap down to the foot of its cells, and stays
    /// with them for [`WHOLE_WORDS_HOLD`], so the glass at its words does not flip from frame to
    /// frame at the bound; it publishes the rail again once a frame past the hold fits. The control
    /// ([`with_no_light_budget`]) lays them all.
    #[test]
    fn a_flood_past_the_light_budget_takes_whole_words_and_holds_them() {
        const COLS: u16 = 60;
        const ROWS: u16 = 12;
        let mut renderer =
            aterm_render::Renderer::from_system(60.0, aterm_render::Theme::default())
                .expect("the bundled face");
        renderer.debug_block_on_lazy_fallbacks();
        let (cw, ch) = renderer.cell_size();
        let (cwi, chi) = (cw as i32, ch as i32);
        let mut term = Terminal::new(ROWS, COLS);
        term.process(b"\x1b]10;rgb:2b/2d/33\x07\x1b]11;rgb:f6/f7/f9\x07\x1b[38;2;196;38;46m");
        for _ in 0..ROWS {
            term.process("gypqgypqgypqgypqgypqgypqgypqgypqgypqgypqgypqgypqgypqgypqgypq".as_bytes());
        }
        let mut base = term.cell_frame(usize::from(ROWS), usize::from(COLS));
        base.cursor_visible = false;
        let _ = renderer.render_input(&base);
        let ink = renderer.glyph_ink_for(&base);
        let quads_over = |rows: u16, cols: u16| -> Vec<GlowQuad> {
            (0..rows)
                .map(|row| {
                    GlowQuad::flat(
                        row,
                        0,
                        (i32::from(row) * chi) as u16,
                        (i32::from(cols) * cwi) as u16,
                        ch as u16,
                        GLASS.0,
                        GLASS.1,
                    )
                })
                .collect()
        };
        let lay = |y: &mut GlassYield, quads: &[GlowQuad], at: Instant| {
            let mut input = base.clone();
            input.glow_under = quads.to_vec();
            let glass = GlassLaid::new(
                at,
                (quads.to_vec(), GlowRamps::default()),
                (FG, BG),
                (0, 0, cwi, chi),
                None,
            );
            let report = y.apply_observed(&mut input, &glass, &[], Some(&ink), true);
            assert!(report.cut(), "premise: the red letters yield");
            input
        };
        let lights = |input: &RenderInput| input.glow_under_ramps.laid(input.glow_under.len());
        let flood = quads_over(ROWS, COLS);
        let t0 = Instant::now();
        let mut held = GlassYield::default();
        let over = lay(&mut held, &flood, t0);
        assert!(
            lights(&over) <= YIELD_LIGHT_BUDGET,
            "the flood lays {} lights",
            lights(&over)
        );
        assert!(
            alpha_at_y(&over, 3 * cwi, chi - 1) < u32::from(GLASS.1) / 4,
            "a whole word's cell lies at its cap, the foot of its cell too"
        );
        let uncapped = with_no_light_budget(|| lay(&mut GlassYield::default(), &flood, t0));
        assert!(
            lights(&uncapped) > YIELD_LIGHT_BUDGET,
            "control: railed, the flood lays {} lights",
            lights(&uncapped)
        );
        // A short glass, which fits: railed alone, whole words while the hold lasts.
        let short = quads_over(1, 4);
        let free = lay(&mut GlassYield::default(), &short, t0);
        assert!(lights(&free) <= YIELD_LIGHT_BUDGET);
        let in_hold = lay(
            &mut held,
            &short,
            t0 + std::time::Duration::from_millis(500),
        );
        assert_ne!(
            in_hold.glow_under, free.glow_under,
            "the hold holds whole words"
        );
        let past = lay(
            &mut held,
            &short,
            t0 + std::time::Duration::from_millis(1_500),
        );
        assert_eq!(past.glow_under, free.glow_under, "past the hold, the rail");
    }

    /// §4 rule 1, sprite-shaped (after-land R2, the word icons): an under-text sprite (a `fox`'s
    /// peeking face, its chin in the top of the word's row, its box five cells wide over the word
    /// and the blanks either side) takes the glass only where its art draws, and the glass runs on
    /// whole through its box's clear margin, so the ribbon stays one run under a decorated word.
    /// Negative control: [`with_sprite_rect_cut`] takes every cell the box reaches, as 1.12 did,
    /// and the run breaks across the word.
    #[test]
    fn an_under_text_sprite_yields_where_it_draws_and_the_glass_runs_on_through_its_box() {
        use aterm_core::render::{FxArt, FxTexels, PxRect};
        // `fox` in the theme's ink on row 1, cols 3..6; the face's box over cols 2..7, from
        // 12 px above the row to 8 px into it, its art drawing cols 15..35 of its 50 down to
        // its 14th row: a chin 6 px into the row, over x 35..55.
        let text = b"xy\r\nab fox cd".as_slice();
        let (w, h) = (5 * CW, 20);
        let (sx, sy) = (2 * CW, CH - 12);
        let (dx0, dx1, drows) = (15, 35, 18);
        let mut texels = vec![0u8; (w * h * 4) as usize];
        for r in 0..drows {
            for c in dx0..dx1 {
                texels[((r * w + c) * 4 + 3) as usize] = 255;
            }
        }
        let face = FreeSprite {
            x: sx,
            y: sy,
            w: w as u16,
            h: h as u16,
            art: std::sync::Arc::new(
                FxArt::new(w as u16, h as u16, FxTexels::Rgba8Straight, texels)
                    .expect("a 50x20 art"),
            ),
            clip: PxRect::EVERYWHERE,
            tint: 0x00FF_FFFF,
            alpha: 255,
            flip_x: false,
            z: FreeZ::UnderText,
        };
        let lay = |rects: bool| {
            let mut input = page(text);
            let glass = glass_over(&mut input, 1, 10, Instant::now(), None);
            let mut apply =
                || GlassYield::default().apply(&mut input, &glass, std::slice::from_ref(&face));
            let report = if rects {
                with_sprite_rect_cut(apply)
            } else {
                apply()
            };
            assert!(report.cut(), "premise: the face's chin lies on the glass");
            no_coloured_zero(&input);
            input
        };
        let whole = u32::from(GLASS.1);
        let (row_top, chin) = (CH, sy + drows);
        assert!(chin > row_top, "premise: the chin reaches into the row");
        let shaped = lay(false);
        // Every pixel the face draws on the row lies on no glass.
        for y in row_top..chin {
            for x in sx + dx0..sx + dx1 {
                assert_eq!(alpha_at_y(&shaped, x, y), 0, "the chin at ({x}, {y})");
            }
        }
        // The glass runs on whole everywhere else on the row: beside the chin and below it,
        // through the box's clear margin and under the word.
        let runs_on = |input: &RenderInput| {
            (row_top..2 * CH).all(|y| {
                (0..10 * CW).all(|x| {
                    let drawn = (row_top..chin).contains(&y) && (sx + dx0..sx + dx1).contains(&x);
                    drawn || alpha_at_y(input, x, y) == whole
                })
            })
        };
        assert!(
            runs_on(&shaped),
            "the glass runs on whole through the face's box and under its word"
        );
        assert!(
            !runs_on(&lay(true)),
            "control: cut across every cell its box reaches, the face breaks the run"
        );
    }

    /// §4 rule 3 for a wide glyph (after-land F2): a yielded CJK word lies flat at its cap across
    /// both cells of each glyph, and on the rail each glyph is read over both its cells (the
    /// frame's evidence of a wide glyph is its lead's): at the cap within its ring, and whole past
    /// that, so the ribbon stays one run across the word. Negative control:
    /// [`aterm_render::glyph_ink::with_narrow_evidence_only`] holds no evidence for a wide glyph,
    /// as before, and the word's cells are cut whole.
    #[test]
    fn a_yielded_wide_word_lies_flat_and_the_glass_runs_on_under_it() {
        // `你好` in the red the measured caps cut, plain words either side.
        let text = "ab \x1b[38;2;196;38;46m\u{4f60}\u{597d}\x1b[39m cd".as_bytes();
        let leads = [3_usize, 5];
        let lay = |narrow: bool| {
            let mut renderer =
                aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                    .expect("the bundled face");
            renderer.debug_block_on_lazy_fallbacks();
            let (cw, ch) = renderer.cell_size();
            let mut input = page(text);
            let ink = if narrow {
                aterm_render::glyph_ink::with_narrow_evidence_only(|| {
                    renderer.glyph_ink_for(&input)
                })
            } else {
                renderer.glyph_ink_for(&input)
            };
            let foot = rail_of(&ink);
            let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
            input.glow_under = vec![quad];
            let glass = GlassLaid::new(
                Instant::now(),
                (vec![quad], GlowRamps::default()),
                (FG, BG),
                (0, 0, cw as i32, ch as i32),
                None,
            );
            let mut glass_yield = GlassYield::default();
            let report = glass_yield.apply_observed(&mut input, &glass, &[], Some(&ink), true);
            assert!(report.cut(), "premise: the red word yields");
            no_coloured_zero(&input);
            let k = glass_yield.ks[3];
            let rows: Vec<(i32, Vec<[i16; 3]>)> = if narrow {
                Vec::new()
            } else {
                let frame = ink
                    .for_frame(&page(text), (cw, ch))
                    .expect("current evidence");
                leads
                    .iter()
                    .map(|&c| {
                        let rows = frame.ink_rows(0, c).expect("each wide glyph's raster");
                        (c as i32 * cw as i32, rows.to_vec())
                    })
                    .collect()
            };
            (input, rows, (cw as i32, ch as i32), foot, k)
        };
        let (shaped, letters, (cw, ch), foot, k) = lay(false);
        assert!(
            letters
                .iter()
                .all(|(_, rows)| rows.iter().any(|&[_, _, z]| i32::from(z) > cw)),
            "premise: each glyph inks its right half"
        );
        let capped = u32::from(scale_end(GLASS.0, GLASS.1, f32::from(k) / 255.0).1);
        assert!(
            (0..foot).all(|y| (3 * cw..7 * cw).all(|x| alpha_at_y(&shaped, x, y) == capped)),
            "the word's cells lie flat at its cap above the rail, both halves of each glyph"
        );
        // What each glyph inks on the rail is at the cap, its ring with it, right half too.
        let ring = LETTER_RING_PX as i32;
        assert!(
            letters.iter().all(|(cx, rows)| {
                rows.iter()
                    .filter(|r| i32::from(r[0]) >= foot)
                    .all(|&[y, a, z]| {
                        (cx + i32::from(a) - ring..cx + i32::from(z) + ring)
                            .all(|x| alpha_at_y(&shaped, x, i32::from(y)) == capped)
                    })
            }),
            "what a glyph lays on the rail lies at the word's cap"
        );
        let whole = u32::from(GLASS.1);
        let runs_on = |input: &RenderInput| {
            [2, 7]
                .iter()
                .all(|&c| alpha_at_y(input, c * cw + cw / 2, ch - 1) == whole)
                && (3 * cw..7 * cw).any(|x| alpha_at_y(input, x, ch - 1) == whole)
        };
        assert!(
            runs_on(&shaped),
            "the glass runs on whole on the rail, under the word and in the blanks beside it"
        );
        let (cut, _, _, _, _) = lay(true);
        assert!(
            !runs_on(&cut),
            "control: with no evidence for a wide glyph, the word's cells are cut whole"
        );
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

    /// The red the measured caps leave next to no glass (§4 Measured).
    const RED: u32 = 0x00C4_262E;

    /// What `ink`'s word lays [`GLASS`] at: the alpha of its cap (§4 rule 3).
    fn capped_alpha(ink: u32) -> u32 {
        let over = aterm_render::over_premul(BG, GLASS.0, GLASS.1) & 0x00FF_FFFF;
        let k = cap(ink, over, BG, solid_ink_bar(ink, BG));
        u32::from(scale_end(GLASS.0, GLASS.1, f32::from(k) / 255.0).1)
    }

    /// The cell's pixel row a word's foot rail starts on.
    fn rail_of(ink: &GlyphInk) -> i32 {
        let [_, _, _, y1] = ink
            .cell_box(GlyphInk::index('o', false))
            .expect("the face inks an o");
        i32::from(y1) + LETTER_RING_PX as i32
    }

    /// [`rail_of`] the face `renderer` lays `input` in.
    fn rail_from(renderer: &mut aterm_render::Renderer, input: &RenderInput) -> i32 {
        rail_of(&renderer.glyph_ink_for(input))
    }

    /// Every end the stream lays: a zero alpha carries no colour.
    fn no_coloured_zero(input: &RenderInput) {
        for q in &input.glow_under {
            for (c, a) in [(q.color, q.alpha), (q.color2, q.alpha2)] {
                assert!(a > 0 || c == 0, "an end of alpha 0 carries {c:06x}: {q:?}");
            }
        }
    }

    /// The measured caps (§4 rule 3 and its Measured table), over Default Light's shipped glass at
    /// its anchors, whole (alpha 255), each ink's glyph read as it is seen (§15.8 amendment 49) at
    /// its letter bar (amendment 64): the theme's ink `#2b2d33` keeps every anchor whole (the glass
    /// is solved for it); the grey `#474d57` keeps the yellow, the green and the cyan whole and
    /// about half of the rest; the inks under AA on the page keep the sliver of glass 97% of their
    /// own reading leaves. An ink lighter than the page keeps no glass. Negative control
    /// ([`LawMutant::FlatBar`]): at the flat 5.25 the inks at their own reading take nothing.
    #[test]
    fn the_caps_are_the_measured_ones() {
        use crate::rainbow_kitty::prism_glass::{ANCHOR_AT, Palette};
        let shipped = Palette::shipped(FG, BG).expect("Default Light's tables ship");
        // Red, orange, yellow, green, cyan, azure, lilac, magenta.
        let anchors = ANCHOR_AT.map(|at| shipped.glass_arc[at]);
        let caps = |ink: u32| anchors.map(|glass| cap(ink, glass, BG, solid_ink_bar(ink, BG)));
        for (ink, measured) in [
            (FG, [255, 255, 255, 255, 255, 255, 255, 255]),
            (0x0047_4D57, [140, 137, 255, 255, 255, 139, 146, 135]),
            (0x00C4_262E, [3, 3, 9, 7, 6, 3, 4, 3]),
            (0x000B_5CC4, [20, 19, 55, 41, 34, 20, 21, 19]),
            (0x00A2_269E, [24, 24, 68, 50, 42, 24, 26, 23]),
            (0x005C_6370, [4, 3, 10, 7, 6, 4, 4, 3]),
            (0x001A_7F37, [3, 3, 9, 7, 6, 3, 4, 3]),
            (0x008A_6100, [3, 3, 9, 7, 6, 3, 4, 3]),
            (0x000F_7684, [3, 3, 9, 7, 6, 3, 4, 3]),
            (0x006B_7280, [3, 3, 9, 7, 6, 3, 4, 3]),
        ] {
            assert_eq!(caps(ink), measured, "#{ink:06x}");
        }
        for (i, &glass) in anchors.iter().enumerate() {
            assert_eq!(
                cap(0x00FF_FFFF, glass, BG, solid_ink_bar(0x00FF_FFFF, BG)),
                0,
                "white ink on anchor {i}"
            );
        }
        with_law_mutant(LawMutant::FlatBar, || {
            assert_eq!(
                caps(0x00C4_262E),
                [0; 8],
                "control: #c4262e under a flat 5.25"
            );
            assert_eq!(
                caps(0x0047_4D57),
                [86, 84, 255, 255, 255, 86, 92, 83],
                "control: #474d57 at a flat 5.25"
            );
        });
        // The bar is read on the glyph as it is seen: the grey #5a5a5a is 6.43:1 on the page
        // itself and 5.27 seen, so a glass that leaves the page no darker keeps it whole.
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
        assert_eq!(
            cap(0x00C4_262E, pale, BG, YIELD_BAR),
            0,
            "#c4262e as seen, at 5.25"
        );
    }

    /// §4 rules 1-3 and 6: an app's red word takes its cap, the grey word beside it and the
    /// theme's letters keep the glass whole, and each blank beside the red word eases back to whole
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
        let red = capped_alpha(RED);
        assert!(red < whole / 4, "premise: the red keeps little: {red}");
        for col in 3..=5 {
            assert_eq!(
                cell_alpha(&input, 0, col),
                (red, red),
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
        let red = capped_alpha(RED);
        for col in 0..=5 {
            assert_eq!(
                cell_alpha(&input, 0, col),
                (red, red),
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
            (red, red),
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
        let red = capped_alpha(RED);
        for col in 2..=6 {
            assert_eq!(
                cell_alpha(&word, 0, col),
                (red, red),
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
        let red = capped_alpha(RED);
        for col in 14..17 {
            assert_eq!(
                cell_alpha(&paned, 0, col),
                (red, red),
                "the red word's cell {col}"
            );
        }
        let whole_row = run(None);
        assert_eq!(
            cell_alpha(&whole_row, 0, 12),
            (0, 0),
            "control: the neighbour's cells bound the blanks"
        );
        assert_eq!(
            cell_alpha(&whole_row, 0, 13).1,
            red,
            "control: the second blank eases only as far as the word's cap"
        );
    }

    /// §4 rule 3 over a ramp: a word's cap is the least over every row of each ramp under it,
    /// each by its own ends. A navy word over one ramp whose lower rows are a bright yellow and
    /// whose upper rows a blue the navy loses its bar over takes the blue's cap on every row, though
    /// the ramp's brightest rows (the yellow, the most alpha) would keep it whole; the negative
    /// control is the same ramp all yellow, which the word keeps whole.
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
            cap(NAVY, over(YELLOW), BG, solid_ink_bar(NAVY, BG)) == 255
                && cap(NAVY, over(BLUE), BG, solid_ink_bar(NAVY, BG)) < 255,
            "premise: the navy keeps its bar over the yellow and not the blue"
        );
        let (two, report) = lay(CH / 2);
        assert!(report.ramps, "the blue rows cut the ramp: {report:?}");
        let k = f32::from(cap(NAVY, over(BLUE), BG, solid_ink_bar(NAVY, BG))) / 255.0;
        for col in 0..4 {
            for (row_y, end) in [(2, BLUE), (CH - 3, YELLOW)] {
                let x = col * CW + CW / 2;
                assert_eq!(
                    alpha_at_y(&two, x, row_y),
                    u32::from(scale_end(end.0, end.1, k).1),
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
        // Through the yield: a dim glass beside a rule-1 ground eases from 0 without a coloured
        // zero.
        let at = Instant::now();
        let mut input = page(b"ab \x1b[48;2;220;230;255mgrn\x1b[49m cd");
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
    /// [`InkCell`]) takes the cap the frame's ink reads at, so the frame's candy ink stands at its
    /// bar on what is left of the glass, whatever its colour is that frame; the theme's word beside
    /// it keeps its glass. Its crown twin waits for 3.10 (§15.2 3.10).
    #[test]
    fn the_first_candy_frame_over_glass_meets_its_bar() {
        use crate::rainbow_kitty::prism_glass::colour::seen;
        const PINK: u32 = 0x00FF_5CA8;
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
            cap(PINK, pink_glass, BG, solid_ink_bar(PINK, BG)) < 255,
            "premise: the frame's candy ink is under its bar on the glass"
        );
        let _ = GlassYield::default().apply(&mut input, &glass, &[]);
        let (lo, hi) = cell_alpha(&input, 0, 0);
        assert_eq!(lo, hi, "the candy's word lies flat");
        for col in 1..3 {
            assert_eq!(
                cell_alpha(&input, 0, col),
                (lo, hi),
                "the candy's cell {col}"
            );
        }
        // No piece over the cell: its ground is the page's.
        let ground = input.glow_under.iter().find(|q| q.x == 0).map_or(BG, |q| {
            aterm_render::over_premul(BG, q.color, q.alpha) & 0x00FF_FFFF
        });
        let bar = solid_ink_bar(PINK, BG);
        assert!(
            seen(PINK, BG, ground) >= bar,
            "the candy reads {} on what its cap leaves, its bar {bar}",
            seen(PINK, BG, ground)
        );
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

    /// The frame's cells of `cat` in `ink`, as the word pass inks them.
    fn inked_word(ink: [u8; 3]) -> Vec<InkCell> {
        (0..3)
            .map(|col| InkCell {
                row: 0,
                col,
                color: ink,
            })
            .collect()
    }

    /// §4 rule 4 (after-land A7: `kitty` ringed at t0, t120, t470, a word popping clean as its
    /// animation ends): a word whose animating ink holds its bar on the whole glass takes no cut
    /// on a frame it animates, and one that does not takes the cap its ink reads at, no less.
    /// Negative control ([`LawMutant::AnimatedTakesNothing`]): rule 4 as it stood, every animating
    /// frame takes `0`.
    #[test]
    fn an_animating_ink_that_holds_takes_no_cut() {
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                .expect("the bundled face");
        let (cw, ch) = renderer.cell_size();
        let mut lay = |ink: [u8; 3]| {
            let mut input = page(b"cat sat");
            let judged = renderer.glyph_ink_for(&input);
            input.ink = inked_word(ink);
            let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
            input.glow_under = vec![quad];
            let glass = GlassLaid::new(
                Instant::now(),
                (vec![quad], GlowRamps::default()),
                (FG, BG),
                (0, 0, cw as i32, ch as i32),
                None,
            );
            let mut glass_yield = GlassYield::default();
            let report = glass_yield.apply_observed(&mut input, &glass, &[], Some(&judged), true);
            (glass_yield.ks[..3].to_vec(), report.cut())
        };
        let dark = [0x30, 0x10, 0x48];
        assert_eq!(
            lay(dark),
            (vec![255; 3], false),
            "the dark ink holds: nothing is cut on the frame it animates"
        );
        // Between the ink that holds and the one that has no room: the cap it reads at.
        let caps: Vec<u8> = (0..48_u8).map(|i| lay([i, 0x50 + i, 0xB8]).0[0]).collect();
        assert!(
            caps.iter().any(|&k| k > 0 && k < 255),
            "an ink that reads past the floor on the page takes the cap it reads at: {caps:?}"
        );
        let (pale, cut) = lay([0xFF, 0x5C, 0xA8]);
        assert!(
            cut && pale == [0; 3],
            "an ink under the floor, which its lattice step's bar leaves no room, takes the page: \
             {pale:?}"
        );
        let (control, cut) = with_law_mutant(LawMutant::AnimatedTakesNothing, || lay(dark));
        assert!(
            cut && control == [0; 3],
            "control: rule 4 as it stood cuts the dark ink too: {control:?}"
        );
    }

    /// §4 rule 4: an animating ink is judged as the grey [`animated_reading`] reads it as, and the
    /// ink itself keeps its bars at every alpha the grey's cap lets through, on every glass anchor:
    /// the lighter grey on the lattice reads no more on the page. The word's judges so stay few (a
    /// grey a lattice step wide) however the word pass moves its colour. Negative control
    /// ([`LawMutant::DarkerGrey`]): read on the lattice's darker side, some ink falls under.
    #[test]
    fn an_animating_ink_keeps_its_bars_under_its_grey_s_cap() {
        use crate::rainbow_kitty::prism_glass::{ANCHOR_AT, Palette};
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                .expect("the bundled face");
        let (cw, ch) = renderer.cell_size();
        let input = page(b"a");
        let ink = renderer.glyph_ink_for(&input);
        let frame = ink.for_frame(&input, (cw, ch)).expect("current evidence");
        let (_, coverage) = frame.coverage(0, 0).expect("the a's raster");
        let corrected = frame.corrected();
        let anchors = ANCHOR_AT.map(|at| {
            Palette::shipped(FG, BG)
                .expect("Default Light's tables ship")
                .glass_arc[at]
        });
        // The word pass's kinds of ink: candy letters, a self-glow's lifts, fully saturated ones.
        let inks = [
            0x00C0_1C6Cu32,
            0x0095_2FA8,
            0x002B_6BB8,
            0x0000_7F73,
            0x0065_7A00,
            0x00A8_5B00,
            0x00FF_5CA8,
            0x0078_4BE0,
            0x0000_6FD6,
            0x00D7_0000,
            0x0000_8700,
            0x00AF_00AF,
        ];
        let fails = || {
            let mut fails = 0;
            for &ink in &inks {
                let grey = animated_reading(ink) | ANIMATED_READING;
                let exact = GlyphJudge::new(coverage, ink, BG, FG, corrected).unwrap();
                let read = GlyphJudge::new(coverage, grey, BG, FG, corrected).unwrap();
                for &glass in &anchors {
                    for alpha in [96_u8, 160, 255] {
                        let lit = aterm_render::premul_rgb(glass, alpha);
                        let k = read.cap((lit, alpha));
                        for step in 1..=k {
                            let (color, a) = scale_end(lit, alpha, f32::from(step) / 255.0);
                            let ground = aterm_render::over_premul(BG, color, a);
                            fails += usize::from(!exact.keeps(ground));
                        }
                    }
                }
            }
            fails
        };
        assert_eq!(
            fails(),
            0,
            "an ink under the bar its grey's cap let through"
        );
        assert!(
            with_law_mutant(LawMutant::DarkerGrey, fails) > 0,
            "control: read on the lattice's darker side, some ink falls under"
        );
    }

    /// §4 rule 3: a cap under what the old floor (`200`) took to nothing is laid as it is, so the
    /// glass leaves a word by degrees. Negative control ([`LawMutant::Floor`]): the floor back, the
    /// word takes `0`.
    #[test]
    fn a_low_cap_is_laid_and_takes_no_floor() {
        const BLUE: u32 = 0x000B_5CC4;
        let lay = || {
            let mut input = page(b"\x1b[38;2;11;92;196mred\x1b[39m");
            let glass = glass_over(&mut input, 0, 8, Instant::now(), None);
            let _ = GlassYield::default().apply(&mut input, &glass, &[]);
            cell_alpha(&input, 0, 1)
        };
        let capped = capped_alpha(BLUE);
        assert!(capped > 0, "premise: a sliver of glass is left");
        assert_eq!(lay(), (capped, capped), "the word's cap");
        assert_eq!(
            with_law_mutant(LawMutant::Floor, lay),
            (0, 0),
            "control: the floor takes it to nothing"
        );
    }

    /// §4 rule 6: a blank between two caps eases from the one to the other across its cell,
    /// holding each for [`LETTER_RING_PX`], the ring its word's letters are read in: no step at a
    /// word's edge, and no letter's ring on a stronger glass than its cap. Negative control
    /// ([`LawMutant::FlatBlanks`]): flat at the lesser, the stronger word stands in a step.
    #[test]
    fn a_blank_between_two_caps_eases_and_holds_each_ring() {
        // A red word, a blank, a blue word: the two caps differ.
        let lay = || {
            let mut input =
                page(b"\x1b[38;2;196;38;46mred\x1b[39m \x1b[38;2;11;92;196mblu\x1b[39m");
            let glass = glass_over(&mut input, 0, 8, Instant::now(), None);
            let _ = GlassYield::default().apply(&mut input, &glass, &[]);
            input
        };
        let (red, blue) = (capped_alpha(RED), capped_alpha(0x000B_5CC4));
        assert!(red < blue, "premise: the caps differ: {red} {blue}");
        let eased = lay();
        let (x0, x1) = (3 * CW, 4 * CW);
        let ring = LETTER_RING_PX as i32;
        for x in x0..x0 + ring {
            assert_eq!(alpha_at(&eased, 0, x), red, "the red word's ring at {x}");
        }
        for x in x1 - ring..x1 {
            assert_eq!(alpha_at(&eased, 0, x), blue, "the blue word's ring at {x}");
        }
        let rises = (x0..x1 - 1).all(|x| alpha_at(&eased, 0, x) <= alpha_at(&eased, 0, x + 1));
        assert!(rises, "the blank eases from the one cap to the other");
        let flat = with_law_mutant(LawMutant::FlatBlanks, lay);
        assert_eq!(
            cell_alpha(&flat, 0, 3),
            (red, red),
            "control: flat at the lesser, the blank steps at the blue word"
        );
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
        let red = capped_alpha(RED);
        assert!(
            input.glow_under[..input.glow_under.len() - 1]
                .iter()
                .all(|q| q.x >= 30 || u32::from(q.alpha) == red),
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
