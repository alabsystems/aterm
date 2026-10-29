// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The exhaustive interval scan (`docs/design/PRISM-GLASS.md` §1 "The lerp-aware lift", §3): the
//! legibility of every colour the light glass can lay behind a glyph.
//!
//! A slab's two vertices read the bed's sampler ([`BedInkLut::sample`]) one slab of walk apart,
//! and the sampler is piecewise CONSTANT in `t`: a `lerp_rgb` channel changes only where
//! `a + (b − a)·u + 0.5` crosses an integer. Every origin between two neighbouring breakpoints of
//! `sample(t)` and of `sample(t + step)` therefore lays the same pair, so scoring one origin
//! inside each such interval scores every origin there is, on both legs of the reflected walk.
//! Each pair is scored under both column models, at every alpha 1..=255 over the page:
//! [`Columns::K16`], `lerp_rgb` at every sixteenth between the vertices, and [`Columns::Gpu`],
//! the renderer's own columns (`glow_lerp_rgb` between the slab's premultiplied ends across the
//! slab's width, then `over_premul`, which is what the CPU raster and the GPU's `glow8` lay). A
//! colour whose proven lower bound clears the bar (each composite channel is at least its least
//! over every alpha) is skipped; every other one is composited at every alpha.

use aterm_hash::{FxHashMap, FxHashSet};
use aterm_render::{glow_lerp_rgb, over_premul, premul_rgb};

use super::colour::{linear_levels, luminance};
use crate::effect_util::lerp_rgb;
use crate::rainbow_kitty::meteor::tri;
use crate::rainbow_kitty::ribbon::{BED_INK_LUT_LEN, BedInkLut};

/// A slab density: the walk between a slab's two vertices and the renderer's column widths.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slabs {
    /// Slabs per cell.
    pub per_cell: u8,
    /// Walk `t` between a slab's vertices.
    pub step: f32,
    /// The widths, in device pixels, of the slabs one 15 px cell splits into.
    pub widths: &'static [u32],
}

/// One slab per cell on the walk's fast leg: vertices `1 / WALK_FAST_CELLS` apart, 15 px wide.
pub const ONE_SLAB: Slabs = Slabs {
    per_cell: 1,
    step: 1.0 / 16.0,
    widths: &[15],
};

/// Two slabs per cell: 8 and 7 px.
pub const TWO_SLABS: Slabs = Slabs {
    per_cell: 2,
    step: 1.0 / 32.0,
    widths: &[8, 7],
};

/// Three slabs per cell: 5 px each.
pub const THREE_SLABS: Slabs = Slabs {
    per_cell: 3,
    step: 1.0 / 48.0,
    widths: &[5],
};

/// One slab per cell on the walk's lay leg (`WALK_LAY_RATE`).
pub const LAY_LEG: Slabs = Slabs {
    per_cell: 1,
    step: 1.0 / 36.0,
    widths: &[15],
};

/// The densities `slabs_for` can choose, which the lift holds to its target.
pub const SLAB_DENSITIES: [Slabs; 3] = [ONE_SLAB, TWO_SLABS, THREE_SLABS];

/// How the colours between a slab's two vertices are laid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Columns {
    /// `lerp_rgb` at `k / 16`, `k` 0..=16.
    K16,
    /// The renderer's columns across each of the density's slab widths.
    Gpu,
}

/// Breakpoints closer than this to a probe are probed on both sides too, so an interval narrower
/// than the sampler's f32 resolution is not missed.
const PROBE: f64 = 2.0e-7;

/// The glass page the scan composites over, and the theme fg it reads against.
///
/// The scan works in WCAG luminance: the theme fg's contrast on a colour is monotone in the
/// colour's luminance, so the darkest composite is the worst and each bar is one luminance.
#[derive(Clone, Debug)]
pub struct Ground {
    /// WCAG's linear light of each 8-bit level.
    lin: [f64; 256],
    fg_y: f64,
    page: u32,
    /// Per channel, each 8-bit level's least composite over the page at any alpha 1..=255.
    least: [[u8; 256]; 3],
    /// The least luminance of a K16 column over every alpha, by colour.
    column_memo: FxHashMap<u32, f64>,
}

impl Ground {
    /// The ground of theme fg `fg` on page `page`.
    #[must_use]
    pub fn new(fg: u32, page: u32) -> Self {
        let page = page & 0x00FF_FFFF;
        let mut least = [[u8::MAX; 256]; 3];
        for v in 0..256u32 {
            let grey = (v << 16) | (v << 8) | v;
            for a in 1..=255u8 {
                let o = over_premul(page, premul_rgb(grey, a), a);
                for (ch, row) in least.iter_mut().enumerate() {
                    let c = ((o >> (16 - 8 * ch)) & 0xff) as u8;
                    row[v as usize] = row[v as usize].min(c);
                }
            }
        }
        Self {
            lin: *linear_levels(),
            fg_y: luminance(fg),
            page,
            least,
            column_memo: FxHashMap::default(),
        }
    }

