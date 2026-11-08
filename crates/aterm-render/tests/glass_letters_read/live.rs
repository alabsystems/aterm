// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The glyph law through a real cursor step, publication, yield and raster.

use super::*;
use crate::trail_letters_read::Pair;
use aterm_core::render::{GlowQuad, RenderInput};
use aterm_core::terminal::ClockReading;
use aterm_effects::cursor_fx::CursorFxInput;
use aterm_effects::cursor_glow::glass_yield::{
    with_least_level_bar, with_whole_word_cut, without_body_glass_for_test as without_body_glass,
};
use aterm_effects::cursor_glow::{
    Geom, GlowConfig, GlowStyle, Light, Live, ProbeTrust, letter_bar, with_share_over_aa,
};
use aterm_effects::cursor_trail::TrailConfig;
use aterm_effects::host::{PageFacts, motion_print_anchor};
use aterm_effects::pipeline::{CursorStep, EffectsPipeline, RowProbe};
use aterm_effects::rainbow_kitty::prism_glass::PAGE_SHARE;
use aterm_render::GlyphInk;
use std::time::Duration;

const PAGE: (u32, u32) = (0x002B_2D33, 0x00F6_F7F9);

/// Observe the lowest actual census reading relative to its own glyph's bar.
/// The same face and input make the clean twin; no static glass background stands
/// in for the effect stream that the word yield can cut.
struct LiveReading {
    cell_height: usize,
    admitted: bool,
    thin_ink: f32,
    glass_frames: usize,
    relative: Option<f64>,
    retained_safe_glyphs: usize,
    /// The most consecutive columns of a frame's glyph row whose foot rail stands out from the
    /// frame with no glass ([`RAIL_SHOWS`]).
    rail_run: usize,
    /// The app-ink glyphs judged on glass frames that read [`AA`] or more on the bare page and under
    /// the floor, and of those the ones the frame reads under AA, and the least such reading.
    at_aa: usize,
    under_aa: usize,
    least_at_aa: f64,
    /// The same as the gallery's judges read a letter (`aterm-lab trail-contrast`'s
    /// `local_contrast`, [`Pair::letter`]: its worst pixel row's lower median against its ring):
    /// the app-ink letters on glass frames whose clean twin reads [`AA_READ`] or more and under the
    /// floor, those the frame reads under AA_READ, and the least such reading.
    judged_at_aa: usize,
    judged_under_aa: usize,
    judged_least_at_aa: f64,
}

/// AA as the judges read it: they give a reading to two decimals, so one of 4.495 reads 4.50.
const AA_READ: f64 = AA - 0.005;

/// WCAG AA for text, the legibility law's bar: a letter that reads it on the bare page reads it
/// under the glass.
const AA: f64 = 4.5;

/// How far a pixel moves from the frame with no glass, in a channel, for the rail to read there:
/// a whole glass moves it by far more, and a cap the yield left a coloured word by far less.
const RAIL_SHOWS: u32 = 24;

/// One line of glyphs a live glass is drawn over.
#[derive(Clone, Copy)]
struct LiveLine<'a> {
    text: &'a str,
    /// The SGR ink of each glyph (its parameters, `38;5;160` or `38;2;r;g;b`): `None`, the theme's.
    ink: &'a dyn Fn(usize) -> Option<String>,
    /// Whether the frame takes the yield.
    yields: bool,
}

