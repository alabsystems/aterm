// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::trick_flash::FLASH_MS;

fn tick(wd: &mut WordDecorations, now: Instant, ink: &mut Vec<InkCell>) -> u64 {
    let (mut out, mut free, mut nova) = (Vec::new(), Vec::new(), Vec::new());
    wd.tick(
        now,
        &DecoConfig::default(),
        EffectGeom::default(),
        None,
        None,
        true,
        &mut out,
        ink,
        &mut free,
        &mut nova,
    )
}

fn flash(term: &mut Terminal, wd: &mut WordDecorations, typed: &str, word: &str, t0: Instant) {
    let (lex, c) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
    term.process(typed.as_bytes());
    wd.rescan(term, 4, 40, &lex, &c, 1, t0);
    wd.note_trick_typed(t0, None, word, 0);
    term.process(b" ");
    wd.rescan(term, 4, 40, &lex, &c, 2, t0);
}

#[test]
fn on_paper_a_glowing_word_keeps_its_letters_over_a_trails_band() {
    use crate::color_math::contrast_ratio;
    let (fg, page) = (0x001F_2328_u32, 0x00FF_FFFF_u32);
    let band = (0u32..=255)
        .map(|v| v * 0x0001_0101)
        .find(|&g| contrast_ratio(fg, g) >= 5.0)
        .expect("a grey reads the text at 5:1");
    let t0 = Instant::now();
    let (lex, c) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
    let mut term = Terminal::new(4, 40);
    term.process(b"\x1b[38;2;31;35;40;48;2;255;255;255m$ kitty \x1b[0m");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 4, 40, &lex, &c, 1, t0);
    let env = |ms: u64| feline_glow_env(ms, FELINE_GLOW_MS, FELINE_GLOW_AMP);
    let lift_of = |rgb: u32| -> f32 {
        [16, 8, 0]
            .map(|sh| ((rgb >> sh) & 0xff) as f32 - ((fg >> sh) & 0xff) as f32)
            .iter()
            .sum()
    };
    let mut lifts = Vec::new();
    let mut ink = Vec::new();
    for ms in (0..FELINE_GLOW_MS).step_by(10) {
        tick(&mut wd, t0 + Duration::from_millis(ms), &mut ink);
        for cell in ink.iter().filter(|cell| (2..=6).contains(&cell.col)) {
            let rgb = rgb3_to_u32(cell.color);
            let over = contrast_ratio(rgb, band);
            assert!(
                over >= 4.5,
                "{ms} ms col {}: {over:.2}:1 over the band",
                cell.col
            );
            if cell.col == 2 {
                lifts.push((env(ms), lift_of(rgb)));
            }
        }
    }
    let peak = (0..FELINE_GLOW_MS).map(env).fold(0.0_f32, f32::max);
    let top = lifts.iter().map(|&(_, l)| l).fold(0.0_f32, f32::max);
    assert!(top >= 15.0, "the word still lifts on paper: {top}");
    for &(e, l) in &lifts {
        assert!(
            (l / top - e / peak).abs() <= 0.15,
            "the lift follows the envelope: {:.2} of its top at {:.2} of the peak",
            l / top,
            e / peak
        );
    }
    let unbound = mix_rgb(fg, mix_rgb(fg, page, FELINE_GLOW_LIFT), peak);
    assert!(
        contrast_ratio(unbound, band) < 4.5,
        "control: the unbounded lift reads {:.2}:1 over the band",
        contrast_ratio(unbound, band)
    );
    let (dark_fg, dark_bg) = (0x00E6_EDF3_u32, 0x000D_1117_u32);
    let lit = mix_rgb(dark_fg, page, FELINE_GLOW_LIFT);
    assert_eq!(
        paper_glow_scale(dark_fg, lit, dark_bg, FELINE_GLOW_AMP).to_bits(),
        1.0_f32.to_bits()
    );
}

