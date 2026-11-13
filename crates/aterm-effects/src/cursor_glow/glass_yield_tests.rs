// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The whole-cell cut's gradient rounding, which a word's wash and a word the frame's evidence
/// shows no letters for both lay: it runs under [`with_whole_word_cut`], since a descender on
/// the rail lays each pixel of its ring as a span of its own
/// (`a_letter_s_ring_lays_each_pixel_and_its_feather_one_span`).
#[test]
fn published_caps_keep_synthetic_ink_readable_after_gradient_rounding() {
    with_whole_word_cut(published_caps_keep_synthetic_ink_readable_after_gradient_rounding_whole);
}

fn published_caps_keep_synthetic_ink_readable_after_gradient_rounding_whole() {
    const WIDTH: usize = 15;
    let mut renderer =
        aterm_render::Renderer::from_system(40.0, aterm_render::Theme::default()).unwrap();
    renderer.debug_block_on_lazy_fallbacks();
    let (cw, ch) = renderer.cell_size();
    assert!(cw >= WIDTH, "the gradient fits wholly inside one word cell");
    let mut term = Terminal::new(1, 1);
    term.process(b"\x1b[?25l\x1b]10;rgb:2b/2d/33\x07\x1b]11;rgb:f6/f7/f9\x07M");
    let base = term.cell_frame(1, 1);
    let clean = renderer.render_input(&base);
    assert_eq!((clean.width, clean.height), (cw, ch));
    let ink = renderer.glyph_ink_for(&base);
    let frame = ink
        .for_frame(&base, (cw, ch))
        .expect("stable actual glyph key");
    let (key, _) = frame
        .coverage(0, 0)
        .expect("the carrier has an observed raster");
    let (epoch, corrected) = (frame.epoch(), frame.corrected());
    assert!(corrected, "the witness uses corrected text blending");
    let baseline = renderer.baseline();
    let image = renderer.glyph_image(key);
    let ink_top = baseline - image.height() as i32 - image.ymin();
    let ink_bottom = baseline - image.ymin();
    let y = (0..ch)
        .find(|&y| {
            !(ink_top..ink_bottom).contains(&(y as i32))
                && (0..WIDTH).all(|x| clean.pixels[y * clean.width + x] & 0x00FF_FFFF == BG)
        })
        .expect("a row outside the actual glyph raster exposes only the published ground");
    let palette = crate::rainbow_kitty::prism_glass::Palette::shipped(FG, BG).unwrap();
    let mut quad = GlowQuad::flat(0, 0, 0, WIDTH as u16, ch as u16, palette.glass[34], 255);
    quad.color2 = palette.glass[42];

    for level in [255, 175] {
        // Only the profile is synthetic. Reuse a real key/epoch in this test's
        // private memo so production word-cap, publication and lay run unchanged.
        // This does not claim that the carrier font has this uniform coverage.
        let judge = GlyphJudge::new(&GlyphCoverage::of(&[level; 32]), FG, BG, FG, corrected)
            .expect("nonempty synthetic coverage");
        assert!(
            judge.keeps(BG),
            "the synthetic ink keeps its clean-page bar"
        );
        let at = Instant::now();
        let mut effects = crate::pipeline::EffectsPipeline::for_window();
        let glow = &mut effects.cursor_mut().glow;
        glow.observe_glyph_ink(Some(std::sync::Arc::clone(&ink)));
        glow.under_out = vec![quad];
        glow.glass_laid = GlassLaid::new(
            at,
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, cw as i32, ch as i32),
            None,
        );
        glow.glass_yield.memo_fg = FG;
        glow.glass_yield.memo_page = BG;
        glow.glass_yield.observed_epoch = Some(epoch.clone());
        glow.glass_yield
            .judges
            .insert((key, FG), Some(std::sync::Arc::new(judge.clone())));
        let mut input = base.clone();
        effects.publish_cursor(&mut input);
        assert_eq!(
            input.glow_under,
            [quad],
            "the actual publisher carries the source gradient"
        );
        let report = effects.cursor_mut().glow.yield_glass(&mut input, &[], at);
        let cap = effects.cursor().glow.glass_yield.ks[0];
        if level == 255 {
            assert_eq!(cap, 255, "the ordinary full-coverage control keeps glass");
            assert!(!report.cut());
        } else {
            assert!(
                (1..255).contains(&cap),
                "the thin profile chooses a finite retained cap"
            );
            assert!(
                report.cut(),
                "the actual lay must rewrite the capped gradient"
            );
        }

        // Check the cap's own predictions through its existing sampling and
        // scaling operations, rather than a second interpolation formula.
        let mut predicted = Vec::new();
        composites_under(
            &mut predicted,
            (0, WIDTH as i32),
            (i32::from(quad.x), quad.w),
            ((quad.color, quad.alpha), (quad.color2, quad.alpha2)),
        );
        for (color, alpha) in predicted {
            let (color, alpha) = scale_end(color, alpha, f32::from(cap) / 255.0);
            assert!(
                judge.keeps(aterm_render::over_premul(BG, color, alpha)),
                "the selected cap's predictions must pass before judging its emitted stream"
            );
        }
        let actual = renderer.render_input(&input);
        assert_eq!((actual.width, actual.height), (clean.width, clean.height));
        assert!(
            (0..WIDTH).any(|x| actual.pixels[y * actual.width + x] & 0x00FF_FFFF != BG),
            "the witness must rasterize retained glass"
        );
        for x in 0..WIDTH {
            let ground = actual.pixels[y * actual.width + x] & 0x00FF_FFFF;
            assert!(
                judge.keeps(ground),
                "coverage {level}, cap {cap}, x {x}: emitted ground {ground:06x} must keep the unchanged glyph bar"
            );
        }
    }
}

#[test]
fn actual_raster_coverage_keeps_safe_glass_and_caps_thin_ink() {
    let (fg, page) = (0x002B_2D33, 0x00F6_F7F9);
    let glass = crate::rainbow_kitty::prism_glass::Palette::shipped(fg, page)
        .unwrap()
        .glass[0];
    let source = (aterm_render::premul_rgb(glass, 255), 255);
    let full = GlyphJudge::new(&GlyphCoverage::of(&[255; 32]), fg, page, fg, true).unwrap();
    let thin = GlyphJudge::new(&GlyphCoverage::of(&[160; 32]), fg, page, fg, true).unwrap();
    assert_eq!(
        full.cap(source),
        255,
        "safe observed ink retains actual glass"
    );
    assert_eq!(
        cap(fg, glass, page, YIELD_BAR),
        255,
        "the calibrated color-only law accepts it"
    );
    assert!(
        thin.cap(source) < 255,
        "the real thin raster cannot borrow full-ink coverage"
    );
    assert!(GlyphJudge::new(&GlyphCoverage::default(), fg, page, fg, true).is_none());
}

/// A judge reads each distinct ground of a glass's alphas once, and each level's corrected
/// alpha is worked out once ([`aterm_render::TextLevel`]): its cap is still the last alpha
/// before the first whose ground the glyph does not keep, as reading every alpha in turn
/// gives. This holds over the glass of Default Light and GitHub Light at every eighth
/// colour, whole and faint, for a full, a thin and an antialiased glyph, corrected and not.
#[test]
fn a_judge_caps_as_every_alpha_read_in_turn() {
    let ramp: Vec<u8> = (0..32_u8).map(|i| i * 8 + 7).collect();
    let (mut read, mut capped) = (0, 0);
    for (fg, page) in [(0x002B_2D33, 0x00F6_F7F9), (0x001F_2328, 0x00FF_FFFF)] {
        let palette = crate::rainbow_kitty::prism_glass::Palette::shipped(fg, page).unwrap();
        for corrected in [true, false] {
            for levels in [&[255_u8; 32][..], &[160; 32], &ramp] {
                let Some(judge) =
                    GlyphJudge::new(&GlyphCoverage::of(levels), fg, page, fg, corrected)
                else {
                    continue;
                };
                for &glass in palette.glass.iter().step_by(8) {
                    for alpha in [255_u8, 160, 64, 12] {
                        let source = (aterm_render::premul_rgb(glass, alpha), alpha);
                        let ground = |a: u16| {
                            let (c, k) = scale_end(source.0, source.1, f32::from(a) / 255.0);
                            aterm_render::over_premul(page, c, k) & 0x00FF_FFFF
                        };
                        let every = if judge.keeps(page) {
                            (1..=255_u16)
                                .find(|&a| !judge.keeps(ground(a)))
                                .map_or(255, |a| (a - 1) as u8)
                        } else {
                            0
                        };
                        assert_eq!(
                            judge.cap(source),
                            every,
                            "#{glass:06x} at {alpha} over {levels:?} (corrected {corrected})"
                        );
                        read += 1;
                        capped += usize::from(every < 255);
                    }
                }
            }
        }
    }
    assert!(
        read > 100 && capped > 0,
        "fixture: {read} glasses read, {capped} capped"
    );
}

