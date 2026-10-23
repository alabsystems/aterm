// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Hinting half of the font oracle over [`Hinter`], [`super::raster`], and [`super::Fnv`].

use super::{Fnv, raster};

/// The recorder's pixels-per-em cases, shared with the replay's coverage check.
pub(crate) const SIZES: [f32; 9] = [10.0, 12.0, 13.0, 13.5, 14.0, 16.0, 17.0, 19.0, 24.0];

/// The recorder's raster modes; LCD modes are recorded only at the default instance.
pub(crate) const MODES: [&str; 4] = ["full", "light", "lcd", "lcd-unhinted"];

/// The grid fits aterm-render asks for: `full` and `light` (the grayscale
/// raster, `font_hinting`) and `lcd` (the subpixel raster).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Full,
    Light,
    Lcd,
}

/// A hinter and the reader around it.
pub(crate) trait Hinter {
    type Instance;

    fn instance(
        &self,
        bytes: &[u8],
        index: u32,
        px: f32,
        mode: Mode,
        coords: &[(u32, f32)],
    ) -> Option<Self::Instance>;

    fn draw(
        &self,
        hint: &Self::Instance,
        bytes: &[u8],
        index: u32,
        gid: u16,
        pen: &mut PathPen,
    ) -> Option<()>;

    fn unhinted(
        &self,
        bytes: &[u8],
        index: u32,
        gid: u16,
        px: f32,
        pen: &mut PathPen,
    ) -> Option<()>;

    fn linear_advance(
        &self,
        bytes: &[u8],
        index: u32,
        gid: u16,
        px: f32,
        coords: &[(u32, f32)],
    ) -> Option<f32>;
}

/// The answer of one corpus line: face `index` of `bytes`, its `gids`, at
/// `coords` and `px` in `mode_name` (`full`, `light`, `lcd`,
/// `lcd-unhinted`).
pub(crate) fn answer<H: Hinter>(
    hinter: &H,
    bytes: &[u8],
    index: u32,
    gids: &[u16],
    coords: &[(u32, f32)],
    mode_name: &str,
    px: f32,
) -> Option<String> {
    let mode = match mode_name {
        "full" => Some(Mode::Full),
        "light" => Some(Mode::Light),
        "lcd" => Some(Mode::Lcd),
        "lcd-unhinted" => None,
        _ => return None,
    };
    let inst = mode.map(|m| {
        hinter.instance(
            bytes,
            index,
            px,
            m,
            if mode_name == "lcd" { &[] } else { coords },
        )
    });
    if let Some(None) = inst {
        return Some("no hinting instance".to_string());
    }
    let (mut raster, mut stream) = (Fnv::new(), Fnv::new());
    let mut none = 0u32;
    for &g in gids {
        raster.u16(g);
        let r = match (mode_name, &inst) {
            ("lcd", Some(Some(i))) => {
                subpixel_glyph_raster(px, &mut stream, |pen| hinter.draw(i, bytes, index, g, pen))
                    .map(|(w, h, x, y, cov)| (w, h, x, y, f32::NAN, cov))
            }
            ("lcd-unhinted", _) => subpixel_glyph_raster(px, &mut stream, |pen| {
                hinter.unhinted(bytes, index, g, px, pen)
            })
            .map(|(w, h, x, y, cov)| (w, h, x, y, f32::NAN, cov)),
            (_, Some(Some(i))) => {
                let advance = hinter.linear_advance(bytes, index, g, px, coords);
                advance.and_then(|advance| {
                    hinted_glyph_raster(advance, &mut stream, |pen| {
                        hinter.draw(i, bytes, index, g, pen)
                    })
                })
            }
            _ => return None,
        };
        match r {
            Some((w, h, x, y, adv, cov)) => {
                raster.u32(w as u32);
                raster.u32(h as u32);
                raster.i32(x);
                raster.i32(y);
                raster.f32(adv);
                raster.bytes(&cov);
            }
            None => {
                none += 1;
                raster.bytes(b"-");
            }
        }
    }
    Some(format!(
        "glyphs={} none={none} fnv={} outline={}",
        gids.len(),
        raster.hex(),
        stream.hex()
    ))
}