/// A final under-stream pixel inside the clipped bounds has nonzero alpha and
/// changes colour compared with the same frame with only its body glass removed.
fn visible_glass_pixel(
    input: &RenderInput,
    sheet: &Sheet,
    without_glass: &[u32],
    quads: &[GlowQuad],
    (x0, y0, x1, y1): (usize, usize, usize, usize),
) -> bool {
    quads.iter().any(|q| {
        if usize::from(q.row) >= input.rows {
            return false;
        }
        let x0 = x0.max(usize::from(q.x));
        let y0 = y0.max(usize::from(q.y));
        let x1 = x1.min(sheet.w).min(usize::from(q.x) + usize::from(q.w));
        let y1 = y1
            .min(sheet.frame.len() / sheet.w)
            .min(usize::from(q.y) + usize::from(q.h));
        (x0..x1).any(|x| {
            aterm_render::glow_lerp(
                u32::from(q.alpha),
                u32::from(q.alpha2),
                (x - usize::from(q.x)) as u32,
                u32::from(q.w),
            ) > 0
                && (y0..y1).any(|y| {
                    let at = y * sheet.w + x;
                    sheet.frame[at] != without_glass[at]
                })
        })
    })
}

fn live_reading(px: f32, line_height: f32, fg: u32, bg: u32) -> LiveReading {
    live_text_reading(px, line_height, fg, bg, GLYPHS)
}

fn live_text_reading(px: f32, line_height: f32, fg: u32, bg: u32, text: &str) -> LiveReading {
    live_line_reading(
        px,
        line_height,
        fg,
        bg,
        LiveLine {
            text,
            ink: &|_| None,
            yields: true,
        },
    )
}