    /// The WCAG luminance of `c`.
    #[inline]
    fn y(&self, c: u32) -> f64 {
        0.2126 * self.lin[((c >> 16) & 0xff) as usize]
            + 0.7152 * self.lin[((c >> 8) & 0xff) as usize]
            + 0.0722 * self.lin[(c & 0xff) as usize]
    }

    /// `y` over the theme fg's: its contrast when the colour is the lighter, under 1 when it is
    /// the darker.
    #[inline]
    fn ratio(&self, y: f64) -> f64 {
        (y + 0.05) / (self.fg_y + 0.05)
    }

    /// The luminance under which a colour's [`Self::ratio`] is under `thr`.
    #[inline]
    fn y_bar(&self, thr: f64) -> f64 {
        thr * (self.fg_y + 0.05) - 0.05
    }

    /// The contrast of the theme fg on `c`, under 1 when `c` is the darker.
    #[inline]
    #[must_use]
    pub fn contrast(&self, c: u32) -> f64 {
        self.ratio(self.y(c))
    }

    /// The least composite of `c` over every alpha, channel by channel: no composite is darker in
    /// any channel.
    #[inline]
    fn least_of(&self, c: u32) -> u32 {
        let ch = |i: usize| u32::from(self.least[i][((c >> (16 - 8 * i)) & 0xff) as usize]);
        (ch(0) << 16) | (ch(1) << 8) | ch(2)
    }

    /// The least luminance of `c` composited over the page at every alpha.
    fn column_y(&mut self, c: u32) -> f64 {
        if let Some(&y) = self.column_memo.get(&c) {
            return y;
        }
        let mut least = f64::INFINITY;
        for a in 1..=255u8 {
            least = least.min(self.y(over_premul(self.page, premul_rgb(c, a), a)));
        }
        self.column_memo.insert(c, least);
        least
    }

    /// The least contrast of the pair's columns under `model` at every alpha, when it is under
    /// `thr`.
    pub fn pair_min(
        &mut self,
        c0: u32,
        c1: u32,
        slabs: Slabs,
        model: Columns,
        thr: f64,
    ) -> Option<f64> {
        self.pair_worst(c0, c1, slabs, model, thr, false)
    }

    /// Whether any of the pair's columns under `model` at any alpha is under `thr`.
    pub fn pair_under(&mut self, c0: u32, c1: u32, slabs: Slabs, model: Columns, thr: f64) -> bool {
        self.pair_worst(c0, c1, slabs, model, thr, true).is_some()
    }

    /// [`Self::pair_min`], which stops at the first column under `thr` when `first` is set.
    fn pair_worst(
        &mut self,
        c0: u32,
        c1: u32,
        slabs: Slabs,
        model: Columns,
        thr: f64,
        first: bool,
    ) -> Option<f64> {
        let bar = self.y_bar(thr);
        if self.y(self.least_of(min_channels(c0, c1))) >= bar {
            return None;
        }
        let mut worst = f64::INFINITY;
        match model {
            Columns::K16 => {
                for k in 0..=16u8 {
                    let col = lerp_rgb(c0, c1, f32::from(k) / 16.0);
                    if self.y(self.least_of(col)) < bar {
                        worst = worst.min(self.column_y(col));
                        if first && worst < bar {
                            break;
                        }
                    }
                }
            }
            Columns::Gpu => {
                'alpha: for a in (1..=255u8).rev() {
                    let (p0, p1) = (premul_rgb(c0, a), premul_rgb(c1, a));
                    if self.y(over_premul(self.page, min_channels(p0, p1), a)) >= bar {
                        continue;
                    }
                    for &w in slabs.widths {
                        for i in 0..w {
                            let o = over_premul(self.page, glow_lerp_rgb(p0, p1, i, w), a);
                            worst = worst.min(self.y(o));
                            if first && worst < bar {
                                break 'alpha;
                            }
                        }
                    }
                }
            }
        }
        (worst < bar).then(|| self.ratio(worst))
    }
}

