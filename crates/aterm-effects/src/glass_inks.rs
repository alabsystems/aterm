// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness glass's one token set (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §4.1):
//! every ink lines B, D and E wear, made from the Autopilot [`CardPalette`] and what the pane
//! itself gives the glass (its ground, its ink and Claude's rule grey). A token that has a hue
//! takes it from the column alone ([`field`]), so the three lines read as one rainbow frame round
//! the composer; the macOS system accent never reaches them.
//!
//! With no palette (the master switch off, the agent's own switch off, an unhosted pane without
//! `unhosted_on`) every token is neutral: the rule is Claude's grey, the hero and the letters are
//! [`GlassInks::lit`], a mark is [`GlassInks::value`] and the warn ink is lit. It is never the
//! moonbow, which says Autopilot Off on a running harness.

use crate::cli_marks::{ICON_CONTRAST, NAME_CONTRAST};
use crate::color_math::{contrast_ratio, floor_contrast_keeping_hue, oklab, relative_luminance};
use crate::effect_util::mix_rgb;
use crate::harness_card::{CardPalette, dark_ground};
use crate::harness_panel::{STATUS_HUE_DRIFT_DEG, STATUS_HUE_GUARD_DEG, status_gap};
use crate::spectrum::{SPECTRUM_ANCHOR_AT, SPECTRUM_LUT_LEN};

#[cfg(test)]
#[path = "glass_inks_tests.rs"]
mod tests;

/// The floor a value reads at on its ground: a figure stands louder than the words round it.
pub const VALUE_CONTRAST: f32 = 7.0;

/// How far a value stands from the quiet words toward the pane's ink, in OKLab lightness: the
/// least step at which a figure reads apart from its words at a glance.
pub const VALUE_APART_L: f32 = 0.10;

/// The least OKLCh hue distance (degrees) of the warn ink from Claude's own status green and red
/// on the same ground ([`status_gap`]): a projected stop never reads as Claude's error.
pub const WARN_HUE_GUARD_DEG: f32 = STATUS_HUE_GUARD_DEG + STATUS_HUE_DRIFT_DEG;

/// The OKLCh chroma a letter keeps on paper wherever sRGB holds that much at its hue and at the
/// lightness it reads at, and else nearly the most sRGB holds there: Day's letters stay vivid,
/// never washed out. The print law's letters ([`CardPalette::letter_on`]) sit on the gamut's
/// edge; a teal darkened to 4.5:1 on white holds no more than about 0.09.
pub const DAY_CHROMA: f32 = 0.12;

/// How near the gamut's edge a Day letter sits where sRGB holds less than [`DAY_CHROMA`] at its
/// hue and lightness, allowing for the 8-bit step ([`day_vivid_floor`]).
#[cfg(any(test, debug_assertions))]
pub const DAY_NEAR_EDGE: f32 = 0.9;

/// The least OKLCh chroma a Day letter or mark `c` (`0x00RRGGBB`) keeps, the vivid law the
/// glass's tests hold Day to: [`DAY_CHROMA`], or nearly ([`DAY_NEAR_EDGE`]) the most sRGB holds
/// at `c`'s hue and lightness where that is less.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn day_vivid_floor(c: u32) -> f32 {
    DAY_CHROMA.min(DAY_NEAR_EDGE * gamut_chroma(c))
}

/// Whether `c` keeps Day's vivid law ([`day_vivid_floor`]).
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn day_vivid(c: u32) -> bool {
    crate::harness_panel::chroma_hue(c).0 >= day_vivid_floor(c)
}

/// The most OKLCh chroma sRGB holds at `c`'s lightness and hue.
#[cfg(any(test, debug_assertions))]
fn gamut_chroma(c: u32) -> f32 {
    let [l, a, b] = oklab(c);
    let hue = b.atan2(a);
    let (sin, cos) = hue.sin_cos();
    let (mut lo, mut hi) = (0.0f32, 0.5f32);
    for _ in 0..40 {
        let k = f32::midpoint(lo, hi);
        if in_srgb([l, k * cos, k * sin]) {
            lo = k;
        } else {
            hi = k;
        }
    }
    lo
}

/// Whether OKLab `[l, a, b]` lies inside sRGB (Björn Ottosson's matrices, unclamped).
#[cfg(any(test, debug_assertions))]
fn in_srgb([l, a, b]: [f32; 3]) -> bool {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3,
        -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3,
    ]
    .iter()
    .all(|v| (-1e-4..=1.0 + 1e-4).contains(v))
}