#[test]
fn a_wordless_screen_with_no_flash_still_returns_fp_zero_and_disarms() {
    let t0 = Instant::now();
    let (lex, c) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
    let mut term = Terminal::new(4, 40);
    term.process(b"$ sit");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 4, 40, &lex, &c, 1, t0);
    assert!(wd.occ.is_empty(), "`sit` is nobody's lexicon word");
    let stale = InkCell {
        row: 0,
        col: 0,
        color: [1, 2, 3],
    };
    for pending in [false, true] {
        if pending {
            wd.note_trick_typed(t0, None, "sit", 0);
        }
        wd.active_until = Some(t0 + Duration::from_secs(5));
        let mut ink = vec![stale];
        assert_eq!(tick(&mut wd, t0, &mut ink), 0, "pending = {pending}");
        assert!(ink.is_empty(), "pending = {pending}");
        assert_eq!(wd.active_until, None, "pending = {pending}");
    }
}

#[test]
fn beside_other_ink_the_flash_extends_the_deadline_and_never_shortens_it() {
    let t0 = Instant::now();
    let mut term = Terminal::new(4, 40);
    let mut wd = WordDecorations::default();
    flash(&mut term, &mut wd, "$ kitty sit", "sit", t0);
    assert!(
        !wd.occ.is_empty(),
        "`kitty` is an occurrence: the tail hook"
    );
    let mut ink = Vec::new();
    tick(&mut wd, t0 + Duration::from_millis(300), &mut ink);
    let glow_end = t0 + Duration::from_millis(FELINE_GLOW_MS);
    assert!(FELINE_GLOW_MS > u64::from(FLASH_MS));
    assert_eq!(wd.active_until, Some(glow_end), "the longer window stands");
    assert!(ink.iter().any(|cell| (8..=10).contains(&cell.col)));
    let later = t0 + Duration::from_secs(2);
    tick(&mut wd, later, &mut ink);
    assert_eq!(wd.active_until, None);
    let (lex, c) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
    term.process(b"jump");
    wd.rescan(&term, 4, 40, &lex, &c, 3, later);
    wd.note_trick_typed(later, None, "jump", 0);
    term.process(b" ");
    wd.rescan(&term, 4, 40, &lex, &c, 4, later);
    tick(&mut wd, later + Duration::from_millis(16), &mut ink);
    assert_eq!(
        wd.active_until,
        Some(later + Duration::from_millis(u64::from(FLASH_MS)))
    );
    let flashed: Vec<u16> = ink.iter().map(|cell| cell.col).collect();
    assert_eq!(flashed, [12, 13, 14, 15]);
}

#[test]
fn reset_clears_a_live_trick_flash_on_the_next_tick() {
    let t0 = Instant::now();
    for hard in [false, true] {
        let mut term = Terminal::new(4, 40);
        let mut wd = WordDecorations::default();
        flash(&mut term, &mut wd, "$ sit", "sit", t0);
        let mut ink = Vec::new();
        tick(&mut wd, t0 + Duration::from_millis(200), &mut ink);
        assert_eq!(ink.len(), 3);
        assert!(wd.is_active(t0 + Duration::from_millis(200)));
        if hard {
            wd.hard_reset_words();
        } else {
            wd.reset();
        }
        let now = t0 + Duration::from_millis(216);
        assert_eq!(tick(&mut wd, now, &mut ink), 0, "hard = {hard}");
        assert!(
            ink.is_empty(),
            "stale flash ink must clear on the next tick"
        );
        assert!(!wd.is_active(now));
        assert_eq!(wd.trick_flash_phase(now), TrickFlashPhase::Idle);
        let (lex, c) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
        wd.rescan(&term, 4, 40, &lex, &c, 3, now);
        assert_eq!(tick(&mut wd, now, &mut ink), 0, "a reset is not a re-arm");
        assert!(ink.is_empty());
    }
}

