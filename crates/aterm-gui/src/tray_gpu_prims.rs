// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The tray raster's prims as GPU tray prims (`aterm_gpu::tray_prims`,
//! `docs/DESIGN-stats-deck-gpu-motion-2026-10-26.md` §3.2): [`gpu_prims`] walks a prim list as
//! [`super::replay_tray`] does (clips, the sub-pixel stack, the device grid) and hands each vector
//! prim to the GPU with the geometry and pixel bounds the CPU raster gives it, and
//! [`tray_prim_law`] is the CPU's own composite tables. A list holding a prim the GPU does not
//! draw (text, a terminal specimen, an image, a band icon, the HSV disk, a fade) has no GPU form.
//! Nothing outside the tests calls it until the deck's live parts draw through it (§4, S5).
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "the deck's live parts draw through it from S5 on")
)]

use aterm_gpu::tray_prims::{TrayPrim, TrayPrimLaw, kind};

use super::{
    Dash, DeviceRect, dash_len, dash_px, device_clip_rect, panel_in_device_px, rim_on_grid,
    rounded_stroke, srgb_quantizer, srgb_to_linear_lut,
};
use crate::widget::DrawPrim;

/// The CPU raster's composite law as the GPU reads it: its decode table, `byte / 255`, and its
/// quantizer's thresholds.
pub(crate) fn tray_prim_law() -> &'static TrayPrimLaw {
    static LAW: std::sync::OnceLock<TrayPrimLaw> = std::sync::OnceLock::new();
    LAW.get_or_init(|| {
        let mut thresholds = [f32::INFINITY; 256];
        thresholds[..255].copy_from_slice(&srgb_quantizer().thresholds);
        TrayPrimLaw {
            to_linear: *srgb_to_linear_lut(),
            unit: std::array::from_fn(|b| b as f32 / 255.0),
            thresholds,
        }
    })
}

/// `prims` at `scale` as GPU tray prims for a canvas over the device pixels `bounds` (`x0, y0, x1,
/// y1`, the extent [`super::rasterize_tray_region`] or [`super::rasterize_tray_pixels`] would
/// raster): drawn in order over the same ground, they land the CPU raster's bytes within the parity
/// the tests declare. `None` when the list holds a prim the GPU does not draw.
pub(crate) fn gpu_prims(
    prims: &[DrawPrim],
    scale: f32,
    bounds: DeviceRect,
) -> Option<Vec<TrayPrim>> {
    let mut m = Mapper {
        bounds,
        clip: Vec::new(),
        subpixel: 0,
        out: Vec::with_capacity(prims.len()),
    };
    let s = scale;
    let mut skipped_clip_depth = 0u32;
    for p in prims {
        match p {
            DrawPrim::SubpixelPush => {
                m.subpixel = m.subpixel.saturating_add(1);
                continue;
            }
            DrawPrim::SubpixelPop => {
                m.subpixel = m.subpixel.saturating_sub(1);
                continue;
            }
            DrawPrim::ClipPush { .. } if skipped_clip_depth > 0 => {
                skipped_clip_depth = skipped_clip_depth.saturating_add(1);
                continue;
            }
            DrawPrim::ClipPush { x, y, w, h } => {
                m.push_clip(x * s, y * s, w * s, h * s);
                if !m.active_clip_intersects_bounds() {
                    skipped_clip_depth = 1;
                }
                continue;
            }
            DrawPrim::ClipPop if skipped_clip_depth > 1 => {
                skipped_clip_depth -= 1;
                continue;
            }
            DrawPrim::ClipPop => {
                skipped_clip_depth = 0;
                m.clip.pop();
                continue;
            }
            _ if skipped_clip_depth > 0 => continue,
            _ => {}
        }
        match p {
            DrawPrim::Panel {
                x,
                y,
                w,
                h,
                radius,
                fill,
            } => {
                let (x, y, w, h) = panel_in_device_px((*x, *y, *w, *h), s, m.subpixel == 0);
                m.round_rect(x, y, w, h, radius * s, *fill);
            }
            DrawPrim::Ring {
                cx,
                cy,
                r_outer,
                thickness,
                track,
                start,
                frac,
                fill,
            } => {
                let r_mid = (r_outer - thickness / 2.0) * s;
                let th = thickness * s;
                m.arc(cx * s, cy * s, r_mid, th, 0.0, 1.0, *track);
                m.arc(cx * s, cy * s, r_mid, th, *start, *frac, *fill);
            }
            DrawPrim::Capsule {
                x,
                y,
                w,
                h,
                frac,
                fill,
                track,
            } => {
                let r = h * s / 2.0;
                m.round_rect(x * s, y * s, w * s, h * s, r, *track);
                let fw = (w * frac).max(if *frac > 0.0 { *h } else { 0.0 }) * s;
                if fw > 0.0 {
                    m.round_rect(x * s, y * s, fw, h * s, r, *fill);
                }
            }
            DrawPrim::Dot { cx, cy, r, color } => m.disc(cx * s, cy * s, r * s, *color),
            DrawPrim::Stroke {
                x,
                y,
                w,
                h,
                radius,
                width,
                color,
            } => {
                let (r, rect) = (radius * s, (x * s, y * s, w * s, h * s));
                let (x, y, w, h, width) = if m.subpixel == 0 {
                    rim_on_grid(rect, r, width * s)
                } else {
                    (rect.0, rect.1, rect.2, rect.3, width * s)
                };
                m.stroke_rect(x, y, w, h, r, width, *color);
            }
            DrawPrim::Line {
                x1,
                y1,
                x2,
                y2,
                width,
                color,
            } => m.line_segment((x1 * s, y1 * s), (x2 * s, y2 * s), width * s, *color),
            DrawPrim::Arc {
                cx,
                cy,
                r,
                width,
                from,
                to,
                color,
            } => m.arc_band((cx * s, cy * s), (r * s, width * s), (*from, *to), *color),
            DrawPrim::Dashed {
                x,
                y,
                w,
                h,
                width,
                on,
                off,
                color,
            } => m.dashed(
                (x * s, y * s, w * s, h * s),
                width * s,
                (on * s, off * s),
                *color,
            ),
            DrawPrim::Text { .. }
            | DrawPrim::TerminalSpecimen { .. }
            | DrawPrim::HsvDisk { .. }
            | DrawPrim::BandIcon { .. }
            | DrawPrim::Image { .. }
            | DrawPrim::FadePush { .. }
            | DrawPrim::FadePop => return None,
            DrawPrim::ClipPush { .. }
            | DrawPrim::ClipPop
            | DrawPrim::SubpixelPush
            | DrawPrim::SubpixelPop => unreachable!("handled above"),
        }
    }
    Some(m.out)
}