/// Where Cruise's warn sits on its spectrum: the orange anchor.
pub const WARN_CRUISE_T: f32 = SPECTRUM_ANCHOR_AT[1] as f32 / (SPECTRUM_LUT_LEN - 1) as f32;

/// Where Land's warn sits on its sunset: its second stop, `#FF8A1F`.
pub const WARN_LAND_T: f32 = 1.0 / 3.0;

/// How far a rule's arc is lifted toward the pane's ink before it is laid over the rule.
const RULE_TOWARD_INK: f32 = 0.25;

/// How much of the lifted arc is laid over Claude's rule grey: a decoration, never a word.
const RULE_OVER: f32 = 0.68;

/// How far the quiet words sit from the pane's ink toward a dark ground.
const QUIET_ON_DARK: f32 = 0.42;

/// How far the quiet words sit from the pane's ink toward a light ground.
const QUIET_ON_LIGHT: f32 = 0.35;

/// How far a value sits from the pane's ink toward its ground before its floors.
const VALUE_TOWARD_GROUND: f32 = 0.15;

/// The steps a floor toward white or black takes ([`toward`]).
const TOWARD_STEPS: u16 = 32;

/// The steps the warn ink's hue guard walks along the palette, from its anchor toward yellow.
const WARN_STEPS: u16 = 32;

/// How far one of the warn ink's steps moves along the palette.
const WARN_STEP: f32 = 1.0 / 128.0;

/// The palette position of column `col` of a span `cols` wide: `1 − col/(cols−1)`, the left end
/// `1.0` and the right end `0.0`. A column past the span takes its last; a span of one cell is
/// all left end.
#[must_use]
#[inline]
pub const fn field(col: usize, cols: usize) -> f32 {
    if cols <= 1 {
        return 1.0;
    }
    let col = if col < cols { col } else { cols - 1 };
    1.0 - col as f32 / (cols - 1) as f32
}

/// How many cell widths tall a cell counts along a card's perimeter ([`perimeter`]): a terminal
/// cell is about twice as tall as it is wide, as the launch card's pixels measure it.
pub const CELL_ASPECT: f32 = 2.0;

/// An edge of a card's border, in the order the arc runs round it ([`perimeter_at`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// The top edge, read left to right.
    Top,
    /// The right side, read down.
    Right,
    /// The bottom edge, read right to left.
    Bottom,
    /// The left side, read up.
    Left,
}

/// The perimeter fraction `s` of the point `at` along `edge` of a box `w` wide and `h` tall (one
/// unit for all three; `at` from the box's left on the top and the bottom edge, from its top on
/// a side): clockwise from the top-left corner, as the launch card maps its border (top `x`,
/// right `w + y`, bottom `w + h + (w − x)`, left `2w + h + (h − y)`, over `2(w + h)`). The one
/// mapping a card's border takes, by cell ([`perimeter`]) or by pixel.
#[must_use]
#[inline]
pub fn perimeter_at(edge: Edge, at: f32, w: f32, h: f32) -> f32 {
    let along = match edge {
        Edge::Top => at,
        Edge::Right => w + at,
        Edge::Bottom => w + h + (w - at),
        Edge::Left => 2.0 * w + h + (h - at),
    };
    along / (2.0 * (w + h))
}

/// The perimeter fraction `s` of the border cell at `row`, `col` of a card `rows` by `cols`
/// cells, at the cell's centre ([`perimeter_at`]), a row counted [`CELL_ASPECT`] cell widths
/// tall. A cell off the border reads as the left side's.
#[must_use]
pub fn perimeter(row: usize, col: usize, rows: usize, cols: usize) -> f32 {
    let (w, h) = (cols.max(1) as f32, rows.max(1) as f32 * CELL_ASPECT);
    let (x, y) = (col as f32 + 0.5, (row as f32 + 0.5) * CELL_ASPECT);
    let (edge, at) = if row == 0 {
        (Edge::Top, x)
    } else if col + 1 >= cols {
        (Edge::Right, y)
    } else if row + 1 >= rows {
        (Edge::Bottom, x)
    } else {
        (Edge::Left, y)
    };
    perimeter_at(edge, at, w, h).rem_euclid(1.0)
}

/// What a [`GlassInks`] is made from: the palette, the pane's ground and ink, and the grey
/// Claude draws its rules in (`0x00RRGGBB` each).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlassKey {
    /// The Autopilot palette the glass wears, `None` with no harness theme.
    pub palette: Option<CardPalette>,
    /// The pane's ground.
    pub ground: u32,
    /// The pane's ink.
    pub ink: u32,
    /// Claude's rule grey.
    pub rule: u32,
}

