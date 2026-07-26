// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Visual demo of the "LUMEN WAKE" cursor aurora styles. Hand-builds the additive
// light quads (the same premultiplied [`GlowQuad`]s the live animator emits) for
// each style and renders them through the real CPU compositor to a PNG, so the
// translucency (text shows THROUGH the light) and per-style colour are visible.
// COLOURS come from the live animator's own shared ramps
// (`aterm_effects::cursor_glow::style_comet_color` / `style_particle_color` —
// a dev-dependency, like the sparkle demo), so the previewed palette can never
// drift from the shipped art the way the old hand-copied ramp table did (its
// laser previewed blue-to-white while the live laser is ELECTRIC YELLOW).
//
//   cargo run -p aterm-render --example cursor_glow_demo -- <out_dir>

use aterm_core::terminal::Terminal;
use aterm_effects::cursor_glow::{
    GlowStyle, LASER_DEFAULT_COLOR, style_comet_color, style_particle_color,
};
use aterm_render::{
    BeamClip, CometSample, GlowQuad, Renderer, Theme, comet_glow_quads, premul_rgb,
};

/// Brighten a packed `0x00RRGGBB` by 1.5× — the accent derivation the live
/// `glow_config` applies when no explicit `cursor_trail_accent` is set.
fn brighten(c: u32) -> u32 {
    let m = |sh: u32| ((((c >> sh) & 0xff) as f32) * 1.5).min(255.0) as u32;
    (m(16) << 16) | (m(8) << 8) | m(0)
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let theme = Theme::default();
    let px = 28.0;
    let (rows, cols) = (5usize, 52usize);
    let mut r = Renderer::from_system(px, theme).expect("no system monospace font");
    let (cw, ch) = r.cell_size();

    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"$ echo 'aterm \xe2\x80\x94 fast, hardened, formally verified'\r\n");
    term.process(b"$ git commit -m 'LUMEN WAKE: the paragon cursor'");

    let head = 41usize; // cursor cell on row 1
    let trow = 1usize;
    let comet_len = 22usize;
    let cursor = theme.cursor & 0x00FF_FFFF;

    // (label, style, intensity, has_particles) — colours resolve per position
    // through the SHARED `style_comet_color` ramp below, with the same base /
    // accent derivation the live `glow_config` uses (laser defaults to its
    // ELECTRIC YELLOW, the rest to the theme cursor; accent = base × 1.5).
    let styles: [(&str, GlowStyle, f32, bool); 5] = [
        ("lumen", GlowStyle::Lumen, 0.85, false),
        ("phaser", GlowStyle::Phaser, 0.85, false),
        ("sparkle", GlowStyle::Sparkle, 0.9, true),
        ("fire", GlowStyle::Fire, 0.95, true),
        ("laser", GlowStyle::Laser, 1.0, false),
    ];

    for (name, style, intensity, particles) in styles {
        let base = match style {
            GlowStyle::Laser => LASER_DEFAULT_COLOR,
            _ => cursor,
        };
        let accent = brighten(base);
        // The live comet ramp at path position `pos` (0 tail .. 1 head); the
        // static demo frame renders hue phase 0.
        let color_at = |pos: f32| style_comet_color(style, base, accent, 0.0, pos);
        let mut input = term.cell_frame(rows, cols);
        let mut q = Vec::<GlowQuad>::new();
        let push = |q: &mut Vec<GlowQuad>, x: i32, y: i32, w: i32, h: i32, color: u32, a: u8| {
            let gw = (cols * cw) as i32;
            let gh = (rows * ch) as i32;
            let x0 = x.max(0);
            let x1 = (x + w).min(gw);
            let y0 = y.max(0);
            let y1 = (y + h).min(gh);
            if x1 <= x0 || y1 <= y0 {
                return;
            }
            let cov = ((a as f32) * intensity) as u8;
            let premul = premul_rgb(color, cov);
            let chh = ch as i32;
            let mut yy = y0;
            while yy < y1 {
                let row = yy / chh;
                let band_end = ((row + 1) * chh).min(y1);
                q.push(GlowQuad {
                    row: row as u16,
                    x: x0 as u16,
                    y: yy as u16,
                    w: (x1 - x0) as u16,
                    h: (band_end - yy) as u16,
                    color: premul,
                });
                yy = band_end;
            }
        };

        // Comet body across row 1 (additive light over the typed text).
        for i in 0..comet_len {
            let Some(col) = head.checked_sub(comet_len - i) else {
                continue;
            };
            let pos = (i as f32 + 1.0) / comet_len as f32; // tail .. head
            let cov = (40.0 + 175.0 * pos) as u8;
            push(
                &mut q,
                (col * cw) as i32,
                (trow * ch) as i32,
                cw as i32,
                ch as i32,
                color_at(pos),
                cov,
            );
        }
        // Bloom crown: 3 concentric additive boxes around the head, in the
        // ramp's own head colour — the laser included (its live look is a tight
        // SAME-HUE bloom with no white flash, per the style's contract).
        let cx = (head * cw) as i32;
        let cy = (trow * ch) as i32;
        let bloom = color_at(1.0);
        for layer in 0..3i32 {
            let grow = (ch as i32 * 6 / 10) * (3 - layer) / 3;
            push(
                &mut q,
                cx - grow,
                cy - grow,
                cw as i32 + 2 * grow,
                ch as i32 + 2 * grow,
                bloom,
                (50 * (layer + 1) / 3) as u8,
            );
        }
        // Particles (sparkle / fire): a scatter of small additive dots near the head.
        if particles {
            let sz = (ch as i32 * 18 / 100).max(2);
            for k in 0..14i32 {
                // deterministic pseudo-scatter
                let a = (k as f32 * 2.39996) % std::f32::consts::TAU;
                let life = (k * 37 % 100) as f32 / 100.0; // 0 fresh .. 1 spent
                let rad = life * (ch as f32 * 1.6);
                let (dx, dy) = if name == "fire" {
                    (a.cos() * rad * 0.5, -(rad)) // embers rise
                } else {
                    (a.cos() * rad, a.sin() * rad) // sparkles radiate
                };
                // The live particle ramp: farther-scattered = older = more
                // faded, per-particle hue seed — the same colours the live
                // ember/spark emitters and the settings-card demo resolve.
                let pcol = style_particle_color(style, base, (k as f32 * 0.11).fract(), 1.0 - life);
                let px0 = cx + cw as i32 / 2 + dx as i32 - sz / 2;
                let py0 = cy + ch as i32 / 2 + dy as i32 - sz / 2;
                push(&mut q, px0, py0, sz, sz, pcol, 210);
            }
        }

        input.cursor_glow_add = q;
        let n = input.cursor_glow_add.len();
        let frame = r.render_input(&input);
        let path = format!("{dir}/glow_{name}.png");
        std::fs::write(&path, frame.to_png()).expect("write png");
        println!(
            "wrote {path}  ({}x{}, {n} light quads)",
            frame.width, frame.height
        );
    }

    // ---- DIAGONAL before/after: the staircase fix ----
    // The SAME diagonal cursor sweep, rendered the OLD way (one opaque cell per
    // Bresenham step → a staircase of rectangles) and the NEW way (an anti-aliased
    // glowing beam via `comet_beam` — soft halo + hot core, attached to the cursor).
    let (drows, dcols) = (12usize, 46usize);
    let mut demo_term = Terminal::new(drows as u16, dcols as u16);
    demo_term.process(b"$ aterm \xe2\x80\x94 smooth crisp cursor zoom\r\n");
    let (o_row, o_col) = (10i32, 3i32); // sweep origin (tail)
    let (h_row, h_col) = (1i32, 34i32); // cursor (head)
    let cells = {
        // Bresenham line origin→head, inclusive.
        let (mut r0, mut c0) = (o_row, o_col);
        let (dr, dc) = ((h_row - r0).abs(), (h_col - c0).abs());
        let (sr, sc) = (
            if r0 < h_row { 1 } else { -1 },
            if c0 < h_col { 1 } else { -1 },
        );
        let mut err = dc - dr;
        let mut v = Vec::new();
        loop {
            v.push((r0, c0));
            if r0 == h_row && c0 == h_col {
                break;
            }
            let e2 = 2 * err;
            if e2 > -dr {
                err -= dr;
                c0 += sc;
            }
            if e2 < dc {
                err += dc;
                r0 += sr;
            }
        }
        v
    };
    let ncells = cells.len();
    // The live Lumen ramp (accent → cursor along the tail), through the same
    // shared function the style panel above used.
    let lumen = |pos: f32| style_comet_color(GlowStyle::Lumen, cursor, brighten(cursor), 0.0, pos);

    // BEFORE: full-cell staircase (exclude the head cell — the cursor owns it).
    {
        let mut input = demo_term.cell_frame(drows, dcols);
        let mut q = Vec::<GlowQuad>::new();
        for (i, (rr, cc)) in cells.iter().enumerate() {
            if i + 1 == ncells {
                continue; // head cell = cursor
            }
            let pos = (i as f32 + 1.0) / ncells as f32;
            let cov = ((40.0 + 175.0 * pos) * 0.9) as u8;
            q.push(GlowQuad {
                row: *rr as u16,
                x: (*cc as usize * cw) as u16,
                y: (*rr as usize * ch) as u16,
                w: cw as u16,
                h: ch as u16,
                color: premul_rgb(lumen(pos), cov),
            });
        }
        input.cursor_glow_add = q;
        let nq = input.cursor_glow_add.len();
        let frame = r.render_input(&input);
        std::fs::write(format!("{dir}/glow_diag_before.png"), frame.to_png()).expect("write png");
        println!("wrote {dir}/glow_diag_before.png  (staircase, {nq} quads)");
    }

    // AFTER: anti-aliased glowing beam — layered additive bloom under a hot core,
    // mirroring `cursor_glow::emit_comet`, attached to the cursor.
    {
        let mut input = demo_term.cell_frame(drows, dcols);
        let mut q = Vec::<GlowQuad>::new();
        let (gw, gh) = (dcols * cw, drows * ch);
        let core_thick = (ch as f32 * 0.13).max(2.0);
        let straighten = (cw.max(ch) as f32) * 0.8;
        let run: Vec<CometSample> = cells
            .iter()
            .enumerate()
            .map(|(i, (rr, cc))| {
                let pos = (i as f32 + 1.0) / ncells as f32;
                CometSample {
                    x: (*cc as f32 + 0.5) * cw as f32,
                    y: (*rr as f32 + 0.5) * ch as f32,
                    cov: ((40.0 + 175.0 * pos) * 0.9) as u8,
                    pos,
                }
            })
            .collect();
        // The exact same layered-bloom look the live cursor uses. Identity
        // convention (pad 0, head 0): `BeamClip::grid` — box == the grid extents.
        comet_glow_quads(
            &mut q,
            BeamClip::grid(gw, gh, ch),
            &run,
            core_thick,
            straighten,
            &lumen,
        );
        input.cursor_glow_add = q;
        let nq = input.cursor_glow_add.len();
        let frame = r.render_input(&input);
        std::fs::write(format!("{dir}/glow_diag_after.png"), frame.to_png()).expect("write png");
        println!("wrote {dir}/glow_diag_after.png  (AA bloom beam, {nq} quads)");
    }
}
