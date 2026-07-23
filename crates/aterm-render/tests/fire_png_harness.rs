// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// EMBERFORGE FirePatch VISUAL SELF-REVIEW harness (dev-only, env-gated): the
// mandated eyes-on-the-art loop. Renders the shared pure-integer fire field
// (`aterm_render::fire_field`) exactly as the CPU rasterizer composites it —
// `add_sat(bg, fire_field_add(..))` over near-black for `FireMode::Add`,
// `over_rgb(white, ..)` for `FireMode::Over` — at temps {0.2, 0.5, 0.9}·255,
// eight phase steps across 2 s, single patches AND a row of adjacent patches
// with head→tail strength falloff, saved as 3×-zoom PNGs.
//
//   FIRE_PNG_DIR=/tmp/fire cargo test -p aterm-render --release \
//       --test fire_png_harness -- --ignored --nocapture
//
// Not a correctness gate: `#[ignore]`d and a no-op without FIRE_PNG_DIR.

use aterm_render::fire_field::{FireFieldParams, fire_field_add, fire_field_over};
use aterm_render::{Frame, add_sat, over_rgb};

const CH: i32 = 40; // cell height px (a ~18pt font at 2x)
const PANEL_W: usize = 320;
const PANEL_H: usize = 170; // one burn panel
const COLS: usize = 4; // phase frames per row
const ROWS: usize = 2; // 8 phase steps total
const ZOOM: usize = 3;

fn params(temp: i32, strength: i32, phase: u32, lean: i32, base_y: i32) -> FireFieldParams {
    FireFieldParams {
        base_y,
        peak_h: 3 * CH,
        phase,
        temp,
        strength,
        lean,
        cov_cap: 200,
        cell_h: CH,
        // Grid-top reference for the v0.32 top-edge fade; `0` (the top of the tile)
        // matches every other fire test (fire_bench + the fire_field unit tests).
        top_fade_y: 0,
    }
}

/// Paint one burn into `buf` (a PANEL_W×PANEL_H tile at `(ox, oy)` in the
/// canvas): `over == false` composites Add over near-black, `true` composites
/// Over over white. `strengths` gives one value per CW-wide column patch —
/// `[k]` alone means one uniform wide patch.
#[allow(clippy::too_many_arguments)]
fn paint_burn(
    buf: &mut [u32],
    canvas_w: usize,
    ox: usize,
    oy: usize,
    temp: i32,
    phase: u32,
    over: bool,
    strengths: &[i32],
) {
    let base_y = (PANEL_H - 18) as i32;
    let x0 = 24i32;
    for (i, &s) in strengths.iter().enumerate() {
        let w = if strengths.len() == 1 {
            PANEL_W as i32 - 48
        } else {
            (PANEL_W as i32 - 48) / strengths.len() as i32
        };
        let px0 = x0 + i as i32 * w;
        let p = params(temp, s, phase, -48, base_y);
        for py in 0..base_y + 1 {
            for px in px0..px0 + w {
                let idx = (oy + py as usize) * canvas_w + ox + px as usize;
                if over {
                    let (rgb, a) = fire_field_over(px, py, &p);
                    if a != 0 {
                        buf[idx] = over_rgb(buf[idx], rgb, a);
                    }
                } else {
                    let pm = fire_field_add(px, py, &p);
                    if pm != 0 {
                        buf[idx] = add_sat(buf[idx], pm);
                    }
                }
            }
        }
    }
}

fn zoom3(buf: &[u32], w: usize, h: usize) -> (Vec<u32>, usize, usize) {
    let (zw, zh) = (w * ZOOM, h * ZOOM);
    let mut out = vec![0u32; zw * zh];
    for y in 0..zh {
        for x in 0..zw {
            out[y * zw + x] = buf[(y / ZOOM) * w + x / ZOOM];
        }
    }
    (out, zw, zh)
}

#[test]
#[ignore = "dev visual harness; set FIRE_PNG_DIR to emit PNGs"]
fn emit_fire_field_pngs() {
    let Ok(dir) = std::env::var("FIRE_PNG_DIR") else {
        return;
    };
    std::fs::create_dir_all(&dir).expect("create FIRE_PNG_DIR");
    // The producer's head->tail falloff, emitted at fine granularity (the
    // patch contract allows arbitrary widths): 68 strips of 4 px with a
    // smoothly interpolated strength ramp 255 -> 12.
    let falloff: Vec<i32> = (0..68).map(|i| 255 - (243 * i) / 67).collect();
    for (mode_name, over) in [("add", false), ("over", true)] {
        for temp in [51i32, 128, 230] {
            let canvas_w = PANEL_W * COLS;
            let canvas_h = PANEL_H * 2 * ROWS; // single + falloff burns per frame
            let bg = if over { 0x00FF_FFFF } else { 0x000A_0A12 };
            let mut buf = vec![bg; canvas_w * canvas_h];
            for f in 0..(COLS * ROWS) {
                // 8 steps across 2 s: 256 ticks (250 ms) apart, quantized like
                // a producer would (whole ticks of 1/1024 s).
                let phase = 90_000 + (f as u32) * 256;
                let ox = (f % COLS) * PANEL_W;
                let oy = (f / COLS) * PANEL_H * 2;
                paint_burn(&mut buf, canvas_w, ox, oy, temp, phase, over, &[235]);
                paint_burn(
                    &mut buf,
                    canvas_w,
                    ox,
                    oy + PANEL_H,
                    temp,
                    phase,
                    over,
                    &falloff,
                );
            }
            let (z, zw, zh) = zoom3(&buf, canvas_w, canvas_h);
            let frame = Frame {
                width: zw,
                height: zh,
                pixels: z,
            };
            let path = format!("{dir}/fire_{mode_name}_t{temp}.png");
            std::fs::write(&path, frame.to_png()).expect("write png");
            println!("wrote {path}");
        }
    }
    // MOTION MICRO-STRIP: eight consecutive 60 fps frames (16 ms ≈ 16 ticks
    // apart) — the per-frame evolution must be smooth (no visible stepping).
    let canvas_w = PANEL_W * COLS;
    let canvas_h = PANEL_H * 2;
    let mut buf = vec![0x000A_0A12u32; canvas_w * canvas_h];
    for f in 0..8 {
        let phase = 90_000 + (f as u32) * 16;
        let ox = (f % COLS) * PANEL_W;
        let oy = (f / COLS) * PANEL_H;
        paint_burn(&mut buf, canvas_w, ox, oy, 200, phase, false, &[235]);
    }
    let (z, zw, zh) = zoom3(&buf, canvas_w, canvas_h);
    let frame = Frame {
        width: zw,
        height: zh,
        pixels: z,
    };
    let path = format!("{dir}/fire_motion_16ms.png");
    std::fs::write(&path, frame.to_png()).expect("write png");
    println!("wrote {path}");
}
