// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::cursor_glow::Live;
use aterm_core::terminal::CursorStyle;
use std::time::Duration;

const fn fx_input(now: Instant) -> CursorFxInput {
    CursorFxInput {
        now,
        cur: Some((2, 3)),
        live_viewport: true,
        cursor_style: CursorStyle::SteadyBlock,
        blink_phase: true,
        live_cursor: 0x0050_FA7B,
        cursor_color_pinned: false,
        default_bg: 0x001A_1B26,
        cursor_cell_bg: None,
        geom: Geom {
            cw: 10,
            ch: 20,
            rows: 24,
            cols: 80,
            origin_x: 0,
            origin_y: 0,
            win_w: 800,
            win_h: 480,
            head: 0,
        },
        focused: true,
        shed_envelope: 1.0,
        trail_brightness: 1.0,
        body_allowed: true,
        momentum_glow: true,
        user_tinted: false,
        accent_pinned: false,
    }
}

const TRAIL: TrailConfig = TrailConfig {
    enabled: false,
    duration: Duration::from_millis(260),
    max_len: 24,
    color: 0x0050_FA7B,
    intensity: 1.0,
    warmth: 0.0,
};

const GEOM: EffectGeom = EffectGeom {
    cell_w: 10,
    cell_h: 20,
    rows: 24,
    cols: 80,
};

/// Every field of a step, each borrow by its address.
fn step_words(s: &CursorStep<'_>) -> String {
    let CursorStep {
        input,
        glow,
        trail,
        animate,
        row_probe,
        frame_cells,
        frame_cells_at,
        frame_cells_cols,
        frame_marks,
        pending_keys,
        print_anchor,
        print_anchor_glyph,
        caret_drawn,
    } = s;
    format!(
        "{:?} {:?} {:p} {trail:?} {animate} {row_probe:?} {:?} {frame_cells_at:?} \
         {frame_cells_cols} {:p} {} {pending_keys} {print_anchor:?} {print_anchor_glyph:?} \
         {caret_drawn}",
        input.now,
        input.cur,
        *glow,
        frame_cells.map(<[Vec<RenderCell>]>::as_ptr),
        frame_marks.as_ptr(),
        frame_marks.len(),
    )
}

/// Every field of a grid pass, each borrow by its address.
fn grid_words(g: &WordGrid<'_>) -> String {
    let WordGrid {
        now,
        lexicon,
        cfg,
        geom,
        cells,
        line_sizes,
        base_y,
        default_bg,
        epoch,
        rescan,
        companion_at,
        sel,
        focused,
    } = g;
    format!(
        "{now:?} {:p} {:p} {geom:?} {:p} {:p} {base_y} {default_bg} {epoch} {rescan:?} \
         {companion_at:?} {:p} {} {focused}",
        *lexicon,
        *cfg,
        cells.as_ptr(),
        line_sizes.as_ptr(),
        sel.sel,
        sel.display_offset,
    )
}

/// Every field of a composed pass, each borrow by its address.
fn panes_words(f: &WordPanes<'_>) -> String {
    let WordPanes {
        now,
        lexicon,
        cfg,
        tick_cfg,
        load_shed,
        word_focus,
        window_focused,
        companion_at,
    } = f;
    format!(
        "{now:?} {:p} {:p} {:p} {load_shed} {word_focus} {window_focused} {companion_at:?}",
        *lexicon, *cfg, *tick_cfg,
    )
}

