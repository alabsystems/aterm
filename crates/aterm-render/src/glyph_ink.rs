// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a face's glyphs lay down: each printable ASCII glyph's raster coverage, so a light that
//! lies under a letter can be judged against the ink the face really draws.
//!
//! A glyph's ink is rarely its colour whole. At a small size a stem straddles two pixel columns
//! and no pixel of it reaches full coverage, and every pixel short of full coverage shows what
//! lies under the glyph ([`crate::blend_text`]: the coverage, remapped from the cell's own
//! colours, laid in linear light over the framebuffer). A light laid under the text therefore
//! changes the letter's ink as well as its ground. SF Mono at 14 px, the macOS default face, draws
//! the `R` of the settings card's `README.md` with a one-column stem at 76% coverage: its core
//! reads 7.2:1 bare on GitHub Light, not the 15.8:1 of its colour, and under Comet's blue body it
//! read 4.41:1 (looks9's settings round 2). [`GlyphInk::core`] is the coverage the lab's
//! `trail-contrast` reads a glyph's core at, and [`GlyphInk::cell_box`] where in its cell the
//! glyph lays ink, so the letter guard can judge that ink where it lies.

use crate::{GlyphImage, GlyphKey, Renderer, StyleBits, TextBlending};

/// The first glyph [`GlyphInk`] measures (U+0020).
const FIRST: u32 = 0x20;
/// How many glyphs it measures per style: printable ASCII, U+0020..=U+007E.
const ASCII: usize = 95;
/// Levels of a glyph's coverage histogram: one per coverage byte.
const LEVELS: usize = 256;
/// A glyph's core, as `aterm-lab trail-contrast` reads one: its pixels at least this share of
/// the way from the ground to its fullest pixel in luminance (its `CORE_SHARE`).
pub const CORE_SHARE: f32 = 0.70;

/// A renderer's last [`GlyphInk`], under what it was measured at: the face epoch, the glyph
/// generation, the quantized size, the baseline and the blend.
pub(crate) type GlyphInkMemo = ((u64, u64, u32, i32, TextBlending), std::sync::Arc<GlyphInk>);

/// One glyph's raster coverage: its fullest pixel, how many inked pixels lie at each coverage
/// level, and where in its cell they lie.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyphCoverage {
    /// The fullest pixel's coverage (`0`: the glyph draws no ink).
    pub max: u8,
    /// Inked pixels per coverage byte, saturating (level `0` holds none).
    pub levels: [u16; LEVELS],
    /// The inked pixels' box in the cell, `[x0, y0, x1, y1]` (end exclusive) from the cell's
    /// top left as the renderer seats the raster (`xmin` across, `baseline - height - ymin`
    /// down); `None` where the placement is not known.
    pub cell_box: Option<[i16; 4]>,
}

impl Default for GlyphCoverage {
    fn default() -> Self {
        Self {
            max: 0,
            levels: [0; LEVELS],
            cell_box: None,
        }
    }
}

impl GlyphCoverage {
    /// The coverage histogram of a mono raster's bytes.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let mut out = Self::default();
        for &a in bytes {
            if a == 0 {
                continue;
            }
            out.max = out.max.max(a);
            let level = &mut out.levels[usize::from(a)];
            *level = level.saturating_add(1);
        }
        out
    }

    /// [`Self::of`] a mono raster `width` pixels wide whose top left lies at `(left, top)` in
    /// its cell, with the box its inked pixels fill there.
    #[must_use]
    pub fn placed(bytes: &[u8], width: usize, (left, top): (i32, i32)) -> Self {
        let mut out = Self::of(bytes);
        if width == 0 {
            return out;
        }
        let mut ink = (usize::MAX, usize::MAX, 0, 0);
        for (y, row) in bytes.chunks_exact(width).enumerate() {
            for (x, _) in row.iter().enumerate().filter(|&(_, &a)| a > 0) {
                ink = (
                    ink.0.min(x),
                    ink.1.min(y),
                    ink.2.max(x + 1),
                    ink.3.max(y + 1),
                );
            }
        }
        let at = |base: i32, n: usize| {
            i16::try_from(base.saturating_add(i32::try_from(n).unwrap_or(i32::MAX))).ok()
        };
        out.cell_box = (ink.0 < ink.2)
            .then(|| {
                Some([
                    at(left, ink.0)?,
                    at(top, ink.1)?,
                    at(left, ink.2)?,
                    at(top, ink.3)?,
                ])
            })
            .flatten();
        out
    }
}