fn live_line_reading(
    px: f32,
    line_height: f32,
    fg: u32,
    bg: u32,
    LiveLine { text, ink, yields }: LiveLine<'_>,
) -> LiveReading {
    let mut renderer = Renderer::from_system(px, Theme::default()).expect("the window face");
    renderer.set_line_height(line_height);
    renderer.debug_block_on_lazy_fallbacks();
    let thin_ink = renderer.glyph_ink().thin_share(
        aterm_render::linear_luminance(fg),
        aterm_render::linear_luminance(bg),
    );
    let admitted = PageFacts {
        thin_ink,
        ..PageFacts::OPAQUE
    }
    .glass();
    let (cw, ch) = renderer.cell_size();
    let cols = text.chars().count() + 2;
    let mut term = Terminal::new(3, cols as u16);
    term.set_default_foreground(rgb(fg));
    term.set_default_background(rgb(bg));
    term.process(b"\x1b[2;2H");
    let mut fx = EffectsPipeline::for_window();
    let cfg = GlowConfig {
        style: GlowStyle::RainbowKitty,
        dark_theme: false,
        theme_fg: fg,
        theme_bg: bg,
        color: fg,
        duration: Duration::from_millis(650),
        length: usize::MAX,
        intensity: 1.0,
        light: Light::at(Live::Glass).on_page(admitted),
        ..Default::default()
    };
    let (w, h) = renderer.frame_size(3, cols);
    let geom = Geom {
        cw,
        ch,
        rows: 3,
        cols,
        origin_x: renderer.pad() as u16,
        origin_y: renderer.grid_top() as u16,
        win_w: w as u16,
        win_h: h as u16,
        head: 0,
    };
    let mut row = Vec::new();
    let glyphs: Vec<_> = text.chars().collect();
    let t0 = ClockReading::now().monotonic;
    let (mut glass_frames, mut judged, mut judged_glass, mut least) = (0, 0, 0, f64::INFINITY);
    let mut retained_safe_glyphs = 0;
    let mut rail_run = 0;
    let (mut at_aa, mut under_aa, mut least_at_aa) = (0, 0, f64::INFINITY);
    let (mut judged_at_aa, mut judged_under_aa, mut judged_least_at_aa) = (0, 0, f64::INFINITY);
    for tick in 0..glyphs.len() * 8 + 48 {
        let now = t0 + Duration::from_nanos(1_000_000_000 / 120) * tick as u32;
        if tick % 8 == 0 && tick / 8 < glyphs.len() {
            fx.cursor_mut().glow.note_synthetic_typed(now, 1);
            let sgr = ink(tick / 8).map_or_else(|| "\x1b[39m".to_owned(), |n| format!("\x1b[{n}m"));
            term.process(sgr.as_bytes());
            term.process_at(
                glyphs[tick / 8].encode_utf8(&mut [0; 4]).as_bytes(),
                ClockReading {
                    monotonic: now,
                    wall_ms: None,
                },
            );
        }
        let base = term.cell_frame(3, cols);
        let observed = renderer.glyph_ink_for(&base);
        fx.cursor_mut()
            .glow
            .observe_glyph_ink(Some(std::sync::Arc::clone(&observed)));
        let caret = (base.cursor_row as u16, base.cursor_col as u16);
        term.row_cols_into(1, &mut row);
        fx.cursor_mut().glow.observe_ribbon_row(1, &row);
        let (print_anchor, print_anchor_glyph) = motion_print_anchor(&term);
        let _ = aterm_render::test_counters::take_glass_slabs();
        let _ = fx.cursor_step(
            &CursorStep::new(
                CursorFxInput {
                    now,
                    cur: Some(caret),
                    live_viewport: true,
                    cursor_style: term.cursor_style(),
                    blink_phase: true,
                    live_cursor: fg,
                    cursor_color_pinned: false,
                    default_bg: bg,
                    cursor_cell_bg: None,
                    geom,
                    focused: true,
                    shed_envelope: 1.0,
                    trail_brightness: 1.0,
                    body_allowed: true,
                    momentum_glow: true,
                    user_tinted: false,
                    accent_pinned: false,
                },
                &cfg,
                TrailConfig {
                    enabled: false,
                    duration: cfg.duration,
                    max_len: cfg.length,
                    color: cfg.color,
                    intensity: cfg.intensity,
                    warmth: 0.0,
                },
            )
            .with_row_probe(Some(RowProbe {
                row: 1,
                caret: caret.1,
                trust: ProbeTrust::Full,
                cols: &row,
                flanks: None,
            }))
            .with_frame_cells(Some(base.cells.as_slice()))
            .with_frame_cells_cols(base.cols)
            .with_frame_marks(&base.combining)
            .with_print_anchor(print_anchor)
            .with_print_anchor_glyph(print_anchor_glyph),
            std::iter::empty(),
        );
        let emitted_glass_slabs = aterm_render::test_counters::take_glass_slabs();
        let mut input = base.clone();
        fx.publish_cursor(&mut input);
        let without_glass = without_body_glass(&input, fx.cursor().glow.glass_laid());
        if yields {
            fx.yield_glass_unworded(&mut input);
        }
        let glass_arm = fx.light_arms().iter().any(|(arm, _)| arm == "glass");
        let clean = renderer.render_input(&base);
        let frame = renderer.render_input(&input);
        let without_glass = renderer.render_input(&without_glass);
        assert_eq!(frame.width, cols * cw, "the judged frame has no padding");
        assert_eq!(
            (frame.width, frame.height),
            (without_glass.width, without_glass.height),
            "the no-body-glass twin has the same frame geometry"
        );
        let without_glass: Vec<u32> = without_glass
            .pixels
            .into_iter()
            .map(|pixel| pixel & 0x00FF_FFFF)
            .collect();
        let sheet = Sheet {
            frame: frame
                .pixels
                .into_iter()
                .map(|pixel| pixel & 0x00FF_FFFF)
                .collect(),
            clean: clean
                .pixels
                .into_iter()
                .map(|pixel| pixel & 0x00FF_FFFF)
                .collect(),
            w: frame.width,
            cw,
            ch,
        };
        let mut glass_quads = Vec::new();
        input
            .glow_under_ramps
            .expand_into(&input.glow_under, &mut glass_quads);
        let glass = glass_arm
            && emitted_glass_slabs > 0
            && visible_glass_pixel(
                &input,
                &sheet,
                &without_glass,
                &glass_quads,
                (0, 0, sheet.w, sheet.frame.len() / sheet.w),
            );
        glass_frames += usize::from(glass);
        if glass {
            // The line's feet, and the rail past them: its run's reading is against the frame
            // with no glass, column by column along the glyph row.
            let foot = observed
                .cell_box(GlyphInk::index('o', false))
                .map(|[_, _, _, y1]| usize::try_from(y1).expect("a row in its cell") + 2);
            if let Some(foot) = foot.filter(|&foot| foot < ch) {
                let shows = |col: usize| {
                    (foot..ch).any(|y| {
                        (0..cw).any(|x| {
                            let at = (ch + y) * sheet.w + col * cw + x;
                            let (a, b) = (sheet.frame[at], without_glass[at]);
                            [16_u32, 8, 0].iter().any(|sh| {
                                ((a >> sh) & 0xff).abs_diff((b >> sh) & 0xff) >= RAIL_SHOWS
                            })
                        })
                    })
                };
                let mut run = 0;
                for col in 0..cols {
                    run = if shows(col) { run + 1 } else { 0 };
                    rail_run = rail_run.max(run);
                }
            }
        }
        // A word in an app's ink keeps its letter bar on what it reads on the bare page, as the
        // census reads a letter: its worst solid pixel on its clean twin. A letter whose bar is
        // over that reading takes no light that lowers it, and reads it.
        let bare = Sheet {
            frame: sheet.clean.clone(),
            clean: sheet.clean.clone(),
            w: sheet.w,
            cw,
            ch,
        };
        let bare = bare.glyph_readings_for(0, text);
        for (col, glyph, reading, own) in sheet.glyph_readings_for(0, text) {
            judged += 1;
            assert!(reading.is_finite() && own.is_finite() && own > 0.0);
            if !glass {
                continue;
            }
            judged_glass += 1;
            let bar = if ink(col - 1).is_some() {
                let (_, _, bare, _) = bare
                    .iter()
                    .find(|(at, ..)| *at == col)
                    .expect("the clean twin holds the glyph");
                if (AA..4.6).contains(bare) {
                    at_aa += 1;
                    under_aa += usize::from(reading < AA);
                    least_at_aa = least_at_aa.min(reading);
                }
                let pair = Pair {
                    frame: &sheet.frame,
                    clean: &sheet.clean,
                    w: sheet.w,
                    cw,
                    ch,
                };
                if let Some(own) = pair
                    .own(1, col, 1)
                    .filter(|own| (AA_READ..4.6).contains(own))
                {
                    let read = pair.letter(1, col, 1, glyph).map_or(own, |l| l.judged());
                    judged_at_aa += 1;
                    judged_under_aa += usize::from(read < AA_READ);
                    judged_least_at_aa = judged_least_at_aa.min(read);
                }
                f64::from(letter_bar(*bare as f32, false)).min(*bare)
            } else {
                GLASS_BAR.min(PAGE_SHARE * own)
            };
            least = least.min(reading / bar);
            let retains_glass = visible_glass_pixel(
                &input,
                &sheet,
                &without_glass,
                &glass_quads,
                (col * cw, ch, (col + 1) * cw, 2 * ch),
            );
            let real_ink = observed
                .for_frame(&base, (cw, ch))
                .and_then(|frame| frame.coverage(1, col))
                .is_some_and(|(_, coverage)| coverage.max > 0);
            retained_safe_glyphs +=
                usize::from(glyph == 'M' && real_ink && retains_glass && reading >= bar);
            if reading < bar {
                eprintln!(
                    "{px}px/{line_height} at {tick}: '{glyph}' {reading:.4}, own {own:.4}, bar {bar:.4}"
                );
            }
        }
    }
    assert!(
        judged > 0,
        "{px}px/{line_height}: the census judged actual glyphs"
    );
    let relative = if glass_frames > 0 {
        assert!(judged_glass > 0, "glass frames must judge actual glyphs");
        assert!(least.is_finite(), "a finite glyph-to-own-bar minimum");
        Some(least)
    } else {
        None
    };
    LiveReading {
        cell_height: ch,
        admitted,
        thin_ink,
        glass_frames,
        relative,
        retained_safe_glyphs,
        rail_run,
        at_aa,
        under_aa,
        least_at_aa,
        judged_at_aa,
        judged_under_aa,
        judged_least_at_aa,
    }
}