/// The least of two colours, channel by channel.
#[inline]
fn min_channels(a: u32, b: u32) -> u32 {
    let ch = |sh: u32| ((a >> sh) & 0xff).min((b >> sh) & 0xff);
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// The colour the bed's sampler lays at walk position `t`.
#[inline]
#[must_use]
pub fn sample(table: &[u32; BED_INK_LUT_LEN], t: f64) -> u32 {
    BedInkLut::sample(table, t as f32)
}

/// The table interval walk position `t` samples: its lower entry.
#[inline]
#[must_use]
pub fn interval_of(t: f64) -> usize {
    let x = tri(t as f32).clamp(0.0, 1.0) * (BED_INK_LUT_LEN - 1) as f32;
    (x as usize).min(BED_INK_LUT_LEN - 2)
}

/// Every walk position in `[0, 2)` where the sampler's colour can change, sorted.
#[must_use]
pub fn breakpoints(table: &[u32; BED_INK_LUT_LEN]) -> Vec<f64> {
    let last = (BED_INK_LUT_LEN - 1) as f64;
    let mut out = vec![0.0, 1.0];
    for (i, pair) in table.windows(2).enumerate() {
        for sh in [16u32, 8, 0] {
            let (ca, cb) = (
                f64::from((pair[0] >> sh) & 0xff),
                f64::from((pair[1] >> sh) & 0xff),
            );
            if ca == cb {
                continue;
            }
            let (lo, hi) = (ca.min(cb) as u32, ca.max(cb) as u32);
            for n in lo + 1..=hi {
                let u = (f64::from(n) - 0.5 - ca) / (cb - ca);
                if u > 0.0 && u < 1.0 {
                    let up = (i as f64 + u) / last;
                    out.extend([up, 2.0 - up]);
                }
            }
        }
    }
    out.sort_unstable_by(f64::total_cmp);
    out.dedup();
    out
}

/// One origin of each interval between the breakpoints of `sample(t)` and of `sample(t + step)`,
/// with probes beside every breakpoint.
#[must_use]
pub fn origins(breaks: &[f64], step: f32) -> Vec<f64> {
    let s = f64::from(step);
    let mut pts: Vec<f64> = breaks
        .iter()
        .flat_map(|&b| [b, (b - s).rem_euclid(2.0)])
        .collect();
    pts.sort_unstable_by(f64::total_cmp);
    pts.dedup();
    let mut out = Vec::with_capacity(pts.len() * 3);
    for (k, &p) in pts.iter().enumerate() {
        let next = pts.get(k + 1).copied().unwrap_or(pts[0] + 2.0);
        out.push((0.5 * (p + next)).rem_euclid(2.0));
        out.push((p - PROBE).rem_euclid(2.0));
        out.push((p + PROBE).rem_euclid(2.0));
    }
    out
}

/// The pair a slab at origin `t` lays.
#[inline]
#[must_use]
pub fn pair_at(table: &[u32; BED_INK_LUT_LEN], t: f64, step: f32) -> (u32, u32) {
    (sample(table, t), sample(table, t + f64::from(step)))
}

/// The entries a ramp from origin `t` reads: every one from its first vertex's interval to past
/// its second's.
#[must_use]
pub fn bracket(t: f64, step: f32) -> core::ops::RangeInclusive<usize> {
    let (i, j) = (interval_of(t), interval_of(t + f64::from(step)));
    i.min(j)..=i.max(j) + 1
}

/// The worst a scan found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Worst {
    /// The least contrast of the theme fg over any column at any alpha.
    pub contrast: f64,
    /// The origin that lays it.
    pub t: f64,
    /// The pair it lays.
    pub pair: (u32, u32),
}

/// One scan of a table at one density and one column model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScanReport {
    /// Intervals between breakpoints, each scored once.
    pub intervals: usize,
    /// The worst.
    pub worst: Worst,
}

/// Scan `table` on `ground` at `slabs` under `model`: the exact least contrast over every origin.
#[must_use]
pub fn scan(
    table: &[u32; BED_INK_LUT_LEN],
    ground: &mut Ground,
    slabs: Slabs,
    model: Columns,
) -> ScanReport {
    let breaks = breakpoints(table);
    let all = origins(&breaks, slabs.step);
    // Every entry is some slab's first vertex at alpha 255, so none of the scan can sit above
    // the table's own least.
    let mut worst = table
        .iter()
        .enumerate()
        .map(|(i, &c)| Worst {
            contrast: ground.contrast(c),
            t: i as f64 / (BED_INK_LUT_LEN - 1) as f64,
            pair: (c, c),
        })
        .min_by(|a, b| a.contrast.total_cmp(&b.contrast))
        .unwrap_or(Worst {
            contrast: f64::INFINITY,
            t: 0.0,
            pair: (0, 0),
        });
    let mut seen = FxHashSet::default();
    for &t in &all {
        let pair = pair_at(table, t, slabs.step);
        if !seen.insert(pair) {
            continue;
        }
        if let Some(v) = ground.pair_min(pair.0, pair.1, slabs, model, worst.contrast) {
            worst = Worst {
                contrast: v,
                t,
                pair,
            };
        }
    }
    ScanReport {
        intervals: all.len() / 3,
        worst,
    }
}

/// Every distinct colour `lerp_rgb` can draw between `a` and `b`: one `u` inside each interval
/// between the channels' breakpoints, with probes beside each, and both ends.
#[must_use]
pub fn all_lerps(a: u32, b: u32) -> Vec<u32> {
    let mut us = vec![0.0_f64, 1.0];
    for sh in [16u32, 8, 0] {
        let (ca, cb) = (f64::from((a >> sh) & 0xff), f64::from((b >> sh) & 0xff));
        if ca == cb {
            continue;
        }
        let (lo, hi) = (ca.min(cb) as u32, ca.max(cb) as u32);
        for n in lo + 1..=hi {
            let u = (f64::from(n) - 0.5 - ca) / (cb - ca);
            if u > 0.0 && u < 1.0 {
                us.push(u);
            }
        }
    }
    us.sort_unstable_by(f64::total_cmp);
    us.dedup();
    let mut out = vec![a & 0x00FF_FFFF, b & 0x00FF_FFFF];
    for w in us.windows(2) {
        for u in [0.5 * (w[0] + w[1]), w[0] + PROBE, w[1] - PROBE] {
            out.push(lerp_rgb(a, b, u as f32));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}