/// `new` is the literal a test host wrote, and each setter moves only its own field: the
/// constructor every host and harness builds a cursor step through (§15.1 P0.1).
#[test]
fn a_cursor_step_built_by_new_is_the_literal() {
    let now = Instant::now();
    let glow = GlowConfig::default();
    let plain = CursorStep {
        input: fx_input(now),
        glow: &glow,
        trail: TRAIL,
        animate: true,
        row_probe: None,
        frame_cells: None,
        frame_cells_at: (0, 0),
        frame_cells_cols: 0,
        frame_marks: &[],
        pending_keys: 0,
        print_anchor: None,
        print_anchor_glyph: None,
        caret_drawn: true,
    };
    assert_eq!(
        step_words(&CursorStep::new(fx_input(now), &glow, TRAIL)),
        step_words(&plain)
    );
    let row = ['a', 'b'];
    let cells = vec![vec![RenderCell::default(); 2]];
    let probe = RowProbe {
        row: 2,
        caret: 1,
        trust: ProbeTrust::ContentOnly,
        cols: &row,
        flanks: Some((None, Some(&row))),
    };
    let marks: Vec<crate::cursor_glow::RowMarks> = vec![vec![(1, Box::from(['\u{301}']))]];
    let every = CursorStep {
        input: fx_input(now),
        glow: &glow,
        trail: TRAIL,
        animate: false,
        row_probe: Some(probe),
        frame_cells: Some(&cells),
        frame_cells_at: (4, 5),
        frame_cells_cols: 2,
        frame_marks: &marks,
        pending_keys: 3,
        print_anchor: Some((2, 1, 9)),
        print_anchor_glyph: Some('b'),
        caret_drawn: false,
    };
    let built = CursorStep::new(fx_input(now), &glow, TRAIL)
        .with_animate(false)
        .with_row_probe(Some(probe))
        .with_frame_cells(Some(&cells))
        .with_frame_cells_at((4, 5))
        .with_frame_cells_cols(2)
        .with_frame_marks(&marks)
        .with_pending_keys(3)
        .with_print_anchor(Some((2, 1, 9)))
        .with_print_anchor_glyph(Some('b'))
        .with_caret_drawn(false);
    assert_eq!(step_words(&built), step_words(&every));
    assert_ne!(
        step_words(&plain),
        step_words(&every),
        "control: the setters moved the step off its defaults"
    );
}

/// The frame light is the light the frame's cursor step ran under, and [`Light::OFF`] on a
/// frame no cursor step ran at, whatever an earlier frame's was (§15.2 1.1).
#[test]
fn the_frame_light_is_the_cursor_steps_and_off_on_a_frame_without_one() {
    let mut fx = EffectsPipeline::for_window();
    let t0 = Instant::now();
    assert_eq!(fx.frame_light(t0), Light::OFF, "before any step");
    let lit = GlowConfig {
        light: Light::at(Live::Glass),
        ..GlowConfig::default()
    };
    let _ = fx.cursor_step(
        &CursorStep::new(fx_input(t0), &lit, TRAIL),
        std::iter::empty(),
    );
    assert_eq!(
        fx.frame_light(t0),
        Light::at(Live::Glass),
        "the step's light"
    );
    let t1 = t0 + Duration::from_millis(16);
    assert_eq!(fx.frame_light(t1), Light::OFF, "a frame with no step");
    let dark = GlowConfig::default();
    let _ = fx.cursor_step(
        &CursorStep::new(fx_input(t1), &dark, TRAIL),
        std::iter::empty(),
    );
    assert_eq!(fx.frame_light(t1), Light::OFF, "a dark step's");
    assert!(fx.light_arms().is_empty(), "no light arm drew");
    assert!(fx.light_words(t0).words().is_empty(), "no light state");
}