fn keeps_current_page_law(reading: &LiveReading) {
    if reading.admitted {
        assert!(
            reading.glass_frames > 0,
            "an admitted window emits actual glass"
        );
        let relative = reading
            .relative
            .expect("actual glyphs judged in glass frames");
        assert!(relative >= 1.0, "{relative:.4} of the existing glyph bar");
    } else {
        assert_eq!(
            reading.glass_frames, 0,
            "a refused face keeps the paper arm"
        );
        assert!(
            reading.relative.is_none(),
            "no glass readability verdict on paper"
        );
    }
}

#[test]
fn ordinary_window_glyphs_keep_the_current_page_law() {
    for px in [20.0, 24.0] {
        let reading = live_reading(px, 1.0, PAGE.0, PAGE.1);
        if px == 24.0 {
            assert!(
                reading.admitted,
                "the ordinary 2x window is a glass positive"
            );
        }
        keeps_current_page_law(&reading);
    }
}

#[test]
fn a_supported_word_keeps_nonzero_final_glass_and_its_same_frame_census_bar() {
    let reading = live_text_reading(24.0, 1.0, PAGE.0, PAGE.1, "MMMM MMMM");
    assert!(reading.admitted, "the ordinary 12pt/2x face admits glass");
    keeps_current_page_law(&reading);
    assert!(
        reading.retained_safe_glyphs > 0,
        "a real M must keep final glass beneath its cell while passing its unchanged census bar in that frame"
    );
}

