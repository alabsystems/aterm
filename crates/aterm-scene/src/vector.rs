// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **Bake-time vector rasterizer** for the cat-art v4 glyph pipeline.

use crate::atlas::Tile;
use crate::clampf;

/// One absolute path command in the glyph's normalized `0..1` frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCmd {
    /// Start a new subpath at `(x, y)` (implicitly closing any open one at fill time).
    Move(f32, f32),
    /// Straight segment to `(x, y)`.
    Line(f32, f32),
    /// Cubic Bézier `(x1, y1, x2, y2, x, y)` — two control points, then the endpoint.
    Cubic(f32, f32, f32, f32, f32, f32),
    /// Close the current subpath back to its `Move` point.
    Close,
}

/// The side length of a glyph's own fixed-point frame.
pub const FIXED_ONE: u16 = 4096;

#[inline]
const fn fixed_to_unit(v: u16) -> f32 {
    const INV_FIXED: f32 = 1.0 / FIXED_ONE as f32;
    (v as f32) * INV_FIXED
}

/// One absolute path command in a glyph's **fixed-point** `0..=FIXED_ONE` frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathSeg {
    /// Start a new subpath at `(x, y)` (implicitly closing any open one at fill time).
    Move(u16, u16),
    /// Straight segment to `(x, y)`.
    Line(u16, u16),
    /// Cubic Bézier `(x1, y1, x2, y2, x, y)` — two control points, then the endpoint.
    Cubic(u16, u16, u16, u16, u16, u16),
    /// Close the current subpath back to its `Move` point.
    Close,
}

impl PathSeg {
    /// Convert this fixed-point command in `0..=FIXED_ONE` to its normalized `0..=1` [`PathCmd`].
    #[inline]
    #[must_use]
    pub const fn to_cmd(self) -> PathCmd {
        match self {
            Self::Move(x, y) => PathCmd::Move(fixed_to_unit(x), fixed_to_unit(y)),
            Self::Line(x, y) => PathCmd::Line(fixed_to_unit(x), fixed_to_unit(y)),
            Self::Cubic(x1, y1, x2, y2, x, y) => PathCmd::Cubic(
                fixed_to_unit(x1),
                fixed_to_unit(y1),
                fixed_to_unit(x2),
                fixed_to_unit(y2),
                fixed_to_unit(x),
                fixed_to_unit(y),
            ),
            Self::Close => PathCmd::Close,
        }
    }
}

/// Affine scale+offset mapping glyph coordinates into device (tile) pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathTransform {
    pub scale_x: f32,
    pub scale_y: f32,
    pub dx: f32,
    pub dy: f32,
}

impl PathTransform {
    /// Map the glyph's 0..1 frame onto a `w`×`h` pixel box at the tile origin.
    #[inline]
    #[must_use]
    pub const fn fit(w: u32, h: u32) -> Self {
        Self {
            scale_x: w as f32,
            scale_y: h as f32,
            dx: 0.0,
            dy: 0.0,
        }
    }
}

/// Device-space coordinate bound before rasterization.
pub(crate) const COORD_LIMIT: f32 = 1.0e6;

const FLATTEN_TOL: f32 = 0.15;

#[derive(Clone, Copy)]
struct Edge {
    path: u32,
    ytop: f32,
    ybot: f32,
    xtop: f32,
    dxdy: f32,
}

struct FillScratch {
    edges: Vec<Edge>,
    active: Vec<Edge>,
    xs: Vec<(u32, f32)>,
    spans: Vec<(f32, f32)>,
    diff: Vec<i32>,
    extra: Vec<i32>,
}

thread_local! {
    static FILL_SCRATCH: std::cell::RefCell<FillScratch> = const {
        std::cell::RefCell::new(FillScratch {
            edges: Vec::new(),
            active: Vec::new(),
            xs: Vec::new(),
            spans: Vec::new(),
            diff: Vec::new(),
            extra: Vec::new(),
        })
    };
}

/// Fills `paths` (even-odd per path, union across paths) into `tile` with 4×4 supersampling.
pub fn fill_path(
    tile: &mut Tile,
    paths: &[&[PathCmd]],
    rgb: (f32, f32, f32),
    alpha: f32,
    t: PathTransform,
) {
    if alpha <= 0.0 || alpha.is_nan() || tile.width() == 0 || tile.height() == 0 || paths.is_empty()
    {
        return;
    }
    FILL_SCRATCH.with_borrow_mut(|scratch| fill_scanline(tile, paths, rgb, alpha, t, scratch));
}

