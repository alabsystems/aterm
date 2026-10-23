// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The knock-out: letters on a companion's body read against the page. A body drawn under the
// text (`FreeZ::UnderText`: the pet, its departures, the idle cat) is cut out under each glyph
// that lies on it, the glyph's coverage dilated by one pixel and stamped in the cell's page
// colour before the trails draw, so the ink reads against the page with a hairline of page
// around it whatever the coat. Judged per ink pixel against the ground next to it (the
// nearest ring of pixels the glyph does not touch, as `aterm-lab trail-contrast` judges a
// companion), on a dark and a light page, with coats near the ink's own lightness: the tan
// and the cream of the 2026-10-22 burst landings (1.3 to 1.9:1 under the fur). Negative
// control: the knock-out off (`Renderer::set_knock_out(false)`) leaves letters under 4.5:1.
// Beside the body nothing changes: a frame with the body away from the letters is byte-equal
// with and without the knock-out, and the body itself keeps every pixel no glyph touches.

use std::sync::Arc;

use aterm_core::render::{FreeSampler, FreeSprite, FreeZ, RenderInput, SceneAtlas};
use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, WindowCpu};

const ROWS: usize = 4;
const COLS: usize = 24;
const LINE: &str = "the kitty flies";

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature")
}

fn lin(c: u32) -> f64 {
    let s = f64::from(c) / 255.0;
    if s <= 0.040_45 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

fn rel_luma(p: u32) -> f64 {
    0.2126 * lin((p >> 16) & 0xff) + 0.7152 * lin((p >> 8) & 0xff) + 0.0722 * lin(p & 0xff)
}

fn contrast(a: u32, b: u32) -> f64 {
    let (la, lb) = (rel_luma(a), rel_luma(b));
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// An opaque single-colour coat, `w`×`h` texels.
fn coat(rgb: u32, w: u32, h: u32, version: u64) -> SceneAtlas {
    let px = [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255];
    SceneAtlas {
        width: w,
        height: h,
        rgba: (0..w * h).flat_map(|_| px).collect(),
        version,
    }
}

fn body(x: i32, y: i32, w: u16, h: u16) -> FreeSprite {
    FreeSprite {
        x,
        y,
        w,
        h,
        ax: 0,
        ay: 0,
        aw: w,
        ah: h,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z: FreeZ::UnderText,
        sampler: FreeSampler::Nearest,
    }
}

/// A terminal with `LINE` on row 1 (the caret hidden), its page and ink set by OSC 10/11.
fn page(ink: &str, paper: &str) -> Terminal {
    let mut term = Terminal::new(ROWS as u16, COLS as u16);
    term.process(format!("\x1b]10;{ink}\x07\x1b]11;{paper}\x07\x1b[?25l\r\n{LINE}").as_bytes());
    term
}

struct Judged {
    /// Ink pixels inside the body's box.
    ink: usize,
    /// The worst ink pixel's contrast against its nearest untouched ring.
    worst: f64,
    /// How many ink pixels read under 4.5:1.
    under: usize,
}

/// Each ink pixel of `clean` (the frame with no body, 4.5:1 or more against the page) inside
/// `bx`, judged in `frame` against the median of the nearest ring (Chebyshev 1, else 2) of
/// pixels the glyphs leave at the page colour in `clean`.
fn judge(clean: &[u32], frame: &[u32], width: usize, paper: u32, bx: [usize; 4]) -> Judged {
    let height = clean.len() / width;
    let mut out = Judged {
        ink: 0,
        worst: f64::INFINITY,
        under: 0,
    };
    for y in bx[1]..bx[3].min(height) {
        for x in bx[0]..bx[2].min(width) {
            let i = y * width + x;
            if contrast(clean[i], paper) < 4.5 {
                continue;
            }
            let ring = |d: i64| -> Vec<u32> {
                let mut g = Vec::new();
                for dy in -d..=d {
                    for dx in -d..=d {
                        if dx.abs().max(dy.abs()) != d {
                            continue;
                        }
                        let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                        if xx < 0 || yy < 0 || xx >= width as i64 || yy >= height as i64 {
                            continue;
                        }
                        let j = yy as usize * width + xx as usize;
                        if clean[j] == paper {
                            g.push(frame[j]);
                        }
                    }
                }
                g
            };
            let mut ground = ring(1);
            if ground.is_empty() {
                ground = ring(2);
            }
            if ground.is_empty() {
                continue;
            }
            ground.sort_by(|a, b| rel_luma(*a).total_cmp(&rel_luma(*b)));
            let c = contrast(frame[i], ground[ground.len() / 2]);
            out.ink += 1;
            out.worst = out.worst.min(c);
            if c < 4.5 {
                out.under += 1;
            }
        }
    }
    out
}

fn with_body(term: &mut Terminal, fur: u32, sprite: FreeSprite) -> RenderInput {
    let mut input = term.cell_frame(ROWS, COLS);
    input.free_atlas = Some(Arc::new(coat(
        fur,
        u32::from(sprite.w),
        u32::from(sprite.h),
        1,
    )));
    input.free_sprites = vec![sprite];
    input
}

/// Every ink pixel on the body reads at least 4.5:1 against the page ring the knock-out cuts
/// round it, on dark and on paper, for coats near the ink's own lightness; with the knock-out
/// off the same letters read under the bar against the fur.
#[test]
fn letters_on_a_body_drawn_under_the_text_read_against_the_page() {
    // (ink, page, coats): the 2026-10-22 burst landings' tan and cream on dark, and the
    // slate and grey-brown on paper.
    let cases = [
        (
            "#d0d0d0",
            "#111318",
            0x00D0_D0D0u32,
            0x0011_1318u32,
            [0x00AA_9568u32, 0x00E8_EAD8],
        ),
        (
            "#1f2328",
            "#ffffff",
            0x001F_2328,
            0x00FF_FFFF,
            [0x0036_3D4D, 0x0059_4A3C],
        ),
    ];
    for (ink, paper, ink_rgb, paper_rgb, coats) in cases {
        let mut rend = renderer();
        let (cw, ch) = rend.cell_size();
        let mut term = page(ink, paper);
        let clean = rend
            .render_input(&term.cell_frame(ROWS, COLS))
            .pixels
            .clone();
        let width = rend.render_input(&term.cell_frame(ROWS, COLS)).width;
        assert_eq!(
            clean[0], paper_rgb,
            "the page colour (OSC 11) must reach the frame"
        );
        assert!(
            clean
                .iter()
                .any(|&p| p == ink_rgb || contrast(p, paper_rgb) > 7.0),
            "the line must ink"
        );
        // The body: over "kitty" (cols 4..9), from the middle of row 0 to the middle of row 2,
        // so it covers whole letters and parts of them, as a resting head does.
        let (bx0, by0) = (4 * cw, ch / 2);
        let (bw, bh) = (5 * cw, 2 * ch);
        let sprite = body(bx0 as i32, by0 as i32, bw as u16, bh as u16);
        assert_eq!(width, COLS * cw, "no pad: sprite and frame pixels coincide");
        let bx = [bx0, by0, bx0 + bw, by0 + bh];
        for fur in coats {
            let input = with_body(&mut term, fur, sprite);
            let on = rend.render_input(&input).pixels.clone();
            let j = judge(&clean, &on, width, paper_rgb, bx);
            assert!(
                j.ink > 40,
                "{ink} on {paper}, coat {fur:06x}: too few ink px judged ({}) — the body must \
                 lie on the letters",
                j.ink
            );
            assert_eq!(
                j.under, 0,
                "{ink} on {paper}, coat {fur:06x}: {} of {} ink px read under 4.5:1 on the body \
                 (worst {:.2})",
                j.under, j.ink, j.worst
            );
            // The body keeps every pixel no glyph touches: the knock-out is the glyphs' own
            // dilation, never a box.
            let kept = (bx[1]..bx[3])
                .flat_map(|y| (bx[0]..bx[2]).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let near_ink = (-2i64..=2).any(|dy| {
                        (-2i64..=2).any(|dx| {
                            let (xx, yy) = (x as i64 + dx, y as i64 + dy);
                            clean[yy as usize * width + xx as usize] != paper_rgb
                        })
                    });
                    !near_ink
                })
                .all(|(x, y)| on[y * width + x] == fur);
            assert!(
                kept,
                "{ink} on {paper}, coat {fur:06x}: a pixel no glyph touches lost its fur"
            );

            rend.set_knock_out(false);
            let off = rend.render_input(&input).pixels.clone();
            rend.set_knock_out(true);
            let j_off = judge(&clean, &off, width, paper_rgb, bx);
            assert!(
                j_off.under * 2 > j_off.ink,
                "negative control, {ink} on {paper}, coat {fur:06x}: with the knock-out off \
                 only {} of {} ink px read under 4.5:1 (worst {:.2}); the coat must take the \
                 letters' contrast away",
                j_off.under,
                j_off.ink,
                j_off.worst
            );
        }
    }
}

/// Away from the letters the knock-out changes nothing: a body on blank cells is byte-equal
/// with and without it, on the full and the damaged path.
#[test]
fn a_body_off_the_letters_is_untouched_by_the_knock_out() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = page("#d0d0d0", "#111318");
    // Rows 2..4 are blank: the body sits there, a pixel clear of row 1's letters.
    let sprite = body(
        2 * cw as i32,
        2 * ch as i32 + 1,
        (6 * cw) as u16,
        (ch + 4) as u16,
    );
    let input = with_body(&mut term, 0x00AA_9568, sprite);
    let on = rend.render_input(&input).pixels.clone();
    rend.set_knock_out(false);
    let off = rend.render_input(&input).pixels.clone();
    rend.set_knock_out(true);
    assert_eq!(
        on, off,
        "a body off the letters must be untouched by the knock-out"
    );

    // The damaged path repaints a moved body onto the letters exactly as a fresh frame does.
    let mut wc = WindowCpu::new();
    let _ = rend.render_input_cached(&mut wc, &input);
    let onto = with_body(
        &mut term,
        0x00AA_9568,
        body(
            4 * cw as i32,
            ch as i32 / 2,
            (5 * cw) as u16,
            (2 * ch) as u16,
        ),
    );
    let cached = rend.render_input_cached(&mut wc, &onto).pixels().to_vec();
    let fresh = rend.render_input(&onto).pixels.clone();
    assert_eq!(
        cached, fresh,
        "the damaged path must knock the letters out as a fresh frame does"
    );
}