#[test]
fn the_twenty_six_pixel_underscore_keeps_the_current_page_law() {
    let reading = live_reading(26.0, 1.0, PAGE.0, PAGE.1);
    keeps_current_page_law(&reading);
}

#[test]
fn a_taller_cell_does_not_override_the_faces_actual_ink() {
    let ordinary = live_reading(19.0, 1.0, PAGE.0, PAGE.1);
    let padded = live_reading(19.0, 1.5, PAGE.0, PAGE.1);
    assert!(
        padded.cell_height > ordinary.cell_height,
        "the padding is real"
    );
    assert_eq!(
        padded.thin_ink, ordinary.thin_ink,
        "padding does not thicken the face"
    );
    assert_eq!(padded.admitted, ordinary.admitted);
    keeps_current_page_law(&ordinary);
    keeps_current_page_law(&padded);
}

/// After-land A7, R2's "one run, one hue phase, letters not cut, not sticker-outlined", on the line
/// that had no reading: a line of coloured words, each in an xterm ink as `ls` or `grep` lays it,
/// through a real cursor step, publication, yield and raster, on Default Light and GitHub Light.
/// Every letter keeps its letter bar on the frame, against its own clean twin's reading, and the
/// ribbon's foot rail stands out under the words as one run. Negative controls: with no yield some
/// letter falls under its bar, and a word cut across its whole cells
/// ([`with_whole_word_cut`]) breaks the run.
#[test]
fn a_coloured_line_keeps_every_letter_at_its_bar_and_one_run_under_it() {
    const TEXT: &str = "the quick brown fox jumps over";
    const INKS: [u8; 6] = [160, 27, 28, 127, 166, 30];
    let word = |at: usize| TEXT[..at].matches(' ').count();
    let ink = move |at: usize| Some(format!("38;5;{}", INKS[word(at) % INKS.len()]));
    for (fg, bg) in [PAGE, (0x001F_2328, 0x00FF_FFFF)] {
        let read = |yields: bool| {
            live_line_reading(
                24.0,
                1.0,
                fg,
                bg,
                LiveLine {
                    text: TEXT,
                    ink: &ink,
                    yields,
                },
            )
        };
        let law = read(true);
        assert!(law.admitted, "the ordinary 2x window admits glass");
        assert!(law.glass_frames > 0, "the line draws its glass");
        let relative = law.relative.expect("letters judged on glass frames");
        assert!(
            relative >= 1.0,
            "#{bg:06x}: {relative:.4} of the letter bar"
        );
        assert!(
            law.rail_run * 2 >= TEXT.len(),
            "#{bg:06x}: the rail's run is {} columns of {}",
            law.rail_run,
            TEXT.len()
        );
        let bare = read(false)
            .relative
            .expect("letters judged on glass frames");
        assert!(
            bare < 1.0,
            "control, #{bg:06x}: with no yield a letter reads {bare:.4} of its bar"
        );
        let whole = with_whole_word_cut(|| read(true));
        assert!(
            whole.rail_run * 4 < law.rail_run,
            "control, #{bg:06x}: cut across its whole cells, the run is {} columns against {}",
            whole.rail_run,
            law.rail_run
        );
    }
}