/// The light report is the last cursor step's (§15.0.2(b), §15.2 1.5): Rainbow Kitty steps
/// typing on Default Light under the glass report the `glass` arm, in the under stream's
/// quads and ramps, with the bed's glass in the light words; the same steps under main's
/// light, or on the dark page under the glass, report no arm and no light word.
#[test]
fn a_glass_step_reports_the_glass_arm_and_main_none() {
    use crate::state_words::{ArmsDrawn, Streams};
    let page = |light: Light, dark: bool| GlowConfig {
        light,
        ..if dark {
            GlowConfig::default()
        } else {
            GlowConfig {
                dark_theme: false,
                theme_fg: 0x002B_2D33,
                theme_bg: 0x00F6_F7F9,
                ..GlowConfig::default()
            }
        }
    };
    let typed = |glow: GlowConfig| {
        let mut fx = EffectsPipeline::for_window();
        let t0 = Instant::now();
        let mut t = t0;
        for col in 3..12u16 {
            t += Duration::from_millis(60);
            fx.cursor_mut().glow.note_synthetic_typed(t, 1);
            let mut input = fx_input(t);
            input.cur = Some((2, col));
            input.default_bg = glow.theme_bg;
            let _ = fx.cursor_step(&CursorStep::new(input, &glow, TRAIL), std::iter::empty());
        }
        (fx.light_arms(), fx.light_words(t0))
    };
    let (arms, words) = typed(page(Light::at(Live::Glass), false));
    let mut glass = ArmsDrawn::NONE;
    glass.note(
        "glass",
        Streams::GLOW_UNDER.union(Streams::GLOW_UNDER_RAMPS),
    );
    assert_eq!(arms, glass, "the glass inked the body");
    assert_eq!(words.get("bed.glass.bg"), Some(0x00F6_F7F9));
    for (what, glow) in [
        ("main's light", page(Light::OFF, false)),
        (
            "a latched light at Live::Off",
            page(Light::at(Live::Off), false),
        ),
        ("the dark page", page(Light::at(Live::Glass), true)),
    ] {
        let (arms, words) = typed(glow);
        assert!(arms.is_empty(), "{what}: {arms:?}");
        assert!(words.words().is_empty(), "{what}: {:?}", words.words());
    }
}

/// The guard's report (§15.2 1.9, §15.9's note for 1.9): Rainbow Kitty steps typing a row of
/// letters on Default Light under the glass, whose letter guard judges the frame's lights
/// against them, report the `guard` arm beside the `glass` arm, in every stream the guard
/// cuts. The same steps with no letters, which leave the guard nothing to judge, and with the
/// guard off (the negative control) report the glass alone; main's light reports no arm.
#[test]
fn a_glass_frame_the_guard_judged_reports_the_guard_arm() {
    use crate::state_words::{ArmsDrawn, Streams};
    let light = |light: Light| GlowConfig {
        light,
        dark_theme: false,
        theme_fg: 0x002B_2D33,
        theme_bg: 0x00F6_F7F9,
        ..GlowConfig::default()
    };
    let page = RenderCell {
        fg: [0x2B, 0x2D, 0x33],
        bg: [0xF6, 0xF7, 0xF9],
        ..RenderCell::default()
    };
    let run = |glow: GlowConfig, letters: bool, guard: bool| {
        let mut fx = EffectsPipeline::for_window();
        fx.set_letter_guard(guard);
        let mut cells = vec![vec![page; 80]; 24];
        let mut t = Instant::now();
        for col in 3..12u16 {
            t += Duration::from_millis(60);
            if letters {
                cells[2][usize::from(col) - 1].ch = 'w';
                fx.cursor_mut()
                    .glow
                    .observe_frame_text(Some((2, col)), &cells, t);
            }
            fx.cursor_mut().glow.note_synthetic_typed(t, 1);
            let mut input = fx_input(t);
            input.cur = Some((2, col));
            input.default_bg = glow.theme_bg;
            let _ = fx.cursor_step(&CursorStep::new(input, &glow, TRAIL), std::iter::empty());
        }
        fx.light_arms()
    };
    let mut alone = ArmsDrawn::NONE;
    alone.note(
        "glass",
        Streams::GLOW_UNDER.union(Streams::GLOW_UNDER_RAMPS),
    );
    let mut judged = alone;
    judged.note(
        "guard",
        Streams::GLOW_UNDER
            .union(Streams::GLOW_UNDER_RAMPS)
            .union(Streams::CURSOR_GLOW_ADD)
            .union(Streams::CURSOR_GLOW_ADD_RAMPS)
            .union(Streams::GLOW_HALO),
    );
    let lit = light(Light::at(Live::Glass));
    assert_eq!(
        run(lit, true, true),
        judged,
        "the guard judged the glass frame's letters"
    );
    assert_eq!(run(lit, false, true), alone, "no letters: nothing judged");
    assert_eq!(
        run(lit, true, false),
        alone,
        "control: the guard off judges nothing"
    );
    let main = run(light(Light::OFF), true, true);
    assert!(main.is_empty(), "main's light: {main:?}");
}