fn fill_scanline(
    tile: &mut Tile,
    paths: &[&[PathCmd]],
    rgb: (f32, f32, f32),
    alpha: f32,
    t: PathTransform,
    scratch: &mut FillScratch,
) {
    let mut bbox = BBox::empty();
    let est = paths.iter().fold(0usize, |n, p| {
        p.iter().fold(n, |n, cmd| {
            let e = match *cmd {
                PathCmd::Cubic(..) => 4,
                _ => 1,
            };
            n.saturating_add(e)
        })
    });
    scratch.edges.clear();
    scratch.edges.reserve(est);
    for (pid, p) in paths.iter().enumerate() {
        flatten_path(
            p.iter().copied(),
            pid as u32,
            t,
            &mut bbox,
            &mut scratch.edges,
        );
    }
    rasterize_edges(tile, rgb, alpha, &bbox, paths.len() > 1, scratch);
}

fn fill_scanline_fixed(
    tile: &mut Tile,
    paths: &[&[PathSeg]],
    rgb: (f32, f32, f32),
    alpha: f32,
    t: PathTransform,
    scratch: &mut FillScratch,
) {
    let mut bbox = BBox::empty();
    let est = paths.iter().fold(0usize, |n, p| {
        p.iter().fold(n, |n, seg| {
            let e = match *seg {
                PathSeg::Cubic(..) => 4,
                _ => 1,
            };
            n.saturating_add(e)
        })
    });
    scratch.edges.clear();
    scratch.edges.reserve(est);
    for (pid, p) in paths.iter().enumerate() {
        flatten_path(
            p.iter().copied().map(PathSeg::to_cmd),
            pid as u32,
            t,
            &mut bbox,
            &mut scratch.edges,
        );
    }
    rasterize_edges(tile, rgb, alpha, &bbox, paths.len() > 1, scratch);
}