/// ON GLASS THE KNOCK-OUT LAYS THE PAGE'S COLOUR, NEVER ITS OPACITY (2026-10-23, review: at
/// background opacity 0.8 a stamp in the opaque page turned 789 px round the letters under an
/// all-transparent body box solid). A body box over the letters whose texels are all
/// transparent (a tile's border, the air round a silhouette) leaves the frame byte-equal with
/// and without the knock-out; a tan coat over them has its letters read against the page's
/// colour as on an opaque window, and no pixel is made more opaque than the body left it.
#[test]
fn on_a_translucent_window_the_knock_out_keeps_the_glass() {
    let mut rend = renderer();
    rend.set_background_opacity(0.8);
    let (cw, ch) = rend.cell_size();
    let mut term = page("#d0d0d0", "#111318");
    let clean = rend
        .render_input(&term.cell_frame(ROWS, COLS))
        .pixels
        .clone();
    let width = rend.render_input(&term.cell_frame(ROWS, COLS)).width;
    assert!(
        clean[0] >> 24 > 0,
        "fixture: the page is glass ({:08x})",
        clean[0]
    );
    let (bx0, by0, bw, bh) = (4 * cw, ch / 2, 5 * cw, 2 * ch);
    let sprite = body(bx0 as i32, by0 as i32, bw as u16, bh as u16);
    let mut air = with_body(&mut term, 0, sprite);
    air.free_atlas = Some(Arc::new(SceneAtlas {
        width: bw as u32,
        height: bh as u32,
        rgba: vec![0; bw * bh * 4],
        version: 1,
    }));
    let on = rend.render_input(&air).pixels.clone();
    rend.set_knock_out(false);
    let off = rend.render_input(&air).pixels.clone();
    rend.set_knock_out(true);
    let changed = on.iter().zip(&off).filter(|(a, b)| a != b).count();
    assert_eq!(
        changed, 0,
        "an all-transparent body box: the knock-out changed {changed} px of the glass"
    );

    let fur = with_body(&mut term, 0x00AA_9568, sprite);
    let on = rend.render_input(&fur).pixels.clone();
    rend.set_knock_out(false);
    let off = rend.render_input(&fur).pixels.clone();
    rend.set_knock_out(true);
    let opaquer = on
        .iter()
        .zip(&off)
        .filter(|(a, b)| (**a >> 24) < (**b >> 24))
        .count();
    assert_eq!(
        opaquer, 0,
        "the knock-out made {opaquer} px more opaque than the body left them"
    );
    let rgb = |px: &[u32]| px.iter().map(|p| p & 0x00FF_FFFF).collect::<Vec<u32>>();
    let paper = 0x0011_1318;
    let bx = [bx0, by0, bx0 + bw, by0 + bh];
    let read = judge(&rgb(&clean), &rgb(&on), width, paper, bx);
    let fur_read = judge(&rgb(&clean), &rgb(&off), width, paper, bx);
    assert!(
        read.ink > 40 && read.under == 0,
        "on glass the letters on the coat read against the page: {} of {} ink px under 4.5:1 \
         (worst {:.2})",
        read.under,
        read.ink,
        read.worst
    );
    assert!(
        fur_read.under * 2 > fur_read.ink,
        "negative control: the knock-out off leaves only {} of {} ink px under 4.5:1",
        fur_read.under,
        fur_read.ink
    );
}