/// The glass draws main's state (§15.2 1.8 and 1.9, §15.0.4): Rainbow Kitty steps typing a
/// row of letters on Default Light under the glass, then resting, then losing the focus (the
/// ember), leave on every step the pipeline's state words, the ink ledger's and the ribbon's
/// among them, exactly as the same steps on main's light leave them, though the glass draws
/// (its arm reports it) and main's does not.
#[test]
fn a_glass_frame_leaves_its_paper_twins_state_words() {
    let light = |light: Light| GlowConfig {
        light,
        dark_theme: false,
        theme_fg: 0x002B_2D33,
        theme_bg: 0x00F6_F7F9,
        ..GlowConfig::default()
    };
    let page = RenderCell {
        fg: [0x2B, 0x2D, 0x33],
        bg: [0xF6, 0xF7, 0xF9],
        ..RenderCell::default()
    };
    let run = |glow: GlowConfig| {
        let mut fx = EffectsPipeline::for_window();
        let mut cells = vec![vec![page; 80]; 24];
        let t0 = Instant::now();
        let (mut frames, mut drew) = (Vec::new(), false);
        let mut step = |fx: &mut EffectsPipeline, t: Instant, col: u16, focused: bool| {
            let mut input = fx_input(t);
            input.cur = Some((2, col));
            input.default_bg = glow.theme_bg;
            input.focused = focused;
            let _ = fx.cursor_step(&CursorStep::new(input, &glow, TRAIL), std::iter::empty());
            drew |= !fx.light_arms().is_empty();
            frames.push(fx.state_words(t0).into_words());
        };
        let mut t = t0;
        for col in 3..12u16 {
            t += Duration::from_millis(60);
            cells[2][usize::from(col) - 1].ch = 'w';
            fx.cursor_mut()
                .glow
                .observe_frame_text(Some((2, col)), &cells, t);
            fx.cursor_mut().glow.note_synthetic_typed(t, 1);
            step(&mut fx, t, col, true);
        }
        let rest = t;
        for k in 1..=90u64 {
            step(&mut fx, rest + Duration::from_millis(16 * k), 11, true);
        }
        let away = rest + Duration::from_millis(16 * 90);
        for k in 1..=30u64 {
            step(&mut fx, away + Duration::from_millis(16 * k), 11, false);
        }
        (frames, drew)
    };
    let (glass, drew) = run(light(Light::at(Live::Glass)));
    assert!(drew, "fixture: the glass drew");
    let (main, main_drew) = run(light(Light::OFF));
    assert!(!main_drew, "main's light draws no arm");
    assert_eq!(glass.len(), main.len());
    for (k, (g, m)) in glass.iter().zip(&main).enumerate() {
        let parted = (0..g.len().max(m.len())).find(|&n| g.get(n) != m.get(n));
        assert_eq!(
            parted.map(|n| (g.get(n), m.get(n))),
            None,
            "step {k}: the glass's state word beside its paper twin's"
        );
    }
}