pub(crate) const RASTER_PAD: usize = 1;

fn crop_padded_coverage(ras: &raster::Rasterizer, w: usize, h: usize) -> Vec<u8> {
    let pad = RASTER_PAD;
    let gw = w + 2 * pad;
    let mut cov = vec![0u8; w * h];
    ras.for_each_pixel(|i, a| {
        let (gx, gy) = (i % gw, i / gw);
        let (Some(x), Some(y)) = (gx.checked_sub(pad), gy.checked_sub(pad)) else {
            return;
        };
        if x < w && y < h {
            cov[y * w + x] = (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
    });
    cov
}

fn hinted_glyph_raster(
    advance: f32,
    stream: &mut Fnv,
    draw: impl FnOnce(&mut PathPen) -> Option<()>,
) -> Option<(usize, usize, i32, i32, f32, Vec<u8>)> {
    let mut pen = PathPen::default();
    draw(&mut pen)?;
    pen.digest(stream);
    if pen.is_blank() {
        return Some((0, 0, 0, 0, advance, Vec::new()));
    }
    let x_min = pen.min_x.floor();
    let x_max = pen.max_x.ceil();
    let y_min = pen.min_y.floor();
    let y_max = pen.max_y.ceil();
    let (w, h) = ((x_max - x_min) as i32, (y_max - y_min) as i32);
    if w <= 0 || h <= 0 || w > 4096 || h > 4096 {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    let pad = RASTER_PAD as f32;
    let mut ras = raster::Rasterizer::new(w + 2 * RASTER_PAD, h + 2 * RASTER_PAD);
    pen.fill(&mut ras, x_min - pad, y_max + pad, 1.0);
    let cov = crop_padded_coverage(&ras, w, h);
    Some((w, h, x_min as i32, y_min as i32, advance, cov))
}

const FIR5: [u32; 5] = [8, 77, 86, 77, 8];

fn subpixel_glyph_raster(
    px: f32,
    stream: &mut Fnv,
    draw: impl FnOnce(&mut PathPen) -> Option<()>,
) -> Option<(usize, usize, i32, i32, Vec<u8>)> {
    if !px.is_finite() || px <= 0.0 {
        return None;
    }
    let mut pen = PathPen::default();
    draw(&mut pen)?;
    pen.digest(stream);
    if pen.is_blank() {
        return None;
    }
    let x_min = pen.min_x.floor() - 1.0;
    let x_max = pen.max_x.ceil() + 1.0;
    let y_min = pen.min_y.floor();
    let y_max = pen.max_y.ceil();
    let (w, h) = ((x_max - x_min) as i32, (y_max - y_min) as i32);
    if w <= 2 || h <= 0 || w > 4098 || h > 4096 {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    let w_sub = w * 3;
    let mut ras = raster::Rasterizer::new(w_sub, h);
    pen.fill(&mut ras, x_min, y_max, 3.0);
    let mut cov = vec![0u8; w_sub * h];
    ras.for_each_pixel(|i, a| {
        if let Some(slot) = cov.get_mut(i) {
            *slot = (a * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
        }
    });
    let mut out = vec![0u8; w * h * 3];
    let mut filtered = vec![0u8; w_sub];
    for j in 0..h {
        let row = &cov[j * w_sub..(j + 1) * w_sub];
        for (i, slot) in filtered.iter_mut().enumerate() {
            let mut acc = 0u32;
            for (k, &f) in FIR5.iter().enumerate() {
                let s = i as isize + k as isize - 2;
                if s >= 0 && (s as usize) < w_sub {
                    acc += f * u32::from(row[s as usize]);
                }
            }
            *slot = ((acc + 128) >> 8).min(255) as u8;
        }
        let orow = &mut out[j * w * 3..(j + 1) * w * 3];
        for i in 0..w {
            let (l, m, r) = (filtered[3 * i], filtered[3 * i + 1], filtered[3 * i + 2]);
            orow[3 * i] = l;
            orow[3 * i + 1] = m;
            orow[3 * i + 2] = r;
        }
    }
    Some((w, h, x_min as i32, y_min as i32, out))
}

enum Cmd {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Curve(f32, f32, f32, f32, f32, f32),
    Close,
}

/// src/hinted.rs `PathPen`: a recorded outline (pixels, y up) and its ink
/// bounds, replayed into a raster grid (y down).
#[derive(Default)]
pub(crate) struct PathPen {
    cmds: Vec<Cmd>,
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

impl PathPen {
    pub(crate) fn is_blank(&self) -> bool {
        self.cmds.is_empty() || !self.min_x.is_finite()
    }

    fn see(&mut self, x: f32, y: f32) {
        if self.cmds.is_empty() {
            (self.min_x, self.max_x) = (x, x);
            (self.min_y, self.max_y) = (y, y);
        } else {
            self.min_x = self.min_x.min(x);
            self.min_y = self.min_y.min(y);
            self.max_x = self.max_x.max(x);
            self.max_y = self.max_y.max(y);
        }
    }

    /// The pen's command stream, every coordinate by its bits.
    pub(crate) fn digest(&self, h: &mut Fnv) {
        for c in &self.cmds {
            match *c {
                Cmd::Move(x, y) => {
                    h.bytes(b"M");
                    h.f32(x);
                    h.f32(y);
                }
                Cmd::Line(x, y) => {
                    h.bytes(b"L");
                    h.f32(x);
                    h.f32(y);
                }
                Cmd::Quad(a, b, x, y) => {
                    h.bytes(b"Q");
                    for v in [a, b, x, y] {
                        h.f32(v);
                    }
                }
                Cmd::Curve(a, b, c, d, x, y) => {
                    h.bytes(b"C");
                    for v in [a, b, c, d, x, y] {
                        h.f32(v);
                    }
                }
                Cmd::Close => h.bytes(b"Z"),
            }
        }
        h.bytes(b"|");
    }

    fn fill(&self, ras: &mut raster::Rasterizer, x_min: f32, y_max: f32, xs: f32) {
        let map = |x: f32, y: f32| raster::point((x - x_min) * xs, y_max - y);
        let mut last = raster::point(0.0, 0.0);
        let mut start = last;
        let close =
            |ras: &mut raster::Rasterizer, last: &mut raster::Point, start: raster::Point| {
                if *last != start {
                    ras.draw_line(*last, start);
                    *last = start;
                }
            };
        for c in &self.cmds {
            match *c {
                Cmd::Move(x, y) => {
                    close(ras, &mut last, start);
                    last = map(x, y);
                    start = last;
                }
                Cmd::Line(x, y) => {
                    let p = map(x, y);
                    ras.draw_line(last, p);
                    last = p;
                }
                Cmd::Quad(cx, cy, x, y) => {
                    let c1 = map(cx, cy);
                    let p = map(x, y);
                    ras.draw_quad(last, c1, p);
                    last = p;
                }
                Cmd::Curve(c0x, c0y, c1x, c1y, x, y) => {
                    let a = map(c0x, c0y);
                    let b = map(c1x, c1y);
                    let p = map(x, y);
                    ras.draw_cubic(last, a, b, p);
                    last = p;
                }
                Cmd::Close => close(ras, &mut last, start),
            }
        }
        close(ras, &mut last, start);
    }

    pub(crate) fn move_to(&mut self, x: f32, y: f32) {
        self.see(x, y);
        self.cmds.push(Cmd::Move(x, y));
    }

    pub(crate) fn line_to(&mut self, x: f32, y: f32) {
        self.see(x, y);
        self.cmds.push(Cmd::Line(x, y));
    }

    pub(crate) fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.see(cx, cy);
        self.see(x, y);
        self.cmds.push(Cmd::Quad(cx, cy, x, y));
    }

    pub(crate) fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        self.see(c0x, c0y);
        self.see(c1x, c1y);
        self.see(x, y);
        self.cmds.push(Cmd::Curve(c0x, c0y, c1x, c1y, x, y));
    }

    pub(crate) fn close(&mut self) {
        self.cmds.push(Cmd::Close);
    }
}