/// After-land round 3 (W1, W2, whole(e)): the legibility law's bar is AA. An app's ink that reads
/// 4.5 or more on its bare page reads 4.5 or more under the glass; the letter bar kept 97% of a
/// reading under 4.6, so an ink of its own 4.54 fell to 4.40 under a word's wash on the coloured
/// line and in the flood, where main reads it on the bare page. A line of the flood's words, nearly
/// every letter reaching past the line's feet onto the rail, each word in an ink of the flood's two
/// hues dark enough that some of its letters read between 4.5 and 4.6 on Default Light or GitHub
/// Light, through a real cursor step, publication, yield and raster: every such letter on a glass
/// frame reads AA or more, by its worst solid pixel as the census reads it and as the gallery's
/// judges read it ([`Pair::letter`], to their two decimals: [`AA_READ`]), and the run still stands
/// on its rail. Negative controls: with such a letter keeping 97% of its reading, as amendment 64
/// held it ([`with_share_over_aa`]), one reads under AA on each page; and with each solid level
/// held to its dimmest level's bar ([`with_least_level_bar`]), one the judges read at AA reads
/// under it on Default Light (on GitHub Light, at that bar, none on this line falls under it: the
/// least reads 4.508). The last two words are magentas of the coloured line and the flood: W1's
/// #b43fb4 and #b441b4 on Default Light, #bb43bb and the flood's #c1479f on GitHub Light. The
/// first is the face's [`uncored_word`] in #b43fb4: a glyph whose core holds its word from every
/// glass holds it under the last control too. A letter the judges read at 4.495 to 4.5 is held by
/// aterm-effects' `a_letter_the_judges_read_at_aa_keeps_aa` and
/// `a_letter_on_a_dim_page_keeps_a_level_in_hand`.
#[test]
fn an_app_ink_at_aa_keeps_aa_under_the_glass() {
    let text = format!(
        "gypsy pygmy jiggy foggy poppy guppy {} wigwag",
        uncored_word(PAGE, "180;63;180")
    );
    // Each page's inks, its letters' readings at this size straddling 4.5 to 4.6 (a step of a
    // level moves a glyph's worst solid pixel by a few hundredths), and two magentas.
    const PAGES: [(u32, u32, [&str; 8]); 2] = [
        (
            PAGE.0,
            PAGE.1,
            [
                "38;2;172;60;114",
                "38;2;40;113;113",
                "38;2;173;61;115",
                "38;2;180;63;119",
                "38;2;41;118;118",
                "38;2;181;63;120",
                "38;2;180;63;180",
                "38;2;180;65;180",
            ],
        ),
        (
            0x001F_2328,
            0x00FF_FFFF,
            [
                "38;2;180;63;119",
                "38;2;41;118;118",
                "38;2;181;63;120",
                "38;2;188;66;124",
                "38;2;43;123;123",
                "38;2;190;66;126",
                "38;2;187;67;187",
                "38;2;193;71;159",
            ],
        ),
    ];
    let word = |at: usize| text[..at].matches(' ').count();
    for (fg, bg, inks) in PAGES {
        let ink = move |at: usize| Some(inks[word(at) % inks.len()].to_owned());
        let read = || {
            live_line_reading(
                24.0,
                1.0,
                fg,
                bg,
                LiveLine {
                    text: &text,
                    ink: &ink,
                    yields: true,
                },
            )
        };
        let law = read();
        assert!(law.glass_frames > 0, "#{bg:06x}: the line draws its glass");
        assert!(
            law.at_aa > 0,
            "#{bg:06x}: premise, no letter on a glass frame reads between AA and the floor"
        );
        assert_eq!(
            law.under_aa, 0,
            "#{bg:06x}: of {} letters at AA, {} read under it, the least {:.3}",
            law.at_aa, law.under_aa, law.least_at_aa
        );
        assert!(
            law.rail_run * 2 >= text.len(),
            "#{bg:06x}: the rail's run is {} columns of {}",
            law.rail_run,
            text.len()
        );
        assert!(
            law.judged_at_aa > 0,
            "#{bg:06x}: premise, the judges read no letter on a glass frame between AA and the \
             floor"
        );
        assert_eq!(
            law.judged_under_aa, 0,
            "#{bg:06x}: of {} letters the judges read at AA, {} read under it, the least {:.3}",
            law.judged_at_aa, law.judged_under_aa, law.judged_least_at_aa
        );
        let share = with_share_over_aa(read);
        assert!(
            share.under_aa > 0,
            "control, #{bg:06x}: at 97% of its reading no letter at AA reads under it (least {:.3} \
             of {})",
            share.least_at_aa,
            share.at_aa
        );
        let least = with_least_level_bar(read);
        assert!(
            bg != PAGE.1 || least.judged_under_aa > 0,
            "control, #{bg:06x}: at its dimmest level's bar no letter the judges read at AA reads \
             under it (least {:.3} of {})",
            least.judged_least_at_aa,
            least.judged_at_aa
        );
    }
}