/// r25 K8-1, THE LIGHT GLASS'S CAPS COST NO FRAME: the glass is animated, its colour and
/// alpha new on almost every frame, so the caps' memo missed and every frame of a line typed
/// on GitHub Light read about 2,300 caps of up to 255 grounds each, level by level (12 ms a
/// frame at the median, 47 at the 95th percentile, in a debug build: frames a person sees
/// jitter). A glass a full glyph keeps at every alpha (the solid ink of every glyph's core)
/// is capped by the judge's margins without one ground read level by level: every glass of
/// Default Light's and GitHub Light's at each alpha here, in the theme's ink and in an app's,
/// corrected and not. Every cap, over an antialiased glyph too, is still the one reading
/// every alpha in turn gives, and a glass the glyph does not keep is read to its first
/// ground under a bar.
#[test]
fn a_kept_glass_is_capped_without_reading_its_alphas() {
    let ramp: Vec<u8> = (0..32_u8).map(|i| i * 8 + 7).collect();
    let (mut settled, mut capped) = (0, 0);
    for (fg, page) in [(0x002B_2D33, 0x00F6_F7F9), (0x001F_2328, 0x00FF_FFFF)] {
        let palette = crate::rainbow_kitty::prism_glass::Palette::shipped(fg, page).unwrap();
        for ink in [fg, 0x0011_1111] {
            for corrected in [true, false] {
                for levels in [&[255_u8; 32][..], &ramp] {
                    let judge =
                        GlyphJudge::new(&GlyphCoverage::of(levels), ink, page, fg, corrected)
                            .expect("an inked glyph");
                    assert!(judge.page_kept, "fixture: the bare page keeps the glyph");
                    for &glass in &palette.glass {
                        for alpha in [255_u8, 192, 128, 64, 16] {
                            let source = (aterm_render::premul_rgb(glass, alpha), alpha);
                            let every = (1..=255_u16)
                                .find(|&a| {
                                    let (c, k) =
                                        scale_end(source.0, source.1, f32::from(a) / 255.0);
                                    !judge.keeps_by_levels(
                                        aterm_render::over_premul(page, c, k) & 0x00FF_FFFF,
                                    )
                                })
                                .map_or(255, |a| (a - 1) as u8);
                            let _ = take_level_reads();
                            let cap = judge.cap(source);
                            let reads = take_level_reads();
                            assert_eq!(cap, every, "#{glass:06x} at {alpha} over {levels:?}");
                            capped += usize::from(every < 255);
                            if every == 255 && levels[0] == 255 {
                                assert_eq!(
                                    reads, 0,
                                    "#{glass:06x} at {alpha} (ink #{ink:06x}, corrected \
                                     {corrected}): a kept glass read {reads} grounds level \
                                     by level"
                                );
                                settled += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        settled > 1000 && capped > 0,
        "fixture: {settled} kept glasses settled, {capped} capped"
    );
}

/// r25 K8-1 through the yield: a line of the face's own glyphs under a glass whose colour
/// and alpha move every frame, as the trail's do, reads no ground level by level on a frame
/// whose every word keeps the whole glass, on Default Light and GitHub Light.
#[test]
fn an_animated_glass_over_a_kept_line_reads_no_alpha() {
    let mut renderer = aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
        .expect("the bundled face");
    let (cw, ch) = renderer.cell_size();
    let mut kept_frames = 0;
    for (fg, bg, colours) in [
        (
            0x002B_2D33,
            0x00F6_F7F9,
            b"\x1b]10;rgb:2b/2d/33\x07\x1b]11;rgb:f6/f7/f9\x07",
        ),
        (
            0x001F_2328,
            0x00FF_FFFF,
            b"\x1b]10;rgb:1f/23/28\x07\x1b]11;rgb:ff/ff/ff\x07",
        ),
    ] {
        let mut term = Terminal::new(1, 24);
        term.process(colours);
        term.process(b"echo the quick brown fox");
        let mut base = term.cell_frame(1, 24);
        base.cursor_visible = false;
        let ink = renderer.glyph_ink_for(&base);
        let glasses = crate::rainbow_kitty::prism_glass::Palette::shipped(fg, bg)
            .unwrap()
            .glass;
        let mut yield_ = GlassYield::default();
        for frame in 0..24_usize {
            let alpha = [64_u8, 96, 128][frame % 3];
            let colour = |i: usize| aterm_render::premul_rgb(glasses[i % glasses.len()], alpha);
            let mut quad = GlowQuad::flat(
                0,
                0,
                0,
                (24 * cw) as u16,
                ch as u16,
                colour(frame * 5),
                alpha,
            );
            quad.color2 = colour(frame * 5 + 40);
            let mut input = base.clone();
            input.glow_under = vec![quad];
            let glass = GlassLaid::new(
                Instant::now(),
                (vec![quad], GlowRamps::default()),
                (fg, bg),
                (0, 0, cw as i32, ch as i32),
                None,
            );
            let _ = take_level_reads();
            let _ = yield_.apply_observed(&mut input, &glass, &[], Some(&ink), true);
            let reads = take_level_reads();
            if yield_.ks.iter().all(|&k| k == 255) {
                assert_eq!(
                    reads, 0,
                    "#{bg:06x} frame {frame}: every word keeps the glass, yet {reads} grounds \
                     were read level by level"
                );
                kept_frames += 1;
            }
        }
    }
    assert!(
        kept_frames >= 40,
        "fixture: {kept_frames} frames kept the glass"
    );
}

/// After-land R0: a word's judge ([`WordJudge`]) keeps exactly the grounds each of its glyphs'
/// judges keeps, and caps a composite at the least of their caps: on every shipped paper, for
/// words of the synthetic profiles (a full ink, a thin one, an antialiased ramp, each with bars
/// of its own), in the page's ink and an app's. The negative control reads the word as its
/// first glyph ([`WordMutant::FirstGlyph`]), and keeps a ground a later glyph fails.
#[test]
fn a_word_keeps_where_each_of_its_glyphs_keeps() {
    let ramp: Vec<u8> = (0..32_u8).map(|i| i * 8 + 7).collect();
    let profiles = [
        GlyphCoverage::of(&[255; 32]),
        GlyphCoverage::of(&[160; 32]),
        GlyphCoverage::of(&ramp),
    ];
    let words: [&[usize]; 4] = [&[0, 2], &[2, 0], &[0, 1, 2], &[1, 2]];
    let (mut grounds, mut capped, mut wrong) = (0_u64, 0_u64, 0_u64);
    for palette in &crate::rainbow_kitty::prism_glass::tables::SHIPPED {
        let (fg, page) = (palette.fg, palette.bg);
        for ink in [fg, 0x00C4_262E] {
            let mut tables = LevelTables::default();
            let judges: Vec<std::sync::Arc<GlyphJudge>> = profiles
                .iter()
                .filter_map(|p| GlyphJudge::new_in(&mut tables, p, ink, page, fg, true))
                .map(std::sync::Arc::new)
                .collect();
            assert_eq!(
                judges.len(),
                3,
                "fixture: every profile judged on #{page:06x}"
            );
            for word in words {
                let glyphs: Vec<&std::sync::Arc<GlyphJudge>> =
                    word.iter().map(|&i| &judges[i]).collect();
                let as_one = |mutant: WordMutant| {
                    MUTANT.with(|c| c.set(mutant));
                    let mut judge = WordJudge::default();
                    for glyph in &glyphs {
                        judge.add(glyph);
                    }
                    MUTANT.with(|c| c.set(WordMutant::None));
                    judge
                };
                let (judge, first) = (as_one(WordMutant::None), as_one(WordMutant::FirstGlyph));
                let mut kept = FxHashMap::default();
                for &glass in palette.glass.iter().step_by(16) {
                    for alpha in [255_u8, 160, 64, 12] {
                        let source = (aterm_render::premul_rgb(glass, alpha), alpha);
                        for a in 0..=255_u16 {
                            let (c, k) = scale_end(source.0, source.1, f32::from(a) / 255.0);
                            let ground = aterm_render::over_premul(page, c, k) & 0x00FF_FFFF;
                            let each = glyphs.iter().all(|glyph| glyph.keeps(ground));
                            assert_eq!(judge.keeps(ground), each, "#{ground:06x} on #{page:06x}");
                            wrong += u64::from(first.keeps(ground) != each);
                            grounds += 1;
                        }
                        let least_cap = glyphs.iter().map(|glyph| glyph.cap(source)).min().unwrap();
                        assert_eq!(
                            judge.cap(0, page, source, (0, 255), &mut kept),
                            least_cap,
                            "#{glass:06x} at {alpha}"
                        );
                        capped += u64::from(least_cap < 255);
                    }
                }
            }
        }
    }
    assert!(
        grounds > 100_000 && capped > 0,
        "fixture: {grounds} grounds, {capped} capped"
    );
    assert!(
        wrong > 0,
        "control: a word read as its first glyph keeps what a later glyph fails"
    );
}

/// A CAP READ UP TO A BOUND IS READ ON PAST IT ([`CapRead::To`]): a word asked for its cap over
/// a composite up to a bound under that cap, and then up to 255, takes the cap a word read whole
/// takes, on every shipped paper, for words of the synthetic profiles in the page's ink and an
/// app's. The equivalence test of the word pass reaches this read only twice. The negative
/// control takes the cap read up to the first bound as the cap past it, unread
/// ([`WordMutant::ReadOnSkipped`]), and publishes a cap over the word's.
#[test]
fn a_cap_read_up_to_a_bound_is_read_on_past_it() {
    let ramp: Vec<u8> = (0..32_u8).map(|i| i * 8 + 7).collect();
    let profiles = [
        GlyphCoverage::of(&[255; 32]),
        GlyphCoverage::of(&[160; 32]),
        GlyphCoverage::of(&ramp),
    ];
    let words: [&[usize]; 3] = [&[0, 2], &[0, 1, 2], &[1, 2]];
    let (mut read_on, mut over) = (0_u64, 0_u64);
    for palette in &crate::rainbow_kitty::prism_glass::tables::SHIPPED {
        let (fg, page) = (palette.fg, palette.bg);
        for ink in [fg, 0x00C4_262E] {
            let mut tables = LevelTables::default();
            let judges: Vec<std::sync::Arc<GlyphJudge>> = profiles
                .iter()
                .filter_map(|p| GlyphJudge::new_in(&mut tables, p, ink, page, fg, true))
                .map(std::sync::Arc::new)
                .collect();
            for word in words {
                let mut judge = WordJudge::default();
                for &i in word {
                    judge.add(&judges[i]);
                }
                for &glass in palette.glass.iter().step_by(16) {
                    for alpha in [255_u8, 160, 64, 12] {
                        let source = (aterm_render::premul_rgb(glass, alpha), alpha);
                        let whole = judge.cap(0, page, source, (0, 255), &mut FxHashMap::default());
                        let first = whole / 2;
                        if first == 0 || whole == 255 {
                            continue;
                        }
                        for mutant in [WordMutant::None, WordMutant::ReadOnSkipped] {
                            let mut glass_yield = GlassYield {
                                word: judge.clone(),
                                ..GlassYield::default()
                            };
                            MUTANT.with(|c| c.set(mutant));
                            let to = glass_yield.memo_word_cap(0, page, source, first);
                            let on = glass_yield.memo_word_cap(0, page, source, 255);
                            MUTANT.with(|c| c.set(WordMutant::None));
                            assert_eq!(to, first, "#{glass:06x} at {alpha} up to {first}");
                            if mutant == WordMutant::None {
                                assert_eq!(
                                    on, whole,
                                    "#{glass:06x} at {alpha} on #{page:06x}: read on past {first}"
                                );
                                read_on += 1;
                            } else {
                                over += u64::from(on > whole);
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(read_on > 0, "fixture: no cap read on past a bound");
    assert!(
        over > 0,
        "control: a cap read up to a bound taken as the cap past it published none over a word's"
    );
}

/// After-land R0: the yield reads a word's glyphs as one ([`WordJudge`]) and publishes
/// exactly what judging each glyph on its own did (`per_glyph_cap`, the reading before: every
/// glyph scanning every composite's grounds and reading every level of its own), the cells'
/// scales with it: on GitHub Light and Default Light, in the page's ink and app inks, over the
/// rasters of the renderer's default face ([`aterm_render::Renderer::from_system`]) at 1x and
/// 2x, under gradient quads and wedge ramps of the shipped spectrum and of three deep lights, at
/// full to faint alpha. The work is counted, not timed: past their margins the word reads fewer
/// levels than its glyphs read in turn by their margins (r25 K8-1's reading), and under a fifth
/// of what they read with none (46 thousand, 737 thousand and 481 million in Linux's bundled
/// JetBrains Mono); and reading each composite only up to the cap the word already took, fewer
/// than reading each to its own cap ([`WordMutant::Unbounded`], 59 thousand), which publishes
/// the same. One-colour glass, a pinned colour's, lays every word the same composites, so what
/// one word kept is what the next word reads, and a yield held across the frames reads its
/// memo: both publish the same. The negative controls read a word as its first glyph, take a
/// ground read once as kept whether it kept or not, and take a ground one word kept as kept for
/// the next ([`WordMutant`]): each publishes what the glyphs judged in turn do not.
///
/// The last control bites only where a word reads, past its margins, a ground another word read
/// otherwise past its own, which the first three lines hold in some faces and not in others.
/// The fourth is there to hold such words whatever the face: `oo` in six reds 3 to 25 levels
/// apart (#c4262e to #dd3f47), whose bars one glass reaches at neighbouring grounds, the gaps
/// many so that no one gap decides it. On the first three lines alone the control published
/// otherwise in none of the 104 frames in Linux's bundled JetBrains Mono, and in 3 in DejaVu
/// Sans Mono, the system face a build of aterm-effects alone finds there; with the fourth, in
/// 85 and 91, and in 76 to 100 in six other faces at hand (Linux arm64, 2026-11-11).
#[test]
fn a_word_judged_as_one_yields_as_its_glyphs_judged_in_turn() {
    use crate::rainbow_kitty::prism_glass::{GLASS_LEN, Palette};
    const TEXT: &[u8] = b"The quick brown fox jumps over\r\n\x1b[38;2;196;38;46mred\x1b[39m \
        words \x1b[38;2;5;80;174mblue\x1b[39m %@{} mm\r\nlazy dog 0123 ,;:\"' wwiill\r\n\
        \x1b[38;2;196;38;46moo\x1b[39m \x1b[38;2;199;41;49moo\x1b[39m \
        \x1b[38;2;203;45;53moo\x1b[39m \x1b[38;2;208;50;58moo\x1b[39m \
        \x1b[38;2;214;56;64moo\x1b[39m \x1b[38;2;221;63;71moo\x1b[39m";
    const DEEP: [u32; 3] = [0x0080_40C0, 0x00C0_6020, 0x0020_8080];
    let (mut frames, mut cut, mut differ) = (0, 0, [0; 3]);
    let (mut word_reads, mut glyph_reads, mut margin_reads) = (0_u64, 0_u64, 0_u64);
    let mut unbounded_reads = 0_u64;
    for (fg, page) in [
        (0x001F_2328_u32, 0x00FF_FFFF_u32),
        (0x002B_2D33, 0x00F6_F7F9),
    ] {
        let palette = Palette::shipped(fg, page).unwrap();
        let light = |i: usize| {
            if i % 7 == 6 {
                DEEP[i % 3]
            } else {
                palette.glass[i % GLASS_LEN]
            }
        };
        for px in [14.0, 28.0] {
            let mut renderer =
                aterm_render::Renderer::from_system(px, aterm_render::Theme::default()).unwrap();
            renderer.debug_block_on_lazy_fallbacks();
            let (cw, ch) = renderer.cell_size();
            let mut term = Terminal::new(4, 40);
            let rgb = |c: u32| {
                format!(
                    "rgb:{:02x}/{:02x}/{:02x}",
                    c >> 16,
                    (c >> 8) & 0xff,
                    c & 0xff
                )
            };
            term.process(format!("\x1b]10;{}\x07\x1b]11;{}\x07", rgb(fg), rgb(page)).as_bytes());
            term.process(TEXT);
            let mut base = term.cell_frame(4, 40);
            base.cursor_visible = false;
            let _ = renderer.render_input(&base);
            let ink = renderer.glyph_ink_for(&base);
            assert!(
                ink.for_frame(&base, (cw, ch)).is_some(),
                "stable actual glyph keys"
            );
            let (cw, ch) = (cw as i32, ch as i32);
            let mut held = GlassYield::default();
            // The first fourteen a gradient and a wedge, the rest one colour.
            for v in 0..26_usize {
                let at = Instant::now();
                let flat = v >= 14;
                let alpha = if flat {
                    [255_u8, 240, 220, 200, 180, 160, 128, 96, 64, 40, 24, 12][v - 14]
                } else {
                    [255_u8, 220, 160, 96, 40, 12, 255][v % 7]
                };
                let fade = [255_u8, 200, 120][v % 3];
                let mut quads = Vec::new();
                let mut ramps = GlowRamps::default();
                for row in 0..4_u16 {
                    let i0 = v * 37 + usize::from(row) * 61;
                    let i1 = i0 + 9 + v;
                    let x0 = ((v * 3 + usize::from(row) * 5) % 9) as i32 * cw;
                    let (x, y, w) = (
                        x0 as u16,
                        (i32::from(row) * ch) as u16,
                        (30 * cw - x0) as u16,
                    );
                    let end = |i: usize, a: u8| {
                        aterm_core::render::glow_end(aterm_render::premul_rgb(light(i), a), a)
                    };
                    if flat {
                        quads.push(GlowQuad::flat(
                            row,
                            x,
                            y,
                            w,
                            ch as u16,
                            aterm_render::premul_rgb(light(3), alpha),
                            alpha,
                        ));
                    } else if row == 1 && v % 2 == 1 {
                        // A wedge: every row its own ends.
                        ramps.open(quads.len(), row, (x, y, w), (end(i0, alpha), end(i1, fade)));
                        for k in 1..ch as usize {
                            ramps.extend_last(0, (end(i0 + k, alpha), end(i1 + k / 2, fade)));
                        }
                    } else {
                        let mut q = GlowQuad::flat(
                            row,
                            x,
                            y,
                            w,
                            ch as u16,
                            aterm_render::premul_rgb(light(i0), alpha),
                            alpha,
                        );
                        (q.color2, q.alpha2) = (aterm_render::premul_rgb(light(i1), fade), fade);
                        quads.push(q);
                    }
                }
                let run_on = |glass_yield: &mut GlassYield,
                              reading: Option<GlyphReading>,
                              mutant: WordMutant| {
                    GLYPH_READING.with(|c| c.set(reading));
                    MUTANT.with(|c| c.set(mutant));
                    let mut input = base.clone();
                    input.glow_under.clone_from(&quads);
                    input.glow_under_ramps.clone_from(&ramps);
                    let laid = GlassLaid::new(
                        at,
                        (quads.clone(), ramps.clone()),
                        (fg, page),
                        (0, 0, cw, ch),
                        None,
                    );
                    let from = level_reads();
                    let report =
                        glass_yield.apply_observed(&mut input, &laid, &[], Some(&ink), true);
                    let reads = level_reads() - from;
                    GLYPH_READING.with(|c| c.set(None));
                    MUTANT.with(|c| c.set(WordMutant::None));
                    (
                        (
                            input.glow_under,
                            input.glow_under_ramps,
                            glass_yield.scales.clone(),
                            report,
                        ),
                        reads,
                    )
                };
                let run = |reading: Option<GlyphReading>, mutant: WordMutant| {
                    run_on(&mut GlassYield::default(), reading, mutant)
                };
                let (word, reads) = run(None, WordMutant::None);
                let (glyphs, glyph_read) = run(Some(GlyphReading::Levels), WordMutant::None);
                let (kept, margin_read) = run(Some(GlyphReading::Margins), WordMutant::None);
                let (unbounded, unbounded_read) = run(None, WordMutant::Unbounded);
                assert_eq!(word, glyphs, "{px} px on #{page:06x}, variation {v}");
                assert_eq!(kept, glyphs, "{px} px on #{page:06x}, variation {v}");
                assert_eq!(unbounded, glyphs, "{px} px on #{page:06x}, variation {v}");
                unbounded_reads += unbounded_read;
                let (read_on, _) = run_on(&mut held, None, WordMutant::None);
                assert_eq!(
                    read_on, glyphs,
                    "{px} px on #{page:06x}, variation {v}, held"
                );
                word_reads += reads;
                glyph_reads += glyph_read;
                margin_reads += margin_read;
                for (n, mutant) in [
                    WordMutant::FirstGlyph,
                    WordMutant::KeptAlways,
                    WordMutant::KeptAcrossWords,
                ]
                .into_iter()
                .enumerate()
                {
                    differ[n] += usize::from(run(None, mutant).0 != glyphs);
                }
                cut += usize::from(word.3.cut());
                frames += 1;
            }
        }
    }
    assert!(
        frames >= 50 && cut * 2 > frames,
        "fixture: {cut} of {frames} frames cut"
    );
    assert!(
        word_reads * 5 < glyph_reads,
        "the word read {word_reads} levels where its glyphs read in turn read {glyph_reads}"
    );
    assert!(
        word_reads < margin_reads,
        "the word read {word_reads} levels where its glyphs read in turn by their margins read \
         {margin_reads}"
    );
    assert!(
        word_reads < unbounded_reads,
        "the word read {word_reads} levels where it read {unbounded_reads} with each composite \
         read to its own cap"
    );
    assert!(
        differ.iter().all(|&n| n > 0),
        "controls: a word read as its first glyph, a ground taken as kept once read, and one \
         taken as kept for the next word, yield otherwise in {differ:?} frames"
    );
}

/// Strict observation measures a theme glyph and keeps it plain for rule 5b: a safe one,
/// which its own raster keeps under the whole glass, is still yielded where rule 5b yields a
/// run, between a yielded word and the caret, the caret's cell with it. A theme glyph touching
/// an app's ink, or an animating one, is measured with that ink's word (rule 3 reads a run of
/// glyphs of any ink) and takes its cap. The negative control is a run of [`ORPHAN_CELLS`]
/// cells between two yielded words, ` M `, which is no orphan: the safe glyph keeps its glass.
/// On the foot rail, a word bounds no run, so this runs under [`with_whole_word_cut`] to read
/// every row one way.
#[test]
fn observed_safe_theme_ink_still_bridges_between_yielded_words_and_caret() {
    with_whole_word_cut(
        observed_safe_theme_ink_still_bridges_between_yielded_words_and_caret_whole,
    );
}

fn observed_safe_theme_ink_still_bridges_between_yielded_words_and_caret_whole() {
    let mut renderer = aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
        .expect("the bundled face");
    let red = |cols: std::ops::Range<u16>| {
        cols.map(|col| InkCell {
            row: 0,
            col,
            color: [196, 38, 46],
        })
        .collect::<Vec<_>>()
    };
    // (text, the caret, the word pass's animated cells, the theme glyph's column, the
    // run's columns, whether rule 5b yields it)
    for (text, caret, animated, safe_col, run, bridged) in [
        (
            b"\x1b[38;2;196;38;46mred\x1b[39mM\x1b[38;2;196;38;46mred".as_slice(),
            None,
            [red(0..3), red(4..7)].concat(),
            3,
            3..4,
            true,
        ),
        (
            b"\x1b[38;2;196;38;46mred\x1b[39m M".as_slice(),
            Some((0, 5)),
            Vec::new(),
            4,
            3..5,
            true,
        ),
        (
            b"\x1b[38;2;196;38;46mred\x1b[39m M \x1b[38;2;196;38;46mred".as_slice(),
            None,
            Vec::new(),
            4,
            3..6,
            false,
        ),
    ] {
        let mut input = page(text);
        input.ink = animated;
        let (cw, ch) = renderer.cell_size();
        let ink = renderer.glyph_ink_for(&input);
        let frame = ink
            .for_frame(&input, (cw, ch))
            .expect("actual current glyph evidence");
        let (_, coverage) = frame
            .coverage(0, safe_col)
            .expect("the theme glyph's actual raster");
        assert_eq!(
            GlyphJudge::new(coverage, FG, BG, FG, frame.corrected())
                .unwrap()
                .cap(GLASS),
            255,
            "the theme glyph is safe before rule5b bridges it"
        );
        let quad = GlowQuad::flat(0, 0, 0, (24 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
        input.glow_under = vec![quad];
        let glass = GlassLaid::new(
            Instant::now(),
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, cw as i32, ch as i32),
            caret,
        );
        let mut yield_ = GlassYield::default();
        assert!(
            yield_
                .apply_observed(&mut input, &glass, &[], Some(&ink), true)
                .cut()
        );
        assert_eq!(
            yield_.classes[safe_col],
            Class::Fg,
            "measurement preserves plain classification"
        );
        assert!(
            yield_.ks[run.start - 1] < 255
                && yield_.classes[run.start - 1].worded()
                && (caret.is_some() || yield_.ks[run.end] < 255),
            "premise: a yielded word bounds the run {run:?} on the left, and one or the caret \
             on the right: {:?}",
            &yield_.ks[..8]
        );
        let word = yield_.ks[run.start - 1];
        if bridged {
            assert!(run.len() < ORPHAN_CELLS, "premise: the run is an orphan");
            assert_eq!(
                yield_.ks[safe_col], word,
                "rule5b bridges the safe theme glyph in the run {run:?}"
            );
        } else {
            assert_eq!(run.len(), ORPHAN_CELLS, "premise: the run is no orphan");
            assert_eq!(
                &yield_.ks[run.clone()],
                [255; ORPHAN_CELLS],
                "control: a run of ORPHAN_CELLS keeps its glass, the safe glyph's too"
            );
        }
        if let Some((_, caret_col)) = caret {
            assert_eq!(
                yield_.ks[usize::from(caret_col)],
                word,
                "the orphan's caret yields too"
            );
        }
    }
}

/// §4 rule 3 (after-land A7, R2's "one run, one hue phase, not sticker-outlined"): a yielded
/// word takes its cap flat across its cells, so nothing outlines its letters, and the glass runs
/// on whole under it on the foot rail, the rows past the ring of the line's feet, and through
/// the blanks there. A descender is read on the rail as any letter: the cap within
/// [`LETTER_RING_PX`] of its ink, and past that the rail goes on. Negative controls:
/// [`LawMutant::RailFromTop`] lays the rail from the cells' top, every letter ringed, and
/// [`with_whole_word_cut`] lays the cap across the whole cells, and the run breaks at the word.
#[test]
fn a_yielded_word_lies_flat_at_its_cap_and_the_glass_runs_on_under_it() {
    let mut renderer = aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
        .expect("the bundled face");
    // `gap` in the red, a descender either side of a letter that stops at the line's feet.
    let text = b"ab \x1b[38;2;196;38;46mgap\x1b[39m cd".as_slice();
    let (cw, ch) = renderer.cell_size();
    let (cwi, chi) = (cw as i32, ch as i32);
    let foot = rail_from(&mut renderer, &page(text));
    assert!(
        foot < chi,
        "premise: the cell holds a rail: {foot} of {chi}"
    );
    let mut lay = || {
        let mut input = page(text);
        let ink = renderer.glyph_ink_for(&input);
        let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
        input.glow_under = vec![quad];
        let glass = GlassLaid::new(
            Instant::now(),
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, cwi, chi),
            None,
        );
        let mut glass_yield = GlassYield::default();
        let report = glass_yield.apply_observed(&mut input, &glass, &[], Some(&ink), true);
        assert!(report.cut(), "premise: the red word yields");
        no_coloured_zero(&input);
        let k = glass_yield.ks[3];
        (input, ink, k)
    };
    let (laid, ink, k) = lay();
    assert!(k < 255, "premise: the word takes a cap");
    let capped = u32::from(scale_end(GLASS.0, GLASS.1, f32::from(k) / 255.0).1);
    let whole = u32::from(GLASS.1);
    let flat = |input: &RenderInput| {
        (0..foot).all(|y| (3 * cwi..6 * cwi).all(|x| alpha_at_y(input, x, y) == capped))
    };
    assert!(
        flat(&laid),
        "the word's cells lie flat at its cap above the rail"
    );
    // What each letter inks on the rail is at the cap, its ring with it.
    let frame = ink
        .for_frame(&page(text), (cw, ch))
        .expect("current evidence");
    let ring = LETTER_RING_PX as i32;
    let mut rail_ink = 0;
    for c in 3..6_usize {
        let cx = c as i32 * cwi;
        let rows = frame.ink_rows(0, c).expect("each letter's raster");
        for &[y, a, z] in rows.iter().filter(|r| i32::from(r[0]) >= foot) {
            for x in cx + i32::from(a) - ring..cx + i32::from(z) + ring {
                rail_ink += 1;
                assert_eq!(alpha_at_y(&laid, x, i32::from(y)), capped, "({x}, {y})");
            }
        }
    }
    assert!(rail_ink > 0, "premise: the g and the p reach the rail");
    // The run: the rail's first row is whole across the line, but where a descender skips it.
    let runs_on = |input: &RenderInput| {
        // The `a` stops at the line's feet: the rail under it is the source's own, and the
        // blanks either side are too.
        let under_a = 4 * cwi + cwi / 2;
        [2 * cwi + cwi / 2, under_a, 6 * cwi + cwi / 2]
            .iter()
            .all(|&x| (foot..chi).all(|y| alpha_at_y(input, x, y) == whole))
    };
    assert!(runs_on(&laid), "the rail runs on whole under the word");
    let (ringed, _, _) = with_law_mutant(LawMutant::RailFromTop, &mut lay);
    assert!(
        !flat(&ringed),
        "control: a rail from the cells' top leaves every letter in a ring"
    );
    let (cut, _, _) = with_whole_word_cut(&mut lay);
    assert!(
        !runs_on(&cut),
        "control: cut across its whole cells, the word breaks the run"
    );
}

/// §4 rule 3: a word whose letters all stop at the line's feet fields nothing on the rail, so
/// the rail is one piece of glass under it. Negative control ([`LawMutant::RailFromTop`]): read
/// from the cells' top, each letter is fielded.
#[test]
fn a_word_with_no_descender_fields_nothing() {
    let mut renderer = aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
        .expect("the bundled face");
    let text = b"\x1b[38;2;196;38;46mwar\x1b[39m".as_slice();
    let (cw, ch) = renderer.cell_size();
    let mut letters = || {
        let mut input = page(text);
        let ink = renderer.glyph_ink_for(&input);
        let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
        input.glow_under = vec![quad];
        let glass = GlassLaid::new(
            Instant::now(),
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, cw as i32, ch as i32),
            None,
        );
        let mut glass_yield = GlassYield::default();
        let report = glass_yield.apply_observed(&mut input, &glass, &[], Some(&ink), true);
        assert!(report.cut(), "premise: the red word yields");
        (glass_yield.letters.len(), glass_yield.fields.len())
    };
    assert_eq!(letters(), (0, 0), "no letter of the word meets the rail");
    assert_eq!(
        with_law_mutant(LawMutant::RailFromTop, &mut letters),
        (3, 1),
        "control: from the cells' top, each letter meets it"
    );
}

/// An app's ink keeps its letter bar on the strict path: 4.6; where it reads under 4.6 on the
/// bare page and AA or more, AA with a level in hand; under AA, 97% of what it reads there; and
/// no more. There are glasses a grey word reads over 4.6 and under 5.25 on, and it keeps them
/// whole. Negative control ([`LawMutant::FlatBar`]): at the flat 5.25 it is cut.
#[test]
fn an_app_ink_keeps_its_letter_bar_and_no_more() {
    let mut renderer = aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
        .expect("the bundled face");
    let (cw, ch) = renderer.cell_size();
    let text = b"\x1b[38;2;71;77;87mgrey\x1b[39m".as_slice();
    // A grey glass, which darkens the page by its alpha.
    let mut lay = |alpha: u8| {
        let mut input = page(text);
        let ink = renderer.glyph_ink_for(&input);
        let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, 0, alpha);
        input.glow_under = vec![quad];
        let glass = GlassLaid::new(
            Instant::now(),
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, cw as i32, ch as i32),
            None,
        );
        let mut glass_yield = GlassYield::default();
        let _ = glass_yield.apply_observed(&mut input, &glass, &[], Some(&ink), true);
        glass_yield.ks[0]
    };
    let glasses = (1..=120_u8)
        .filter(|&alpha| {
            lay(alpha) == 255 && with_law_mutant(LawMutant::FlatBar, || lay(alpha)) < 255
        })
        .count();
    assert!(
        glasses > 0,
        "no glass the word keeps at its letter bar and loses at a flat 5.25"
    );
    let whole_at_flat = with_law_mutant(LawMutant::FlatBar, || {
        (1..=120).filter(|&a| lay(a) == 255).count()
    });
    let whole_at_letter = (1..=120).filter(|&a| lay(a) == 255).count();
    assert!(
        whole_at_letter > whole_at_flat,
        "control: {whole_at_letter} whole glasses at its letter bar, {whole_at_flat} at 5.25"
    );
}

/// The caps of `wow` in `ink` (`r;g;b`) on Default Light under a grey glass at each alpha from
/// 1 to 60, which darkens the page by it, on the strict path at 24 px.
fn grey_glass_caps(ink: &str) -> Vec<u8> {
    grey_glass_word_caps("wow", ink)
}

/// [`grey_glass_caps`] of `word`.
fn grey_glass_word_caps(word: &str, ink: &str) -> Vec<u8> {
    let mut renderer = aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
        .expect("the bundled face");
    let (cw, ch) = renderer.cell_size();
    let text = format!("\x1b[38;2;{ink}m{word}\x1b[39m");
    (1..=60_u8)
        .map(|alpha| {
            let mut input = page(text.as_bytes());
            let glyphs = renderer.glyph_ink_for(&input);
            let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, 0, alpha);
            input.glow_under = vec![quad];
            let glass = GlassLaid::new(
                Instant::now(),
                (vec![quad], GlowRamps::default()),
                (FG, BG),
                (0, 0, cw as i32, ch as i32),
                None,
            );
            let mut glass_yield = GlassYield::default();
            let _ = glass_yield.apply_observed(&mut input, &glass, &[], Some(&glyphs), true);
            glass_yield.ks[0]
        })
        .collect()
}

/// After-land round 3 (W1): each solid level of an app's ink keeps the letter bar of its own
/// reading. The judges read a letter by the lower median of its solid pixels in a row, and
/// `w` in #b43fb4 on Default Light, 4.545 at its full ink, reads AA by them while its
/// antialiased solid levels read under it: held by its dimmest level's bar, a word's wash
/// lowered its full ink to that level's share, and the coloured line's `w` read 4.40 of its
/// own 4.50. Its full ink reads under AA's level in hand, so it takes no glass that lowers it.
/// Negative control ([`with_least_level_bar`]): every level at its dimmest level's bar, the
/// word takes a wash. The word is the face's [`uncored_word`]: a glyph whose core holds its
/// word from every glass holds it under the control too, which could not bite through it.
#[test]
fn each_solid_level_of_an_app_ink_keeps_the_bar_of_its_own_reading() {
    let word = uncored_word("180;63;180");
    let caps = grey_glass_word_caps(&word, "180;63;180");
    assert!(
        caps.iter().all(|&k| k == 0),
        "`{word}` in #b43fb4 under a grey glass takes {caps:?}"
    );
    let least = with_least_level_bar(|| grey_glass_word_caps(&word, "180;63;180"));
    assert!(
        least.iter().any(|&k| k > 0),
        "control: at its dimmest level's bar `{word}` in #b43fb4 takes no wash ({least:?})"
    );
}

/// The first of `wow`, `xxx` and each lowercase letter three times over, in `ink` (`r;g;b`) on
/// Default Light at 24 px, no glyph of which holds the word from every glass by its core alone:
/// a core whose letter bar lies over its own reading, as one reading AA as the judges read it
/// and under AA's level in hand does, keeps the word whole whatever its solid levels' bars.
/// Under macOS's SF Mono, where amendment 65's control was measured, #b43fb4's is `wow`: that
/// control's caps, [255, 255, 255, 223, 178, 148, ...], were `wow`'s, and a core holding it
/// would have left them 0. Under Linux's bundled JetBrains Mono (14 x 28 px cells) `o`'s core
/// (coverage 253) reads 4.4976, `x`'s (248) 4.32, and the word is `xxx`, whose caps under the
/// control are those (Linux arm64, 2026-11-07). Windows' Iosevka Term NL is hinted, and most of
/// its cores are the whole ink, which #b43fb4 reads at 4.5447, at AA as the judges read it: in
/// its 14 x 28 px cells `w`'s core (254) reads 4.5212 and `x`'s (255) 4.5447, so neither word
/// is one, and the first letter whose core reads under AA is `o` (252, 4.4175): the word is
/// `ooo` (Windows x64, 2026-11-11).
fn uncored_word(ink: &str) -> String {
    use crate::rainbow_kitty::prism_glass::colour::{contrast, luminance};
    let mut renderer = aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
        .expect("the bundled face");
    let (cw, ch) = renderer.cell_size();
    let rgb = ink
        .split(';')
        .map(|c| c.parse::<u32>().expect("a channel"))
        .fold(0, |rgb, c| rgb << 8 | c);
    ["wow".to_string(), "xxx".to_string()]
        .into_iter()
        .chain(('a'..='z').map(|c| c.to_string().repeat(3)))
        .find(|word| {
            let input = page(format!("\x1b[38;2;{ink}m{word}\x1b[39m").as_bytes());
            let glyphs = renderer.glyph_ink_for(&input);
            let frame = glyphs
                .for_frame(&input, (cw, ch))
                .expect("the frame's glyph evidence");
            let corrected = frame.corrected();
            (0..word.len()).all(|col| {
                let (_, coverage) = frame.coverage(0, col).expect("the glyph's raster");
                let core = coverage
                    .core_level(luminance(rgb) as f32, luminance(BG) as f32, corrected)
                    .expect("an inked glyph");
                let read =
                    contrast(aterm_render::blend_text(BG, rgb, BG, core, corrected), BG) as f32;
                crate::cursor_glow::letter_bar(read, false) <= read
            })
        })
        .expect("a word no core holds")
}

/// After-land round 3: a letter the judges read at AA keeps AA. They give a reading to two
/// decimals, so #b441b4, 4.4976 on Default Light, reads 4.50 there, and held at the share a
/// letter under AA keeps, it took a wash that read 4.47 (the flood's `o` in #c1479f on GitHub
/// Light, 4.4973). It takes no glass that lowers it. Negative control
/// ([`with_aa_unrounded`](crate::cursor_glow::with_aa_unrounded)): AA held from 4.5 itself,
/// the word takes a wash.
#[test]
fn a_letter_the_judges_read_at_aa_keeps_aa() {
    let caps = grey_glass_caps("180;65;180");
    assert!(
        caps.iter().all(|&k| k == 0),
        "#b441b4 under a grey glass takes {caps:?}"
    );
    let unrounded = crate::cursor_glow::with_aa_unrounded(|| grey_glass_caps("180;65;180"));
    assert!(
        unrounded.iter().any(|&k| k > 0),
        "control: with AA held from 4.5 #b441b4 takes no wash ({unrounded:?})"
    );
}

/// The lay of one pixel row a letter's field scales: the whole pixels either side stay one
/// span each, and the letter's ring, whose scale the yield's law sets and the cap judged column
/// by column, a span a pixel; the feather between, which only eases back to whole, one span
/// along the line through its first and last pixels. A run the ring takes whole away lays one
/// span of nothing. The control ([`LayMutant::PixelSpans`]) lays a span a feather pixel.
#[test]
fn a_letter_s_ring_lays_each_pixel_and_its_feather_one_span() {
    const S: [f32; 15] = [
        1.0, 1.0, 0.9, 0.85, 0.82, 0.8, 0.8, 0.8, 0.85, 0.95, 1.0, 0.5, 0.0, 0.0, 0.5,
    ];
    const R: [bool; 15] = [
        false, false, false, false, false, true, true, true, false, false, false, false, true,
        true, false,
    ];
    let spans = || {
        let mut y = GlassYield {
            scales: vec![Scale::WHOLE; 2],
            ..GlassYield::default()
        };
        let row = FieldRow {
            s: &S,
            ring: &R,
            x0: 0,
        };
        assert!(y.cut_spans((0, 2), (0, 0, 10, 20), (0, 15), Some(row)));
        y.spans
    };
    let laid = spans();
    let want = [
        (0, 2, 1.0, 1.0),
        (2, 5, 0.92, 0.8),
        (5, 6, 0.8, 0.8),
        (6, 7, 0.8, 0.8),
        (7, 8, 0.8, 0.8),
        (8, 10, 0.8, 1.0),
        (10, 11, 1.0, 1.0),
        (11, 12, 0.5, 0.5),
        (12, 14, 0.0, 0.0),
        (14, 15, 0.5, 0.5),
    ];
    assert_eq!(laid.len(), want.len(), "{laid:?}");
    for (got, want) in laid.iter().zip(want) {
        assert!(
            got.0 == want.0
                && got.1 == want.1
                && (got.2 - want.2).abs() < 1e-5
                && (got.3 - want.3).abs() < 1e-5,
            "{got:?} against {want:?}"
        );
    }
    let control = with_lay_mutant(LayMutant::PixelSpans, spans);
    assert!(
        control.len() >= 13 && control.iter().all(|&(a, b, ..)| b - a <= 2),
        "control: a span a feather pixel: {control:?}"
    );
}

/// The red word over a whole glass at 24 px, its descenders' feathers on the rail one span
/// each: fewer lights than a span a pixel ([`LayMutant::PixelSpans`]), on the same whole pixels
/// and the same ring, and no feather pixel far from what a span a pixel laid it.
#[test]
fn a_word_s_feather_spans_spare_lights_and_keep_its_ring() {
    let mut renderer = aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
        .expect("the bundled face");
    let text = b"ab \x1b[38;2;196;38;46mgypq\x1b[39m cd".as_slice();
    let (cw, ch) = renderer.cell_size();
    let (cwi, chi) = (cw as i32, ch as i32);
    let mut lay = || {
        let mut input = page(text);
        let ink = renderer.glyph_ink_for(&input);
        let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
        input.glow_under = vec![quad];
        let glass = GlassLaid::new(
            Instant::now(),
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, cwi, chi),
            None,
        );
        let mut glass_yield = GlassYield::default();
        let report = glass_yield.apply_observed(&mut input, &glass, &[], Some(&ink), true);
        assert!(report.cut(), "premise: the red word yields");
        no_coloured_zero(&input);
        (input, glass_yield)
    };
    let (spans, shaped) = lay();
    let (pixels, _) = with_lay_mutant(LayMutant::PixelSpans, &mut lay);
    assert!(
        spans.glow_under.len() < pixels.glow_under.len(),
        "{} lights against {} a span a pixel",
        spans.glow_under.len(),
        pixels.glow_under.len()
    );
    let whole = u32::from(GLASS.1);
    let field = shaped.fields.first().expect("the word's field");
    let mut feather = 0;
    for y in 0..chi {
        for x in 0..10 * cwi {
            let (a, b) = (alpha_at_y(&spans, x, y), alpha_at_y(&pixels, x, y));
            let row = field.row_at(y);
            if row.is_some_and(|row| row.eases(x)) {
                feather += 1;
                assert!(
                    a.abs_diff(b) as f32 <= FEATHER_SPAN_SLACK * whole as f32 + 2.0,
                    "({x}, {y}) in the feather: {a} against {b}"
                );
            } else {
                assert_eq!(a, b, "({x}, {y}) off the feather");
            }
        }
    }
    assert!(feather > 0, "premise: the word has a feather");
}

/// A piece of glass on the rail between two descenders, its pixel rows inside the box the row's
/// field spans but scaled nowhere, stays one piece, and a word's letters no glass comes near are
/// not fielded at all. The controls cut every row of the field's box ([`LayMutant::BandRows`])
/// and field every letter of the word ([`LayMutant::AllLetters`]).
#[test]
fn glass_no_letter_reaches_is_laid_whole_and_fields_nothing() {
    let mut renderer = aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
        .expect("the bundled face");
    // Two descenders in the red, with glass over them and between them, on the rail.
    let red = "\x1b[38;2;196;38;46m";
    let text = format!("{red}g\x1b[39m{}{red}p\x1b[39m", " ".repeat(9));
    let (cw, ch) = renderer.cell_size();
    let (cwi, chi) = (cw as i32, ch as i32);
    let foot = rail_from(&mut renderer, &page(text.as_bytes()));
    let between = GlowQuad::flat(
        0,
        (4 * cw) as u16,
        foot as u16,
        (3 * cw) as u16,
        (chi - foot) as u16,
        GLASS.0,
        GLASS.1,
    );
    let mut lay = || {
        let mut input = page(text.as_bytes());
        let ink = renderer.glyph_ink_for(&input);
        let over = |col: usize| {
            GlowQuad::flat(
                0,
                (col * cw) as u16,
                0,
                cw as u16,
                ch as u16,
                GLASS.0,
                GLASS.1,
            )
        };
        let quads = vec![over(0), between, over(10)];
        input.glow_under = quads.clone();
        let glass = GlassLaid::new(
            Instant::now(),
            (quads, GlowRamps::default()),
            (FG, BG),
            (0, 0, cwi, chi),
            None,
        );
        let mut glass_yield = GlassYield::default();
        let report = glass_yield.apply_observed(&mut input, &glass, &[], Some(&ink), true);
        assert!(report.cut(), "premise: the red letters yield");
        (input, glass_yield)
    };
    let pieces = |input: &RenderInput| {
        input
            .glow_under
            .iter()
            .filter(|q| q.x == between.x && q.w == between.w)
            .count()
    };
    let (input, shaped) = lay();
    assert_eq!(
        pieces(&input),
        1,
        "the glass between the letters stays whole"
    );
    assert!(
        input.glow_under.contains(&between),
        "the piece between the letters is the source's own"
    );
    assert_eq!(shaped.letters.len(), 2, "premise: both letters yield");
    let (control, _) = with_lay_mutant(LayMutant::BandRows, &mut lay);
    assert!(
        pieces(&control) > 1,
        "control: every row of the field's box cut, the piece is cut too"
    );
    // A twelve-letter word with glass over its first two cells.
    let mut fielded = || {
        let mut input = page(format!("{red}gpqgpqgpqgpq\x1b[39m").as_bytes());
        let ink = renderer.glyph_ink_for(&input);
        let quad = GlowQuad::flat(0, 0, 0, (2 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
        input.glow_under = vec![quad];
        let glass = GlassLaid::new(
            Instant::now(),
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, cwi, chi),
            None,
        );
        let mut glass_yield = GlassYield::default();
        let report = glass_yield.apply_observed(&mut input, &glass, &[], Some(&ink), true);
        assert!(report.cut(), "premise: the red word yields");
        glass_yield.letters.len()
    };
    let near = fielded();
    assert!(near <= 3, "{near} letters of the word fielded");
    assert_eq!(
        with_lay_mutant(LayMutant::AllLetters, &mut fielded),
        12,
        "control: every letter of the word fielded"
    );
}

/// A letter's ring and feather are read off its glyph once ([`LetterStamp`]): a word's repeated
/// letters share one stamp, a frame after it reads none anew, and what it lays is what a yield
/// with nothing held lays.
#[test]
fn a_glyph_s_stamp_is_read_once() {
    let mut renderer = aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
        .expect("the bundled face");
    let text = b"\x1b[38;2;196;38;46mgpq gpq\x1b[39m".as_slice();
    let (cw, ch) = renderer.cell_size();
    let mut input = page(text);
    let ink = renderer.glyph_ink_for(&input);
    let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
    let glass = GlassLaid::new(
        Instant::now(),
        (vec![quad], GlowRamps::default()),
        (FG, BG),
        (0, 0, cw as i32, ch as i32),
        None,
    );
    let mut held = GlassYield::default();
    let frame = |y: &mut GlassYield, input: &mut RenderInput| {
        input.glow_under = vec![quad];
        input.glow_under_ramps.clear();
        assert!(y.apply_observed(input, &glass, &[], Some(&ink), true).cut());
        input.glow_under.clone()
    };
    let first = frame(&mut held, &mut input);
    assert_eq!(held.letters.len(), 6, "premise: six letters yield");
    assert_eq!(held.letter_stamps.len(), 3, "three glyphs, three stamps");
    let second = frame(&mut held, &mut input);
    assert_eq!(held.letter_stamps.len(), 3, "a later frame reads none anew");
    assert_eq!(first, second);
    assert_eq!(first, frame(&mut GlassYield::default(), &mut input));
}

/// A word no piece of the glass reaches takes `255` unjudged: on a typed line the glass reaches
/// the few words by the caret, and judging the rest each frame was a cost the typing path
/// paid for nothing. Here the glass lies under `gpq` alone and `xyz` builds no glyph judge.
#[test]
fn a_word_the_glass_does_not_reach_is_not_judged() {
    let mut renderer = aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
        .expect("the bundled face");
    let text = b"\x1b[38;2;196;38;46mgpq xyz\x1b[39m".as_slice();
    let (cw, ch) = renderer.cell_size();
    let mut input = page(text);
    let ink = renderer.glyph_ink_for(&input);
    let quad = GlowQuad::flat(0, 0, 0, (3 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
    let glass = GlassLaid::new(
        Instant::now(),
        (vec![quad], GlowRamps::default()),
        (FG, BG),
        (0, 0, cw as i32, ch as i32),
        None,
    );
    let mut y = GlassYield::default();
    input.glow_under = vec![quad];
    assert!(
        y.apply_observed(&mut input, &glass, &[], Some(&ink), true)
            .cut()
    );
    assert_eq!(y.judges.len(), 3, "the glyphs of `gpq`, and none of `xyz`");
}

/// The frame's lights keep to [`YIELD_LIGHT_BUDGET`], which nothing else bounds past the step
/// that laid the glass: a frame whose railed lay would run past it, here a flood of descenders,
/// takes whole words, every yielded word at its cap down to the foot of its cells, and stays
/// with them for [`WHOLE_WORDS_HOLD`], so the glass at its words does not flip from frame to
/// frame at the bound; it publishes the rail again once a frame past the hold fits. The control
/// ([`with_no_light_budget`]) lays them all.
#[test]
fn a_flood_past_the_light_budget_takes_whole_words_and_holds_them() {
    const COLS: u16 = 60;
    const ROWS: u16 = 12;
    let mut renderer = aterm_render::Renderer::from_system(60.0, aterm_render::Theme::default())
        .expect("the bundled face");
    renderer.debug_block_on_lazy_fallbacks();
    let (cw, ch) = renderer.cell_size();
    let (cwi, chi) = (cw as i32, ch as i32);
    let mut term = Terminal::new(ROWS, COLS);
    term.process(b"\x1b]10;rgb:2b/2d/33\x07\x1b]11;rgb:f6/f7/f9\x07\x1b[38;2;196;38;46m");
    for _ in 0..ROWS {
        term.process("gypqgypqgypqgypqgypqgypqgypqgypqgypqgypqgypqgypqgypqgypqgypq".as_bytes());
    }
    let mut base = term.cell_frame(usize::from(ROWS), usize::from(COLS));
    base.cursor_visible = false;
    let _ = renderer.render_input(&base);
    let ink = renderer.glyph_ink_for(&base);
    let quads_over = |rows: u16, cols: u16| -> Vec<GlowQuad> {
        (0..rows)
            .map(|row| {
                GlowQuad::flat(
                    row,
                    0,
                    (i32::from(row) * chi) as u16,
                    (i32::from(cols) * cwi) as u16,
                    ch as u16,
                    GLASS.0,
                    GLASS.1,
                )
            })
            .collect()
    };
    let lay = |y: &mut GlassYield, quads: &[GlowQuad], at: Instant| {
        let mut input = base.clone();
        input.glow_under = quads.to_vec();
        let glass = GlassLaid::new(
            at,
            (quads.to_vec(), GlowRamps::default()),
            (FG, BG),
            (0, 0, cwi, chi),
            None,
        );
        let report = y.apply_observed(&mut input, &glass, &[], Some(&ink), true);
        assert!(report.cut(), "premise: the red letters yield");
        input
    };
    let lights = |input: &RenderInput| input.glow_under_ramps.laid(input.glow_under.len());
    let flood = quads_over(ROWS, COLS);
    let t0 = Instant::now();
    let mut held = GlassYield::default();
    let over = lay(&mut held, &flood, t0);
    assert!(
        lights(&over) <= YIELD_LIGHT_BUDGET,
        "the flood lays {} lights",
        lights(&over)
    );
    assert!(
        alpha_at_y(&over, 3 * cwi, chi - 1) < u32::from(GLASS.1) / 4,
        "a whole word's cell lies at its cap, the foot of its cell too"
    );
    let uncapped = with_no_light_budget(|| lay(&mut GlassYield::default(), &flood, t0));
    assert!(
        lights(&uncapped) > YIELD_LIGHT_BUDGET,
        "control: railed, the flood lays {} lights",
        lights(&uncapped)
    );
    // A short glass, which fits: railed alone, whole words while the hold lasts.
    let short = quads_over(1, 4);
    let free = lay(&mut GlassYield::default(), &short, t0);
    assert!(lights(&free) <= YIELD_LIGHT_BUDGET);
    let in_hold = lay(
        &mut held,
        &short,
        t0 + std::time::Duration::from_millis(500),
    );
    assert_ne!(
        in_hold.glow_under, free.glow_under,
        "the hold holds whole words"
    );
    let past = lay(
        &mut held,
        &short,
        t0 + std::time::Duration::from_millis(1_500),
    );
    assert_eq!(past.glow_under, free.glow_under, "past the hold, the rail");
}

/// §4 rule 1, sprite-shaped (after-land R2, the word icons): an under-text sprite (a `fox`'s
/// peeking face, its chin in the top of the word's row, its box five cells wide over the word
/// and the blanks either side) takes the glass only where its art draws, and the glass runs on
/// whole through its box's clear margin, so the ribbon stays one run under a decorated word.
/// Negative control: [`with_sprite_rect_cut`] takes every cell the box reaches, as 1.12 did,
/// and the run breaks across the word.
#[test]
fn an_under_text_sprite_yields_where_it_draws_and_the_glass_runs_on_through_its_box() {
    use aterm_core::render::{FxArt, FxTexels, PxRect};
    // `fox` in the theme's ink on row 1, cols 3..6; the face's box over cols 2..7, from
    // 12 px above the row to 8 px into it, its art drawing cols 15..35 of its 50 down to
    // its 14th row: a chin 6 px into the row, over x 35..55.
    let text = b"xy\r\nab fox cd".as_slice();
    let (w, h) = (5 * CW, 20);
    let (sx, sy) = (2 * CW, CH - 12);
    let (dx0, dx1, drows) = (15, 35, 18);
    let mut texels = vec![0u8; (w * h * 4) as usize];
    for r in 0..drows {
        for c in dx0..dx1 {
            texels[((r * w + c) * 4 + 3) as usize] = 255;
        }
    }
    let face = FreeSprite {
        x: sx,
        y: sy,
        w: w as u16,
        h: h as u16,
        art: std::sync::Arc::new(
            FxArt::new(w as u16, h as u16, FxTexels::Rgba8Straight, texels).expect("a 50x20 art"),
        ),
        clip: PxRect::EVERYWHERE,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z: FreeZ::UnderText,
    };
    let lay = |rects: bool| {
        let mut input = page(text);
        let glass = glass_over(&mut input, 1, 10, Instant::now(), None);
        let mut apply =
            || GlassYield::default().apply(&mut input, &glass, std::slice::from_ref(&face));
        let report = if rects {
            with_sprite_rect_cut(apply)
        } else {
            apply()
        };
        assert!(report.cut(), "premise: the face's chin lies on the glass");
        no_coloured_zero(&input);
        input
    };
    let whole = u32::from(GLASS.1);
    let (row_top, chin) = (CH, sy + drows);
    assert!(chin > row_top, "premise: the chin reaches into the row");
    let shaped = lay(false);
    // Every pixel the face draws on the row lies on no glass.
    for y in row_top..chin {
        for x in sx + dx0..sx + dx1 {
            assert_eq!(alpha_at_y(&shaped, x, y), 0, "the chin at ({x}, {y})");
        }
    }
    // The glass runs on whole everywhere else on the row: beside the chin and below it,
    // through the box's clear margin and under the word.
    let runs_on = |input: &RenderInput| {
        (row_top..2 * CH).all(|y| {
            (0..10 * CW).all(|x| {
                let drawn = (row_top..chin).contains(&y) && (sx + dx0..sx + dx1).contains(&x);
                drawn || alpha_at_y(input, x, y) == whole
            })
        })
    };
    assert!(
        runs_on(&shaped),
        "the glass runs on whole through the face's box and under its word"
    );
    assert!(
        !runs_on(&lay(true)),
        "control: cut across every cell its box reaches, the face breaks the run"
    );
}

/// §4 rule 3 for a wide glyph (after-land F2): a yielded CJK word lies flat at its cap across
/// both cells of each glyph, and on the rail each glyph is read over both its cells (the
/// frame's evidence of a wide glyph is its lead's): at the cap within its ring, and whole past
/// that, so the ribbon stays one run across the word. Negative control:
/// [`aterm_render::glyph_ink::with_narrow_evidence_only`] holds no evidence for a wide glyph,
/// as before, and the word's cells are cut whole.
#[test]
fn a_yielded_wide_word_lies_flat_and_the_glass_runs_on_under_it() {
    // `你好` in the red the measured caps cut, plain words either side.
    let text = "ab \x1b[38;2;196;38;46m\u{4f60}\u{597d}\x1b[39m cd".as_bytes();
    let leads = [3_usize, 5];
    let lay = |narrow: bool| {
        let mut renderer =
            aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
                .expect("the bundled face");
        renderer.debug_block_on_lazy_fallbacks();
        let (cw, ch) = renderer.cell_size();
        let mut input = page(text);
        let ink = if narrow {
            aterm_render::glyph_ink::with_narrow_evidence_only(|| renderer.glyph_ink_for(&input))
        } else {
            renderer.glyph_ink_for(&input)
        };
        let foot = rail_of(&ink);
        let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
        input.glow_under = vec![quad];
        let glass = GlassLaid::new(
            Instant::now(),
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, cw as i32, ch as i32),
            None,
        );
        let mut glass_yield = GlassYield::default();
        let report = glass_yield.apply_observed(&mut input, &glass, &[], Some(&ink), true);
        assert!(report.cut(), "premise: the red word yields");
        no_coloured_zero(&input);
        let k = glass_yield.ks[3];
        let rows: Vec<(i32, Vec<[i16; 3]>)> = if narrow {
            Vec::new()
        } else {
            let frame = ink
                .for_frame(&page(text), (cw, ch))
                .expect("current evidence");
            leads
                .iter()
                .map(|&c| {
                    let rows = frame.ink_rows(0, c).expect("each wide glyph's raster");
                    (c as i32 * cw as i32, rows.to_vec())
                })
                .collect()
        };
        (input, rows, (cw as i32, ch as i32), foot, k)
    };
    let (shaped, letters, (cw, ch), foot, k) = lay(false);
    assert!(
        letters
            .iter()
            .all(|(_, rows)| rows.iter().any(|&[_, _, z]| i32::from(z) > cw)),
        "premise: each glyph inks its right half"
    );
    let capped = u32::from(scale_end(GLASS.0, GLASS.1, f32::from(k) / 255.0).1);
    assert!(
        (0..foot).all(|y| (3 * cw..7 * cw).all(|x| alpha_at_y(&shaped, x, y) == capped)),
        "the word's cells lie flat at its cap above the rail, both halves of each glyph"
    );
    // What each glyph inks on the rail is at the cap, its ring with it, right half too.
    let ring = LETTER_RING_PX as i32;
    assert!(
        letters.iter().all(|(cx, rows)| {
            rows.iter()
                .filter(|r| i32::from(r[0]) >= foot)
                .all(|&[y, a, z]| {
                    (cx + i32::from(a) - ring..cx + i32::from(z) + ring)
                        .all(|x| alpha_at_y(&shaped, x, i32::from(y)) == capped)
                })
        }),
        "what a glyph lays on the rail lies at the word's cap"
    );
    let whole = u32::from(GLASS.1);
    let runs_on = |input: &RenderInput| {
        [2, 7]
            .iter()
            .all(|&c| alpha_at_y(input, c * cw + cw / 2, ch - 1) == whole)
            && (3 * cw..7 * cw).any(|x| alpha_at_y(input, x, ch - 1) == whole)
    };
    assert!(
        runs_on(&shaped),
        "the glass runs on whole on the rail, under the word and in the blanks beside it"
    );
    let (cut, _, _, _, _) = lay(true);
    assert!(
        !runs_on(&cut),
        "control: with no evidence for a wide glyph, the word's cells are cut whole"
    );
}

#[test]
fn unavailable_frame_evidence_keeps_the_guard_table_and_yields_glass() {
    let mut renderer =
        aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default()).unwrap();
    let mut input = page(b"M");
    let (cw, ch) = renderer.cell_size();
    let ink = renderer.glyph_ink_for(&input);
    assert!(
        ink.for_frame(&input, (cw, ch))
            .unwrap()
            .coverage(0, 0)
            .is_some()
    );
    let quad = GlowQuad::flat(0, 0, 0, (24 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
    input.glow_under = vec![quad];
    let at = Instant::now();
    let glass = GlassLaid::new(
        at,
        (vec![quad], GlowRamps::default()),
        (FG, BG),
        (0, 0, cw as i32, ch as i32),
        None,
    );
    let mut known = input.clone();
    assert!(
        !GlassYield::default()
            .apply_observed(&mut known, &glass, &[], Some(&ink), true)
            .cut(),
        "actual safe ink keeps its glass before the renderer becomes unavailable"
    );
    let mut glow = super::super::CursorGlow {
        glass_laid: glass,
        ..Default::default()
    };
    glow.observe_glyph_ink(Some(std::sync::Arc::clone(&ink)));
    glow.observe_glyph_ink_unavailable();
    assert!(
        std::sync::Arc::ptr_eq(glow.glyph_ink.as_ref().unwrap(), &ink),
        "the existing non-glass guard retains its table"
    );
    assert!(
        glow.yield_glass(&mut input, &[], at).cut(),
        "retaining the guard does not confer fresh frame evidence"
    );
    assert_eq!(glow.glass_yield.ks[0], 0);
}
use aterm_core::render::InkCell;
use aterm_core::terminal::Terminal;

/// Default Light's page.
const FG: u32 = 0x002B_2D33;
const BG: u32 = 0x00F6_F7F9;
/// A warm glass light over the page: premultiplied, `(colour, alpha)`.
const GLASS: (u32, u8) = (0x005A_4B00, 90);
/// The cell, px.
const CW: i32 = 10;
const CH: i32 = 20;

/// A frame of `text` on Default Light's page, 3 rows of 24 columns.
fn page(text: &[u8]) -> RenderInput {
    let mut term = Terminal::new(3, 24);
    term.process(b"\x1b]10;rgb:2b/2d/33\x07\x1b]11;rgb:f6/f7/f9\x07");
    term.process(text);
    let mut input = term.cell_frame(3, 24);
    input.cursor_visible = false;
    assert_eq!(
        input.default_bg & 0x00FF_FFFF,
        BG,
        "premise: the page is Default Light's"
    );
    assert_eq!(
        input.cells[0][0].bg,
        [0xF6, 0xF7, 0xF9],
        "premise: a cell's ground is the page"
    );
    input
}

/// The glass over `row`, columns `0 .. cols`: one flat quad, laid as the record names it.
fn glass_over(
    input: &mut RenderInput,
    row: u16,
    cols: i32,
    at: Instant,
    caret: Option<(u16, u16)>,
) -> GlassLaid {
    let quad = GlowQuad::flat(
        row,
        0,
        row * CH as u16,
        (cols * CW) as u16,
        CH as u16,
        GLASS.0,
        GLASS.1,
    );
    input.glow_under = vec![quad];
    input.glow_under_ramps.clear();
    GlassLaid::new(
        at,
        (vec![quad], GlowRamps::default()),
        (FG, BG),
        (0, 0, CW, CH),
        caret,
    )
}

/// The glass's alpha at pixel `x` of the stream's quads on `row`, the most of them.
fn alpha_at(input: &RenderInput, row: u16, x: i32) -> u32 {
    let y = i32::from(row) * CH + CH / 2;
    input
        .glow_under
        .iter()
        .filter(|q| {
            (i32::from(q.x)..i32::from(q.x) + i32::from(q.w)).contains(&x)
                && (i32::from(q.y)..i32::from(q.y) + i32::from(q.h)).contains(&y)
        })
        .map(|q| {
            aterm_render::glow_lerp(
                u32::from(q.alpha),
                u32::from(q.alpha2),
                (x - i32::from(q.x)) as u32,
                u32::from(q.w),
            )
        })
        .max()
        .unwrap_or(0)
}

/// The alpha at pixel `(x, y)` of the stream's quads, the most of them.
fn alpha_at_y(input: &RenderInput, x: i32, y: i32) -> u32 {
    input
        .glow_under
        .iter()
        .filter(|q| {
            (i32::from(q.x)..i32::from(q.x) + i32::from(q.w)).contains(&x)
                && (i32::from(q.y)..i32::from(q.y) + i32::from(q.h)).contains(&y)
        })
        .map(|q| {
            aterm_render::glow_lerp(
                u32::from(q.alpha),
                u32::from(q.alpha2),
                (x - i32::from(q.x)) as u32,
                u32::from(q.w),
            )
        })
        .max()
        .unwrap_or(0)
}

/// The glass's alpha across cell `col` of `row`: its least and its most.
fn cell_alpha(input: &RenderInput, row: u16, col: i32) -> (u32, u32) {
    (col * CW..(col + 1) * CW).fold((u32::MAX, 0), |(lo, hi), x| {
        let a = alpha_at(input, row, x);
        (lo.min(a), hi.max(a))
    })
}

/// The red the measured caps leave next to no glass (§4 Measured).
const RED: u32 = 0x00C4_262E;

/// What `ink`'s word lays [`GLASS`] at: the alpha of its cap (§4 rule 3).
fn capped_alpha(ink: u32) -> u32 {
    let over = aterm_render::over_premul(BG, GLASS.0, GLASS.1) & 0x00FF_FFFF;
    let k = cap(ink, over, BG, solid_ink_bar(ink, BG));
    u32::from(scale_end(GLASS.0, GLASS.1, f32::from(k) / 255.0).1)
}

/// The cell's pixel row a word's foot rail starts on.
fn rail_of(ink: &GlyphInk) -> i32 {
    let [_, _, _, y1] = ink
        .cell_box(GlyphInk::index('o', false))
        .expect("the face inks an o");
    i32::from(y1) + LETTER_RING_PX as i32
}

/// [`rail_of`] the face `renderer` lays `input` in.
fn rail_from(renderer: &mut aterm_render::Renderer, input: &RenderInput) -> i32 {
    rail_of(&renderer.glyph_ink_for(input))
}

/// Every end the stream lays: a zero alpha carries no colour.
fn no_coloured_zero(input: &RenderInput) {
    for q in &input.glow_under {
        for (c, a) in [(q.color, q.alpha), (q.color2, q.alpha2)] {
            assert!(a > 0 || c == 0, "an end of alpha 0 carries {c:06x}: {q:?}");
        }
    }
}

/// The measured caps (§4 rule 3 and its Measured table), over Default Light's shipped glass at
/// its anchors, whole (alpha 255), each ink's glyph read as it is seen (§15.8 amendment 49) at
/// its letter bar (amendment 64): the theme's ink `#2b2d33` keeps every anchor whole (the glass
/// is solved for it); the grey `#474d57` keeps the yellow, the green and the cyan whole and
/// about half of the rest; the inks under AA on the page keep the sliver of glass 97% of their
/// own reading leaves. An ink lighter than the page keeps no glass. Negative control
/// ([`LawMutant::FlatBar`]): at the flat 5.25 the inks at their own reading take nothing.
#[test]
fn the_caps_are_the_measured_ones() {
    use crate::rainbow_kitty::prism_glass::{ANCHOR_AT, Palette};
    let shipped = Palette::shipped(FG, BG).expect("Default Light's tables ship");
    // Red, orange, yellow, green, cyan, azure, lilac, magenta.
    let anchors = ANCHOR_AT.map(|at| shipped.glass_arc[at]);
    let caps = |ink: u32| anchors.map(|glass| cap(ink, glass, BG, solid_ink_bar(ink, BG)));
    for (ink, measured) in [
        (FG, [255, 255, 255, 255, 255, 255, 255, 255]),
        (0x0047_4D57, [140, 137, 255, 255, 255, 139, 146, 135]),
        (0x00C4_262E, [3, 3, 9, 7, 6, 3, 4, 3]),
        (0x000B_5CC4, [20, 19, 55, 41, 34, 20, 21, 19]),
        (0x00A2_269E, [24, 24, 68, 50, 42, 24, 26, 23]),
        (0x005C_6370, [4, 3, 10, 7, 6, 4, 4, 3]),
        (0x001A_7F37, [3, 3, 9, 7, 6, 3, 4, 3]),
        (0x008A_6100, [3, 3, 9, 7, 6, 3, 4, 3]),
        (0x000F_7684, [3, 3, 9, 7, 6, 3, 4, 3]),
        (0x006B_7280, [3, 3, 9, 7, 6, 3, 4, 3]),
    ] {
        assert_eq!(caps(ink), measured, "#{ink:06x}");
    }
    for (i, &glass) in anchors.iter().enumerate() {
        assert_eq!(
            cap(0x00FF_FFFF, glass, BG, solid_ink_bar(0x00FF_FFFF, BG)),
            0,
            "white ink on anchor {i}"
        );
    }
    with_law_mutant(LawMutant::FlatBar, || {
        assert_eq!(
            caps(0x00C4_262E),
            [0; 8],
            "control: #c4262e under a flat 5.25"
        );
        assert_eq!(
            caps(0x0047_4D57),
            [86, 84, 255, 255, 255, 86, 92, 83],
            "control: #474d57 at a flat 5.25"
        );
    });
    // The bar is read on the glyph as it is seen: the grey #5a5a5a is 6.43:1 on the page
    // itself and 5.27 seen, so a glass that leaves the page no darker keeps it whole.
    let seen = |ink: u32| crate::rainbow_kitty::prism_glass::colour::seen(ink, BG, BG);
    assert!(
        (5.25..5.35).contains(&seen(0x005A_5A5A)),
        "{}",
        seen(0x005A_5A5A)
    );
    let pale = aterm_render::over_premul(BG, 0x0005_0505, 3) & 0x00FF_FFFF;
    assert_eq!(
        cap(0x005A_5A5A, pale, BG, YIELD_BAR),
        255,
        "#5a5a5a over #{pale:06x}"
    );
    assert_eq!(
        cap(0x00C4_262E, pale, BG, YIELD_BAR),
        0,
        "#c4262e as seen, at 5.25"
    );
}

/// §4 rules 1-3 and 6: an app's red word takes its cap, the grey word beside it and the
/// theme's letters keep the glass whole, and each blank beside the red word eases back to whole
/// across its cell. Negative control: the same row in the theme's ink keeps its whole glass.
#[test]
fn app_colour_wins_over_the_light_glass() {
    let at = Instant::now();
    // "ab red grey cd": a, b at 0-1, red at 3-5, grey at 7-10, c, d at 12-13.
    let mut input = page(b"ab \x1b[38;2;196;38;46mred\x1b[39m \x1b[38;2;71;77;87mgrey\x1b[39m cd");
    let glass = glass_over(&mut input, 0, 16, at, None);
    let report = GlassYield::default().apply(&mut input, &glass, &[]);
    assert!(report.quads && !report.ramps, "{report:?}");
    let whole = u32::from(GLASS.1);
    let red = capped_alpha(RED);
    assert!(red < whole / 4, "premise: the red keeps little: {red}");
    for col in 3..=5 {
        assert_eq!(
            cell_alpha(&input, 0, col),
            (red, red),
            "the red word's cell {col}"
        );
    }
    for col in [0, 1, 7, 8, 9, 10, 12, 13] {
        assert_eq!(
            cell_alpha(&input, 0, col),
            (whole, whole),
            "cell {col} keeps its glass"
        );
    }
    // The blanks beside the red word ease from it back to whole.
    // Each piece is read at its pixels' centres, so the ease starts a step under whole.
    let (lo, hi) = cell_alpha(&input, 0, 2);
    assert!(
        lo < whole / 4 && hi > whole * 9 / 10,
        "cell 2 eases {lo}..{hi}"
    );
    assert!(alpha_at(&input, 0, 2 * CW) >= alpha_at(&input, 0, 3 * CW - 1));
    let (lo, hi) = cell_alpha(&input, 0, 6);
    assert!(
        lo < whole / 4 && hi > whole * 9 / 10,
        "cell 6 eases {lo}..{hi}"
    );
    assert!(alpha_at(&input, 0, 6 * CW) <= alpha_at(&input, 0, 7 * CW - 1));
    no_coloured_zero(&input);
    // Control: the row in the theme's ink.
    let mut plain = page(b"ab red grey cd");
    let glass = glass_over(&mut plain, 0, 16, at, None);
    let before = plain.glow_under.clone();
    assert_eq!(
        GlassYield::default().apply(&mut plain, &glass, &[]),
        YieldReport::default()
    );
    assert_eq!(
        plain.glow_under, before,
        "control: the theme's row keeps its glass"
    );
}

/// §4 rule 5b: `shit.` with `shit` in an app's red and the caret just past the `.`: the
/// glass between the yielded word and the caret, the caret's cell with it, is yielded, and
/// the blank past the caret eases back. Negative control: the caret on another row, the
/// `.` keeps its glass.
#[test]
fn an_orphan_glass_run_beside_the_caret_is_yielded() {
    let at = Instant::now();
    let text: &[u8] = b"\x1b[38;2;196;38;46mshit\x1b[39m.";
    let mut input = page(text);
    let glass = glass_over(&mut input, 0, 12, at, Some((0, 5)));
    let report = GlassYield::default().apply(&mut input, &glass, &[]);
    assert!(report.cut(), "{report:?}");
    let red = capped_alpha(RED);
    for col in 0..=5 {
        assert_eq!(
            cell_alpha(&input, 0, col),
            (red, red),
            "cell {col} through the caret"
        );
    }
    let whole = u32::from(GLASS.1);
    let (lo, hi) = cell_alpha(&input, 0, 6);
    assert!(
        lo < whole / 4 && hi > whole * 9 / 10,
        "cell 6 eases {lo}..{hi}"
    );
    for col in 7..12 {
        assert_eq!(cell_alpha(&input, 0, col), (whole, whole), "cell {col}");
    }
    no_coloured_zero(&input);
    let mut away = page(text);
    let glass = glass_over(&mut away, 0, 12, at, Some((2, 5)));
    let _ = GlassYield::default().apply(&mut away, &glass, &[]);
    assert_eq!(
        cell_alpha(&away, 0, 3),
        (red, red),
        "control: the word still yields"
    );
    assert_eq!(
        cell_alpha(&away, 0, 4),
        (whole, whole),
        "control: the caret elsewhere, the `.` keeps its glass"
    );
}

/// §4 rule 5b is bounded by words and the caret alone: `a two.` with `two` on its own
/// ground (a rule-1 ground, which takes no glass) and the caret just past the `.` leaves the
/// `.` its glass, where the same `two` in an app's red (a yielded word) takes the `.` and the
/// caret's cell with it (the negative control).
#[test]
fn a_ground_off_the_page_bounds_no_orphan_run() {
    let at = Instant::now();
    let whole = u32::from(GLASS.1);
    let run = |text: &[u8]| {
        let mut input = page(text);
        let glass = glass_over(&mut input, 0, 12, at, Some((0, 6)));
        let _ = GlassYield::default().apply(&mut input, &glass, &[]);
        input
    };
    let ground = run(b"a \x1b[48;2;220;230;255mtwo\x1b[49m.");
    for col in 2..5 {
        assert_eq!(
            cell_alpha(&ground, 0, col),
            (0, 0),
            "the ground's cell {col}"
        );
    }
    assert_eq!(
        cell_alpha(&ground, 0, 5),
        (whole, whole),
        "the `.` beside a ground keeps its glass"
    );
    let word = run(b"a \x1b[38;2;196;38;46mtwo\x1b[39m.");
    let red = capped_alpha(RED);
    for col in 2..=6 {
        assert_eq!(
            cell_alpha(&word, 0, col),
            (red, red),
            "control: beside a red word, cell {col} through the caret yields"
        );
    }
}

/// A composed row's cells outside the step's pane (another session's) bound nothing: rules 5
/// and 6 read the pane's own columns. Two blanks at the pane's left edge, between a
/// neighbour pane on a dark page and an app's red word, keep their glass by the edge, the
/// one beside the word easing back to whole; the same row recorded with no pane (the whole
/// row, the negative control) takes the neighbour's off-page cells as yielded and cuts the
/// two blanks between them and the word.
#[test]
fn a_neighbour_pane_bounds_nothing() {
    let at = Instant::now();
    let whole = u32::from(GLASS.1);
    let run = |pane: Option<(u16, u16)>| {
        let mut input = page(b"              \x1b[38;2;196;38;46mtwo\x1b[39m");
        // The neighbour pane's twelve cells, on its own dark ground.
        for cell in &mut input.cells[0][..12] {
            cell.bg = [26, 27, 38];
        }
        let quad = GlowQuad::flat(
            0,
            (12 * CW) as u16,
            0,
            (12 * CW) as u16,
            CH as u16,
            GLASS.0,
            GLASS.1,
        );
        input.glow_under = vec![quad];
        input.glow_under_ramps.clear();
        let glass = GlassLaid::new(
            at,
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, CW, CH),
            None,
        )
        .in_pane(pane);
        let _ = GlassYield::default().apply(&mut input, &glass, &[]);
        input
    };
    let paned = run(Some((12, 12)));
    assert_eq!(
        cell_alpha(&paned, 0, 12),
        (whole, whole),
        "the pane's edge blank keeps its glass"
    );
    let (lo, hi) = cell_alpha(&paned, 0, 13);
    assert!(
        lo < hi,
        "the blank beside the word eases back to whole: {lo} ..= {hi}"
    );
    let red = capped_alpha(RED);
    for col in 14..17 {
        assert_eq!(
            cell_alpha(&paned, 0, col),
            (red, red),
            "the red word's cell {col}"
        );
    }
    let whole_row = run(None);
    assert_eq!(
        cell_alpha(&whole_row, 0, 12),
        (0, 0),
        "control: the neighbour's cells bound the blanks"
    );
    assert_eq!(
        cell_alpha(&whole_row, 0, 13).1,
        red,
        "control: the second blank eases only as far as the word's cap"
    );
}

/// §4 rule 3 over a ramp: a word's cap is the least over every row of each ramp under it,
/// each by its own ends. A navy word over one ramp whose lower rows are a bright yellow and
/// whose upper rows a blue the navy loses its bar over takes the blue's cap on every row, though
/// the ramp's brightest rows (the yellow, the most alpha) would keep it whole; the negative
/// control is the same ramp all yellow, which the word keeps whole.
#[test]
fn every_ramp_row_under_a_word_bounds_its_cap() {
    use aterm_render::{BeamRows, RampCost, RampSink};
    const NAVY: u32 = 0x0010_104A;
    const YELLOW: (u32, u8) = (0x00FF_FF00, 255);
    const BLUE: (u32, u8) = (0x0000_00FA, 250);
    let at = Instant::now();
    let lay = |blue_rows: i32| {
        let mut input = page(b"\x1b[38;2;16;16;74mnavy\x1b[39m ok");
        let (mut quads, mut ramps) = (Vec::new(), GlowRamps::default());
        {
            let mut sink = RampSink::new(&mut quads, &mut ramps).costed(RampCost::ALWAYS);
            sink.slab();
            for y in 0..CH {
                let end = if y < blue_rows { BLUE } else { YELLOW };
                sink.row(0, 0, (0, y as u16, (12 * CW) as u16), end, end);
            }
        }
        assert!(
            quads.is_empty() && ramps.items.len() == 1,
            "fixture: one ramp"
        );
        input.glow_under.clone_from(&quads);
        input.glow_under_ramps.clone_from(&ramps);
        let glass = GlassLaid::new(at, (quads, ramps), (FG, BG), (0, 0, CW, CH), None);
        let report = GlassYield::default().apply(&mut input, &glass, &[]);
        let mut laid = Vec::new();
        input
            .glow_under_ramps
            .expand_into(&input.glow_under, &mut laid);
        input.glow_under = laid;
        input.glow_under_ramps.clear();
        (input, report)
    };
    let over = |(c, a): (u32, u8)| aterm_render::over_premul(BG, c, a) & 0x00FF_FFFF;
    assert!(
        cap(NAVY, over(YELLOW), BG, solid_ink_bar(NAVY, BG)) == 255
            && cap(NAVY, over(BLUE), BG, solid_ink_bar(NAVY, BG)) < 255,
        "premise: the navy keeps its bar over the yellow and not the blue"
    );
    let (two, report) = lay(CH / 2);
    assert!(report.ramps, "the blue rows cut the ramp: {report:?}");
    let k = f32::from(cap(NAVY, over(BLUE), BG, solid_ink_bar(NAVY, BG))) / 255.0;
    for col in 0..4 {
        for (row_y, end) in [(2, BLUE), (CH - 3, YELLOW)] {
            let x = col * CW + CW / 2;
            assert_eq!(
                alpha_at_y(&two, x, row_y),
                u32::from(scale_end(end.0, end.1, k).1),
                "the navy's cell {col} at y {row_y}"
            );
        }
    }
    let (yellow, report) = lay(0);
    assert_eq!(
        report,
        YieldReport::default(),
        "control: the all-yellow ramp is kept whole"
    );
    assert_eq!(alpha_at_y(&yellow, CW / 2, CH - 3), u32::from(YELLOW.1));
}

/// §4: a scaled end whose alpha rounds to 0 carries no colour. A dim premultiplied end
/// (`#050505` at alpha 3) scaled to a tenth rounds its alpha to 0 and its colour to 1, an
/// additive light where nothing should be; [`scale_end`] lays it as no light. Each channel
/// stays under the alpha, and a whole scale leaves the end as it is.
#[test]
fn a_zero_scaled_alpha_carries_no_colour() {
    let naive = |c: u32, a: u8, s: f32| {
        let r = |v: u32| (v as f32 * s + 0.5).floor() as u32;
        (
            (r(c >> 16 & 0xff) << 16) | (r(c >> 8 & 0xff) << 8) | r(c & 0xff),
            r(u32::from(a)),
        )
    };
    assert_eq!(naive(0x0005_0505, 3, 0.1), (0x0001_0101, 0), "premise");
    assert_eq!(scale_end(0x0005_0505, 3, 0.1), (0, 0));
    assert_eq!(scale_end(0x0014_1414, 20, 0.5), (0x000A_0A0A, 10));
    assert_eq!(
        scale_end(0x0009_0000, 4, 0.5),
        (0x0002_0000, 2),
        "a channel under its alpha"
    );
    assert_eq!(scale_end(0x0012_3456, 0x80, 1.0), (0x0012_3456, 0x80));
    // Through the yield: a dim glass beside a rule-1 ground eases from 0 without a coloured
    // zero.
    let at = Instant::now();
    let mut input = page(b"ab \x1b[48;2;220;230;255mgrn\x1b[49m cd");
    let quad = GlowQuad::flat(0, 0, 0, 120, CH as u16, 0x0005_0505, 3);
    input.glow_under = vec![quad];
    let glass = GlassLaid::new(
        at,
        (vec![quad], GlowRamps::default()),
        (FG, BG),
        (0, 0, CW, CH),
        None,
    );
    let report = GlassYield::default().apply(&mut input, &glass, &[]);
    assert!(report.cut(), "premise: the yield cut the dim glass");
    assert!(
        input
            .glow_under
            .iter()
            .any(|q| q.alpha == 0 || q.alpha2 == 0),
        "premise: the word scaled an end of the glass to alpha 0: {:?}",
        input.glow_under
    );
    no_coloured_zero(&input);
}

/// §4 rule 4: the word pass's first candy frame over the glass (its cells' ink animated,
/// [`InkCell`]) takes the cap the frame's ink reads at, so the frame's candy ink stands at its
/// bar on what is left of the glass, whatever its colour is that frame; the theme's word beside
/// it keeps its glass. Its crown twin waits for 3.10 (§15.2 3.10).
#[test]
fn the_first_candy_frame_over_glass_meets_its_bar() {
    use crate::rainbow_kitty::prism_glass::colour::seen;
    const PINK: u32 = 0x00FF_5CA8;
    let at = Instant::now();
    let mut input = page(b"cat sat");
    let glass = glass_over(&mut input, 0, 10, at, None);
    // The candy's first frame: the word `cat` inked pink, which on the glass is under its bar.
    input.ink = (0..3)
        .map(|col| InkCell {
            row: 0,
            col,
            color: [0xFF, 0x5C, 0xA8],
        })
        .collect();
    let pink_glass = aterm_render::over_premul(BG, GLASS.0, GLASS.1) & 0x00FF_FFFF;
    assert!(
        cap(PINK, pink_glass, BG, solid_ink_bar(PINK, BG)) < 255,
        "premise: the frame's candy ink is under its bar on the glass"
    );
    let _ = GlassYield::default().apply(&mut input, &glass, &[]);
    let (lo, hi) = cell_alpha(&input, 0, 0);
    assert_eq!(lo, hi, "the candy's word lies flat");
    for col in 1..3 {
        assert_eq!(
            cell_alpha(&input, 0, col),
            (lo, hi),
            "the candy's cell {col}"
        );
    }
    // No piece over the cell: its ground is the page's.
    let ground = input.glow_under.iter().find(|q| q.x == 0).map_or(BG, |q| {
        aterm_render::over_premul(BG, q.color, q.alpha) & 0x00FF_FFFF
    });
    let bar = solid_ink_bar(PINK, BG);
    assert!(
        seen(PINK, BG, ground) >= bar,
        "the candy reads {} on what its cap leaves, its bar {bar}",
        seen(PINK, BG, ground)
    );
    let whole = u32::from(GLASS.1);
    for col in 4..7 {
        assert_eq!(
            cell_alpha(&input, 0, col),
            (whole, whole),
            "the theme's cell {col}"
        );
    }
    // Control: the same word not animated, in the theme's ink, keeps its glass.
    let mut still = page(b"cat sat");
    let glass = glass_over(&mut still, 0, 10, at, None);
    let _ = GlassYield::default().apply(&mut still, &glass, &[]);
    assert_eq!(cell_alpha(&still, 0, 1), (whole, whole), "control");
}

/// The frame's cells of `cat` in `ink`, as the word pass inks them.
fn inked_word(ink: [u8; 3]) -> Vec<InkCell> {
    (0..3)
        .map(|col| InkCell {
            row: 0,
            col,
            color: ink,
        })
        .collect()
}

/// §4 rule 4 (after-land A7: `kitty` ringed at t0, t120, t470, a word popping clean as its
/// animation ends): a word whose animating ink holds its bar on the whole glass takes no cut
/// on a frame it animates, and one that does not takes the cap its ink reads at, no less.
/// Negative control ([`LawMutant::AnimatedTakesNothing`]): rule 4 as it stood, every animating
/// frame takes `0`.
#[test]
fn an_animating_ink_that_holds_takes_no_cut() {
    let mut renderer = aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
        .expect("the bundled face");
    let (cw, ch) = renderer.cell_size();
    let mut lay = |ink: [u8; 3]| {
        let mut input = page(b"cat sat");
        let judged = renderer.glyph_ink_for(&input);
        input.ink = inked_word(ink);
        let quad = GlowQuad::flat(0, 0, 0, (10 * cw) as u16, ch as u16, GLASS.0, GLASS.1);
        input.glow_under = vec![quad];
        let glass = GlassLaid::new(
            Instant::now(),
            (vec![quad], GlowRamps::default()),
            (FG, BG),
            (0, 0, cw as i32, ch as i32),
            None,
        );
        let mut glass_yield = GlassYield::default();
        let report = glass_yield.apply_observed(&mut input, &glass, &[], Some(&judged), true);
        (glass_yield.ks[..3].to_vec(), report.cut())
    };
    let dark = [0x30, 0x10, 0x48];
    assert_eq!(
        lay(dark),
        (vec![255; 3], false),
        "the dark ink holds: nothing is cut on the frame it animates"
    );
    // Between the ink that holds and the one that has no room: the cap it reads at.
    let caps: Vec<u8> = (0..48_u8).map(|i| lay([i, 0x50 + i, 0xB8]).0[0]).collect();
    assert!(
        caps.iter().any(|&k| k > 0 && k < 255),
        "an ink that reads past the floor on the page takes the cap it reads at: {caps:?}"
    );
    let (pale, cut) = lay([0xFF, 0x5C, 0xA8]);
    assert!(
        cut && pale == [0; 3],
        "an ink under the floor, which its lattice step's bar leaves no room, takes the page: \
         {pale:?}"
    );
    let (control, cut) = with_law_mutant(LawMutant::AnimatedTakesNothing, || lay(dark));
    assert!(
        cut && control == [0; 3],
        "control: rule 4 as it stood cuts the dark ink too: {control:?}"
    );
}

/// §4 rule 4: an animating ink is judged as the grey [`animated_reading`] reads it as, and the
/// ink itself keeps its bars at every alpha the grey's cap lets through, on every glass anchor:
/// the lighter grey on the lattice reads no more on the page. The word's judges so stay few (a
/// grey a lattice step wide) however the word pass moves its colour. Negative control
/// ([`LawMutant::DarkerGrey`]): read on the lattice's darker side, some ink falls under.
#[test]
fn an_animating_ink_keeps_its_bars_under_its_grey_s_cap() {
    use crate::rainbow_kitty::prism_glass::{ANCHOR_AT, Palette};
    let mut renderer = aterm_render::Renderer::from_system(24.0, aterm_render::Theme::default())
        .expect("the bundled face");
    let (cw, ch) = renderer.cell_size();
    let input = page(b"a");
    let ink = renderer.glyph_ink_for(&input);
    let frame = ink.for_frame(&input, (cw, ch)).expect("current evidence");
    let (_, coverage) = frame.coverage(0, 0).expect("the a's raster");
    let corrected = frame.corrected();
    let anchors = ANCHOR_AT.map(|at| {
        Palette::shipped(FG, BG)
            .expect("Default Light's tables ship")
            .glass_arc[at]
    });
    // The word pass's kinds of ink: candy letters, a self-glow's lifts, fully saturated ones.
    let inks = [
        0x00C0_1C6Cu32,
        0x0095_2FA8,
        0x002B_6BB8,
        0x0000_7F73,
        0x0065_7A00,
        0x00A8_5B00,
        0x00FF_5CA8,
        0x0078_4BE0,
        0x0000_6FD6,
        0x00D7_0000,
        0x0000_8700,
        0x00AF_00AF,
    ];
    let fails = || {
        let mut fails = 0;
        for &ink in &inks {
            let grey = animated_reading(ink) | ANIMATED_READING;
            let exact = GlyphJudge::new(coverage, ink, BG, FG, corrected).unwrap();
            let read = GlyphJudge::new(coverage, grey, BG, FG, corrected).unwrap();
            for &glass in &anchors {
                for alpha in [96_u8, 160, 255] {
                    let lit = aterm_render::premul_rgb(glass, alpha);
                    let k = read.cap((lit, alpha));
                    for step in 1..=k {
                        let (color, a) = scale_end(lit, alpha, f32::from(step) / 255.0);
                        let ground = aterm_render::over_premul(BG, color, a);
                        fails += usize::from(!exact.keeps(ground));
                    }
                }
            }
        }
        fails
    };
    assert_eq!(
        fails(),
        0,
        "an ink under the bar its grey's cap let through"
    );
    assert!(
        with_law_mutant(LawMutant::DarkerGrey, fails) > 0,
        "control: read on the lattice's darker side, some ink falls under"
    );
}

/// §4 rule 3: a cap under what the old floor (`200`) took to nothing is laid as it is, so the
/// glass leaves a word by degrees. Negative control ([`LawMutant::Floor`]): the floor back, the
/// word takes `0`.
#[test]
fn a_low_cap_is_laid_and_takes_no_floor() {
    const BLUE: u32 = 0x000B_5CC4;
    let lay = || {
        let mut input = page(b"\x1b[38;2;11;92;196mred\x1b[39m");
        let glass = glass_over(&mut input, 0, 8, Instant::now(), None);
        let _ = GlassYield::default().apply(&mut input, &glass, &[]);
        cell_alpha(&input, 0, 1)
    };
    let capped = capped_alpha(BLUE);
    assert!(capped > 0, "premise: a sliver of glass is left");
    assert_eq!(lay(), (capped, capped), "the word's cap");
    assert_eq!(
        with_law_mutant(LawMutant::Floor, lay),
        (0, 0),
        "control: the floor takes it to nothing"
    );
}

/// §4 rule 6: a blank between two caps eases from the one to the other across its cell,
/// holding each for [`LETTER_RING_PX`], the ring its word's letters are read in: no step at a
/// word's edge, and no letter's ring on a stronger glass than its cap. Negative control
/// ([`LawMutant::FlatBlanks`]): flat at the lesser, the stronger word stands in a step.
#[test]
fn a_blank_between_two_caps_eases_and_holds_each_ring() {
    // A red word, a blank, a blue word: the two caps differ.
    let lay = || {
        let mut input = page(b"\x1b[38;2;196;38;46mred\x1b[39m \x1b[38;2;11;92;196mblu\x1b[39m");
        let glass = glass_over(&mut input, 0, 8, Instant::now(), None);
        let _ = GlassYield::default().apply(&mut input, &glass, &[]);
        input
    };
    let (red, blue) = (capped_alpha(RED), capped_alpha(0x000B_5CC4));
    assert!(red < blue, "premise: the caps differ: {red} {blue}");
    let eased = lay();
    let (x0, x1) = (3 * CW, 4 * CW);
    let ring = LETTER_RING_PX as i32;
    for x in x0..x0 + ring {
        assert_eq!(alpha_at(&eased, 0, x), red, "the red word's ring at {x}");
    }
    for x in x1 - ring..x1 {
        assert_eq!(alpha_at(&eased, 0, x), blue, "the blue word's ring at {x}");
    }
    let rises = (x0..x1 - 1).all(|x| alpha_at(&eased, 0, x) <= alpha_at(&eased, 0, x + 1));
    assert!(rises, "the blank eases from the one cap to the other");
    let flat = with_law_mutant(LawMutant::FlatBlanks, lay);
    assert_eq!(
        cell_alpha(&flat, 0, 3),
        (red, red),
        "control: flat at the lesser, the blank steps at the blue word"
    );
}

/// §4 rule 1: a selected word, and a cell whose own ground is not the page, takes no glass.
#[test]
fn a_ground_off_the_page_takes_no_glass() {
    use aterm_core::selection::{SelectionSide, SelectionType};
    let at = Instant::now();
    let mut term = Terminal::new(3, 24);
    term.process(b"\x1b]10;rgb:2b/2d/33\x07\x1b]11;rgb:f6/f7/f9\x07");
    term.process(b"one \x1b[48;2;220;230;255mtwo\x1b[49m three");
    let selection = term.text_selection_mut();
    selection.start_selection(0, 8, SelectionSide::Left, SelectionType::Simple);
    selection.update_selection(0, 12, SelectionSide::Right);
    selection.complete_selection();
    let mut input = term.cell_frame(3, 24);
    let glass = glass_over(&mut input, 0, 16, at, None);
    let _ = GlassYield::default().apply(&mut input, &glass, &[]);
    let whole = u32::from(GLASS.1);
    for col in [4, 5, 6, 8, 9, 10, 11, 12] {
        assert_eq!(cell_alpha(&input, 0, col), (0, 0), "cell {col}");
    }
    for col in [0, 1, 2, 14, 15] {
        assert_eq!(cell_alpha(&input, 0, col), (whole, whole), "cell {col}");
    }
}

/// The yield finds the glass by the record (§4 rule 7): a light the record does not name,
/// laid on the row after the glass, is never cut.
#[test]
fn only_the_glass_yields() {
    let at = Instant::now();
    let mut input = page(b"\x1b[38;2;196;38;46mred\x1b[39m");
    let glass = glass_over(&mut input, 0, 8, at, None);
    let lip = GlowQuad::flat(0, 0, 18, 80, 2, 0x0030_2010, 0x40);
    input.glow_under.push(lip);
    let _ = GlassYield::default().apply(&mut input, &glass, &[]);
    assert_eq!(input.glow_under.last(), Some(&lip), "the lip is laid whole");
    let red = capped_alpha(RED);
    assert!(
        input.glow_under[..input.glow_under.len() - 1]
            .iter()
            .all(|q| q.x >= 30 || u32::from(q.alpha) == red),
        "the glass under the red word is yielded: {:?}",
        input.glow_under
    );
}

#[test]
fn a_body_only_twin_restores_selections_and_keeps_unrecorded_lights() {
    use aterm_core::render::{PaneSelection, SelectionClip};
    use aterm_core::selection::{SelectionSide, SelectionType};

    let mut input = page(b"ordinary words");
    input
        .selection
        .start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
    input.selection.update_selection(0, 3, SelectionSide::Right);
    input.selection.complete_selection();
    input.selection_clip = Some(SelectionClip::new(0, 1, 1, 4));
    input.selection_bg = 0x0012_3456;
    input.selection_fg = 0x0065_4321;
    input.selections.push(PaneSelection {
        selection: input.selection.clone(),
        clip: SelectionClip::new(0, 1, 8, 12),
        bg: 0x0023_4567,
        fg: 0x0076_5432,
        inactive: true,
    });
    let glass = glass_over(&mut input, 0, 16, Instant::now(), None);
    let lip = GlowQuad::flat(0, 0, 18, 160, 2, 0x0030_2010, 0x40);
    input.glow_under.push(lip);
    input.cursor_glow_add.push(lip);
    assert_eq!(
        without_body_glass_for_test(&input, &GlassLaid::default()),
        input,
        "an absent body record removes no light or selection"
    );
    let twin = without_body_glass_for_test(&input, &glass);
    let mut expected = input.clone();
    expected.glow_under = vec![lip];
    assert_eq!(
        twin, expected,
        "only the recorded body leaves; all five selection fields and other lights survive"
    );
}