/// [`gpu_prims`]' walk: the canvas's device bounds, the clip stack and the sub-pixel depth, as
/// [`super::Canvas`] keeps them, and the prims so far.
struct Mapper {
    bounds: DeviceRect,
    clip: Vec<DeviceRect>,
    subpixel: u32,
    out: Vec<TrayPrim>,
}

impl Mapper {
    /// [`super::Canvas::push_clip`].
    fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32) {
        let mut r = device_clip_rect(x, y, w, h);
        if let Some(&(cx0, cy0, cx1, cy1)) = self.clip.last() {
            r.0 = r.0.max(cx0);
            r.1 = r.1.max(cy0);
            r.2 = r.2.min(cx1);
            r.3 = r.3.min(cy1);
        }
        self.clip.push(r);
    }

    /// [`super::Canvas::active_clip_intersects_tile`].
    fn active_clip_intersects_bounds(&self) -> bool {
        let Some(&(clip_x0, clip_y0, clip_x1, clip_y1)) = self.clip.last() else {
            return true;
        };
        let (tile_x0, tile_y0, tile_x1, tile_y1) = self.bounds;
        clip_x1 > tile_x0 && clip_y1 > tile_y0 && clip_x0 < tile_x1 && clip_y0 < tile_y1
    }

    /// [`super::Canvas::clipped_bounds`].
    fn clipped(&self, mut x0: i32, mut y0: i32, mut x1: i32, mut y1: i32) -> DeviceRect {
        let (canvas_x0, canvas_y0, canvas_x1, canvas_y1) = self.bounds;
        x0 = x0.max(canvas_x0);
        y0 = y0.max(canvas_y0);
        x1 = x1.min(canvas_x1);
        y1 = y1.min(canvas_y1);
        if let Some(&(clip_x0, clip_y0, clip_x1, clip_y1)) = self.clip.last() {
            x0 = x0.max(clip_x0);
            y0 = y0.max(clip_y0);
            x1 = x1.min(clip_x1);
            y1 = y1.min(clip_y1);
        }
        (x0, y0, x1.max(x0), y1.max(y0))
    }

    /// One prim inking `rect`, unless it inks nothing (a clear colour composites to what is there).
    fn push(&mut self, kind: u32, rect: DeviceRect, geom: &[f32], c: [u8; 4]) {
        let prim = TrayPrim::new(kind, [rect.0, rect.1, rect.2, rect.3], geom, c);
        if c[3] != 0 && !prim.is_empty() {
            self.out.push(prim);
        }
    }

    /// [`super::Canvas::round_rect_under`] (its hole is a saving, not a change of bytes).
    fn round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, c: [u8; 4]) {
        let rect = self.clipped(
            (x - 1.0).floor() as i32,
            (y - 1.0).floor() as i32,
            (x + w + 1.0).ceil() as i32,
            (y + h + 1.0).ceil() as i32,
        );
        self.push(kind::ROUND_RECT, rect, &[x, y, w, h, r], c);
    }

    /// [`super::Canvas::disc`].
    fn disc(&mut self, cx: f32, cy: f32, r: f32, c: [u8; 4]) {
        let rect = self.clipped(
            (cx - r - 1.0) as i32,
            (cy - r - 1.0) as i32,
            (cx + r + 1.0) as i32,
            (cy + r + 1.0) as i32,
        );
        self.push(kind::DISC, rect, &[cx, cy, r], c);
    }

    /// [`super::Canvas::line_segment`].
    fn line_segment(&mut self, start: (f32, f32), end: (f32, f32), width: f32, c: [u8; 4]) {
        let radius = width.max(0.0) / 2.0;
        if radius <= 0.0 {
            return;
        }
        let (x1, y1) = start;
        let (x2, y2) = end;
        let vx = x2 - x1;
        let vy = y2 - y1;
        let length_squared = vx.mul_add(vx, vy * vy);
        if length_squared <= f32::EPSILON {
            self.disc(x1, y1, radius, c);
            return;
        }
        let rect = self.clipped(
            (x1.min(x2) - radius - 1.0).floor() as i32,
            (y1.min(y2) - radius - 1.0).floor() as i32,
            (x1.max(x2) + radius + 1.0).ceil() as i32,
            (y1.max(y2) + radius + 1.0).ceil() as i32,
        );
        self.push(
            kind::SEGMENT,
            rect,
            &[x1, y1, vx, vy, length_squared, radius],
            c,
        );
    }

    /// [`super::Canvas::arc_band`]: its ends are placed here, by the CPU's `sin` and `cos`.
    fn arc_band(
        &mut self,
        (cx, cy): (f32, f32),
        (r, width): (f32, f32),
        (from, to): (f32, f32),
        c: [u8; 4],
    ) {
        use std::f32::consts::TAU;
        let half = width.max(0.0) / 2.0;
        let span = (to - from).clamp(0.0, 1.0);
        if half <= 0.0 || r <= 0.0 {
            return;
        }
        let reach = r + half + 1.0;
        let rect = self.clipped(
            (cx - reach).floor() as i32,
            (cy - reach).floor() as i32,
            (cx + reach).ceil() as i32,
            (cy + reach).ceil() as i32,
        );
        let end = |turn: f32| {
            let t = turn * TAU;
            (cx + t.sin() * r, cy - t.cos() * r)
        };
        let (e0, e1) = (end(from), end(from + span));
        self.push(
            kind::ARC_BAND,
            rect,
            &[cx, cy, r, half, from, span, e0.0, e0.1, e1.0, e1.1],
            c,
        );
    }

    /// [`super::Canvas::arc`].
    #[expect(
        clippy::too_many_arguments,
        reason = "the CPU raster's `Canvas::arc`, argument for argument"
    )]
    fn arc(
        &mut self,
        cx: f32,
        cy: f32,
        r_mid: f32,
        thickness: f32,
        start: f32,
        frac: f32,
        c: [u8; 4],
    ) {
        let frac = frac.clamp(0.0, 1.0);
        if frac <= 0.0 || c[3] == 0 {
            return;
        }
        let r_out = r_mid + thickness / 2.0;
        let rect = self.clipped(
            (cx - r_out - 1.0) as i32,
            (cy - r_out - 1.0) as i32,
            (cx + r_out + 1.0) as i32,
            (cy + r_out + 1.0) as i32,
        );
        self.push(
            kind::RING_ARC,
            rect,
            &[cx, cy, r_mid, thickness, frac, start],
            c,
        );
    }

    /// [`super::Canvas::fill_px_rect`].
    fn fill_px_rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: [u8; 4]) {
        let at = (
            x.round() as i32,
            y.round() as i32,
            (x + w).round() as i32,
            (y + h).round() as i32,
        );
        let rect = self.clipped(at.0, at.1, at.2, at.3);
        self.push(
            kind::FILL,
            rect,
            &[at.0 as f32, at.1 as f32, at.2 as f32, at.3 as f32],
            c,
        );
    }

    /// [`super::Canvas::stroke_rect`] (its ink-free middle is a saving, not a change of bytes).
    #[allow(
        clippy::too_many_arguments,
        reason = "Canvas::stroke_rect's own inputs, mapped one for one"
    )]
    fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, c: [u8; 4]) {
        let sw = width.max(1.0);
        if !rounded_stroke(r, w, h, sw) {
            let t = sw.round().max(1.0);
            if w <= t || h <= t {
                self.fill_px_rect(x, y, w.max(t), h.max(t), c);
                return;
            }
            self.fill_px_rect(x, y, w, t, c);
            self.fill_px_rect(x, y + h - t, w, t, c);
            let mid_h = (h - 2.0 * t).max(0.0);
            if mid_h > 0.0 {
                self.fill_px_rect(x, y + t, t, mid_h, c);
                self.fill_px_rect(x + w - t, y + t, t, mid_h, c);
            }
            return;
        }
        let (cx, cy, hx, hy) = (x + w / 2.0, y + h / 2.0, w / 2.0, h / 2.0);
        let r = r.min(hx).min(hy);
        let hw = sw / 2.0;
        let rect = self.clipped(
            (x - sw - 1.0).floor() as i32,
            (y - sw - 1.0).floor() as i32,
            (x + w + sw + 1.0).ceil() as i32,
            (y + h + sw + 1.0).ceil() as i32,
        );
        self.push(kind::STROKE, rect, &[cx, cy, hx, hy, r, hw], c);
    }

    /// [`super::Canvas::dashed`].
    fn dashed(
        &mut self,
        (x, y, w, h): (f32, f32, f32, f32),
        width: f32,
        (on, off): (f32, f32),
        c: [u8; 4],
    ) {
        if on.is_nan() || on <= 0.0 {
            return;
        }
        let dash = Dash {
            on: dash_len(on),
            off: if off > 0.0 { dash_len(off) } else { 0 },
        };
        let t = dash_len(width);
        let (x0, x1) = (dash_px(x.min(x + w)), dash_px(x.max(x + w)));
        let (y0, y1) = (dash_px(y.min(y + h)), dash_px(y.max(y + h)));
        if x1 - x0 > 2 * t && y1 - y0 > 2 * t {
            for (top, bottom) in [(y0, y0 + t), (y1 - t, y1)] {
                self.fill_device((x0, top, x0 + t, bottom), c);
                self.dash_band(dash, x0, (x0 + t, x1 - t), (top, bottom), true, c);
                self.fill_device((x1 - t, top, x1, bottom), c);
            }
            for (left, right) in [(x0, x0 + t), (x1 - t, x1)] {
                self.dash_band(dash, y0, (y0 + t, y1 - t), (left, right), false, c);
            }
        } else if x1 - x0 >= y1 - y0 {
            let band = (y0, y0 + (y1 - y0).max(t));
            self.dash_band(dash, x0, (x0, x1), band, true, c);
        } else {
            let band = (x0, x0 + (x1 - x0).max(t));
            self.dash_band(dash, y0, (y0, y1), band, false, c);
        }
    }

    /// [`super::Canvas::dash_band`].
    fn dash_band(
        &mut self,
        dash: Dash,
        start: i64,
        run: (i64, i64),
        band: (i64, i64),
        across: bool,
        c: [u8; 4],
    ) {
        let (vx0, vy0, vx1, vy1) = self.clipped(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
        let (lo, hi) = if across {
            (run.0.max(vx0.into()), run.1.min(vx1.into()))
        } else {
            (run.0.max(vy0.into()), run.1.min(vy1.into()))
        };
        for (a, b) in dash.within(start, lo, hi) {
            let rect = if across {
                (a, band.0, b, band.1)
            } else {
                (band.0, a, band.1, b)
            };
            self.fill_device(rect, c);
        }
    }

    /// [`super::Canvas::fill_device`]: the rect's pixels within the canvas and the clip, whole.
    fn fill_device(&mut self, (x0, y0, x1, y1): (i64, i64, i64, i64), c: [u8; 4]) {
        let (vx0, vy0, vx1, vy1) = self.clipped(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
        let fit = |v: i64, lo: i32, hi: i32| v.clamp(lo.into(), hi.into()) as i32;
        let rect = (
            fit(x0, vx0, vx1),
            fit(y0, vy0, vy1),
            fit(x1, vx0, vx1),
            fit(y1, vy0, vy1),
        );
        if rect.0 >= rect.2 || rect.1 >= rect.3 {
            return;
        }
        // Its geometry is the pixels it fills (within the canvas, so exact in `f32`).
        self.push(
            kind::FILL,
            rect,
            &[rect.0 as f32, rect.1 as f32, rect.2 as f32, rect.3 as f32],
            c,
        );
    }
}

#[cfg(all(test, target_os = "macos"))]
#[path = "tray_gpu_prims_parity_tests.rs"]
mod parity_tests;

#[cfg(test)]
#[path = "tray_gpu_prims_tests.rs"]
mod tests;