/// The yield's report (§15.2 1.9): Rainbow Kitty steps typing on Default Light under the
/// glass, published over a row of an app's red words, report the `yield` arm beside the
/// `glass` arm, in the streams it cut, and take the glass off the words' cells; the same
/// frame in the theme's ink reports the glass alone and publishes its glass whole. Main's
/// light, and the dark page under the glass, publish no yield.
#[test]
fn a_glass_frame_s_words_take_the_yield_and_main_none() {
    use crate::state_words::Streams;
    let light = |light: Light| GlowConfig {
        light,
        dark_theme: false,
        theme_fg: 0x002B_2D33,
        theme_bg: 0x00F6_F7F9,
        ..GlowConfig::default()
    };
    let run = |glow: GlowConfig, ink: [u8; 3]| {
        let mut fx = EffectsPipeline::for_window();
        let mut t = Instant::now();
        for col in 3..12u16 {
            t += Duration::from_millis(60);
            fx.cursor_mut().glow.note_synthetic_typed(t, 1);
            let mut input = fx_input(t);
            input.cur = Some((2, col));
            input.default_bg = glow.theme_bg;
            let _ = fx.cursor_step(&CursorStep::new(input, &glow, TRAIL), std::iter::empty());
        }
        let ground = [
            (glow.theme_bg >> 16) as u8,
            (glow.theme_bg >> 8) as u8,
            glow.theme_bg as u8,
        ];
        let page = RenderCell {
            fg: [0x2B, 0x2D, 0x33],
            bg: ground,
            ..RenderCell::default()
        };
        let mut frame = RenderInput {
            cells: vec![vec![page; 80]; 24],
            cols: 80,
            default_bg: glow.theme_bg,
            ..RenderInput::default()
        };
        for cells in &mut frame.cells[1..4] {
            for (col, cell) in cells.iter_mut().enumerate().take(20) {
                cell.ch = if col % 5 == 4 { ' ' } else { 'w' };
                cell.fg = ink;
            }
        }
        // The under light at the words' cells' centres, summed.
        let on_words = |frame: &RenderInput| {
            let mut all = Vec::new();
            frame
                .glow_under_ramps
                .expand_into(&frame.glow_under, &mut all);
            let mut sum = 0u32;
            for row in 1..4i32 {
                for col in (0..20i32).filter(|c| c % 5 != 4) {
                    let (x, y) = (col * 10 + 5, row * 20 + 10);
                    sum += all
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
                        .sum::<u32>();
                }
            }
            sum
        };
        fx.publish_cursor(&mut frame);
        let before = on_words(&frame);
        fx.publish_words(&mut frame);
        (fx.light_arms(), before, on_words(&frame))
    };
    const RED: [u8; 3] = [0xC4, 0x26, 0x2E];
    let (arms, before, after) = run(light(Light::at(Live::Glass)), RED);
    let names: Vec<_> = arms.iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["glass", "yield"], "{arms:?}");
    let (_, cut) = arms.iter().nth(1).expect("the yield");
    assert!(
        !cut.is_empty()
            && Streams::GLOW_UNDER
                .union(Streams::GLOW_UNDER_RAMPS)
                .contains(cut),
        "the yield cuts the under stream alone: {cut:?}"
    );
    assert!(
        after < before,
        "the red words keep {after} of the {before} under light laid on them"
    );
    let (arms, before, after) = run(light(Light::at(Live::Glass)), [0x2B, 0x2D, 0x33]);
    let names: Vec<_> = arms.iter().map(|(n, _)| n).collect();
    assert_eq!(names, ["glass"], "control: the theme's ink takes no yield");
    assert!(before > 0, "premise: the glass lies on the words' cells");
    assert_eq!(after, before, "control: the theme's words keep their glass");
    for (what, glow) in [
        ("main's light", light(Light::OFF)),
        (
            "the dark page",
            GlowConfig {
                light: Light::at(Live::Glass),
                ..GlowConfig::default()
            },
        ),
    ] {
        let (arms, _, _) = run(glow, RED);
        assert!(arms.is_empty(), "{what}: {arms:?}");
    }
}

/// The glass on Default Light at the glass level, light latched.
fn glass_glow() -> GlowConfig {
    GlowConfig {
        light: Light::at(Live::Glass),
        dark_theme: false,
        theme_fg: 0x002B_2D33,
        theme_bg: 0x00F6_F7F9,
        ..GlowConfig::default()
    }
}

/// A frame of an app's red words on `glow`'s page: rows 1-3, columns 0-19, a blank every
/// fifth cell.
fn red_words(glow: &GlowConfig) -> RenderInput {
    let page = RenderCell {
        fg: [0x2B, 0x2D, 0x33],
        bg: [0xF6, 0xF7, 0xF9],
        ..RenderCell::default()
    };
    let mut words = RenderInput {
        cells: vec![vec![page; 80]; 24],
        cols: 80,
        default_bg: glow.theme_bg,
        ..RenderInput::default()
    };
    for cells in &mut words.cells[1..4] {
        for (col, cell) in cells.iter_mut().enumerate().take(20) {
            cell.ch = if col % 5 == 4 { ' ' } else { 'w' };
            cell.fg = [0xC4, 0x26, 0x2E];
        }
    }
    words
}