fn rasterize_edges(
    tile: &mut Tile,
    rgb: (f32, f32, f32),
    alpha: f32,
    bbox: &BBox,
    multi_path: bool,
    scratch: &mut FillScratch,
) {
    let FillScratch {
        edges,
        active,
        xs,
        spans,
        diff,
        extra,
    } = scratch;
    let Some((x0, x1, y0, y1)) = bbox.pixel_range(tile.width(), tile.height()) else {
        return;
    };
    let width = x1 - x0;
    edges.sort_unstable_by(|a, b| a.ytop.total_cmp(&b.ytop));
    active.clear();
    let mut next = 0usize;
    diff.clear();
    diff.resize(width + 1, 0);
    extra.clear();
    extra.resize(width, 0);
    const INV16: f32 = 1.0 / 16.0;
    for py in y0..y1 {
        let mut lo_t = width;
        let mut hi_t = 0usize;
        for sub in 0..4u32 {
            let sy = py as f32 + (sub as f32 + 0.5) * 0.25;
            while let Some(e) = edges.get(next)
                && e.ytop <= sy
            {
                if e.ybot > sy {
                    active.push(*e);
                }
                next += 1;
            }
            active.retain(|e| e.ybot > sy);
            if active.is_empty() {
                continue;
            }
            xs.clear();
            for e in active.iter() {
                xs.push((e.path, e.xtop + (sy - e.ytop) * e.dxdy));
            }
            if multi_path {
                xs.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
                spans.clear();
                let mut i = 0usize;
                while let (Some(a), Some(b)) = (xs.get(i), xs.get(i + 1)) {
                    if a.0 == b.0 {
                        spans.push((a.1, b.1));
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                if spans.len() > 1 {
                    spans.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
                }
                let mut cur: Option<(f32, f32)> = None;
                for &(a, b) in spans.iter() {
                    if b <= a {
                        continue;
                    }
                    cur = match cur {
                        None => Some((a, b)),
                        Some((ca, cb)) if a <= cb => Some((ca, cb.max(b))),
                        Some((ca, cb)) => {
                            add_span(diff, extra, x0, ca, cb, &mut lo_t, &mut hi_t);
                            Some((a, b))
                        }
                    };
                }
                if let Some((ca, cb)) = cur {
                    add_span(diff, extra, x0, ca, cb, &mut lo_t, &mut hi_t);
                }
            } else {
                xs.sort_unstable_by(|a, b| a.1.total_cmp(&b.1));
                let mut cur: Option<(f32, f32)> = None;
                for &[(_, a), (_, b)] in xs.as_chunks::<2>().0 {
                    if b <= a {
                        continue;
                    }
                    cur = match cur {
                        None => Some((a, b)),
                        Some((ca, cb)) if a <= cb => Some((ca, cb.max(b))),
                        Some((ca, cb)) => {
                            add_span(diff, extra, x0, ca, cb, &mut lo_t, &mut hi_t);
                            Some((a, b))
                        }
                    };
                }
                if let Some((ca, cb)) = cur {
                    add_span(diff, extra, x0, ca, cb, &mut lo_t, &mut hi_t);
                }
            }
        }
        if lo_t >= hi_t {
            continue;
        }
        if let (Some(ds), Some(es)) = (diff.get_mut(lo_t..hi_t), extra.get_mut(lo_t..hi_t)) {
            let mut run = 0i32;
            for (d, e) in ds.iter_mut().zip(es.iter_mut()) {
                run += *d;
                *d = 0;
                *e = (*e + run).clamp(0, 16);
            }
        }
        if let Some(d) = diff.get_mut(hi_t) {
            *d = 0;
        }
        let mut i = lo_t;
        while i < hi_t {
            let c = extra.get_mut(i).map_or(0, std::mem::take);
            if c <= 0 {
                i += 1;
            } else if c >= 16 {
                let mut j = i + 1;
                while j < hi_t && extra.get(j).copied() == Some(16) {
                    if let Some(e) = extra.get_mut(j) {
                        *e = 0;
                    }
                    j += 1;
                }
                tile.over_run((x0 + i) as i32, py as i32, (j - i) as u32, rgb, alpha);
                i = j;
            } else {
                tile.over((x0 + i) as i32, py as i32, rgb, alpha * c as f32 * INV16);
                i += 1;
            }
        }
    }
}

/// Fill fixed-point [`PathSeg`] drawlists (the codegen'd cat glyph layers) into `tile`.
pub fn fill_path_fixed(
    tile: &mut Tile,
    paths: &[&[PathSeg]],
    rgb: (f32, f32, f32),
    alpha: f32,
    t: PathTransform,
) {
    if alpha <= 0.0 || alpha.is_nan() || tile.width() == 0 || tile.height() == 0 || paths.is_empty()
    {
        return;
    }
    FILL_SCRATCH
        .with_borrow_mut(|scratch| fill_scanline_fixed(tile, paths, rgb, alpha, t, scratch));
}

#[inline]
fn add_span(
    diff: &mut [i32],
    extra: &mut [i32],
    x0: usize,
    a: f32,
    b: f32,
    lo_t: &mut usize,
    hi_t: &mut usize,
) {
    let width = extra.len();
    let clamp_px = |v: f32| -> usize { clampf(v - x0 as f32, 0.0, width as f32) as usize };
    let p_lo = clamp_px((a - 0.875).ceil());
    let p_hi = clamp_px((b - 0.125).ceil());
    if p_lo >= p_hi {
        return;
    }
    *lo_t = (*lo_t).min(p_lo);
    *hi_t = (*hi_t).max(p_hi);
    let f0 = clamp_px((a - 0.125).ceil()).max(p_lo);
    let f1 = clamp_px((b - 0.875).ceil()).min(p_hi);
    let bits = |px: usize| -> i32 {
        let base = (x0 + px) as f32;
        let mut n = 0i32;
        for off in [0.125f32, 0.375, 0.625, 0.875] {
            let pos = base + off;
            if pos >= a && pos < b {
                n += 1;
            }
        }
        n
    };
    if f0 < f1 {
        if let Some(d) = diff.get_mut(f0) {
            *d += 4;
        }
        if let Some(d) = diff.get_mut(f1) {
            *d -= 4;
        }
        for px in p_lo..f0 {
            if let Some(e) = extra.get_mut(px) {
                *e += bits(px);
            }
        }
        for px in f1..p_hi {
            if let Some(e) = extra.get_mut(px) {
                *e += bits(px);
            }
        }
    } else {
        for px in p_lo..p_hi {
            if let Some(e) = extra.get_mut(px) {
                *e += bits(px);
            }
        }
    }
}

struct BBox {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

impl BBox {
    #[inline]
    const fn empty() -> Self {
        Self {
            min_x: f32::INFINITY,
            min_y: f32::INFINITY,
            max_x: f32::NEG_INFINITY,
            max_y: f32::NEG_INFINITY,
        }
    }

    #[inline]
    const fn add(&mut self, x: f32, y: f32) {
        self.min_x = self.min_x.min(x);
        self.min_y = self.min_y.min(y);
        self.max_x = self.max_x.max(x);
        self.max_y = self.max_y.max(y);
    }

    #[inline]
    fn pixel_range(&self, w: u32, h: u32) -> Option<(usize, usize, usize, usize)> {
        if self.min_x > self.max_x || self.min_y > self.max_y {
            return None;
        }
        let x0 = clampf(self.min_x.floor(), 0.0, w as f32) as usize;
        let x1 = clampf(self.max_x.ceil() + 1.0, 0.0, w as f32) as usize;
        let y0 = clampf(self.min_y.floor(), 0.0, h as f32) as usize;
        let y1 = clampf(self.max_y.ceil() + 1.0, 0.0, h as f32) as usize;
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        Some((x0, x1, y0, y1))
    }
}

#[inline]
const fn sanitize(v: f32) -> f32 {
    if v.is_finite() {
        clampf(v, -COORD_LIMIT, COORD_LIMIT)
    } else {
        0.0
    }
}

fn flatten_path(
    cmds: impl IntoIterator<Item = PathCmd>,
    path: u32,
    t: PathTransform,
    bbox: &mut BBox,
    edges: &mut Vec<Edge>,
) {
    let map = |x: f32, y: f32| -> (f32, f32) {
        (
            sanitize(x * t.scale_x + t.dx),
            sanitize(y * t.scale_y + t.dy),
        )
    };
    let mut cur: Option<(f32, f32)> = None;
    let mut start: Option<(f32, f32)> = None;
    let mut push = |edges: &mut Vec<Edge>, p0: (f32, f32), p1: (f32, f32)| {
        bbox.add(p0.0, p0.1);
        bbox.add(p1.0, p1.1);
        if p0.1 == p1.1 {
            return;
        }
        let (top, bot) = if p0.1 < p1.1 { (p0, p1) } else { (p1, p0) };
        edges.push(Edge {
            path,
            ytop: top.1,
            ybot: bot.1,
            xtop: top.0,
            dxdy: (bot.0 - top.0) / (bot.1 - top.1),
        });
    };
    for cmd in cmds {
        match cmd {
            PathCmd::Move(x, y) => {
                if let (Some(c), Some(s)) = (cur, start)
                    && c != s
                {
                    push(edges, c, s);
                }
                let p = map(x, y);
                cur = Some(p);
                start = Some(p);
            }
            PathCmd::Line(x, y) => {
                let p = map(x, y);
                if let Some(c) = cur {
                    push(edges, c, p);
                } else {
                    start = Some(p);
                }
                cur = Some(p);
            }
            PathCmd::Cubic(x1, y1, x2, y2, x, y) => {
                let p3 = map(x, y);
                if let Some(p0) = cur {
                    let p1 = map(x1, y1);
                    let p2 = map(x2, y2);
                    flatten_cubic(p0, p1, p2, p3, &mut |a, b| push(edges, a, b));
                } else {
                    start = Some(p3);
                }
                cur = Some(p3);
            }
            PathCmd::Close => {
                if let (Some(c), Some(s)) = (cur, start)
                    && c != s
                {
                    push(edges, c, s);
                }
                cur = start;
            }
        }
    }
    if let (Some(c), Some(s)) = (cur, start)
        && c != s
    {
        push(edges, c, s);
    }
}

fn flatten_cubic(
    p0: (f32, f32),
    p1: (f32, f32),
    p2: (f32, f32),
    p3: (f32, f32),
    emit: &mut impl FnMut((f32, f32), (f32, f32)),
) {
    let lerp3 = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let e1x = p1.0 - lerp3(p0.0, p3.0, 1.0 / 3.0);
    let e1y = p1.1 - lerp3(p0.1, p3.1, 1.0 / 3.0);
    let e2x = p2.0 - lerp3(p0.0, p3.0, 2.0 / 3.0);
    let e2y = p2.1 - lerp3(p0.1, p3.1, 2.0 / 3.0);
    let err = 0.75 * e1x.abs().max(e1y.abs()).max(e2x.abs()).max(e2y.abs());
    let n = if err <= FLATTEN_TOL {
        1
    } else {
        clampf((err / FLATTEN_TOL).sqrt().ceil(), 1.0, 64.0) as u32
    };
    let mut prev = p0;
    for k in 1..=n {
        let t = k as f32 / n as f32;
        let mt = 1.0 - t;
        let a = mt * mt * mt;
        let b = 3.0 * mt * mt * t;
        let c = 3.0 * mt * t * t;
        let d = t * t * t;
        let q = (
            a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0,
            a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1,
        );
        emit(prev, q);
        prev = q;
    }
}

/// Parse one glyph-asset path string.
#[must_use]
pub fn parse_path(d: &str) -> Option<Vec<PathCmd>> {
    let mut out = Vec::new();
    let mut nums: Vec<f32> = Vec::new();
    let mut cmd: Option<char> = None;
    let bytes = d.as_bytes();
    let mut i = 0usize;
    let flush = |cmd: char, nums: &mut Vec<f32>, out: &mut Vec<PathCmd>| -> bool {
        let need = match cmd {
            'M' | 'L' => 2,
            'C' => 6,
            'Z' => return nums.is_empty(),
            _ => return false,
        };
        if !nums.len().is_multiple_of(need) {
            return false;
        }
        let mut first = true;
        for chunk in nums.chunks_exact(need) {
            match (cmd, chunk) {
                ('M', [x, y]) => {
                    if first {
                        out.push(PathCmd::Move(*x, *y));
                    } else {
                        out.push(PathCmd::Line(*x, *y));
                    }
                }
                ('L', [x, y]) => out.push(PathCmd::Line(*x, *y)),
                ('C', [x1, y1, x2, y2, x, y]) => {
                    out.push(PathCmd::Cubic(*x1, *y1, *x2, *y2, *x, *y));
                }
                _ => return false,
            }
            first = false;
        }
        let had = !nums.is_empty();
        nums.clear();
        had || cmd == 'Z'
    };
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_whitespace() || c == ',' {
            i += 1;
        } else if c.is_ascii_alphabetic() {
            if let Some(prev) = cmd
                && !flush(prev, &mut nums, &mut out)
            {
                return None;
            }
            match c {
                'M' | 'L' | 'C' => cmd = Some(c),
                'Z' => {
                    out.push(PathCmd::Close);
                    cmd = Some('Z');
                }
                _ => return None,
            }
            i += 1;
        } else {
            let s = i;
            if c == '-' || c == '+' {
                i += 1;
            }
            while i < bytes.len()
                && (bytes[i].is_ascii_digit()
                    || bytes[i] == b'.'
                    || bytes[i] == b'e'
                    || bytes[i] == b'E'
                    || ((bytes[i] == b'-' || bytes[i] == b'+')
                        && matches!(bytes.get(i.wrapping_sub(1)), Some(b'e' | b'E'))))
            {
                i += 1;
            }
            let tok = d.get(s..i)?;
            let v: f32 = tok.parse().ok()?;
            if cmd.is_none() || cmd == Some('Z') {
                return None;
            }
            nums.push(v);
        }
    }
    if let Some(prev) = cmd
        && !flush(prev, &mut nums, &mut out)
    {
        return None;
    }
    if !nums.is_empty() {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(t: &Tile, x: u32, y: u32) -> u8 {
        t.pixels()[((y * t.width() + x) * 4 + 3) as usize]
    }

    #[test]
    fn even_odd_holes_work() {
        let donut = [
            PathCmd::Move(0.1, 0.1),
            PathCmd::Line(0.9, 0.1),
            PathCmd::Line(0.9, 0.9),
            PathCmd::Line(0.1, 0.9),
            PathCmd::Close,
            PathCmd::Move(0.35, 0.35),
            PathCmd::Line(0.65, 0.35),
            PathCmd::Line(0.65, 0.65),
            PathCmd::Line(0.35, 0.65),
            PathCmd::Close,
        ];
        let mut t = Tile::new(80, 80);
        fill_path(
            &mut t,
            &[&donut],
            (0.0, 0.0, 0.0),
            1.0,
            PathTransform::fit(80, 80),
        );
        assert_eq!(alpha_at(&t, 40, 40), 0, "hole is transparent");
        assert!(alpha_at(&t, 40, 16) > 200, "ring is painted");
        let sq1 = [
            PathCmd::Move(0.1, 0.1),
            PathCmd::Line(0.6, 0.1),
            PathCmd::Line(0.6, 0.6),
            PathCmd::Line(0.1, 0.6),
            PathCmd::Close,
        ];
        let sq2 = [
            PathCmd::Move(0.4, 0.4),
            PathCmd::Line(0.9, 0.4),
            PathCmd::Line(0.9, 0.9),
            PathCmd::Line(0.4, 0.9),
            PathCmd::Close,
        ];
        let mut t2 = Tile::new(80, 80);
        fill_path(
            &mut t2,
            &[&sq1, &sq2],
            (0.0, 0.0, 0.0),
            1.0,
            PathTransform::fit(80, 80),
        );
        assert!(
            alpha_at(&t2, 40, 40) > 200,
            "sibling overlap unions, never cancels"
        );
    }

    #[test]
    fn fill_path_fixed_matches_float_frame() {
        let q = |f: f32| (f * FIXED_ONE as f32) as u16;
        let seg = [
            PathSeg::Move(q(0.125), q(0.75)),
            PathSeg::Line(q(0.875), q(0.75)),
            PathSeg::Cubic(q(0.875), q(0.25), q(0.125), q(0.25), q(0.5), q(0.5)),
            PathSeg::Close,
        ];
        let cmd = [
            PathCmd::Move(0.125, 0.75),
            PathCmd::Line(0.875, 0.75),
            PathCmd::Cubic(0.875, 0.25, 0.125, 0.25, 0.5, 0.5),
            PathCmd::Close,
        ];
        let mut a = Tile::new(48, 48);
        let mut b = Tile::new(48, 48);
        fill_path_fixed(
            &mut a,
            &[&seg],
            (0.2, 0.7, 0.9),
            0.9,
            PathTransform::fit(48, 48),
        );
        fill_path(
            &mut b,
            &[&cmd],
            (0.2, 0.7, 0.9),
            0.9,
            PathTransform::fit(48, 48),
        );
        assert_eq!(
            a.pixels(),
            b.pixels(),
            "fixed-point frame == float frame raster"
        );
        assert!(a.pixels().iter().any(|&x| x != 0), "actually painted");
        let mut t = Tile::new(16, 16);
        let before = t.pixels().to_vec();
        fill_path_fixed(
            &mut t,
            &[],
            (1.0, 1.0, 1.0),
            1.0,
            PathTransform::fit(16, 16),
        );
        fill_path_fixed(
            &mut t,
            &[&[PathSeg::Move(0, 0)]],
            (1.0, 1.0, 1.0),
            1.0,
            PathTransform::fit(16, 16),
        );
        assert_eq!(
            t.pixels(),
            &before[..],
            "degenerate fixed input paints nothing"
        );
    }

    #[test]
    fn degenerate_paths_are_safe() {
        let mut t = Tile::new(16, 16);
        let before = t.pixels().to_vec();
        fill_path(
            &mut t,
            &[],
            (1.0, 1.0, 1.0),
            1.0,
            PathTransform::fit(16, 16),
        );
        let move_only = [PathCmd::Move(0.5, 0.5)];
        let zero_area = [
            PathCmd::Move(0.2, 0.2),
            PathCmd::Line(0.8, 0.2),
            PathCmd::Close,
        ];
        let lone_close = [PathCmd::Close];
        fill_path(
            &mut t,
            &[&move_only, &zero_area, &lone_close, &[]],
            (1.0, 1.0, 1.0),
            1.0,
            PathTransform::fit(16, 16),
        );
        let box_cmd = [
            PathCmd::Move(0.0, 0.0),
            PathCmd::Line(1.0, 0.0),
            PathCmd::Line(1.0, 1.0),
            PathCmd::Line(0.0, 1.0),
            PathCmd::Close,
        ];
        fill_path(
            &mut t,
            &[&box_cmd],
            (1.0, 1.0, 1.0),
            f32::NAN,
            PathTransform::fit(16, 16),
        );
        assert_eq!(t.pixels(), &before[..], "degenerate input paints nothing");
    }

    #[test]
    fn out_of_range_coords_clamp() {
        let mut t = Tile::new(32, 32);
        let wild = [
            PathCmd::Move(-1.0e9, -1.0e9),
            PathCmd::Line(1.0e9, 0.5),
            PathCmd::Line(f32::NAN, f32::INFINITY),
            PathCmd::Close,
        ];
        fill_path(
            &mut t,
            &[&wild],
            (1.0, 0.0, 0.0),
            1.0,
            PathTransform::fit(32, 32),
        );
        let half_off = [
            PathCmd::Move(-0.5, 0.25),
            PathCmd::Line(0.5, 0.25),
            PathCmd::Line(0.5, 0.75),
            PathCmd::Line(-0.5, 0.75),
            PathCmd::Close,
        ];
        let mut t2 = Tile::new(32, 32);
        fill_path(
            &mut t2,
            &[&half_off],
            (0.0, 1.0, 0.0),
            1.0,
            PathTransform::fit(32, 32),
        );
        assert!(alpha_at(&t2, 0, 16) > 200, "clipped fill reaches the edge");
        assert_eq!(alpha_at(&t2, 31, 16), 0, "right half stays empty");
    }

    #[test]
    fn parse_path_roundtrips_the_asset_grammar() {
        let render = |cmds: &[PathCmd]| -> String {
            cmds.iter()
                .map(|cmd| match *cmd {
                    PathCmd::Move(x, y) => format!("M {x} {y}"),
                    PathCmd::Line(x, y) => format!("L {x} {y}"),
                    PathCmd::Cubic(x1, y1, x2, y2, x, y) => {
                        format!("C {x1} {y1} {x2} {y2} {x} {y}")
                    }
                    PathCmd::Close => "Z".to_string(),
                })
                .collect::<Vec<_>>()
                .join(" ")
        };
        let cmds = parse_path("M 0.1 0.2 C 0.3 0.4 0.5 0.6 0.7 0.8 L 0.9 1.0 Z").unwrap();
        assert_eq!(
            cmds,
            vec![
                PathCmd::Move(0.1, 0.2),
                PathCmd::Cubic(0.3, 0.4, 0.5, 0.6, 0.7, 0.8),
                PathCmd::Line(0.9, 1.0),
                PathCmd::Close,
            ]
        );
        assert_eq!(parse_path(&render(&cmds)).as_ref(), Some(&cmds));
        let imp = parse_path("M0 0 1 0 C1 0 1 1 0 1 0 1 0 0 0 0Z").unwrap();
        assert_eq!(
            imp,
            vec![
                PathCmd::Move(0.0, 0.0),
                PathCmd::Line(1.0, 0.0),
                PathCmd::Cubic(1.0, 0.0, 1.0, 1.0, 0.0, 1.0),
                PathCmd::Cubic(0.0, 1.0, 0.0, 0.0, 0.0, 0.0),
                PathCmd::Close,
            ]
        );
        assert_eq!(parse_path(&render(&imp)).as_ref(), Some(&imp));
        let signed = vec![PathCmd::Move(-0.0, -12.375), PathCmd::Line(1e-6, 4096.0)];
        assert_eq!(parse_path(&render(&signed)).as_ref(), Some(&signed));
        assert!(parse_path("M 0.1").is_none(), "dangling args");
        assert!(parse_path("Q 1 2 3 4").is_none(), "unsupported command");
        assert!(parse_path("M x y").is_none(), "bare command before letter");
        assert!(parse_path("M 1.2.3 4").is_none(), "malformed decimal");
        assert!(parse_path("M 1e 2").is_none(), "malformed exponent");
        assert!(parse_path("Z 1 2").is_none(), "numbers after close");
        assert!(parse_path("1 2 3").is_none(), "numbers before a command");
        assert_eq!(parse_path("").unwrap(), vec![]);
    }
}