/// The glass's tokens on one pane (§4.1). The column-free tokens are solved once per key; the
/// ones with a hue ([`Self::rule`], [`Self::hero`], [`Self::letter`], [`Self::mark`]) are read at
/// a palette position, the column's [`field`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlassInks {
    key: GlassKey,
    dark: bool,
    lit: u32,
    value: u32,
    quiet: u32,
    track: u32,
    warn: u32,
}

/// How many solved keys a thread keeps ([`GlassInks::of`]): one pane's top line, rule and chips
/// share one, and a window rarely shows more than a few grounds at once.
const KEPT: usize = 4;

thread_local! {
    static SOLVED: std::cell::Cell<[Option<GlassInks>; KEPT]> =
        const { std::cell::Cell::new([None; KEPT]) };
}

impl GlassInks {
    /// The tokens for `palette` on a pane of `ground` and `ink` whose rules Claude draws in `rule`
    /// (`0x00RRGGBB` each), kept per key on this thread so a frame that asks again solves
    /// nothing.
    #[must_use]
    pub fn of(palette: Option<CardPalette>, ground: u32, ink: u32, rule: u32) -> Self {
        let key = GlassKey {
            palette,
            ground: ground & 0x00FF_FFFF,
            ink: ink & 0x00FF_FFFF,
            rule: rule & 0x00FF_FFFF,
        };
        SOLVED.with(|kept| {
            let mut all = kept.get();
            let at = all.iter().position(|g| g.is_some_and(|g| g.key == key));
            let inks = match at.and_then(|i| all[i]) {
                Some(inks) => inks,
                None => Self::solve(key),
            };
            let last = at.unwrap_or(KEPT - 1);
            all.copy_within(0..last, 1);
            all[0] = Some(inks);
            kept.set(all);
            inks
        })
    }

    /// The tokens solved for `key`, with no cache.
    #[must_use]
    pub fn solve(key: GlassKey) -> Self {
        let key = GlassKey {
            palette: key.palette,
            ground: key.ground & 0x00FF_FFFF,
            ink: key.ink & 0x00FF_FFFF,
            rule: key.rule & 0x00FF_FFFF,
        };
        let GlassKey {
            palette,
            ground,
            ink,
            rule,
        } = key;
        let dark = dark_ground(ground);
        let lit = floor_contrast_keeping_hue(ink, ground, NAME_CONTRAST);
        // On a mid grey a floor's own direction can flip: the quiet words and the values keep
        // the lit ink's side of the ground, lighter or darker.
        let light = relative_luminance(lit) > relative_luminance(ground);
        let reads = |c: u32, min: f32| contrast_ratio(c, ground) >= min;
        let quiet = toward(
            mix_rgb(
                ink,
                ground,
                if dark { QUIET_ON_DARK } else { QUIET_ON_LIGHT },
            ),
            light,
            |c| reads(c, NAME_CONTRAST),
        );
        // Never louder than a lit word: where the pane's own ink reads under the floor, both sit
        // at it.
        let quiet = if contrast_ratio(quiet, ground) > contrast_ratio(lit, ground) {
            lit
        } else {
            quiet
        };
        let quiet_l = oklab(quiet)[0];
        let apart = |c: u32| {
            let l = oklab(c)[0];
            if light {
                l - quiet_l >= VALUE_APART_L
            } else {
                quiet_l - l >= VALUE_APART_L
            }
        };
        let value = toward(mix_rgb(ink, ground, VALUE_TOWARD_GROUND), light, |c| {
            reads(c, VALUE_CONTRAST) && apart(c)
        });
        // A lit word (a hovered value, an unexpected mode) never reads quieter than a value at
        // rest: where the pane's own ink reads between the name floor and the value's (Solarized
        // reads at about 4.8:1), the lit ink is moved on toward its side until it reads as loud.
        let lit = toward(lit, light, |c| {
            contrast_ratio(c, ground) >= contrast_ratio(value, ground)
        });
        let track = floor_contrast_keeping_hue(rule, ground, ICON_CONTRAST);
        let warn = match palette {
            Some(CardPalette::Cruise) => warm(CardPalette::Cruise, WARN_CRUISE_T, ground, dark),
            Some(CardPalette::Land) => warm(CardPalette::Land, WARN_LAND_T, ground, dark),
            Some(CardPalette::Off) | None => lit,
        };
        Self {
            key,
            dark,
            lit,
            value,
            quiet,
            track,
            warn,
        }
    }