/// One typed step at `t` under `glow`, the caret at row 2, column `col`.
fn typed_step(fx: &mut EffectsPipeline, glow: &GlowConfig, t: Instant, col: u16) {
    fx.cursor_mut().glow.note_synthetic_typed(t, 1);
    let mut input = fx_input(t);
    input.cur = Some((2, col));
    input.default_bg = glow.theme_bg;
    let _ = fx.cursor_step(&CursorStep::new(input, glow, TRAIL), std::iter::empty());
}

/// The yield cuts each stream of a step's glass once (§15.2 1.9): the first word publish
/// after a glass step cuts the glass off an app's red words; a second publish of the frame it
/// left (a host's second word pass) leaves it as it is; a copy of the step's uncut stream
/// published again (a retained projection's, a windowed capture's) is cut as the frame's own
/// was; the next step's first publish cuts its own glass. The negative control is each
/// step's first publish, which cuts.
#[test]
fn a_step_s_glass_yields_once_per_stream() {
    let glow = glass_glow();
    let words = red_words(&glow);
    let mut fx = EffectsPipeline::for_window();
    let mut t = Instant::now();
    for col in 3..12u16 {
        t += Duration::from_millis(60);
        typed_step(&mut fx, &glow, t, col);
    }
    let mut frame = words.clone();
    fx.publish_cursor(&mut frame);
    let uncut = frame.clone();
    fx.publish_words(&mut frame);
    assert!(
        fx.cursor().glow.yielded_at(t).is_some() && frame.glow_under != uncut.glow_under,
        "control: the step's first publish cuts its glass"
    );
    let mut again = frame.clone();
    fx.publish_words(&mut again);
    assert!(
        again.glow_under == frame.glow_under && again.glow_under_ramps == frame.glow_under_ramps,
        "a second publish of the cut frame cuts nothing more"
    );
    let mut copy = uncut.clone();
    fx.publish_words(&mut copy);
    assert!(
        copy.glow_under == frame.glow_under && copy.glow_under_ramps == frame.glow_under_ramps,
        "a copy of the step's uncut glass yields as the frame's own did"
    );
    t += Duration::from_millis(60);
    typed_step(&mut fx, &glow, t, 12);
    let mut next = words.clone();
    fx.publish_cursor(&mut next);
    let next_uncut = next.clone();
    fx.publish_words(&mut next);
    assert!(
        fx.cursor().glow.yielded_at(t).is_some() && next.glow_under != next_uncut.glow_under,
        "control: the next step's first publish cuts its own glass"
    );
}

/// The yield's state goes with the light (§10.1, §15.2 1.9): a glass step on Default Light
/// whose publish cut an app's red words holds the cut's report, the step it spent, the caps
/// it solved and its glass's record (the negative control); the first step on a dark page
/// after it holds none of them.
#[test]
fn the_yield_s_state_goes_with_the_light() {
    let glow = glass_glow();
    let mut fx = EffectsPipeline::for_window();
    let mut t = Instant::now();
    for col in 3..12u16 {
        t += Duration::from_millis(60);
        typed_step(&mut fx, &glow, t, col);
    }
    let mut frame = red_words(&glow);
    fx.publish_cursor(&mut frame);
    fx.publish_words(&mut frame);
    let held = |fx: &EffectsPipeline| {
        let glow = &fx.cursor().glow;
        (
            glow.last_yield().map(|(at, _)| at),
            glow.yield_held(),
            glow.glass_laid().is_empty(),
        )
    };
    let (yielded, (spent, memo), unlaid) = held(&fx);
    assert!(
        yielded == Some(t) && spent == Some(t) && memo > 0 && !unlaid,
        "control: the light step holds its yield: {:?}",
        held(&fx)
    );
    t += Duration::from_millis(60);
    let dark = GlowConfig::default();
    fx.cursor_mut().glow.note_synthetic_typed(t, 1);
    let mut input = fx_input(t);
    input.cur = Some((2, 12));
    let _ = fx.cursor_step(&CursorStep::new(input, &dark, TRAIL), std::iter::empty());
    assert_eq!(
        held(&fx),
        (None, (None, 0), true),
        "the first dark step forgets the yield"
    );
}