/// The first of `wow` and `xxx` in `ink` (`r;g;b`) on the page `(fg, bg)` at 24 px no glyph of
/// which holds the word from every glass by its core alone: a core whose letter bar lies over its
/// own reading keeps its word whole whatever its solid levels' bars (aterm-effects' `uncored_word`,
/// the yield's own test of the same law, says why). #b43fb4's on Default Light is `wow` under
/// macOS's SF Mono, where the control was measured on it, and `xxx` under Linux's bundled JetBrains
/// Mono, whose `o` core (coverage 253) reads 4.4976 there (Linux arm64, 2026-11-07).
fn uncored_word((fg, bg): (u32, u32), ink: &str) -> &'static str {
    let mut renderer = Renderer::from_system(24.0, Theme::default()).expect("the window face");
    let (cw, ch) = renderer.cell_size();
    let rgb_ink = ink
        .split(';')
        .map(|c| c.parse::<u32>().expect("a channel"))
        .fold(0, |rgb, c| rgb << 8 | c);
    ["wow", "xxx"]
        .into_iter()
        .find(|word| {
            let mut term = Terminal::new(1, 8);
            term.set_default_foreground(rgb(fg));
            term.set_default_background(rgb(bg));
            term.process(format!("\x1b[38;2;{ink}m{word}").as_bytes());
            let input = term.cell_frame(1, 8);
            let glyphs = renderer.glyph_ink_for(&input);
            let frame = glyphs
                .for_frame(&input, (cw, ch))
                .expect("the frame's glyph evidence");
            let corrected = frame.corrected();
            (0..word.len()).all(|col| {
                let (_, coverage) = frame.coverage(0, col).expect("the glyph's raster");
                let core = coverage
                    .core_level(
                        aterm_render::linear_luminance(rgb_ink),
                        aterm_render::linear_luminance(bg),
                        corrected,
                    )
                    .expect("an inked glyph");
                let read = contrast(
                    aterm_render::blend_text(bg, rgb_ink, bg, core, corrected),
                    bg,
                ) as f32;
                letter_bar(read, false) <= read
            })
        })
        .expect("a word no core holds")
}
