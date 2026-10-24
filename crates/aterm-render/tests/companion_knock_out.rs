// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// The knock-out: letters on a companion's body read against the page. A body drawn under the
// text (a knock-out sprite layer, `FreeZ::UnderText`: the pet, its departures, the idle cat) is cut out under each glyph
// that lies on it, the glyph's coverage dilated by one pixel and stamped in the cell's page
// colour before the trails draw, so the ink reads against the page with a hairline of page
// around it whatever the coat. Judged per ink pixel against the ground next to it (the
// nearest ring of pixels the glyph does not touch, as `aterm-lab trail-contrast` judges a
// companion), on a dark and a light page, with coats near the ink's own lightness: the tan
// and the cream of the trail gallery's `burst` landings (1.3 to 1.9:1 under the fur). Negative
// control: the knock-out off (`Renderer::set_knock_out(false)`) leaves letters under 4.5:1.
// Beside the body nothing changes: a frame with the body away from the letters is byte-equal
// with and without the knock-out, and the body itself keeps every pixel no glyph touches.

use crate::wcag::{contrast, luma as rel_luma};
use aterm_core::render::RenderInput;
use aterm_core::terminal::Terminal;
use aterm_render::{Renderer, Theme, WindowCpu};

const ROWS: usize = 4;
const COLS: usize = 24;
const LINE: &str = "the kitty flies";

fn renderer() -> Renderer {
    Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature")
}

/// A body's box: `(x, y, w, h)` in grid pixels.
type Body = (i32, i32, u16, u16);

fn body(x: i32, y: i32, w: u16, h: u16) -> Body {
    (x, y, w, h)
}

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

/// The page with a body of one straight colour `rgba` (a knock-out sprite
/// layer) over `(x, y, w, h)`.
fn with_coat(term: &mut Terminal, rgba: [u8; 4], (x, y, w, h): Body) -> RenderInput {
    let mut input = term.cell_frame(ROWS, COLS);
    crate::sprites::put(
        &mut input,
        &[crate::sprites::under(
            x,
            y,
            crate::sprites::solid(rgba, w, h),
        )],
    );
    input
}

fn with_body(term: &mut Terminal, fur: u32, sprite: Body) -> RenderInput {
    with_coat(
        term,
        [(fur >> 16) as u8, (fur >> 8) as u8, fur as u8, 255],
        sprite,
    )
}

#[test]
fn letters_on_a_body_drawn_under_the_text_read_against_the_page() {
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

#[test]
fn a_body_off_the_letters_is_untouched_by_the_knock_out() {
    let mut rend = renderer();
    let (cw, ch) = rend.cell_size();
    let mut term = page("#d0d0d0", "#111318");
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
    let air = with_coat(&mut term, [0; 4], sprite);
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
