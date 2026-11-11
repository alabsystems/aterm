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
#[path = "glass_yield_tests.rs"]
mod tests;