#[test]
fn a_frozen_engine_neither_finds_nor_paints_the_flash() {
    let t0 = Instant::now();
    let (lex, c) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
    let mut term = Terminal::new(4, 40);
    term.process(b"$ sit");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 4, 40, &lex, &c, 1, t0);
    wd.note_trick_typed(t0, None, "sit", 0);
    term.process(b" ");
    wd.freeze(t0);
    wd.rescan(&term, 4, 40, &lex, &c, 2, t0);
    assert_eq!(wd.trick_flash_phase(t0), TrickFlashPhase::Pending);
    let mut ink = Vec::new();
    assert_eq!(tick(&mut wd, t0, &mut ink), 0);
    assert!(ink.is_empty());
    let thawed = t0 + Duration::from_millis(100);
    wd.thaw(thawed);
    wd.rescan(&term, 4, 40, &lex, &c, 2, thawed);
    assert_eq!(wd.trick_flash_phase(thawed), TrickFlashPhase::Live);
    tick(&mut wd, thawed + Duration::from_millis(200), &mut ink);
    assert_eq!(ink.len(), 3);
}

#[test]
fn reset_clears_pet_ink_spans_and_tightens_burst_hint() {
    let t0 = Instant::now();
    let (lex, c) = (Lexicon::with_languages(&["en"]), DecoConfig::default());
    let mut term = Terminal::new(4, 40);
    term.process(b"kitty");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 4, 40, &lex, &c, 1, t0);
    wd.burst_hint = Some(t0 + Duration::from_secs(5));
    assert!(!wd.pet_ink().0.is_empty());
    assert!(wd.burst_hint.is_some());

    wd.reset();
    assert_eq!(wd.pet_ink(), (&[][..], None));
    assert_eq!(wd.burst_hint, None);
}

#[test]
fn custom_kitty_sprite_skips_exact_size_resample_and_reuses_cache_across_reloads() {
    let mut wd = WordDecorations::default();
    let geom = EffectGeom {
        cols: 80,
        rows: 24,
        cell_w: 10,
        cell_h: 20,
    };
    let frame = KittyCursorFrame {
        geom,
        cursor: (2, 2),
        look: KittyLook::default(),
        colors: CatColorKey::default(),
        bob: 0.0,
        lift: 0.0,
        alpha: 255,
        pose: crate::kitty_cursor::CatPose::STILL,
        sing: 0.0,
        notes: [None; crate::kitty_sing::MAX_NOTES],
    };
    let mut free = Vec::new();

    let exact_rgba: Arc<[u8]> = vec![200u8; 36 * 36 * 4].into();
    wd.set_kitty_sprite_source(KittySpriteSource::Custom {
        source_fp: 0x1111,
        w: 36,
        h: 36,
        rgba: exact_rgba,
    });
    wd.begin_host_frame();
    assert!(wd.kitty_cursor(frame, &mut free).is_some());
    assert_eq!(
        wd.kitty_custom.as_ref().unwrap().cache_rgba.capacity(),
        0,
        "exact-size custom sprite must blit directly without allocating resample cache"
    );

    let small_a: Arc<[u8]> = vec![120u8; 8 * 8 * 4].into();
    wd.set_kitty_sprite_source(KittySpriteSource::Custom {
        source_fp: 0x2222,
        w: 8,
        h: 8,
        rgba: small_a,
    });
    wd.begin_host_frame();
    assert!(wd.kitty_cursor(frame, &mut free).is_some());
    let cust = wd.kitty_custom.as_ref().unwrap();
    assert_eq!(cust.cache_size, (36, 36));
    let ptr_a = cust.cache_rgba.as_ptr();
    let cap_a = cust.cache_rgba.capacity();
    assert!(cap_a >= 36 * 36 * 4);

    let small_b: Arc<[u8]> = vec![180u8; 8 * 8 * 4].into();
    wd.set_kitty_sprite_source(KittySpriteSource::Custom {
        source_fp: 0x3333,
        w: 8,
        h: 8,
        rgba: small_b,
    });
    wd.begin_host_frame();
    assert!(wd.kitty_cursor(frame, &mut free).is_some());
    let cust_b = wd.kitty_custom.as_ref().unwrap();
    assert_eq!(cust_b.cache_rgba.as_ptr(), ptr_a);
    assert_eq!(cust_b.cache_rgba.capacity(), cap_a);
}