/// Each printable ASCII glyph's coverage at one renderer's face and size, regular and bold, and
/// how that renderer remaps coverage ([`TextBlending`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlyphInk {
    regular: Box<[GlyphCoverage; ASCII]>,
    bold: Box<[GlyphCoverage; ASCII]>,
    /// The glyph every other character is judged as: the alphanumeric whose core lies lowest.
    other: u8,
    corrected: bool,
    /// A hash of everything above, so a host can key on the table cheaply.
    fingerprint: u64,
}

impl GlyphInk {
    /// The index [`Self::core`] reads a cell's glyph by: printable ASCII by its code (bold after
    /// regular), anything else as [`Self::OTHER`].
    #[inline]
    #[must_use]
    pub const fn index(ch: char, bold: bool) -> u8 {
        let code = ch as u32;
        if code < FIRST || code >= FIRST + ASCII as u32 {
            return Self::OTHER;
        }
        let i = (code - FIRST) as u8;
        if bold { i + ASCII as u8 } else { i }
    }

    /// The index of a glyph [`Self::index`] knows only as some other character.
    pub const OTHER: u8 = 2 * ASCII as u8;

    /// A table from coverage histograms, `regular` and `bold` printable ASCII in code order:
    /// what [`Renderer::glyph_ink`] measures (and a test's own face).
    #[must_use]
    pub fn from_coverage(
        regular: [GlyphCoverage; ASCII],
        bold: [GlyphCoverage; ASCII],
        corrected: bool,
    ) -> Self {
        // The alphanumeric whose mid-coverage pixels sit lowest stands in for every other glyph.
        let thin = |c: &GlyphCoverage| median_level(c, c.max / 2);
        let other = ('0'..='9')
            .chain('A'..='Z')
            .chain('a'..='z')
            .map(|ch| usize::from(Self::index(ch, false)))
            .filter(|&i| regular[i].max > 0)
            .min_by_key(|&i| thin(&regular[i]))
            .map_or(Self::OTHER, |i| u8::try_from(i).unwrap_or(Self::OTHER));
        let mut fingerprint =
            0xcbf2_9ce4_8422_2325_u64 ^ u64::from(corrected) ^ u64::from(other) << 8;
        for glyph in regular.iter().chain(bold.iter()) {
            let place = glyph.cell_box.unwrap_or([i16::MIN; 4]);
            for v in std::iter::once(u16::from(glyph.max))
                .chain(glyph.levels)
                .chain(place.map(|v| v.cast_unsigned()))
            {
                fingerprint = (fingerprint ^ u64::from(v)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        Self {
            regular: Box::new(regular),
            bold: Box::new(bold),
            other,
            corrected,
            fingerprint,
        }
    }

    /// A hash of the whole table: two tables with one fingerprint read every glyph alike.
    #[inline]
    #[must_use]
    pub const fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// Where glyph `index` ([`Self::index`]) lays its ink in its cell
    /// ([`GlyphCoverage::cell_box`]); `None` for a glyph known only as another
    /// ([`Self::OTHER`]), one that draws nothing, or one whose seat is not known.
    #[must_use]
    pub fn cell_box(&self, index: u8) -> Option<[i16; 4]> {
        let i = usize::from(index);
        let glyph = if i < ASCII {
            self.regular.get(i)?
        } else {
            self.bold.get(i.checked_sub(ASCII)?)?
        };
        glyph.cell_box
    }

    /// Glyph `index`'s ([`Self::index`]) core coverage in linear light, drawn in a colour of
    /// luminance `fg_l` on a cell ground of `bg_l`: the lower median coverage of its core, the
    /// pixels that reach [`CORE_SHARE`] of its fullest pixel's way from the ground in
    /// luminance; `None` for a glyph that draws no ink or an index the table does not hold.
    #[must_use]
    pub fn core(&self, index: u8, fg_l: f32, bg_l: f32) -> Option<f32> {
        let index = if index == Self::OTHER {
            self.other
        } else {
            index
        };
        let i = usize::from(index);
        let glyph = if i < ASCII {
            self.regular.get(i)?
        } else {
            self.bold.get(i - ASCII)?
        };
        if glyph.max == 0 {
            return None;
        }
        let cover = |a: u8| {
            let a = f32::from(a) / 255.0;
            if self.corrected {
                crate::correct_alpha(fg_l, bg_l, a)
            } else {
                a
            }
        };
        let full = cover(glyph.max);
        // The core's lowest level: the first that reaches the share (the remap only rises
        // with coverage).
        let floor = (1..=glyph.max)
            .find(|&a| cover(a) >= CORE_SHARE * full)
            .unwrap_or(glyph.max);
        Some(cover(median_level(glyph, floor)))
    }
}

/// The lower median coverage of `glyph`'s pixels at or above `floor`.
fn median_level(glyph: &GlyphCoverage, floor: u8) -> u8 {
    let from = usize::from(floor);
    let total: u32 = glyph.levels[from..].iter().map(|&n| u32::from(n)).sum();
    if total == 0 {
        return glyph.max;
    }
    // The lower median, counted from the fullest level down: its rank from the top.
    let rank = total / 2 + 1;
    let mut seen = 0u32;
    for a in (from..LEVELS).rev() {
        seen += u32::from(glyph.levels[a]);
        if seen >= rank {
            return u8::try_from(a).unwrap_or(u8::MAX);
        }
    }
    floor
}

impl Renderer {
    /// [`GlyphInk`] at this renderer's face and size: each printable ASCII glyph's raster
    /// coverage, regular and bold, as [`Self::glyph_image`] rasterizes it. Memoized until the
    /// face, the size or the blend changes.
    pub fn glyph_ink(&mut self) -> std::sync::Arc<GlyphInk> {
        let key = (
            self.font_epoch,
            self.glyph_generation,
            self.px_q,
            self.baseline,
            self.text_blending,
        );
        if let Some((memo_key, ink)) = &self.glyph_ink_memo
            && *memo_key == key
        {
            return std::sync::Arc::clone(ink);
        }
        let baseline = self.baseline;
        let mut measure = |style: Option<StyleBits>| -> [GlyphCoverage; ASCII] {
            std::array::from_fn(|i| {
                let Some(ch) = u32::try_from(i)
                    .ok()
                    .and_then(|i| char::from_u32(FIRST + i))
                else {
                    return GlyphCoverage::default();
                };
                let key: GlyphKey = match style {
                    Some(style) => self.glyph_key_styled(ch, style),
                    None => self.glyph_key(ch),
                };
                match self.glyph_image(key) {
                    GlyphImage::Mono {
                        bytes,
                        width,
                        height,
                        xmin,
                        ymin,
                        ..
                    } => {
                        let top = i32::try_from(*height).map_or(i32::MIN, |h| {
                            baseline.saturating_sub(h).saturating_sub(*ymin)
                        });
                        GlyphCoverage::placed(bytes, *width, (*xmin, top))
                    }
                    GlyphImage::Rgba { .. } => GlyphCoverage::default(),
                }
            })
        };
        let regular = measure(None);
        let bold = measure(Some(StyleBits::BOLD));
        let ink = std::sync::Arc::new(GlyphInk::from_coverage(
            regular,
            bold,
            matches!(self.text_blending, TextBlending::LinearCorrected),
        ));
        self.glyph_ink_memo = Some((key, std::sync::Arc::clone(&ink)));
        ink
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A glyph whose pixels all sit at coverage `a`.
    fn flat(a: u8, n: u16) -> GlyphCoverage {
        let mut c = GlyphCoverage {
            max: a,
            ..GlyphCoverage::default()
        };
        c.levels[usize::from(a)] = n;
        c
    }

    fn table(glyph: GlyphCoverage, corrected: bool) -> GlyphInk {
        GlyphInk::from_coverage([glyph; ASCII], [glyph; ASCII], corrected)
    }

    /// The core of a glyph drawn at full coverage is its colour whole, and a thin one's lies
    /// lower; with the remap, dark ink on paper covers more than its raw coverage, light ink on
    /// a dark ground less (`correct_alpha`). A glyph that draws nothing has no core.
    #[test]
    fn a_glyphs_core_is_its_median_core_coverage() {
        let (ink, paper) = (0.0164, 1.0);
        let full = table(flat(255, 40), false);
        assert_eq!(
            full.core(GlyphInk::index('R', false), ink, paper),
            Some(1.0)
        );
        let mut thin = flat(255, 8);
        thin.levels[224] = 30;
        let raw = table(thin, false).core(GlyphInk::index('R', false), ink, paper);
        assert_eq!(raw, Some(224.0 / 255.0), "the stem's coverage: {raw:?}");
        let on_paper = table(thin, true)
            .core(GlyphInk::index('R', false), ink, paper)
            .expect("a core");
        assert!(on_paper > 0.9, "dark ink on paper, remapped: {on_paper}");
        let on_dark = table(thin, true)
            .core(GlyphInk::index('R', false), 0.6, 0.006)
            .expect("a core");
        assert!(
            on_dark < 224.0 / 255.0,
            "light ink on dark, remapped: {on_dark}"
        );
        assert_eq!(
            table(GlyphCoverage::default(), false).core(0, ink, paper),
            None
        );
    }

    /// A glyph's ink box is its inked pixels' box, seated where the raster's top left lies in
    /// the cell; a raster with no ink has none.
    #[test]
    fn a_glyphs_ink_box_is_where_its_raster_seats_its_ink() {
        let raster = [0, 0, 0, 0, 9, 0, 0, 0, 7];
        let placed = GlyphCoverage::placed(&raster, 3, (2, 5));
        assert_eq!(placed.cell_box, Some([3, 6, 5, 8]));
        assert_eq!(placed.max, 9);
        assert_eq!(GlyphCoverage::placed(&[0; 9], 3, (2, 5)).cell_box, None);
        assert_eq!(GlyphCoverage::placed(&raster, 0, (2, 5)).cell_box, None);
        let mut regular = [flat(255, 40); ASCII];
        regular[usize::from(GlyphInk::index('m', false))] = placed;
        let ink = GlyphInk::from_coverage(regular, [flat(255, 40); ASCII], false);
        assert_eq!(
            ink.cell_box(GlyphInk::index('m', false)),
            Some([3, 6, 5, 8])
        );
        assert_eq!(ink.cell_box(GlyphInk::OTHER), None);
    }

    /// Printable ASCII reads by its code, bold after regular; anything else as the thinnest
    /// alphanumeric.
    #[test]
    fn a_cell_reads_its_own_glyph_or_the_thinnest_letter() {
        assert_eq!(GlyphInk::index(' ', false), 0);
        assert_eq!(GlyphInk::index('~', true), 2 * ASCII as u8 - 1);
        assert_eq!(GlyphInk::index('你', false), GlyphInk::OTHER);
        let mut regular = [flat(255, 40); ASCII];
        regular[usize::from(GlyphInk::index('l', false))] = flat(128, 20);
        let ink = GlyphInk::from_coverage(regular, [flat(255, 40); ASCII], false);
        assert_eq!(
            ink.core(GlyphInk::OTHER, 0.0164, 1.0),
            ink.core(GlyphInk::index('l', false), 0.0164, 1.0)
        );
    }
}