    /// What the tokens were made from.
    #[must_use]
    #[inline]
    pub const fn key(&self) -> GlassKey {
        self.key
    }

    /// The palette the glass wears, `None` with no harness theme.
    #[must_use]
    #[inline]
    pub const fn palette(&self) -> Option<CardPalette> {
        self.key.palette
    }

    /// Whether the ground is dark ([`dark_ground`]).
    #[must_use]
    #[inline]
    pub const fn dark(&self) -> bool {
        self.dark
    }

    /// A rule cell at palette position `t`: the arc lifted a quarter toward the pane's ink and
    /// laid 68% over Claude's rule grey, a decoration with no floor; Claude's grey itself with no
    /// palette. Where the arc lands on the grey it is moved by one step, so a tinted rule is never
    /// the grid's own.
    #[must_use]
    #[inline]
    pub const fn rule(&self, t: f32) -> u32 {
        let GlassKey {
            palette, ink, rule, ..
        } = self.key;
        let Some(palette) = palette else {
            return rule;
        };
        let lifted = mix_rgb(palette.art(t, self.dark), ink, RULE_TOWARD_INK);
        let fg = mix_rgb(rule, lifted, RULE_OVER);
        if fg == rule { fg ^ 0x0001_0000 } else { fg }
    }

    /// A card border's ink at perimeter fraction `s` (`0.0..1.0`, clockwise from the top-left
    /// corner, [`perimeter_at`] of a point on the border): the static arc, [`Self::rule`] at `tri(2s)`
    /// (the launch card's border at φ = 0, `docs/DESIGN-harness-glass-rainbow-2026-11-05.md`
    /// §1.4), worn by every fact card the glass opens; Claude's rule grey with no palette. The
    /// arc closes on itself: `s` and `s + 1` are one ink.
    #[must_use]
    #[inline]
    pub fn border_at(&self, s: f32) -> u32 {
        self.rule(crate::rainbow_kitty::meteor::tri(2.0 * s))
    }

    /// The hero word's letter at `k` across it (`0.0..=1.0`): the launch card's settled hero
    /// ([`CardPalette::hero_on`]), at 4.5:1 or more; [`Self::lit`] with no palette.
    #[must_use]
    #[inline]
    pub const fn hero(&self, k: f32) -> u32 {
        match self.key.palette {
            Some(palette) => palette.hero_with_polarity(k, self.key.ground, self.dark),
            None => self.lit,
        }
    }

    /// A letter at palette position `t` ([`CardPalette::letter_on`]), at 4.5:1 or more and on
    /// paper vivid ([`DAY_CHROMA`]): a chip's glyph and words, the projected line; [`Self::lit`]
    /// with no palette.
    #[must_use]
    #[inline]
    pub const fn letter(&self, t: f32) -> u32 {
        match self.key.palette {
            Some(palette) => palette.letter_with_polarity(t, self.key.ground, self.dark),
            None => self.lit,
        }
    }

    /// A mark at palette position `t`: on a dark ground the arc's own light floored along its hue
    /// to 3:1, on paper the letter ([`Self::letter`], 4.5:1); [`Self::value`] with no palette.
    #[must_use]
    #[inline]
    pub const fn mark(&self, t: f32) -> u32 {
        match self.key.palette {
            Some(palette) if self.dark => {
                floor_contrast_keeping_hue(palette.art(t, true), self.key.ground, ICON_CONTRAST)
            }
            Some(_) => self.letter(t),
            None => self.value,
        }
    }

    /// A lit word: the pane's ink floored to 4.5:1 along its hue, and moved on toward white or
    /// black where it would read quieter than [`Self::value`], so a lit fact never dims.
    #[must_use]
    #[inline]
    pub const fn lit(&self) -> u32 {
        self.lit
    }

    /// A value: the pane's ink a little toward its ground, floored to 7:1 and kept at least
    /// [`VALUE_APART_L`] of OKLab lightness from [`Self::quiet`] toward the ink.
    #[must_use]
    #[inline]
    pub const fn value(&self) -> u32 {
        self.value
    }

    /// A quiet word: the pane's ink well toward its ground, floored to 4.5:1 on the lit ink's
    /// side of the ground, and never louder than [`Self::lit`] (where the pane's own ink reads
    /// under the floor, the lit ink itself).
    #[must_use]
    #[inline]
    pub const fn quiet(&self) -> u32 {
        self.quiet
    }