/// `new` is the literal a grid pass with no extras is, and each setter moves only its own
/// field (§15.1 P0.1).
#[test]
fn a_word_grid_built_by_new_is_the_literal() {
    let now = Instant::now();
    let lexicon = Lexicon::with_languages(&["en"]);
    let cfg = DecoConfig::default();
    let cells = vec![vec![RenderCell::default(); 80]; 24];
    let line_sizes = vec![LineSize::default(); 24];
    let selection = aterm_core::selection::TextSelection::new();
    let sel = SelView {
        sel: &selection,
        display_offset: 0,
    };
    let plain = WordGrid {
        now,
        lexicon: &lexicon,
        cfg: &cfg,
        geom: GEOM,
        cells: &cells,
        line_sizes: &line_sizes,
        base_y: 0,
        default_bg: 0,
        epoch: 0,
        rescan: None,
        companion_at: None,
        sel,
        focused: true,
    };
    let new = WordGrid::new(now, &lexicon, &cfg, GEOM, &cells, &line_sizes, sel);
    assert_eq!(grid_words(&new), grid_words(&plain));
    let rescan = WordRescan {
        birth: now,
        cursor: Some((1, 2)),
    };
    let companion = CompanionOnGlass::at_cell((1, 2));
    let every = WordGrid {
        now,
        lexicon: &lexicon,
        cfg: &cfg,
        geom: GEOM,
        cells: &cells,
        line_sizes: &line_sizes,
        base_y: -7,
        default_bg: 0x00F6_F7F9,
        epoch: 11,
        rescan: Some(rescan),
        companion_at: Some(companion),
        sel,
        focused: false,
    };
    let built = WordGrid::new(now, &lexicon, &cfg, GEOM, &cells, &line_sizes, sel)
        .with_base_y(-7)
        .with_default_bg(0x00F6_F7F9)
        .with_epoch(11)
        .with_rescan(Some(rescan))
        .with_companion_at(Some(companion))
        .with_focused(false);
    assert_eq!(grid_words(&built), grid_words(&every));
    assert_ne!(
        grid_words(&plain),
        grid_words(&every),
        "control: the setters moved the pass off its defaults"
    );
}

/// `new` is the literal an unshed focused composed pass is, and each setter moves only its own
/// field (§15.1 P0.1).
#[test]
fn word_panes_built_by_new_are_the_literal() {
    let now = Instant::now();
    let lexicon = Lexicon::with_languages(&["en"]);
    let (cfg, tick_cfg) = (DecoConfig::default(), DecoConfig::default());
    let plain = WordPanes {
        now,
        lexicon: &lexicon,
        cfg: &cfg,
        tick_cfg: &tick_cfg,
        load_shed: false,
        word_focus: true,
        window_focused: true,
        companion_at: None,
    };
    let new = WordPanes::new(now, &lexicon, &cfg, &tick_cfg);
    assert_eq!(panes_words(&new), panes_words(&plain));
    let companion = CompanionOnGlass::at_cell((3, 4));
    let every = WordPanes {
        now,
        lexicon: &lexicon,
        cfg: &cfg,
        tick_cfg: &tick_cfg,
        load_shed: true,
        word_focus: false,
        window_focused: false,
        companion_at: Some(companion),
    };
    let built = WordPanes::new(now, &lexicon, &cfg, &tick_cfg)
        .with_load_shed(true)
        .with_word_focus(false)
        .with_window_focused(false)
        .with_companion_at(Some(companion));
    assert_eq!(panes_words(&built), panes_words(&every));
    assert_ne!(
        panes_words(&plain),
        panes_words(&every),
        "control: the setters moved the pass off its defaults"
    );
}