    /// A track: Claude's rule grey floored to 3:1.
    #[must_use]
    #[inline]
    pub const fn track(&self) -> u32 {
        self.track
    }

    /// The warn ink, always drawn bold and always behind a shape (`╳`, `⧗` or `$`): Cruise's
    /// orange, Land's `#FF8A1F`, each a letter at 4.5:1 that keeps [`WARN_HUE_GUARD_DEG`] from
    /// Claude's status hues (on a ground where no step of its walk does, the step that stands
    /// farthest from them); under Off and with no palette [`Self::lit`], with no hue.
    #[must_use]
    #[inline]
    pub const fn warn(&self) -> u32 {
        self.warn
    }

    /// How a cell of a footer element reads under the pointer or the keyboard's selection
    /// (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1.5), the one place the look is
    /// decided: every element a click acts on is painted from it, and nothing else is. `rest` is
    /// the ink the cell wears at rest.
    ///
    /// Words take [`Self::lit`], whatever their column and their ink at rest, and a warning's
    /// keep [`Self::warn`]; both are underlined in the lit ink, so one line of one ink runs from
    /// the element's first glyph to its last under the blanks between them too. A chart keeps
    /// the inks it reads by and takes no line: under an eighth-block level a line reads as a
    /// second level. The weight is never changed, so no width and no fit moves with the pointer.
    #[must_use]
    #[inline]
    pub const fn hovered(&self, rest: u32, part: HoverPart) -> Hovered {
        match part {
            HoverPart::Text => Hovered {
                fg: self.lit,
                underline: Some(self.lit),
            },
            HoverPart::Warn => Hovered {
                fg: self.warn,
                underline: Some(self.lit),
            },
            HoverPart::Chart => Hovered {
                fg: rest,
                underline: None,
            },
        }
    }
}

/// What a cell of a hovered footer element is ([`GlassInks::hovered`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverPart {
    /// A glyph of its words or its mark, or a blank between two of them.
    Text,
    /// A glyph of a warning (`╳`, `⧗`, `$` and their words).
    Warn,
    /// A cell of a chart.
    Chart,
}

/// A hovered cell's paint ([`GlassInks::hovered`]): its ink, and the ink of the single underline
/// it takes, `None` for no line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hovered {
    /// The cell's ink (`0x00RRGGBB`).
    pub fg: u32,
    /// The underline's ink.
    pub underline: Option<u32>,
}

/// `start` moved toward white (`light`) or black in [`TOWARD_STEPS`] steps until `meets` holds,
/// the end itself where nothing nearer does: never across to the other side of the ground, as a
/// floor along the hue may go on a mid grey.
fn toward(start: u32, light: bool, meets: impl Fn(u32) -> bool) -> u32 {
    let end = if light { 0x00FF_FFFF } else { 0 };
    (0..=TOWARD_STEPS)
        .map(|k| mix_rgb(start, end, f32::from(k) / f32::from(TOWARD_STEPS)))
        .find(|&c| meets(c))
        .unwrap_or(end)
}

/// `palette`'s warn letter on `ground` from `t`: the letter there, walked toward yellow until its
/// hue keeps [`WARN_HUE_GUARD_DEG`] from Claude's status hues on a ground of that polarity
/// ([`clearest`]).
fn warm(palette: CardPalette, t: f32, ground: u32, dark: bool) -> u32 {
    clearest(
        palette.letter_with_polarity(t, ground, dark),
        (1..=WARN_STEPS)
            .map(|k| palette.letter_with_polarity(t + f32::from(k) * WARN_STEP, ground, dark)),
        |c| status_gap(c, dark),
    )
}

/// The first of `anchor` and then `walk` whose hue `gap` from Claude's status hues is
/// [`WARN_HUE_GUARD_DEG`] or more (a colour with no hue, `None`, keeps clear of them all); where
/// none is, the one that stands farthest from them (the first of equals), never one nearer.
fn clearest(anchor: u32, walk: impl Iterator<Item = u32>, gap: impl Fn(u32) -> Option<f32>) -> u32 {
    let far = |c: u32| gap(c).unwrap_or(f32::INFINITY);
    let mut best = (far(anchor), anchor);
    if best.0 >= WARN_HUE_GUARD_DEG {
        return anchor;
    }
    for c in walk {
        let k = far(c);
        if k >= WARN_HUE_GUARD_DEG {
            return c;
        }
        if k > best.0 {
            best = (k, c);
        }
    }
    best.1
}
