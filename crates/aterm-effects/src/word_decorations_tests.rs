// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A sprite's visible window `(x, y, w, h)` and the art row it starts at
/// (a 1:1 peek's skipped rows).
fn window(s: &FreeSprite) -> (i32, i32, u16, u16, u16) {
    let v = s.visible();
    (
        v.x0,
        v.y0,
        (v.x1 - v.x0) as u16,
        (v.y1 - v.y0) as u16,
        (v.y0 - s.y) as u16,
    )
}

#[test]
fn an_ambient_cat_on_the_flying_head_yields_to_it() {
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 6,
        cols: 20,
    };
    let cell = (3u16, 0u16);
    let wd = WordDecorations::default();
    let head = wd
        .kitty_cursor_footprint(still_layout(geom, cell))
        .expect("the head is placeable on a 20-column grid");
    let head_rect = (
        head.x,
        head.x + i32::from(head.w),
        head.y,
        head.y + i32::from(head.h),
    );
    assert!(
        head.x >= i32::from(geom.cell_w),
        "the head leads the caret column by more than a whole cell (x = {})",
        head.x
    );

    let (cx, cw) = (head.x, i32::from(head.w));
    let (ctop, cbot) = (head.y, head.y + i32::from(head.h));

    let fixed = CompanionOnGlass::at_head(cell, head_rect).footprint(geom);
    assert!(
        fixed.stacked_on(cx, cw, ctop, cbot),
        "a cat drawn on the flying head must yield to it"
    );

    let caret_band_only = CompanionOnGlass::at_cell(cell).footprint(geom);
    assert!(
        !caret_band_only.stacked_on(cx, cw, ctop, cbot),
        "the caret band alone cannot see the head — this WAS the bug"
    );

    assert!(
        fixed.stacked_on(2, 20, 58, 82),
        "a cat on the caret cell still yields"
    );
    assert!(
        caret_band_only.stacked_on(2, 20, 58, 82),
        "non-vacuous: the caret band is what catches the caret cat"
    );

    assert!(
        !fixed.stacked_on(8, 33, 76, 104),
        "the away cat still peeks beside the companion"
    );

    let pet = CompanionOnGlass::at_body(Some(cell), (100, 128, 40, 74)).footprint(geom);
    assert!(
        pet.stacked_on(100, 28, 40, 74),
        "a cat on the pet's body yields to it"
    );
    assert!(
        !pet.stacked_on(2, 20, 58, 82),
        "the pet claims no caret band"
    );
}

#[test]
fn a_cheers_motes_on_paper_keep_their_hue_at_the_marks_contrast() {
    use crate::kitty_pet::{PetMoteKind, PetMoteSprite};
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 12,
        cols: 40,
    };
    let mut pet =
        crate::kitty_pet::PetBrain::default().tick_static_capture(crate::kitty_pet::PetSense {
            caret_drawn: true,
            now: Instant::now(),
            caret: Some((6, 10)),
            rows: geom.rows,
            cols: geom.cols,
            cell_w: geom.cell_w,
            cell_h: geom.cell_h,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
            wrapped: false,
        });
    assert!(pet.alpha > 0, "fixture: the pet is on glass");
    for (k, kind) in [PetMoteKind::Note, PetMoteKind::Heart, PetMoteKind::Note]
        .into_iter()
        .enumerate()
    {
        pet.motes[k] = Some(PetMoteSprite {
            kind,
            col: 14.0 + k as f32,
            row: 4.0,
            scale: 1.0,
            rot: 0.4,
            alpha: 255,
            rgb: None,
        });
    }
    let tints = |page: Option<u32>| {
        let mut wd = WordDecorations::default();
        let mut free = Vec::new();
        for _ in 0..3 {
            free.clear();
            wd.begin_host_frame();
            let _ = wd.pet_cursor(
                PetCursorFrame {
                    geom,
                    colors: CatColorKey::default(),
                    coat: 0,
                    iris: 0,
                    pet,
                    page,
                },
                &mut free,
            );
        }
        free.iter()
            .filter(|s| s.tint != 0x00FF_FFFF)
            .map(|s| s.tint)
            .collect::<Vec<_>>()
    };
    let contrast = |a: u32, b: u32| {
        let (ya, yb) = (relative_luminance(a), relative_luminance(b));
        (ya.max(yb) + 0.05) / (ya.min(yb) + 0.05)
    };
    let hue = |rgb: u32| crate::color_math::rgb2hsv(rgb).0;
    let dark = tints(None);
    assert_eq!(dark.len(), 3, "fixture: the three motes drew: {dark:06x?}");
    for (k, &tint) in dark.iter().enumerate() {
        let kind = pet.motes[k].map(|m| m.kind).unwrap();
        let want = WordDecorations::pet_mote_tint(kind, CatColorKey::default());
        assert_eq!(tint, want, "a dark ground keeps the per-kind tint");
    }
    for (name, bg) in [("white", 0x00F8_FAFDu32), ("cream", 0x00F9_F3E6)] {
        let paper = tints(Some(bg));
        assert_eq!(paper.len(), 3, "{name}");
        for (&ink, &tint) in paper.iter().zip(&dark) {
            assert!(
                contrast(ink, bg) >= 4.5 - 1e-3,
                "{name}: {ink:06x} on {bg:06x} reads at {:.2}:1",
                contrast(ink, bg)
            );
            let d = (hue(ink) - hue(tint)).rem_euclid(360.0);
            assert!(
                d.min(360.0 - d) <= 12.0,
                "{name}: {ink:06x} keeps {tint:06x}'s hue ({} vs {})",
                hue(ink),
                hue(tint)
            );
        }
    }
}

#[test]
fn one_host_frame_admits_exactly_one_companion_body() {
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 6,
        cols: 20,
    };
    let now = Instant::now();
    let cell = (3u16, 4u16);
    let head_frame = |wd: &mut WordDecorations, free: &mut Vec<FreeSprite>| {
        let layout = still_layout(geom, cell);
        let placement = wd
            .resolve_kitty_cursor_placement(layout, settled_at(now))
            .expect("placeable companion");
        wd.kitty_cursor_at_placement(still_frame(geom, cell), placement, free)
    };

    let mut wd = WordDecorations::default();
    wd.begin_host_frame();
    let mut alone = Vec::new();
    assert!(
        head_frame(&mut wd, &mut alone).is_some() && !alone.is_empty(),
        "non-vacuous: the flying head emits a body in a frame of its own"
    );

    wd.begin_host_frame();
    let mut free = Vec::new();
    let pet =
        crate::kitty_pet::PetBrain::default().tick_static_capture(crate::kitty_pet::PetSense {
            caret_drawn: true,
            now,
            caret: Some(cell),
            rows: geom.rows,
            cols: geom.cols,
            cell_w: geom.cell_w,
            cell_h: geom.cell_h,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
            wrapped: false,
        });
    assert!(pet.alpha > 0, "non-vacuous: the pet is on glass");
    assert!(
        wd.pet_cursor(
            PetCursorFrame {
                geom,
                colors: CatColorKey::default(),
                coat: 0,
                iris: 0,
                pet,
                page: None,
            },
            &mut free,
        )
        .is_some(),
        "the pet claims the frame"
    );
    let after_pet = free.len();
    assert!(after_pet > 0, "non-vacuous: the pet body landed");
    assert!(
        head_frame(&mut wd, &mut free).is_none(),
        "the flying head may not add a second body to the pet's frame"
    );
    assert_eq!(
        free.len(),
        after_pet,
        "not one sprite — notes included — from the refused companion"
    );

    wd.begin_host_frame();
    let mut next = Vec::new();
    assert!(
        head_frame(&mut wd, &mut next).is_some(),
        "the next frame admits a companion again"
    );
}

fn cfg() -> DecoConfig {
    DecoConfig {
        profanity: true,
        feline: true,
        canine: true,
        emphasis: true,
        ink_enabled: true,
        ink_strength: 0.75,
        ink_sweep_ms: 2200,
        ink_loop: false,
        reduced_motion: false,
        suppress_in_alt_screen: false,
        allow_bare_cat: true,
        cjk_single_char: false,
        feline_style: FelineStyle::Cat,
        feline_magic: true,
        profanity_style: ProfanityStyle::Sparkle,
        profanity_magic: true,
        supernova_chance: 30,
        spec_table: SpecTable::default(),
        palette: vec![0x00FF_D447, 0x00FF_7CE5],
        density: 3,
        anim_ms: 2000,
        jitter: 2,
        intensity: 0.85,
        glyphs: vec![DecoGlyph::Star4, DecoGlyph::Dot],
        ignore: HashSet::new(),
    }
}

fn occ(class: Class, now: Instant) -> Occurrence {
    Occurrence {
        row: 0,
        start_col: 2,
        end_col: 5,
        class,
        langs: LangSet::EMPTY,
        form_id: FormId::UNKNOWN,
        seed: 0xDEAD_BEEF,
        ident: mix(0xDEAD_BEEF),
        appeared: now,
        genome: Genome { gkey: 0, magic: 0 },
        ink_base: 0,
        ink_cells: 0,
        ink_bg: [0; 3],
        cat_colors: CatColorKey::default(),
        cat_text_clear: true,
        cat_peek_down: false,
        dec_line: false,
        inert: false,
        spec: class_default_spec(class, &cfg()),
        custom: false,
        species: None,
    }
}

fn tick_deco(
    wd: &mut WordDecorations,
    now: Instant,
    cfg: &DecoConfig,
    out: &mut Vec<WordDecoration>,
) -> u64 {
    let mut ink = Vec::new();
    let mut fr = Vec::new();
    let mut nova = Vec::new();
    wd.tick(
        now,
        cfg,
        EffectGeom::default(),
        None,
        None,
        true,
        out,
        &mut ink,
        &mut fr,
        &mut nova,
    )
}

#[test]
fn profanity_sparkles_then_self_terminates() {
    let now = Instant::now();
    let mut wd = WordDecorations {
        occ: vec![occ(Class::Profanity, now)],
        cols: 80,
        have_scanned: true,
        ..WordDecorations::default()
    };
    let c = cfg();
    let mut out = Vec::new();

    let fp1 = tick_deco(&mut wd, now, &c, &mut out);
    assert_eq!(
        out.len(),
        c.density as usize,
        "sparks == density while animating"
    );
    assert!(out.iter().all(|d| matches!(d.blend, DecoBlend::Add)));
    let fp2 = tick_deco(&mut wd, now, &c, &mut out);
    assert_ne!(fp1, fp2, "animating sparkle fingerprint changes each frame");
    assert!(wd.is_active(now));

    let fading = now + Duration::from_millis(c.anim_ms + 50);
    tick_deco(&mut wd, fading, &c, &mut out);
    assert_eq!(out.len(), 1, "the residual spark is fading");
    let alpha_early = out[0].alpha;
    assert!(wd.is_active(fading), "the fade is still animating");
    let later = now + Duration::from_millis(c.anim_ms + 1500);
    tick_deco(&mut wd, later, &c, &mut out);
    assert_eq!(out.len(), 1);
    assert!(
        out[0].alpha < alpha_early,
        "the residual fades monotonically"
    );
    let done = now + Duration::from_millis(c.anim_ms + 2100);
    let fpa = tick_deco(&mut wd, done, &c, &mut out);
    assert!(out.is_empty(), "the residual self-terminates to zero decos");
    let fpb = tick_deco(&mut wd, done + Duration::from_millis(16), &c, &mut out);
    assert_eq!(fpa, fpb, "post-fade fp is stable");
    assert!(!wd.is_active(done), "must self-terminate after the fade");
}

#[test]
fn reduced_motion_forces_static() {
    let now = Instant::now();
    let mut wd = WordDecorations {
        occ: vec![occ(Class::Profanity, now)],
        cols: 80,
        have_scanned: true,
        ..WordDecorations::default()
    };
    let mut c = cfg();
    c.reduced_motion = true;
    let mut out = Vec::new();
    tick_deco(&mut wd, now, &c, &mut out);
    assert_eq!(out.len(), 1, "reduced motion = single steady spark");
    assert!(!wd.is_active(now));
}

#[test]
fn fix_never_triggers_or_inherits_profanity_rainbow() {
    let lex = Lexicon::with_languages(&["en"]);
    let mut c = cfg();
    c.profanity_style = ProfanityStyle::Rainbow;
    let now = Instant::now();
    let mut term = Terminal::new(4, 48);
    let mut wd = WordDecorations::default();

    for (i, byte) in b"fix".iter().copied().enumerate() {
        term.process(&[byte]);
        wd.rescan(&term, 4, 48, &lex, &c, i as u64 + 1, now);
        assert!(
            wd.occ.iter().all(|o| o.class != Class::Profanity),
            "incremental prefix {:?} classified as profanity",
            &b"fix"[..=i]
        );
    }

    for token in ["FIX", "fix!", "prefix", "suffix", "fixes", "ﬁx"] {
        let mut candidate = Terminal::new(2, 48);
        candidate.process(token.as_bytes());
        let mut scan = WordDecorations::default();
        scan.rescan(&candidate, 2, 48, &lex, &c, 1, now);
        assert!(
            scan.occ.iter().all(|o| o.class != Class::Profanity),
            "near-miss {token:?} classified as profanity"
        );
    }

    term.process(b"\rfuck\x1b[K");
    wd.rescan(&term, 4, 48, &lex, &c, 10, now);
    assert!(wd.occ.iter().any(|o| o.class == Class::Profanity));

    term.process(b"\rfix\x1b[K");
    wd.rescan(&term, 4, 48, &lex, &c, 11, now + Duration::from_millis(16));
    assert!(wd.occ.iter().all(|o| o.class != Class::Profanity));
    let (mut deco, mut ink, mut free, mut nova) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let fp = wd.tick(
        now + Duration::from_millis(32),
        &c,
        EffectGeom::default(),
        None,
        None,
        true,
        &mut deco,
        &mut ink,
        &mut free,
        &mut nova,
    );
    assert_eq!(
        fp, 0,
        "replacing fuck with fix leaves no animated fingerprint"
    );
    assert!(deco.is_empty() && ink.is_empty() && free.is_empty() && nova.is_empty());
}

#[test]
fn ambiguous_future_prefix_waits_for_delimiter_in_ro_and_all() {
    let mut c = cfg();
    c.profanity_style = ProfanityStyle::Rainbow;
    let now = Instant::now();

    for language in ["ro", "all"] {
        let lex = Lexicon::with_languages(&[language]);
        let mut term = Terminal::new(2, 48);
        let mut wd = WordDecorations::default();

        for (i, byte) in b"future".iter().copied().enumerate() {
            term.process(&[byte]);
            wd.rescan(&term, 2, 48, &lex, &c, i as u64 + 1, now);
            assert!(
                wd.occ.iter().all(|o| o.class != Class::Profanity),
                "{language} incremental future prefix {:?} classified as profanity",
                &b"future"[..=i]
            );
        }

        for token in ["fu", "fix", "future"] {
            let mut candidate = Terminal::new(2, 48);
            candidate.process(token.as_bytes());
            let mut scan = WordDecorations::default();
            scan.rescan(&candidate, 2, 48, &lex, &c, 20, now);
            assert!(
                scan.occ.iter().all(|o| o.class != Class::Profanity),
                "{language} exact-token path classified {token:?} as profanity"
            );
        }

        for token in ["fut ", "futu "] {
            let mut settled = Terminal::new(2, 48);
            settled.process(token.as_bytes());
            let mut scan = WordDecorations::default();
            scan.rescan(&settled, 2, 48, &lex, &c, 21, now);
            assert!(
                scan.occ.iter().any(|o| o.class == Class::Profanity),
                "{language} delimiter must settle exact Romanian form {token:?}"
            );
        }

        let mut profanity = Terminal::new(2, 48);
        profanity.process(b"fuck");
        let mut scan = WordDecorations::default();
        scan.rescan(&profanity, 2, 48, &lex, &c, 30, now);
        assert!(
            scan.occ.iter().any(|o| o.class == Class::Profanity),
            "{language} exact fuck must retain the immediate profanity effect"
        );

        let geom = EffectGeom {
            cell_w: 8,
            cell_h: 16,
            rows: 2,
            cols: 48,
        };
        let mut term = Terminal::new(2, 48);
        let mut wd = WordDecorations::default();
        let mut snap = aterm_core::render::RenderInput::default();
        for (epoch, (bytes, settled)) in [(b"fut".as_slice(), false), (b" ".as_slice(), true)]
            .into_iter()
            .enumerate()
        {
            term.process(bytes);
            term.cell_frame_into(&mut snap, 2, 48);
            let cursor = term.cursor();
            wd.rescan_from_cells_with_geom_at_cursor(
                &snap.cells,
                &snap.line_sizes,
                2,
                48,
                &lex,
                &c,
                epoch as u64 + 1,
                now,
                geom,
                snap.default_bg,
                Some((cursor.row, cursor.col)),
            );
            assert_eq!(
                wd.occ.iter().any(|o| o.class == Class::Profanity),
                settled,
                "{language} snapshot path after {bytes:?}"
            );
        }
    }
}

#[test]
fn fuc_is_ordinary_and_only_complete_fuck_activates() {
    let lex = Lexicon::with_languages(&["all"]);
    let mut c = cfg();
    c.profanity_style = ProfanityStyle::Rainbow;
    c.supernova_chance = 0;
    let t0 = Instant::now();
    let model = aterm_models_misc::exact_profanity_completion_model();
    let mut state = model.init_state();
    let mut term = Terminal::new(2, 48);
    let mut wd = WordDecorations::default();

    for (epoch, (byte, action)) in [(b'f', "TypeF"), (b'u', "TypeU"), (b'c', "TypeC")]
        .into_iter()
        .enumerate()
    {
        term.process(&[byte]);
        assert!(model.fire(action, &mut state));
        wd.rescan(
            &term,
            2,
            48,
            &lex,
            &c,
            epoch as u64 + 1,
            t0 + Duration::from_millis(epoch as u64 * 8),
        );
        assert!(wd.occ.iter().all(|o| o.class != Class::Profanity));
        assert_eq!(state["active"], 0);
    }

    let (mut deco, mut ink, mut free, mut nova) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    wd.tick(
        t0 + Duration::from_millis(16),
        &c,
        EffectGeom::default(),
        None,
        None,
        true,
        &mut deco,
        &mut ink,
        &mut free,
        &mut nova,
    );
    let prefix_cues = wd.drain_curse_cues().count();
    assert_eq!(prefix_cues, 0, "`fuc` emitted a curse cue");
    assert_eq!(state["episode"], prefix_cues as i64);

    term.process(b"k");
    assert!(model.fire("TypeK", &mut state));
    let completed_at = t0 + Duration::from_millis(24);
    wd.note_typed_edit(completed_at, None);
    wd.rescan(&term, 2, 48, &lex, &c, 4, completed_at);
    assert!(wd.occ.iter().any(|o| o.class == Class::Profanity));
    assert_eq!(state["active"], 1);
    wd.tick(
        completed_at,
        &c,
        EffectGeom::default(),
        None,
        None,
        true,
        &mut deco,
        &mut ink,
        &mut free,
        &mut nova,
    );
    let completion_cues = wd.drain_curse_cues().count();
    assert_eq!(completion_cues, 1, "complete `fuck` must cue once");
    assert_eq!(state["episode"], completion_cues as i64);

    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut early = buggy.init_state();
    for action in ["TypeF", "TypeU", "TypeC"] {
        assert!(buggy.fire(action, &mut early), "{action}");
    }
    assert_ne!(early["episode"], prefix_cues as i64);
    assert!(buggy.fire("TypeK", &mut early));
    assert_ne!(early["episode"], completion_cues as i64);
    assert!(!model.check_invariant("CompletionCreatesExactlyOneEpisode", &early));
}

#[test]
fn passive_output_at_terminal_cursor_stays_spent() {
    let lex = Lexicon::with_languages(&["all"]);
    let mut c = cfg();
    c.profanity_style = ProfanityStyle::Rainbow;
    let t0 = Instant::now();
    let mut output = Terminal::new(4, 48);
    output.process(b"fuck");
    assert_eq!(output.cursor().col, 4, "word must end at terminal cursor");

    let mut wd = WordDecorations::default();
    wd.rescan(&output, 4, 48, &lex, &c, 1, t0);
    let ident = wd
        .occ
        .iter()
        .find(|occ| occ.class == Class::Profanity)
        .expect("initial profanity")
        .ident;
    wd.persist
        .get_mut(&ident)
        .expect("initial episode")
        .nova_done = true;

    let blank = Terminal::new(4, 48);
    wd.rescan(&blank, 4, 48, &lex, &c, 2, t0 + Duration::from_secs(16));
    assert!(wd.persist.is_empty(), "fixture must cross identity expiry");
    wd.rescan(&output, 4, 48, &lex, &c, 3, t0 + Duration::from_secs(17));

    let restored = wd
        .occ
        .iter()
        .find(|occ| occ.class == Class::Profanity)
        .expect("restored profanity");
    assert!(restored.inert, "passive cursor-end output rearmed");
    assert!(wd.persist[&restored.ident].born_done);
    assert!(wd.persist[&restored.ident].nova_done);
}

#[test]
fn repeated_complete_fuck_rearms_while_fuc_stays_ordinary() {
    let lex = Lexicon::with_languages(&["all"]);
    let mut c = cfg();
    c.profanity_style = ProfanityStyle::Rainbow;
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();

    let mut initial = Terminal::new(4, 48);
    initial.process(b"fuck");
    wd.rescan(&initial, 4, 48, &lex, &c, 1, t0);
    let initial_ident = wd
        .occ
        .iter()
        .find(|occ| occ.class == Class::Profanity)
        .expect("initial profanity")
        .ident;
    wd.persist
        .get_mut(&initial_ident)
        .expect("initial episode")
        .nova_done = true;
    let births_before = wd.birth_seq;

    for (i, text) in ["f", "fi", "fix"].into_iter().enumerate() {
        let mut typing = Terminal::new(4, 48);
        typing.process(text.as_bytes());
        wd.rescan(
            &typing,
            4,
            48,
            &lex,
            &c,
            i as u64 + 2,
            t0 + Duration::from_millis(16 * (i as u64 + 1)),
        );
        assert!(wd.occ.iter().all(|occ| occ.class != Class::Profanity));
    }
    assert!(wd.persist[&initial_ident].continuity_tainted);

    let first_retype_at = t0 + Duration::from_millis(64);
    let mut live = Terminal::new(4, 48);
    live.process(b"fuc");
    wd.rescan(&live, 4, 48, &lex, &c, 5, first_retype_at);
    assert!(wd.occ.iter().all(|occ| occ.class != Class::Profanity));
    assert_eq!(
        wd.birth_seq, births_before,
        "`fuc` must not create an episode"
    );

    live.process(b"k");
    wd.note_typed_edit(first_retype_at + Duration::from_millis(8), None);
    wd.rescan(
        &live,
        4,
        48,
        &lex,
        &c,
        6,
        first_retype_at + Duration::from_millis(8),
    );
    let completed = wd
        .occ
        .iter()
        .find(|occ| occ.class == Class::Profanity)
        .expect("completed fuck")
        .clone();
    assert_eq!(
        completed.appeared,
        first_retype_at + Duration::from_millis(8)
    );
    assert_eq!(
        wd.birth_seq,
        births_before + 1,
        "completion is one fresh birth"
    );
    wd.persist
        .get_mut(&completed.ident)
        .expect("completed episode")
        .nova_done = true;

    for (i, text) in ["f", "fu", "fut", "futu", "futur", "future"]
        .into_iter()
        .enumerate()
    {
        let mut typing = Terminal::new(4, 48);
        typing.process(text.as_bytes());
        wd.rescan(
            &typing,
            4,
            48,
            &lex,
            &c,
            i as u64 + 7,
            t0 + Duration::from_millis(80 + 16 * i as u64),
        );
        assert!(
            wd.occ.iter().all(|occ| occ.class != Class::Profanity),
            "provisional {text:?} classified as profanity"
        );
    }

    let second_retype_at = t0 + Duration::from_millis(192);
    let mut repeated = Terminal::new(4, 48);
    repeated.process(b"fuck");
    wd.note_typed_edit(second_retype_at, None);
    wd.rescan(&repeated, 4, 48, &lex, &c, 13, second_retype_at);
    let second = wd
        .occ
        .iter()
        .find(|occ| occ.class == Class::Profanity)
        .expect("second live fuck");
    assert_eq!(second.appeared, second_retype_at);
    assert_eq!(
        wd.birth_seq,
        births_before + 2,
        "each live retype is one birth"
    );
    assert!(!second.inert);
    assert!(!wd.persist[&second.ident].nova_done);
}

#[test]
fn fix_churn_cannot_rearm_spent_profanity_on_restore() {
    let lex = Lexicon::with_languages(&["en"]);
    let mut c = cfg();
    c.profanity_style = ProfanityStyle::Rainbow;
    let t0 = Instant::now();
    let mut original = Terminal::new(4, 48);
    original.process(b"old alpha fuck tail");
    let mut wd = WordDecorations::default();
    wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
    let old = wd
        .occ
        .iter()
        .find(|o| o.class == Class::Profanity)
        .expect("positive-control profanity")
        .clone();
    wd.persist.get_mut(&old.ident).expect("old live").nova_done = true;

    for (i, text) in [b"f".as_slice(), b"fi".as_slice(), b"fix".as_slice()]
        .into_iter()
        .enumerate()
    {
        let mut composer = Terminal::new(4, 48);
        composer.process(text);
        wd.rescan(
            &composer,
            4,
            48,
            &lex,
            &c,
            i as u64 + 2,
            t0 + Duration::from_millis(16 * (i as u64 + 1)),
        );
        assert!(wd.occ.iter().all(|o| o.class != Class::Profanity));
    }

    let mut restored_frame = Terminal::new(4, 48);
    restored_frame.process(b"STATUS fix alpha beta fuck gamma delta");
    let births_before = wd.birth_seq;
    wd.rescan(
        &restored_frame,
        4,
        48,
        &lex,
        &c,
        5,
        t0 + Duration::from_millis(64),
    );
    let restored = wd
        .occ
        .iter()
        .find(|o| o.class == Class::Profanity)
        .expect("restored profanity line");
    assert_eq!(wd.birth_seq, births_before, "no synthetic profanity birth");
    assert_eq!(restored.appeared, t0, "the spent episode transfers");
    assert!(wd.persist[&restored.ident].nova_done, "no nova re-arm");
}

#[test]
fn profanity_rainbow_opens_with_bounded_sparkles() {
    let lex = Lexicon::with_languages(&["en"]);
    let mut c = cfg();
    c.profanity_style = ProfanityStyle::Rainbow;
    c.supernova_chance = 0;
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 4,
        cols: 48,
    };
    let t0 = Instant::now();
    let mut term = Terminal::new(4, 48);
    term.process(b"fuck");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 4, 48, &lex, &c, 1, t0);
    let (mut deco, mut ink, mut free, mut nova) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());

    wd.tick(
        t0 + Duration::from_millis(RAINBOW_SPARKLE_MS / 2),
        &c,
        geom,
        None,
        None,
        true,
        &mut deco,
        &mut ink,
        &mut free,
        &mut nova,
    );
    assert_eq!(
        deco.len(),
        3,
        "the rainbow opens with a small constellation"
    );
    assert!(deco.iter().all(|d| matches!(d.blend, DecoBlend::Add)));
    assert!(
        deco.iter()
            .all(|d| matches!(d.glyph, DecoGlyph::Star4 | DecoGlyph::Star5))
    );
    assert!(!ink.is_empty(), "sparkles accompany the rainbow ink");

    wd.tick(
        t0 + Duration::from_millis(RAINBOW_SPARKLE_MS + 1),
        &c,
        geom,
        None,
        None,
        true,
        &mut deco,
        &mut ink,
        &mut free,
        &mut nova,
    );
    assert!(
        deco.is_empty(),
        "sparkles self-terminate inside the ink drift"
    );
    assert!(
        !ink.is_empty(),
        "the rainbow may continue after the stars settle"
    );
}

fn lex_with_emphasis() -> Lexicon {
    Lexicon::with_languages_and_override(
        &["en"],
        Some(
            "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"ultrathink\"]\n",
        ),
    )
    .expect("emphasis override parses")
}

#[test]
fn rescan_from_cells_matches_term_rescan() {
    let (rows, cols) = (5usize, 48usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"I love cats and say fuck sometimes\r\n");
    term.process("\u{5BBD}\u{5BBD} kitty ultrathink\r\n".as_bytes());
    term.process(b"\x1b#6cat nap\r\n");
    term.process(b"plain row, nothing to match");
    let lex = Lexicon::with_languages(&["en"]);
    let c = cfg();
    let epoch = term.damage_epoch();
    let now = Instant::now();

    let mut wd_term = WordDecorations::default();
    wd_term.rescan(&term, rows, cols, &lex, &c, epoch, now);

    let mut snap = aterm_core::render::RenderInput::default();
    term.cell_frame_into(&mut snap, rows, cols);
    let mut wd_cells = WordDecorations::default();
    wd_cells.rescan_from_cells(
        &snap.cells,
        &snap.line_sizes,
        rows,
        cols,
        &lex,
        &c,
        epoch,
        now,
    );
    assert!(
        !wd_cells.needs_rescan(epoch),
        "snapshot scan must record the epoch"
    );

    let key = |o: &Occurrence| {
        (
            o.row,
            o.start_col,
            o.end_col,
            o.class,
            o.seed,
            o.ident,
            o.appeared,
            o.genome,
            o.ink_base,
            o.ink_cells,
            o.ink_bg,
            o.dec_line,
        )
    };
    assert!(
        !wd_term.occ.is_empty(),
        "the fixture must actually match words"
    );
    assert!(
        wd_term.occ.iter().any(|o| o.dec_line),
        "the DECDWL row's match must flag dec_line"
    );
    assert!(
        wd_term.occ.iter().any(|o| o.ink_cells > 0),
        "the fixture must capture ink"
    );
    assert_eq!(
        wd_term.occ.iter().map(key).collect::<Vec<_>>(),
        wd_cells.occ.iter().map(key).collect::<Vec<_>>(),
        "snapshot scan must match the term-walking scan"
    );
    assert_eq!(
        wd_term.occ.iter().map(|o| o.cat_colors).collect::<Vec<_>>(),
        wd_cells
            .occ
            .iter()
            .map(|o| o.cat_colors)
            .collect::<Vec<_>>(),
        "snapshot scan must resolve the same local cat palettes"
    );
    assert_eq!(
        wd_term.ink_base_fg, wd_cells.ink_base_fg,
        "captured base fg must match"
    );
    assert_eq!(
        wd_term.ink_cols, wd_cells.ink_cols,
        "captured ink columns must match"
    );
}

fn deco_state(wd: &WordDecorations) -> (Vec<String>, Vec<[u8; 3]>, Vec<u16>, Vec<u64>) {
    let occ = wd
        .occ
        .iter()
        .map(|o| {
            format!(
                "{:?}{:?}",
                (
                    o.row,
                    o.start_col,
                    o.end_col,
                    o.class,
                    o.form_id,
                    o.seed,
                    o.ident,
                    o.appeared,
                    o.genome,
                ),
                (
                    o.ink_base,
                    o.ink_cells,
                    o.ink_bg,
                    o.cat_colors,
                    o.cat_text_clear,
                    o.dec_line,
                    o.inert,
                    o.custom,
                )
            )
        })
        .collect();
    let mut idents: Vec<u64> = wd.persist.keys().copied().collect();
    idents.sort_unstable();
    (occ, wd.ink_base_fg.clone(), wd.ink_cols.clone(), idents)
}

#[test]
fn scan_memo_frames_are_identical_to_an_always_rescanning_engine() {
    let (rows, cols) = (14usize, 44usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    let lex = Lexicon::with_languages(&["en"]);
    let c = cfg();
    let geom = EffectGeom {
        cell_w: 8,
        cell_h: 16,
        rows: rows as u16,
        cols: cols as u16,
    };
    let mut memoized = WordDecorations::default();
    let mut control = WordDecorations::default();
    control.scan_memo.bypass = true;
    let t0 = Instant::now();

    let steps: &[&[u8]] = &[
        b"I love cats and say fuck sometimes\r\n",
        b"a kitty naps here\r\n",
        b"plain build output, nothing to match\r\n",
        "\u{5BBD}\u{5BBD} kitty ultrathink\r\n".as_bytes(),
        b"\x1b#6cat nap\r\n",
        b"$ fu",
        b"c",
        b"k",
        b" ",
        b"and a cat",
        b"\r\n",
        b"the cat sat on the mat\r\n",
        b"the cat sat on the mat\r\n",
        b"the cat sat on the mat\r\n",
        b"scroll me one\r\nscroll me two\r\nfuck this build\r\nkitty again\r\n",
        b"more cats\r\nmore kittens\r\nmore fuck\r\n",
        b"\x1b[2J\x1b[H",
        b"kitty again\r\n",
    ];

    let mut snap = aterm_core::render::RenderInput::default();
    for (i, step) in steps.iter().enumerate() {
        term.process(step);
        term.cell_frame_into(&mut snap, rows, cols);
        let now = t0 + Duration::from_millis(16 * (i as u64 + 1));
        let cursor = (term.grid().display_offset() == 0).then(|| {
            let cur = term.cursor();
            (cur.row, cur.col)
        });
        let epoch = term.damage_epoch();
        for wd in [&mut memoized, &mut control] {
            wd.rescan_from_cells_with_geom_at_cursor(
                &snap.cells,
                &snap.line_sizes,
                rows,
                cols,
                &lex,
                &c,
                epoch,
                now,
                geom,
                0x0011_1111,
                cursor,
            );
        }
        assert_eq!(
            deco_state(&memoized),
            deco_state(&control),
            "frame {i}: the memoized rescan diverged from the full rescan"
        );
    }
    assert!(
        !memoized.occ.is_empty(),
        "the fixture must end with live decorations"
    );
    assert!(
        memoized.scan_memo.hits > u64::try_from(steps.len()).expect("small") * 4,
        "the memo must serve the bulk of the rows (hits: {}, misses: {})",
        memoized.scan_memo.hits,
        memoized.scan_memo.misses
    );
    assert_eq!(
        control.scan_memo.hits, 0,
        "the control engine must never be served from the memo"
    );
}

#[test]
fn scan_memo_retires_when_the_gates_or_the_lexicon_change() {
    let (rows, cols) = (3usize, 32usize);
    let en = Lexicon::with_languages(&["en"]);
    let en_ro = Lexicon::with_languages(&["en", "ro"]);
    let c = cfg();
    let mut ignoring = cfg();
    ignoring.ignore.insert("kitty".to_string());
    for (label, text, class, (lex_on, c_on), (lex_off, c_off)) in [
        (
            "deny list",
            "the kitty is here\r\n",
            Class::Feline,
            (&en, &c),
            (&en, &ignoring),
        ),
        (
            "lexicon",
            "totally futu here\r\n",
            Class::Profanity,
            (&en_ro, &c),
            (&en, &c),
        ),
    ] {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(text.as_bytes());
        let mut snap = aterm_core::render::RenderInput::default();
        term.cell_frame_into(&mut snap, rows, cols);
        let now = Instant::now();
        let mut wd = WordDecorations::default();
        let scans = [
            (lex_on, c_on, true),
            (lex_off, c_off, false),
            (lex_on, c_on, true),
        ];
        for (epoch, (lex, c, decorated)) in scans.into_iter().enumerate() {
            let epoch = epoch as u64 + 1;
            wd.rescan_from_cells(
                &snap.cells,
                &snap.line_sizes,
                rows,
                cols,
                lex,
                c,
                epoch,
                now,
            );
            assert_eq!(
                wd.occ.iter().any(|o| o.class == class),
                decorated,
                "{label}: scan {epoch} must follow its own inputs, not a stale memo"
            );
        }
    }
}

fn probe_class(h: u64) -> (u64, u64) {
    (h & 0xff, h >> 57)
}

fn largest_probe_class<K>(keys: &[K], hash: impl Fn(&K) -> u64) -> usize {
    let mut classes: FxHashMap<(u64, u64), usize> = FxHashMap::default();
    keys.iter()
        .map(|k| {
            let n = classes.entry(probe_class(hash(k))).or_default();
            *n += 1;
            *n
        })
        .max()
        .unwrap_or(0)
}

fn fx_flood<K: std::hash::Hash>(candidate: impl Fn(u32) -> K) -> Vec<K> {
    use std::hash::BuildHasher;
    let fx = |k: &K| aterm_hash::FxBuildHasher.hash_one(k);
    let mut flood = Vec::with_capacity(128);
    let mut i = 0u32;
    while flood.len() < 128 {
        let k = candidate(i);
        if probe_class(fx(&k)) == (0, 0) {
            flood.push(k);
        }
        i += 1;
    }
    assert_eq!(largest_probe_class(&flood, fx), flood.len());
    flood
}

#[test]
fn scan_memo_spreads_rows_precomputed_to_collide() {
    use std::hash::BuildHasher;
    let flood = fx_flood(|mut i| {
        let mut s = String::from("row ");
        for _ in 0..6 {
            s.extend(char::from_u32(u32::from(b'a') + i % 26));
            i /= 26;
        }
        s
    });

    let mut memo = ScanMemo::default();
    memo.fit(64, 0);
    for r in &flood {
        assert!(!memo.touch(r));
        memo.remember(r, &[]);
    }
    for r in &flood {
        assert!(memo.touch(r), "a remembered row is resident");
    }
    for (generation, map) in [("hot", &memo.hot), ("cold", &memo.cold)] {
        let n = largest_probe_class(&flood, |r| map.hasher().hash_one(r.as_str()));
        assert!(
            n <= 8,
            "{generation}: {n} of {} precomputed rows share one bucket and tag, \
             so a probe among them compares that many rows",
            flood.len()
        );
    }
}

#[test]
fn done_marks_spread_keys_precomputed_to_collide() {
    use std::hash::BuildHasher;
    let ident = mix(seed_of(
        10,
        Class::Feline,
        aterm_lexicon::form_hash("kitty"),
    ));
    let flood = fx_flood(|i| ident ^ mix(u64::from(i)));

    let mut marks = DoneMarkLru::default();
    for &key in &flood {
        assert_eq!(marks.insert(key).evicted, None);
    }
    for key in &flood {
        assert!(marks.contains_key(key), "a written mark is resident");
    }
    let n = largest_probe_class(&flood, |k| marks.index.hasher().hash_one(k));
    assert!(
        n <= 8,
        "{n} of {} precomputed done marks share one bucket and tag, \
         so a birth's probe among them compares that many marks",
        flood.len()
    );
}

#[test]
fn geometry_scan_samples_and_freezes_the_prospective_cat_footprint() {
    let (rows, cols) = (4usize, 20usize);
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\r\n\r\nkitty");
    let mut snap = aterm_core::render::RenderInput::default();
    term.cell_frame_into(&mut snap, rows, cols);
    let dark = [4, 4, 4];
    let light = [248, 248, 248];
    for (r, line) in snap.cells.iter_mut().enumerate() {
        for cell in line {
            cell.bg = if r == 0 { light } else { dark };
        }
    }
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: rows as u16,
        cols: cols as u16,
    };
    let lex = lex();
    let c = cfg();
    let now = Instant::now();
    let mut wd = WordDecorations::default();
    wd.rescan_from_cells_with_geom_at_cursor(
        &snap.cells,
        &snap.line_sizes,
        rows,
        cols,
        &lex,
        &c,
        1,
        now,
        geom,
        rgb3_to_u32(light),
        None,
    );
    let mixed = feline(&wd).cat_colors;
    let direct = cat_color_context_footprint(
        &snap.cells,
        geom,
        feline(&wd).row,
        feline(&wd).start_col,
        feline(&wd).end_col,
        feline(&wd).genome,
        c.feline_magic,
        rgb3_to_u32(light),
    );
    assert_eq!(mixed, direct, "rescan stores the footprint sampler's key");
    assert!(
        !mixed.dark(),
        "the dark+light footprint uses a dedicated non-dark mixed class"
    );
    assert_eq!(mixed.background, 4);

    for line in &mut snap.cells {
        for cell in line {
            cell.bg = dark;
        }
    }
    wd.rescan_from_cells_with_geom_at_cursor(
        &snap.cells,
        &snap.line_sizes,
        rows,
        cols,
        &lex,
        &c,
        2,
        now + Duration::from_millis(100),
        geom,
        rgb3_to_u32(dark),
        None,
    );
    assert_eq!(
        feline(&wd).cat_colors,
        mixed,
        "a live episode freezes its footprint-derived palette"
    );
    let mut fresh = WordDecorations::default();
    fresh.rescan_from_cells_with_geom_at_cursor(
        &snap.cells,
        &snap.line_sizes,
        rows,
        cols,
        &lex,
        &c,
        1,
        now,
        geom,
        rgb3_to_u32(dark),
        None,
    );
    assert!(
        feline(&fresh).cat_colors.dark(),
        "a fresh episode sees the now-dark full footprint"
    );
}

fn ink_at(
    wd: &mut WordDecorations,
    term: &mut Terminal,
    rows: usize,
    cols: usize,
    c: &DecoConfig,
    scanned: Instant,
    at: Instant,
) -> (Vec<InkCell>, u64) {
    let lex = lex_with_emphasis();
    let epoch = term.damage_epoch();
    wd.hard_reset();
    wd.rescan(term, rows, cols, &lex, c, epoch, scanned);
    let mut out = Vec::new();
    let mut ink = Vec::new();
    let fp = tick_ink(wd, at, c, &mut out, &mut ink);
    (ink, fp)
}

fn tick_ink(
    wd: &mut WordDecorations,
    now: Instant,
    cfg: &DecoConfig,
    out: &mut Vec<WordDecoration>,
    ink: &mut Vec<InkCell>,
) -> u64 {
    let mut fr = Vec::new();
    let mut nova = Vec::new();
    wd.tick(
        now,
        cfg,
        EffectGeom::default(),
        None,
        None,
        true,
        out,
        ink,
        &mut fr,
        &mut nova,
    )
}

#[test]
fn ink_settles_to_constant_bytes_and_disarms() {
    let mut term = Terminal::new(2, 48);
    term.process(b"ultrathink");
    let mut wd = WordDecorations::default();
    let c = cfg();
    let now = Instant::now();
    let (live, fp_live) = ink_at(&mut wd, &mut term, 2, 48, &c, now, now);
    assert_eq!(live.len(), 10, "one InkCell per glyph of `ultrathink`");
    assert!(wd.is_active(now), "a live sweep must arm the scheduler");
    let mut out = Vec::new();
    let mut ink2 = Vec::new();
    let fp_live2 = tick_ink(
        &mut wd,
        now + Duration::from_millis(16),
        &c,
        &mut out,
        &mut ink2,
    );
    assert_ne!(fp_live, fp_live2, "animating ink fp changes each frame");
    let later = now + Duration::from_millis(u64::from(c.ink_sweep_ms) + 250 + 250);
    let fp_a = tick_ink(&mut wd, later, &c, &mut out, &mut ink2);
    let settled = ink2.clone();
    let fp_b = tick_ink(
        &mut wd,
        later + Duration::from_millis(16),
        &c,
        &mut out,
        &mut ink2,
    );
    assert_eq!(fp_a, fp_b, "settled ink fingerprint is stable");
    assert_eq!(settled, ink2, "settled ink is byte-constant across frames");
    assert!(
        !wd.is_active(later),
        "settled ink must disarm the scheduler"
    );
    let mut rm = cfg();
    rm.reduced_motion = true;
    let (static_ink, _) = ink_at(&mut wd, &mut term, 2, 48, &rm, now, now);
    assert_eq!(
        settled, static_ink,
        "settled sweep == static gradient bytes"
    );
}

#[test]
fn ink_truncation_is_whole_word_and_stable() {
    let rows = 20usize;
    let cols = 44usize;
    let mut term = Terminal::new(rows as u16, cols as u16);
    let line = b"ultrathink ultrathink ultrathink ultrathink".to_vec();
    for r in 0..rows {
        if r > 0 {
            term.process(b"\r\n");
        }
        term.process(&line);
    }
    let mut wd = WordDecorations::default();
    let mut c = cfg();
    c.reduced_motion = true;
    let now = Instant::now();
    let (ink, fp_a) = ink_at(&mut wd, &mut term, rows, cols, &c, now, now);
    assert!(!wd.is_active(now), "reduced-motion ink never animates");
    assert_eq!(ink.len(), 510, "51 whole words of 10 cells, never partial");
    assert!(
        ink.iter().filter(|i| i.row == 12).count() == 30 && ink.iter().all(|i| i.row <= 12),
        "row-major prefix: 3 words on row 12, nothing below"
    );
    assert!(
        ink.windows(2)
            .all(|w| (w[0].row, w[0].col) < (w[1].row, w[1].col))
    );
    let mut out = Vec::new();
    let mut again = Vec::new();
    let fp_b = tick_ink(
        &mut wd,
        now + Duration::from_millis(16),
        &c,
        &mut out,
        &mut again,
    );
    assert_eq!(ink, again, "truncated emission is a stable fixed point");
    assert_eq!(fp_a, fp_b);
}

#[test]
fn ink_legibility_guard_pulls_on_light_bg() {
    let mut term = Terminal::new(2, 48);
    term.process(b"\x1b[38;2;30;30;30m\x1b[48;2;250;250;250multrathink");
    let mut wd = WordDecorations::default();
    let mut c = cfg();
    c.reduced_motion = true;
    c.ink_strength = 1.0;
    let now = Instant::now();
    let (ink, _) = ink_at(&mut wd, &mut term, 2, 48, &c, now, now);
    assert_eq!(ink.len(), 10);
    let (c0, c1) = nudged_ink_pair(Class::Emphasis, wd.occ[0].genome.gkey).unwrap();
    let (fg, bg) = (rgb3_to_u32([30, 30, 30]), rgb3_to_u32([250, 250, 250]));
    let mid = mix_rgb(c0, c1, 0.5);
    let mut s = c.ink_strength;
    while s > 0.0 && contrast_ratio(mix_rgb(fg, mid, s), bg) < MIN_INK_CONTRAST {
        s = (s - c.ink_strength / 8.0).max(0.0);
    }
    assert!(
        s < c.ink_strength,
        "the pastel pair on near-white must actually engage the pull"
    );
    assert!(contrast_ratio(mix_rgb(fg, mid, s), bg) >= MIN_INK_CONTRAST);
    for (i, cell) in ink.iter().enumerate() {
        let expect = mix_rgb(fg, mix_rgb(c0, c1, i as f32 / 9.0), s);
        assert_eq!(cell.color, u32_to_rgb3(expect), "cell {i} guarded color");
    }
}

#[test]
fn emphasis_and_ink_gates() {
    let mut term = Terminal::new(2, 48);
    term.process(b"ultrathink and a kitten say fuck");
    let mut wd = WordDecorations::default();
    let now = Instant::now();
    let lit = now + Duration::from_millis(350);
    let (ink, _) = ink_at(&mut wd, &mut term, 2, 48, &cfg(), now, lit);
    assert_eq!(
        ink.len(),
        10 + 6 + 4,
        "ultrathink + kitten + fuck lead cells, got {ink:?}"
    );
    let mut c = cfg();
    c.emphasis = false;
    let (ink, _) = ink_at(&mut wd, &mut term, 2, 48, &c, now, lit);
    assert_eq!(ink.len(), 6 + 4, "emphasis occurrences dropped at rescan");
    let mut c = cfg();
    c.ink_enabled = false;
    let (ink, fp) = ink_at(&mut wd, &mut term, 2, 48, &c, now, lit);
    assert!(ink.is_empty(), "ink_enabled = false emits zero InkCells");
    assert_ne!(fp, 0);
}

#[test]
fn reset_clears_stale_ink_on_next_tick() {
    let mut term = Terminal::new(2, 48);
    term.process(b"ultrathink");
    let mut wd = WordDecorations::default();
    let c = cfg();
    let now = Instant::now();
    let lex = lex_with_emphasis();
    let epoch = term.damage_epoch();
    wd.rescan(&term, 2, 48, &lex, &c, epoch, now);
    let mut out = Vec::new();
    let mut ink = Vec::new();
    tick_ink(&mut wd, now, &c, &mut out, &mut ink);
    assert!(!ink.is_empty());
    wd.reset();
    let fp = tick_ink(&mut wd, now, &c, &mut out, &mut ink);
    assert_eq!(fp, 0);
    assert!(ink.is_empty(), "stale ink must clear on the next tick");
    assert!(!wd.is_active(now));
}

fn lex() -> Lexicon {
    Lexicon::with_languages(&["en"])
}

fn feline(wd: &WordDecorations) -> &Occurrence {
    wd.occ
        .iter()
        .find(|o| o.class == Class::Feline)
        .expect("a feline occurrence")
}

#[test]
fn genome_frozen_across_neighbor_churn() {
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(2, 64);
    term.process(b"\x1b[31ma happy kitty sits here");
    wd.rescan(&term, 2, 64, &lex, &c, 1, t0);
    let g1 = feline(&wd).genome;
    let colors1 = feline(&wd).cat_colors;

    let mut churned = Terminal::new(2, 64);
    churned.process(b"\x1b[34ma grump kitty sits here");
    wd.rescan(
        &churned,
        2,
        64,
        &lex,
        &c,
        2,
        t0 + Duration::from_millis(500),
    );
    assert_eq!(feline(&wd).genome, g1, "persist hit must freeze the genome");
    assert_eq!(
        feline(&wd).cat_colors,
        colors1,
        "persist hit must freeze the atlas palette key"
    );
    assert_eq!(feline(&wd).appeared, t0, "hit keeps the episode's appeared");

    let blank = Terminal::new(2, 64);
    wd.rescan(&blank, 2, 64, &lex, &c, 3, t0 + Duration::from_secs(12));
    assert!(wd.persist.is_empty(), "grace expiry sweeps the episode");

    wd.rescan(&churned, 2, 64, &lex, &c, 4, t0 + Duration::from_secs(13));
    assert_ne!(
        feline(&wd).genome.gkey,
        g1.gkey,
        "rebirth re-rolls from the new neighbors"
    );
    assert_ne!(
        feline(&wd).cat_colors,
        colors1,
        "rebirth resolves the new surrounding colors"
    );
    assert_eq!(feline(&wd).appeared, t0 + Duration::from_secs(13));
}

#[test]
fn allocation_regression_resident_capacities_stable() {
    let lex = lex_with_emphasis();
    let c = cfg();
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let mut term_a = Terminal::new(4, 64);
    term_a.process(b"a happy kitty says fuck\r\nultrathink beats the orca");
    let mut term_b = Terminal::new(4, 64);
    term_b.process(b" a happy kitty says fuck\r\n ultrathink beats the orca");

    let caps = |wd: &WordDecorations| {
        [
            wd.occ.capacity(),
            wd.scan_cells.capacity(),
            wd.scan_chars.capacity(),
            wd.scan_matches.capacity(),
            wd.scan_text.capacity(),
            wd.scan_colmap.capacity(),
            wd.scan_row_occupied.capacity(),
            wd.ink_base_fg.capacity(),
            wd.ink_cols.capacity(),
            wd.persist.capacity(),
            wd.seed_ordinals.capacity(),
            wd.ctx_tok.capacity(),
            wd.ctx_folded.capacity(),
            wd.pending.capacity(),
            wd.align_old.capacity(),
            wd.align_decisions.capacity(),
            wd.scan_memo.hot.capacity(),
            wd.scan_memo.cold.capacity(),
        ]
    };
    let mut out = Vec::new();
    let mut ink = Vec::new();
    wd.rescan(&term_a, 4, 64, &lex, &c, 1, t0);
    tick_ink(&mut wd, t0, &c, &mut out, &mut ink);
    wd.rescan(&term_b, 4, 64, &lex, &c, 2, t0);
    tick_ink(&mut wd, t0, &c, &mut out, &mut ink);
    let warm = caps(&wd);
    assert!(
        wd.align_decisions.capacity() >= ALIGN_DECISION_CELLS,
        "align_pending must hand its decision plane back"
    );
    for i in 0..1000u64 {
        let term = if i % 2 == 0 { &term_a } else { &term_b };
        wd.rescan(term, 4, 64, &lex, &c, 3 + i, t0);
        tick_ink(&mut wd, t0, &c, &mut out, &mut ink);
    }
    assert_eq!(
        caps(&wd),
        warm,
        "resident capacities must not grow after warmup"
    );
}

#[test]
fn allocation_regression_all_miss_repaint_reuses_retired_buffers() {
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let (rows, cols) = (8usize, 48usize);
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(rows as u16, cols as u16);
    let mut snap = aterm_core::render::RenderInput::default();
    let mut line = String::new();
    let mut repaint = |wd: &mut WordDecorations, term: &mut Terminal, e: u64| {
        term.process(b"\x1b[H");
        for r in 0..rows {
            line.clear();
            for digit in [1000u64, 100, 10, 1] {
                let d = u8::try_from(e / digit % 10).expect("one digit");
                line.push(char::from(b'0' + d));
            }
            line.push(' ');
            line.push_str(&r.to_string());
            line.push_str(" a happy kitty says fuck");
            term.process(line.as_bytes());
            if r + 1 < rows {
                term.process(b"\r\n");
            }
        }
        term.cell_frame_into(&mut snap, rows, cols);
        wd.rescan_from_cells(&snap.cells, &snap.line_sizes, rows, cols, &lex, &c, e, t0);
    };
    for e in 1..=60u64 {
        repaint(&mut wd, &mut term, e);
    }
    let caps = |wd: &WordDecorations| {
        [
            wd.scan_memo.hot.capacity(),
            wd.scan_memo.cold.capacity(),
            wd.scan_memo.spare.capacity(),
            wd.scan_text.capacity(),
            wd.scan_chars.capacity(),
            wd.scan_matches.capacity(),
            wd.scan_colmap.capacity(),
            wd.occ.capacity(),
            wd.persist.capacity(),
        ]
    };
    let warm = caps(&wd);
    let fresh = wd.scan_memo.fresh_buffers;
    for e in 61..=260u64 {
        repaint(&mut wd, &mut term, e);
    }
    assert_eq!(
        wd.scan_memo.hits, 0,
        "the fixture must miss on every row, or it is not measuring the miss path"
    );
    assert_eq!(
        wd.scan_memo.misses,
        260 * rows as u64,
        "every row of every frame must have reached the scanner"
    );
    assert_eq!(
        wd.scan_memo.fresh_buffers, fresh,
        "a warmed all-miss repaint must refill retired buffers, not allocate new ones"
    );
    assert_eq!(
        caps(&wd),
        warm,
        "resident capacities must not grow on the all-miss path either"
    );

    let mut control = WordDecorations::default();
    control.scan_memo.no_recycle = true;
    let mut control_term = Terminal::new(rows as u16, cols as u16);
    for e in 1..=260u64 {
        repaint(&mut control, &mut control_term, e);
    }
    assert_eq!(
        control.scan_memo.fresh_buffers,
        260 * rows as u64,
        "the control must allocate one buffer pair per missed row"
    );
    assert!(
        fresh < 260 * rows as u64 / 10,
        "the pooled engine must allocate only while it warms up (fresh: {fresh})"
    );
}

fn assert_projection(
    wd: &WordDecorations,
    ident: u64,
    rolls: i64,
    st: &std::collections::BTreeMap<&'static str, i64>,
) {
    let live = wd.occ.iter().any(|o| mix(o.seed) == ident);
    let state = if live {
        1
    } else if wd.persist.contains_key(&ident) {
        2
    } else {
        0
    };
    assert_eq!(state, st[&"state"], "projected state must match the model");
    assert_eq!(
        rolls, st[&"rolls"],
        "accepted fresh-episode genome generations must match the model"
    );
    if state != 0 {
        let ep = &wd.persist[&ident];
        let nova = i64::from(ep.nova_done && !ep.born_done);
        assert_eq!(nova, st[&"nova"], "projected nova must match the model");
    }
}

fn repeated_kitty_reflow_frames() -> (Terminal, Terminal, Terminal) {
    let mut original = Terminal::new(6, 48);
    original.process(b"\x1b[1;1Hold alpha kitty tail\x1b[3;1Hold bravo kitty tail");

    let mut moved = Terminal::new(6, 48);
    moved.process(b"\x1b[2;1Hnew gamma kitty purrs\x1b[4;1Hnew delta kitty sleeps");

    let mut grown = Terminal::new(6, 48);
    grown.process(
        b"\x1b[2;1Hnew gamma kitty purrs\x1b[4;1Hnew delta kitty sleeps\x1b[6;1Hnew omega kitty glows",
    );
    (original, moved, grown)
}

#[test]
fn sparkle_identity_conformance_real_persist_map() {
    let m = aterm_models_effects::sparkle_identity_model();
    let mut st = m.init_state();
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(2, 48);
    term.process(b"a fluffy kitty naps");
    let blank = Terminal::new(2, 48);
    let mut rolls = 0i64;

    assert!(wd.persist.is_empty());
    wd.rescan(&term, 2, 48, &lex, &c, 1, t0);
    rolls += 1;
    assert!(m.fire("Appear", &mut st));
    let mut ident = mix(feline(&wd).seed);
    let g1 = feline(&wd).genome;
    assert_eq!(feline(&wd).appeared, t0);
    assert_projection(&wd, ident, rolls, &st);

    wd.persist.get_mut(&ident).expect("live episode").nova_done = true;
    assert!(m.fire("Ignite", &mut st));
    assert_projection(&wd, ident, rolls, &st);
    assert!(m.check_invariant("OneNovaPerEpisode", &st));

    let mut moved = Terminal::new(2, 48);
    moved.process(b" a fluffy kitty naps");
    let old_ident = ident;
    wd.rescan(&moved, 2, 48, &lex, &c, 2, at(1));
    assert!(m.fire("Rekey", &mut st));
    ident = mix(feline(&wd).seed);
    assert_ne!(ident, old_ident, "the horizontal move rekeys the map");
    assert!(
        !wd.persist.contains_key(&old_ident),
        "the old key was moved"
    );
    assert_projection(&wd, ident, rolls, &st);
    assert_eq!(feline(&wd).genome, g1, "Rekey preserves the frozen genome");
    assert_eq!(
        feline(&wd).appeared,
        t0,
        "Rekey preserves the episode clock"
    );
    assert!(
        wd.persist[&ident].nova_done,
        "Rekey preserves the spent guard"
    );
    assert!(m.check_invariant("PlayedOnce", &st));

    let mut context_moved = Terminal::new(2, 48);
    context_moved
        .process(b"build output changed; wrapping follows here.    status a bright kitty naps");
    let frozen_ctx = wd.persist[&ident].ctx_fp;
    let mut probe = WordDecorations::default();
    probe.rescan(&context_moved, 2, 48, &lex, &c, 1, t0);
    let candidate_ctx = probe.persist[&mix(feline(&probe).seed)].ctx_fp;
    assert_ne!(
        candidate_ctx, frozen_ctx,
        "the wrapped status token must change the row-local fingerprint"
    );
    assert_eq!(
        feline(&probe).form_id,
        feline(&wd).form_id,
        "recognition evidence is the unchanged exact lexical surface"
    );

    let old_ident = ident;
    let births_before = wd.birth_seq;
    wd.rescan(&context_moved, 2, 48, &lex, &c, 3, at(2));
    assert!(m.fire("ContextMove", &mut st));
    ident = mix(feline(&wd).seed);
    assert_ne!(ident, old_ident, "the wrapped move changes the map key");
    assert_eq!(feline(&wd).row, 1, "the logical occurrence crossed a wrap");
    assert!(
        !wd.persist.contains_key(&old_ident),
        "recognition transfers rather than retaining a stale old episode"
    );
    assert_eq!(
        wd.birth_seq, births_before,
        "a recognized changed-context move is not a fresh allocation"
    );
    assert_projection(&wd, ident, rolls, &st);
    assert_eq!(
        feline(&wd).genome,
        g1,
        "ContextMove preserves the frozen genome"
    );
    assert_eq!(
        feline(&wd).appeared,
        t0,
        "ContextMove preserves the episode clock"
    );
    assert_eq!(
        wd.persist[&ident].ctx_fp, candidate_ctx,
        "recognition adopts the current context for later matching/done marks"
    );
    assert!(
        wd.persist[&ident].nova_done,
        "ContextMove preserves the spent guard"
    );
    assert!(m.check_invariant("RecognitionComplete", &st));
    assert!(m.check_invariant("NoFalseBirths", &st));
    assert!(m.check_invariant("PlayedOnce", &st));

    wd.rescan(&blank, 2, 48, &lex, &c, 4, at(3));
    assert!(m.fire("Vanish", &mut st));
    assert_projection(&wd, ident, rolls, &st);

    wd.rescan(&context_moved, 2, 48, &lex, &c, 5, at(4));
    assert!(m.fire("Rehit", &mut st));
    assert_projection(&wd, ident, rolls, &st);
    assert_eq!(feline(&wd).genome, g1);
    assert_eq!(
        feline(&wd).appeared,
        t0,
        "grace re-hit continues the episode (B-3)"
    );
    assert!(m.check_invariant("GenomeFrozen", &st));

    wd.rescan(&blank, 2, 48, &lex, &c, 6, at(5));
    assert!(m.fire("Vanish", &mut st));
    wd.rescan(&blank, 2, 48, &lex, &c, 7, at(21));
    for _ in 0..3 {
        assert!(m.fire("Tick", &mut st));
    }
    assert!(m.fire("Expire", &mut st));
    assert!(wd.persist.is_empty(), "true identity death");
    assert_projection(&wd, ident, rolls, &st);

    wd.rescan(&context_moved, 2, 48, &lex, &c, 8, at(22));
    rolls += 1;
    assert!(m.fire("Appear", &mut st));
    assert_projection(&wd, ident, rolls, &st);
    assert_eq!(feline(&wd).appeared, at(22));
    assert!(
        wd.persist[&ident].born_done,
        "the identity is done-marked: the re-birth is born-done"
    );
    assert!(
        !m.action_enabled("Ignite", &st),
        "the model's done flag survives Expire — no second Ignite"
    );
    assert!(m.check_invariant("PlayedOnce", &st));
    assert!(m.check_invariant("GenomeFrozen", &st));
}

#[test]
fn a_kitty_retyped_after_partial_frames_is_fresh_and_armed() {
    let model = aterm_models_effects::sparkle_reflow_cardinality_model();
    let lex = lex();
    let c = cfg();
    let changed: &[&[u8]] = &[
        b"\x1b[2;1Hstatus a noisy k purrs",
        b"\x1b[2;1Hstatus a noisy ki purrs",
        b"\x1b[2;1Hstatus a noisy kitt purrs",
    ];
    let same: &[&[u8]] = &[
        b"old alpha k tail",
        b"old alpha ki tail",
        b"old alpha kitt tail",
    ];
    let one: &[&[u8]] = &[b"old alpha kitt tail"];
    for (label, partials, complete, action) in [
        (
            "changed column",
            changed,
            b"\x1b[2;1Hstatus a noisy kitty purrs".as_slice(),
            None,
        ),
        (
            "same key after more typing",
            same,
            b"old alpha kitty tail".as_slice(),
            Some("TypedRetype"),
        ),
        (
            "same key after one partial",
            one,
            b"old alpha kitty tail".as_slice(),
            Some("RecentTypedRetype"),
        ),
    ] {
        let t0 = Instant::now();
        let at = |n: usize| t0 + Duration::from_millis(16 * n as u64);
        let mut original = Terminal::new(4, 48);
        original.process(b"old alpha kitty tail");
        let mut wd = WordDecorations::default();
        wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
        let old = feline(&wd).clone();
        let old_ctx = wd.persist[&old.ident].ctx_fp;
        wd.persist.get_mut(&old.ident).expect("old live").nova_done = true;

        for (i, partial) in partials.iter().enumerate() {
            let mut typing = Terminal::new(4, 48);
            typing.process(partial);
            wd.rescan(&typing, 4, 48, &lex, &c, i as u64 + 2, at(i + 1));
            assert!(
                wd.occ.iter().all(|o| o.class != Class::Feline),
                "{label}: partial token {i} must not be recognized"
            );
        }
        if action.is_some() {
            let tainted = wd.persist[&old.ident];
            assert!(
                tainted.continuity_tainted,
                "{label}: PRECONDITION — nonblank partial scans taint the old episode"
            );
            if partials.len() == 1 {
                assert_eq!(
                    wd.rescan_seq.wrapping_sub(tainted.seen_seq),
                    1,
                    "{label}: PRECONDITION — inside the weak-continuity window"
                );
            }
        }

        let births_before = wd.birth_seq;
        let appeared = at(partials.len() + 1);
        let mut replacement = Terminal::new(4, 48);
        replacement.process(complete);
        let seq = partials.len() as u64 + 2;
        wd.rescan(&replacement, 4, 48, &lex, &c, seq, appeared);
        let new = feline(&wd);
        assert_eq!(
            new.form_id, old.form_id,
            "{label}: the exact surface is kitty"
        );
        assert_eq!(
            new.ident == old.ident,
            action.is_some(),
            "{label}: PRECONDITION — the fixture's identity key"
        );
        if action.is_some() {
            assert_eq!(
                wd.persist[&new.ident].ctx_fp, old_ctx,
                "{label}: PRECONDITION — the exact context returns"
            );
        }
        assert_eq!(
            wd.birth_seq,
            births_before + 1,
            "{label}: one genuine new birth"
        );
        assert_eq!(
            new.appeared, appeared,
            "{label}: the new kitty activates now"
        );
        assert!(
            !wd.persist[&new.ident].nova_done,
            "{label}: the retyped kitty is armed rather than inheriting spent state"
        );
        if let Some(action) = action {
            let mut state = model.init_state();
            assert!(model.fire(action, &mut state), "{label}");
            assert_eq!(
                state[&"fresh"], 1,
                "{label}: real birth projects onto {action}"
            );
            assert_eq!(state[&"armed"], 1, "{label}");
            assert_eq!(state[&"transferred"], 0, "{label}");
            assert!(model.check_invariant("NoFalseTransfers", &state), "{label}");
        }
    }
}

#[test]
fn two_consecutive_recent_kitty_retypes_both_arm() {
    let model = aterm_models_effects::sparkle_retype_rearm_model();
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let mut complete = Terminal::new(4, 48);
    complete.process(b"old alpha kitty");
    let mut partial = Terminal::new(4, 48);
    partial.process(b"old alpha kitt");

    let mut wd = WordDecorations::default();
    wd.rescan(&complete, 4, 48, &lex, &c, 1, t0);
    let ident = feline(&wd).ident;
    {
        let occ = feline(&wd);
        assert_eq!(
            (complete.cursor().row, complete.cursor().col),
            (occ.row, occ.end_col + 1),
            "the fixture must stage a caret-witnessed completion"
        );
    }
    wd.persist.get_mut(&ident).expect("initial kitty").nova_done = true;
    let births_before = wd.birth_seq;
    let mut state = model.init_state();

    for cycle in 1..=2u64 {
        let partial_seq = cycle * 2;
        wd.rescan(
            &partial,
            4,
            48,
            &lex,
            &c,
            partial_seq,
            t0 + Duration::from_millis(16 * (partial_seq - 1)),
        );
        assert!(wd.persist[&ident].continuity_tainted, "cycle {cycle}");

        let complete_seq = partial_seq + 1;
        let appeared = t0 + Duration::from_millis(16 * (complete_seq - 1));
        wd.note_typed_edit(appeared, None);
        wd.rescan(&complete, 4, 48, &lex, &c, complete_seq, appeared);
        assert!(model.fire("TypeAgain", &mut state));
        let current = feline(&wd);
        assert_eq!(current.ident, ident);
        assert_eq!(current.appeared, appeared, "cycle {cycle} is fresh");
        assert_eq!(
            wd.birth_seq - births_before,
            state[&"armed"] as u64,
            "every completed retype is armed"
        );
        assert!(!current.inert, "cycle {cycle} must not be born-done");
        assert!(!wd.persist[&ident].nova_done, "cycle {cycle} is armed");
        let done_key = ident ^ wd.persist[&ident].ctx_fp;
        assert!(
            !wd.done_marks.contains_key(&done_key),
            "cycle {cycle} leaves no typed-reentry poison mark"
        );

        wd.persist.get_mut(&ident).expect("live kitty").nova_done = true;
    }
    assert_eq!(state[&"retypes"], 2);
    assert_eq!(state[&"armed"], 2);
    assert!(model.check_invariant("EveryRetypeArmed", &state));
}

#[test]
fn one_keystroke_re_arms_one_cat_however_many_repaints_follow() {
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut complete = Terminal::new(4, 48);
    complete.process(b"old alpha kitty");
    let mut partial = Terminal::new(4, 48);
    partial.process(b"old alpha kitt");

    let mut wd = WordDecorations::default();
    wd.rescan(&complete, 4, 48, &lex, &c, 1, t0);
    let ident = feline(&wd).ident;
    assert!(
        !feline(&wd).inert,
        "PRECONDITION: the first typed kitty is armed"
    );

    let mut out = Vec::new();
    let mut cycle = |wd: &mut WordDecorations, seq: u64, base_ms: u64, key: bool| -> bool {
        wd.persist.get_mut(&ident).expect("live kitty").nova_done = true;
        tick_deco(wd, at(base_ms), &c, &mut out);
        let done_key = ident ^ wd.persist[&ident].ctx_fp;
        assert!(
            wd.done_marks.contains_key(&done_key),
            "PRECONDITION: cycle at {base_ms} ms starts from a stamped \
             guard, so an armed verdict can only come from a re-arm"
        );
        wd.rescan(&partial, 4, 48, &lex, &c, seq, at(base_ms + 4));
        assert!(
            wd.persist[&ident].continuity_tainted,
            "PRECONDITION: cycle at {base_ms} ms must taint, or it is not \
             exercising the disputed branch"
        );
        if key {
            wd.note_typed_edit(at(base_ms + 8), None);
        }
        wd.rescan(&complete, 4, 48, &lex, &c, seq + 1, at(base_ms + 16));
        !feline(wd).inert
    };

    assert!(
        cycle(&mut wd, 2, 100, true),
        "cycle 1: a key was committed, so the retype re-arms"
    );
    assert!(
        !cycle(&mut wd, 4, 200, false),
        "cycle 2: SAME key, SAME window, no new keystroke — the repaint \
         must not ride it a second time"
    );
    assert!(
        at(316).duration_since(at(108)) < Duration::from_millis(TYPED_EDIT_WITNESS_MS),
        "PRECONDITION: cycle 2 really is inside cycle 1's witness window, \
         so its silence is the SPEND and not the clock"
    );
    assert!(
        cycle(&mut wd, 6, 300, true),
        "cycle 3: a fresh key re-arms again — the spend retires the KEY, \
         not the feature"
    );
}

#[test]
fn a_repaint_midway_through_the_cat_plays_no_second_cat() {
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let (rows, cols) = (g.rows as usize, g.cols as usize);
    let t0 = Instant::now();
    let mut typed = Terminal::new(g.rows, g.cols);
    typed.process(b"hello kitty");
    let mut overwritten = Terminal::new(g.rows, g.cols);
    overwritten.process(b"hello ------");
    let mut restored_elsewhere = Terminal::new(g.rows, g.cols);
    restored_elsewhere.process(b"hello kitty\r\n\r\n$ ");

    let mut wd = WordDecorations::default();
    wd.rescan(&typed, rows, cols, &lex, &c, 1, t0);
    let first = feline(&wd).clone();
    assert!(!first.inert, "PRECONDITION: the typed kitty is armed");

    let mid = t0 + Duration::from_millis(60);
    let (free, _, _) = tick_cat(&mut wd, mid, &c, g);
    assert!(
        !free.is_empty(),
        "PRECONDITION: the cat is actually drawing"
    );
    assert!(
        wd.persist[&first.ident].peek_started && !wd.persist[&first.ident].peek_done,
        "PRECONDITION: mid-animation — started, NOT finished"
    );
    assert!(
        wd.done_marks
            .contains_key(&(first.ident ^ wd.persist[&first.ident].ctx_fp)),
        "a STARTED one-shot is already guarded, not only a finished one"
    );

    wd.rescan(
        &overwritten,
        rows,
        cols,
        &lex,
        &c,
        2,
        mid + Duration::from_millis(16),
    );
    assert!(
        wd.persist[&first.ident].continuity_tainted,
        "PRECONDITION: the overwrite tainted the in-flight episode"
    );
    wd.rescan(
        &restored_elsewhere,
        rows,
        cols,
        &lex,
        &c,
        3,
        mid + Duration::from_millis(32),
    );
    assert!(
        feline(&wd).inert,
        "a repaint mid-cat must not restart the entrance"
    );
}

#[test]
fn persist_cap_drops_unmatched_grace_after_fresh_move() {
    let model = aterm_models_effects::sparkle_persist_capacity_model();
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let mut original = Terminal::new(4, 48);
    original.process(b"old alpha kitty tail");
    let mut replacement = Terminal::new(4, 48);
    replacement.process(b"status beta kitty tail");

    let mut wd = WordDecorations::default();
    wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
    let old = feline(&wd).clone();
    wd.persist.get_mut(&old.ident).expect("old kitty").nova_done = true;
    let old_ep = wd.persist[&old.ident];

    let mut probe = WordDecorations::default();
    probe.rescan(&replacement, 4, 48, &lex, &c, 1, t0);
    let replacement_ident = feline(&probe).ident;
    assert_ne!(replacement_ident, old.ident, "fixture changes the column");

    let mut synthetic = 0u64;
    while wd.persist.len() < PERSIST_CAP {
        let key = 0xC0DE_0000_0000_0000u64.wrapping_add(synthetic);
        synthetic += 1;
        if key == old.ident || key == replacement_ident {
            continue;
        }
        let mut ep = old_ep;
        ep.form_id = FormId::UNKNOWN;
        ep.seed = key;
        ep.ctx_fp = key.rotate_left(17);
        ep.last_seen = t0 + Duration::from_millis(1);
        ep.last_row = 3;
        ep.last_col = 0;
        ep.nova_done = false;
        wd.persist.insert(key, ep);
    }
    assert_eq!(wd.persist.len(), PERSIST_CAP);

    for i in 0..3 {
        let blank = Terminal::new(4, 48);
        wd.rescan(
            &blank,
            4,
            48,
            &lex,
            &c,
            i as u64 + 2,
            t0 + Duration::from_millis(16 * (i as u64 + 1)),
        );
        assert_eq!(wd.persist.len(), PERSIST_CAP);
    }
    assert!(!wd.persist[&old.ident].continuity_tainted);

    let mut state = model.init_state();
    for action in ["Pull", "Fresh", "Reinsert"] {
        assert!(model.fire(action, &mut state));
    }
    let births_before = wd.birth_seq;
    let appeared = t0 + Duration::from_millis(64);
    wd.rescan(&replacement, 4, 48, &lex, &c, 5, appeared);
    let new = feline(&wd);
    assert_eq!(new.ident, replacement_ident);
    assert_eq!(new.appeared, appeared, "replacement kitty is freshly armed");
    assert_eq!(wd.birth_seq, births_before + 1);
    assert_eq!(state[&"resident"], 3);
    assert_eq!(state[&"admitted"], 1);
    assert_eq!(state[&"departed"], 1);
    assert_eq!(
        wd.persist.len(),
        PERSIST_CAP * state[&"resident"] as usize / 3,
        "the real full-map transaction projects onto ResidentBounded"
    );
    assert!(wd.persist.contains_key(&replacement_ident));
    assert!(!wd.persist.contains_key(&old.ident));
    let old_marked = wd.done_marks.contains_key(&(old.ident ^ old_ep.ctx_fp));
    assert_eq!(
        i64::from(old_marked),
        state[&"departed"],
        "dropping grace history preserves its spent one-shot guard"
    );

    wd.rescan(
        &replacement,
        4,
        48,
        &lex,
        &c,
        6,
        appeared + Duration::from_millis(16),
    );
    assert_eq!(wd.persist.len(), PERSIST_CAP);
}

#[test]
fn a_returning_spent_kitty_transfers_only_inside_its_redraw_windows() {
    let model = aterm_models_effects::sparkle_reflow_cardinality_model();
    let lex = lex();
    let c = cfg();
    let mut original = Terminal::new(4, 48);
    original.process(b"old alpha kitty tail");
    let moved_at = |cols: u16| {
        let mut t = Terminal::new(4, cols);
        t.process(b"\x1b[2;1Hstatus a noisy kitty purrs");
        t
    };
    let (moved, moved_wider) = (moved_at(48), moved_at(49));
    let blank = Terminal::new(4, 48);
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Verdict {
        Transferred,
        Armed,
        BornDone,
        Fresh,
    }
    for (label, blanks, ret_ms, target, cols, verdict, action) in [
        (
            "gap 2",
            &[16u64][..],
            32,
            &moved,
            48,
            Verdict::Transferred,
            None,
        ),
        ("gap 3", &[16, 32][..], 48, &moved, 48, Verdict::Armed, None),
        (
            "three blank scans, exact context",
            &[1_000, 2_000, 3_000][..],
            4_000,
            &original,
            48,
            Verdict::Transferred,
            Some("BlankGrace"),
        ),
        (
            "hour old, moved",
            &[1_000][..],
            3_600_000,
            &moved,
            48,
            Verdict::Armed,
            None,
        ),
        (
            "hour old, same position",
            &[1_000][..],
            3_600_000,
            &original,
            48,
            Verdict::BornDone,
            None,
        ),
        (
            "width change",
            &[][..],
            16,
            &moved_wider,
            49,
            Verdict::Fresh,
            None,
        ),
    ] {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut wd = WordDecorations::default();
        wd.rescan(&original, 4, 48, &lex, &c, 1, t0);
        let old = feline(&wd).clone();
        let old_ctx = wd.persist[&old.ident].ctx_fp;
        wd.persist.get_mut(&old.ident).expect("old live").nova_done = true;
        for (n, &ms) in blanks.iter().enumerate() {
            wd.rescan(&blank, 4, 48, &lex, &c, n as u64 + 2, at(ms));
        }
        let births_before = wd.birth_seq;
        let seq = blanks.len() as u64 + 2;
        wd.rescan(target, 4, cols, &lex, &c, seq, at(ret_ms));
        let back = feline(&wd);
        let ep = &wd.persist[&back.ident];
        let fresh = wd.birth_seq - births_before;
        if verdict == Verdict::Transferred {
            assert_eq!(fresh, 0, "{label}: the episode transfers");
            assert_eq!(back.appeared, t0, "{label}: the episode clock survives");
            assert!(ep.nova_done, "{label}: the spent guard survives");
        } else {
            assert_eq!(fresh, 1, "{label}: a fresh birth, nothing adopted");
            assert_eq!(back.appeared, at(ret_ms), "{label}");
        }
        match verdict {
            Verdict::Armed => assert!(!ep.nova_done, "{label}: armed"),
            Verdict::BornDone => assert!(
                ep.born_done,
                "{label}: true death records the one-shot before the identity returns"
            ),
            Verdict::Transferred | Verdict::Fresh => {}
        }
        if let Some(action) = action {
            assert!(
                !ep.continuity_tainted,
                "{label}: PRECONDITION — blank occlusion never taints"
            );
            assert_eq!(ep.ctx_fp, old_ctx, "{label}: the exact context returns");
            let mut state = model.init_state();
            assert!(model.fire(action, &mut state), "{label}");
            assert_eq!(
                fresh, state[&"fresh"] as u64,
                "{label}: projects onto {action}"
            );
            assert_eq!(state[&"transferred"], 1, "{label}");
            assert_eq!(state[&"armed"], 0, "{label}");
            assert!(
                model.check_invariant("RecognitionComplete", &state),
                "{label}"
            );
        }
    }
}

#[test]
fn sparkle_reflow_cardinality_conformance_repeated_surfaces() {
    let model = aterm_models_effects::sparkle_reflow_cardinality_model();
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let (original, moved, grown) = repeated_kitty_reflow_frames();
    let mut wd = WordDecorations::default();

    wd.rescan(&original, 6, 48, &lex, &c, 1, t0);
    assert_eq!(wd.occ.len(), 2, "the old group has two equal surfaces");
    assert_eq!(wd.birth_seq, 2);
    let form = wd.occ[0].form_id;
    assert!(wd.occ.iter().all(|o| o.form_id == form));
    let old_contexts: Vec<u64> = wd.occ.iter().map(|o| wd.persist[&o.ident].ctx_fp).collect();
    for o in &wd.occ {
        wd.persist.get_mut(&o.ident).expect("old live").nova_done = true;
    }

    let mut probe = WordDecorations::default();
    probe.rescan(&moved, 6, 48, &lex, &c, 1, t0);
    assert_eq!(probe.occ.len(), 2);
    let moved_contexts: Vec<(u64, u64)> = probe
        .occ
        .iter()
        .map(|o| (o.ident, probe.persist[&o.ident].ctx_fp))
        .collect();
    assert!(
        moved_contexts
            .iter()
            .all(|(_, ctx)| !old_contexts.contains(ctx)),
        "each repeated-surface candidate has genuinely changed context"
    );

    let births_before = wd.birth_seq;
    wd.rescan(&moved, 6, 48, &lex, &c, 2, at(1));
    let mut pair = model.init_state();
    assert!(model.fire("MovePair", &mut pair));
    assert_eq!(
        wd.birth_seq - births_before,
        pair[&"fresh"] as u64,
        "2→2 births project onto the model"
    );
    assert_eq!(wd.occ.len() as i64, pair[&"new_count"]);
    assert_eq!(wd.persist.len(), 2, "no unmatched stale episode remains");
    let armed = wd
        .occ
        .iter()
        .filter(|o| !wd.persist[&o.ident].nova_done)
        .count() as i64;
    assert_eq!(armed, pair[&"armed"]);
    for o in &wd.occ {
        let ep = &wd.persist[&o.ident];
        let expected_ctx = moved_contexts
            .iter()
            .find_map(|(ident, ctx)| (*ident == o.ident).then_some(*ctx))
            .expect("probe candidate");
        assert_eq!(ep.ctx_fp, expected_ctx, "recognition context advances");
        assert_eq!(ep.appeared, t0, "the survivor clock is preserved");
        assert!(ep.nova_done, "every 2→2 survivor remains spent");
    }
    assert!(model.check_invariant("NoFalseBirths", &pair));
    assert!(model.check_invariant("FreshAtMostNetGrowth", &pair));

    let births_before = wd.birth_seq;
    wd.rescan(&grown, 6, 48, &lex, &c, 3, at(2));
    let mut growth = model.init_state();
    assert!(model.fire("GrowOne", &mut growth));
    assert_eq!(
        wd.birth_seq - births_before,
        growth[&"fresh"] as u64,
        "2→3 births project onto net growth"
    );
    assert_eq!(wd.occ.len() as i64, growth[&"new_count"]);
    assert_eq!(wd.persist.len(), 3);
    let old_spent = wd
        .occ
        .iter()
        .filter(|o| {
            let ep = &wd.persist[&o.ident];
            ep.appeared == t0 && ep.nova_done
        })
        .count() as i64;
    let fresh_armed = wd
        .occ
        .iter()
        .filter(|o| {
            let ep = &wd.persist[&o.ident];
            ep.appeared == at(2) && !ep.nova_done
        })
        .count() as i64;
    assert_eq!(old_spent, growth[&"transferred"]);
    assert_eq!(fresh_armed, growth[&"armed"]);
    assert!(model.check_invariant("FreshAtMostNetGrowth", &growth));
    assert!(model.check_invariant("ArmedAtMostNetGrowth", &growth));
    assert!(model.check_invariant("NoFalseBirths", &growth));
}

#[test]
fn sparkle_reflow_cardinality_negative_control_context_gate() {
    let mut buggy = aterm_models_effects::sparkle_reflow_cardinality_model();
    for cst in &mut buggy.consts {
        if cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let (original, moved, grown) = repeated_kitty_reflow_frames();

    let run = |next: &Terminal, action: &str| {
        let mut wd = WordDecorations::default();
        wd.rescan(&original, 6, 48, &lex, &c, 1, t0);
        assert_eq!(wd.birth_seq, 2);
        for o in &wd.occ {
            wd.persist.get_mut(&o.ident).expect("old live").nova_done = true;
        }
        wd.persist.clear();
        wd.done_marks.clear();
        let births_before = wd.birth_seq;
        wd.rescan(next, 6, 48, &lex, &c, 2, t0 + Duration::from_secs(1));

        let mut st = buggy.init_state();
        assert!(buggy.fire(action, &mut st));
        assert_eq!(wd.occ.len() as i64, st[&"new_count"]);
        assert_eq!(wd.birth_seq - births_before, st[&"fresh"] as u64);
        let armed = wd
            .occ
            .iter()
            .filter(|o| !wd.persist[&o.ident].nova_done)
            .count() as i64;
        assert_eq!(armed, st[&"armed"]);
        assert!(!buggy.check_invariant("FreshAtMostNetGrowth", &st));
        assert!(!buggy.check_invariant("ArmedAtMostNetGrowth", &st));
        assert!(!buggy.check_invariant("NoFalseBirths", &st));
    };

    run(&moved, "MovePair");
    run(&grown, "GrowOne");
}

#[test]
fn sparkle_identity_negative_control_graceless_map_is_buggy_trace() {
    let healthy = aterm_models_effects::sparkle_identity_model();
    let mut buggy = aterm_models_effects::sparkle_identity_model();
    for cst in &mut buggy.consts {
        if cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
    let mut st = buggy.init_state();
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let mut wd = WordDecorations {
        grace_override: Some(Duration::ZERO),
        ..WordDecorations::default()
    };
    let mut term = Terminal::new(2, 48);
    term.process(b"a fluffy kitty naps");
    let blank = Terminal::new(2, 48);
    let mut rolls = 0i64;

    wd.rescan(&term, 2, 48, &lex, &c, 1, t0);
    rolls += 1;
    assert!(buggy.fire("Appear", &mut st));
    let ident = mix(feline(&wd).seed);
    wd.persist.get_mut(&ident).expect("live").nova_done = true;
    assert!(buggy.fire("Ignite", &mut st));

    wd.rescan(&blank, 2, 48, &lex, &c, 2, t0 + Duration::from_secs(1));
    assert!(buggy.fire("Vanish", &mut st));
    assert!(
        wd.persist.is_empty(),
        "grace-less: the episode dies with one epoch"
    );

    wd.done_marks.clear();
    wd.rescan(&term, 2, 48, &lex, &c, 3, t0 + Duration::from_secs(2));
    rolls += 1;
    assert!(buggy.fire("Rehit", &mut st));
    assert_eq!(st[&"rolls"], 2, "Buggy Rehit re-rolls");
    assert_eq!(
        rolls, st[&"rolls"],
        "the grace-less map reproduces the Buggy trace"
    );
    assert!(
        !wd.persist[&ident].nova_done,
        "the spent-nova guard is lost — the strobe hole"
    );
    assert!(
        !healthy.check_invariant("GenomeFrozen", &st),
        "births=1 rolls=2 must violate GenomeFrozen"
    );
    assert!(buggy.fire("Ignite", &mut st));
    assert!(!healthy.check_invariant("OneNovaPerEpisode", &st));
}

fn geom20() -> EffectGeom {
    EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 6,
        cols: 20,
    }
}

fn scan_grid(
    term: &mut Terminal,
    c: &DecoConfig,
    geom: EffectGeom,
    now: Instant,
    caret: Option<(u16, u16)>,
) -> WordDecorations {
    let (rows, cols) = (usize::from(geom.rows), usize::from(geom.cols));
    let lex = lex();
    let mut snap = aterm_core::render::RenderInput::default();
    term.cell_frame_into(&mut snap, rows, cols);
    let bg = snap
        .cells
        .iter()
        .find_map(|line| line.first())
        .map_or(0, |cell| rgb3_to_u32(cell.bg));
    let mut wd = WordDecorations::default();
    wd.rescan_from_cells_with_geom_at_cursor(
        &snap.cells,
        &snap.line_sizes,
        rows,
        cols,
        &lex,
        c,
        1,
        now,
        geom,
        bg,
        caret,
    );
    wd
}

#[test]
fn tui_prompt_box_frame_still_summons_a_cat() {
    let (rows, cols) = (6usize, 20usize);
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(
        "\r\n\r\n╭──────────────────╮\r\n│ kitty            │\r\n╰──────────────────╯".as_bytes(),
    );
    let mut wd = scan_grid(&mut term, &c, g, t0, None);
    let occ = feline(&wd);
    assert!(
        occ.cat_text_clear && cat_eligible(occ, &c, g),
        "a boxed prompt line is eligible — the frame draws OVER the fur"
    );
    let (cats, _, _) = tick_cat(&mut wd, t0 + Duration::from_millis(600), &c, g);
    assert!(
        !cats.is_empty(),
        "typing `kitty` inside a TUI prompt box summons a cat"
    );
}

#[test]
fn words_arriving_while_away_never_storm_on_return() {
    let (rows, cols) = (6usize, 20usize);
    let c = cfg();
    let g = geom20();
    let lex = lex();
    let t0 = Instant::now();
    let mut term = Terminal::new(rows as u16, cols as u16);
    let mut wd = WordDecorations::default();

    wd.set_presentable(t0, true);
    wd.rescan(&term, rows, cols, &lex, &c, 1, t0);

    wd.set_presentable(t0 + Duration::from_secs(1), false);
    term.process(b"kitty\r\nkitty\r\nkitty\r\nkitty");

    let back = t0 + Duration::from_secs(120);
    wd.set_presentable(back, true);
    wd.rescan(&term, rows, cols, &lex, &c, 2, back);
    let felines = wd.occ.iter().filter(|o| o.spec.graphic.is_some()).count();
    assert!(felines >= 3, "the fixture really does scan several cats");
    let (cats, _, _) = tick_cat(&mut wd, back + Duration::from_millis(600), &c, g);
    assert!(
        cats.is_empty(),
        "returning to a screen of feline words plays NO entrances, got {}",
        cats.len()
    );
    for occ in wd.occ.iter().filter(|o| o.spec.graphic.is_some()) {
        assert!(
            wd.persist[&occ.ident].born_settled,
            "each word that arrived while away is born spent"
        );
    }

    term.process(b"\r\nkitty");
    let after = back + Duration::from_secs(1);
    wd.rescan(&term, rows, cols, &lex, &c, 3, after);
    let fresh = wd
        .occ
        .iter()
        .filter(|o| o.spec.graphic.is_some())
        .filter(|o| !wd.persist[&o.ident].born_settled)
        .count();
    assert_eq!(fresh, 1, "the newly typed word is armed normally");
}

#[test]
fn a_settled_kitty_on_screen_never_plays_again_without_a_keystroke() {
    let (rows, cols) = (6usize, 20usize);
    let c = cfg();
    let g = geom20();
    let lex = lex();
    let t0 = Instant::now();
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\r\n\r\nnice kitty");
    let mut wd = WordDecorations::default();
    wd.set_presentable(t0, true);
    wd.rescan(&term, rows, cols, &lex, &c, 1, t0);
    let ident = feline(&wd).ident;

    let (first, _, _) = tick_cat(&mut wd, t0 + Duration::from_millis(100), &c, g);
    assert!(!first.is_empty(), "PRECONDITION: the first cat draws");
    let spent = t0 + Duration::from_secs(8);
    tick_cat(&mut wd, spent, &c, g);
    assert!(
        wd.persist[&ident].peek_done,
        "PRECONDITION: the one-shot is spent before the watch begins"
    );
    let done_key = ident ^ wd.persist[&ident].ctx_fp;

    let mut drew_again = Vec::new();
    for i in 0..3600u64 {
        let at = spent + Duration::from_millis(500 * (i + 1));
        wd.rescan(&term, rows, cols, &lex, &c, 2 + i, at);
        let (cats, _, _) = tick_cat(&mut wd, at + Duration::from_millis(60), &c, g);
        if !cats.is_empty() {
            drew_again.push(at.saturating_duration_since(t0));
        }
    }
    assert!(
        drew_again.is_empty(),
        "a word nobody retyped played its cat again at {drew_again:?} — \
         that is the owner's 'new kitties appear' on a longer timer"
    );
    assert!(
        wd.persist[&ident].peek_done,
        "the settled episode is never re-armed"
    );
    assert!(
        wd.done_marks.contains_key(&done_key),
        "…and its ledger guard is never deleted out from under it"
    );
}

#[test]
fn busy_band_above_flips_the_peek_downward() {
    let (rows, cols) = (6usize, 20usize);
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"XXXXXXXXXXXXXXXXXXXX\r\nXXXXXXXXXXXXXXXXXXXX\r\nkitty");
    let wd = scan_grid(&mut term, &c, g, t0, None);
    let occ = feline(&wd);
    assert_eq!(occ.row, 2);
    assert!(
        occ.cat_text_clear,
        "a busy band above no longer erases the cat"
    );
    assert!(occ.cat_peek_down, "it takes the clear side instead");
}

#[test]
fn a_head_never_lands_on_letters() {
    let (rows, cols) = (4usize, 20usize);
    let c = cfg();
    let g = EffectGeom {
        rows: rows as u16,
        ..geom20()
    };
    let t0 = Instant::now();
    let plan = |above: &[u8]| {
        let mut term = Terminal::new(rows as u16, cols as u16);
        let mut bytes = b"\r\n\r\n".to_vec();
        bytes.extend_from_slice(above);
        bytes.extend_from_slice(b"\r\nsee my kitty");
        term.process(&bytes);
        let wd = scan_grid(&mut term, &c, g, t0, None);
        let occ = feline(&wd);
        assert_eq!(occ.row, 3, "fixture: the word is on the bottom row");
        (occ.cat_text_clear, occ.cat_peek_down)
    };
    assert_eq!(
        plan(b"the quick brown fox jumps"),
        (false, false),
        "a head rose over the letters above"
    );
    assert_eq!(
        plan(b""),
        (true, false),
        "control: a blank line above hosts it"
    );
    assert_eq!(
        plan("\u{2500}".repeat(cols).as_bytes()),
        (true, false),
        "control: a frame's lines above host it"
    );
}

fn cat_occ(row: u16, start: u16, end: u16, gkey: u64, now: Instant) -> Occurrence {
    Occurrence {
        row,
        start_col: start,
        end_col: end,
        class: Class::Feline,
        langs: LangSet::EMPTY,
        form_id: FormId::UNKNOWN,
        seed: 0xCA7,
        ident: mix(0xCA7 ^ (u64::from(row) << 32) ^ u64::from(start)),
        appeared: now,
        genome: Genome {
            gkey,
            magic: (200 << 24) | 100,
        },
        ink_base: 0,
        ink_cells: 0,
        ink_bg: [0; 3],
        cat_colors: CatColorKey::default(),
        cat_text_clear: true,
        cat_peek_down: false,
        dec_line: false,
        inert: false,
        spec: class_default_spec(Class::Feline, &cfg()),
        custom: false,
        species: None,
    }
}

const DWELL_QUIET_MS: u64 = 950;

const GKEY_ADULT_HEAD: u64 = 3 << 13;

fn tick_cat(
    wd: &mut WordDecorations,
    now: Instant,
    cfg: &DecoConfig,
    geom: EffectGeom,
) -> (Vec<FreeSprite>, Vec<WordDecoration>, u64) {
    tick_cat_at(wd, now, cfg, geom, None, true)
}

/// The rows a peek sprite shows: its box within its clip (the box is the
/// whole art; the rise and the descent move the clip).
fn shown_h(p: &FreeSprite) -> u32 {
    let v = p.visible();
    u32::try_from(v.y1.saturating_sub(v.y0)).unwrap_or(0)
}

fn tick_cat_at(
    wd: &mut WordDecorations,
    now: Instant,
    cfg: &DecoConfig,
    geom: EffectGeom,
    companion_at: Option<(u16, u16)>,
    focused: bool,
) -> (Vec<FreeSprite>, Vec<WordDecoration>, u64) {
    tick_cat_companion(
        wd,
        now,
        cfg,
        geom,
        companion_at.map(CompanionOnGlass::at_cell),
        focused,
    )
}

fn tick_cat_companion(
    wd: &mut WordDecorations,
    now: Instant,
    cfg: &DecoConfig,
    geom: EffectGeom,
    companion: Option<CompanionOnGlass>,
    focused: bool,
) -> (Vec<FreeSprite>, Vec<WordDecoration>, u64) {
    let mut out = Vec::new();
    let mut ink = Vec::new();
    let mut fr = Vec::new();
    let mut nova = Vec::new();
    let fp = wd.tick(
        now, cfg, geom, companion, None, focused, &mut out, &mut ink, &mut fr, &mut nova,
    );
    (fr, out, fp)
}

fn still_frame(geom: EffectGeom, cursor: (u16, u16)) -> KittyCursorFrame {
    KittyCursorFrame {
        geom,
        cursor,
        look: KittyLook::default(),
        colors: CatColorKey::default(),
        bob: 0.0,
        lift: 0.0,
        alpha: 255,
        pose: crate::kitty_cursor::CatPose::STILL,
        sing: 0.0,
        notes: [None; crate::kitty_sing::MAX_NOTES],
    }
}

fn still_layout(geom: EffectGeom, cursor: (u16, u16)) -> KittyCursorLayout {
    KittyCursorLayout {
        geom,
        cursor,
        look: KittyLook::default(),
        bob: 0.0,
        lift: 0.0,
    }
}

fn settled_at(at: Instant) -> crate::kitty_cursor::CursorCatPlacementFrame {
    crate::kitty_cursor::CursorCatPlacementFrame {
        sampled_at: at,
        fold: None,
        facing_left: false,
    }
}

fn emit_placed(
    wd: &mut WordDecorations,
    geom: EffectGeom,
    cursor: (u16, u16),
    motion: crate::kitty_cursor::CursorCatPlacementFrame,
) -> (KittyCursorPlacement, FreeSprite) {
    let placement = wd
        .resolve_kitty_cursor_placement(still_layout(geom, cursor), motion)
        .expect("placeable companion");
    let mut free = Vec::new();
    wd.kitty_cursor_at_placement(still_frame(geom, cursor), placement, &mut free)
        .expect("companion emitted");
    (placement, free.last().expect("the body emits last").clone())
}

fn companion_frame(
    wd: &mut WordDecorations,
    now: Instant,
    c: &DecoConfig,
    g: EffectGeom,
    companion: Option<(u16, u16)>,
) -> Vec<FreeSprite> {
    let (mut free, _, _) = tick_cat_at(wd, now, c, g, companion, true);
    if let Some(cell) = companion {
        wd.kitty_cursor(still_frame(g, cell), &mut free);
    }
    free
}

fn ambient(free: &[FreeSprite]) -> Vec<FreeSprite> {
    free.iter()
        .filter(|s| matches!(s.z, FreeZ::UnderText))
        .cloned()
        .collect()
}

#[test]
fn incrementally_typed_kitty_always_starts_a_visible_top_row_cat() {
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let mut term = Terminal::new(g.rows, g.cols);
    let mut wd = WordDecorations::default();

    for (i, byte) in b"kitty".iter().copied().enumerate() {
        term.process(&[byte]);
        let now = t0 + Duration::from_millis(i as u64 * 40);
        wd.rescan(
            &term,
            g.rows as usize,
            g.cols as usize,
            &lex,
            &c,
            i as u64 + 1,
            now,
        );
        if i < 4 {
            assert!(
                wd.occ.iter().all(|o| o.class != Class::Feline),
                "a partial token must not trigger early"
            );
        }
    }

    let birth = t0 + Duration::from_millis(4 * 40);
    assert_eq!(
        wd.occ.iter().filter(|o| o.class == Class::Feline).count(),
        1,
        "the final y deterministically creates one kitty episode"
    );
    let (cats, _, _) = tick_cat_at(
        &mut wd,
        birth + Duration::from_millis(100),
        &c,
        g,
        None,
        true,
    );
    assert!(
        !cats.is_empty(),
        "a kitty typed on the top prompt row visibly peeks in"
    );
}

#[test]
fn a_downward_peek_is_cut_only_at_the_text_line() {
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let mut term = Terminal::new(g.rows, g.cols);
    term.process(b"\x1b[1;6Hkitty");
    let mut wd = scan_grid(&mut term, &c, g, t0, Some((0, 10)));
    assert!(
        feline(&wd).cat_peek_down,
        "a row-0 word has no habitable side above it — the plan must peek DOWN"
    );

    let mut seen: Vec<(u16, u16, u16)> = Vec::new();
    for ms in (0..(CAT_RISE_MS + DWELL_QUIET_MS + CAT_DESCEND_MS + 400)).step_by(16) {
        let (free, _, _) = tick_cat_at(&mut wd, t0 + Duration::from_millis(ms), &c, g, None, true);
        for s in free.iter().filter(|s| matches!(s.z, FreeZ::UnderText)) {
            let (_, y, _, h, skip) = window(s);
            assert!(
                y >= i32::from(g.cell_h) / 2,
                "no sprite pixel may bury the top-row word it decorates: y={y}"
            );
            seen.push((y as u16, h, skip));
        }
    }
    assert!(
        seen.len() >= 8,
        "the fixture must actually play a peek: {} frames",
        seen.len()
    );
    let hmin = seen.iter().map(|s| s.1).min().expect("non-empty");
    let hmax = seen.iter().map(|s| s.1).max().expect("non-empty");
    assert!(
        hmax > hmin,
        "the reveal must actually grow and shrink (h stuck at {hmin})"
    );

    let bottoms: Vec<u16> = seen.iter().map(|&(_, h, skip)| skip + h).collect();
    let b0 = bottoms[0];
    assert!(
        bottoms.iter().all(|&b| b == b0),
        "a DOWN peek must keep the art's BOTTOM edge on screen at every \
         frame — the cut belongs on the text line, never in open space. \
         Source bottoms seen: {bottoms:?}"
    );
}

#[test]
fn the_summoning_word_is_claimed_even_after_the_caret_moved_on() {
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let mut term = Terminal::new(g.rows, g.cols);
    term.process(b"\x1b[4;6Hkitty");
    let on_span = (3u16, 10u16);
    let past_span = (3u16, 11u16);
    assert!(
        caret_on_span(on_span, 3, 5, 9) && !caret_on_span(past_span, 3, 5, 9),
        "the rescan caret is on the word's span and the companion's is not"
    );
    let mut wd = scan_grid(&mut term, &c, g, t0, Some(on_span));
    let mut ctl = scan_grid(&mut term, &c, g, t0, Some(on_span));
    let occ = feline(&wd);
    assert_eq!((occ.row, occ.start_col, occ.end_col), (3, 5, 9));
    let (mut held, mut control) = (Vec::new(), Vec::new());
    for k in 0..6u64 {
        let now = t0 + Duration::from_millis(DWELL_QUIET_MS + 16 * k);
        held = companion_frame(&mut wd, now, &c, g, Some(past_span));
        control = companion_frame(&mut ctl, now, &c, g, None);
    }
    assert_eq!(
        ambient(&control).len(),
        1,
        "the word is still mid-peek at the sample instants"
    );
    assert!(
        ambient(&held).is_empty(),
        "the caret left the span before the companion existed — the claim \
         must still reach the word that summoned it"
    );
    assert_eq!(held.len(), 1, "exactly one cat on glass");
}

#[test]
fn a_feline_word_away_from_the_caret_still_peeks_beside_the_companion() {
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let mut term = Terminal::new(g.rows, g.cols);
    term.process(b"\x1b[6;1Hkitty\x1b[4;1H");
    let caret = (3u16, 0u16);
    let nat_h = (crate::kitty_pet::ART_ROWS * f32::from(g.cell_h)).round() as i32;
    let nat_w = (nat_h as f32 * crate::kitty_pet::ART_ASPECT).round() as i32;
    let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
    let feet = 2 * i32::from(g.cell_h);
    let pet = CompanionOnGlass::at_body(Some(caret), (grid_w - nat_w, grid_w, feet - nat_h, feet));
    let mut wd = scan_grid(&mut term, &c, g, t0, Some(caret));
    let mut ctl = scan_grid(&mut term, &c, g, t0, Some(caret));
    let mut resident = scan_grid(&mut term, &c, g, t0, Some(caret));
    let (mut with_companion, mut control, mut with_pet) = (Vec::new(), Vec::new(), Vec::new());
    for k in 0..6u64 {
        let now = t0 + Duration::from_millis(DWELL_QUIET_MS + 16 * k);
        with_companion = companion_frame(&mut wd, now, &c, g, Some(caret));
        control = companion_frame(&mut ctl, now, &c, g, None);
        with_pet = tick_cat_companion(&mut resident, now, &c, g, Some(pet), true).0;
    }
    assert_eq!(ambient(&control).len(), 1, "the away kitty peeks");
    assert_eq!(
        ambient(&with_pet),
        ambient(&control),
        "a pet standing rows away leaves the word's peek untouched"
    );
    assert_eq!(
        ambient(&with_companion),
        ambient(&control),
        "a companion at the caret leaves every OTHER feline word alone"
    );
    assert_eq!(
        with_companion.len(),
        2,
        "one away cat plus the companion — the intended pair"
    );
}

#[test]
fn animal_words_peek_their_species_heads() {
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let mut term = Terminal::new(g.rows, g.cols);
    term.process(b"\x1b[3;1Hmonkey\x1b[7;1Hcamel\x1b[10;1H");
    let mut wd = scan_grid(&mut term, &c, g, t0, Some((9, 0)));
    let species: Vec<_> = wd.occ.iter().filter_map(|o| o.species).collect();
    assert_eq!(
        species,
        vec![AnimalGlyphId::Monkey, AnimalGlyphId::Camel],
        "each word carries its own authored head"
    );
    assert!(
        wd.occ.iter().all(|o| o.class == Class::Animal),
        "both words classify as animals"
    );
    let mut free = Vec::new();
    for k in 0..6u64 {
        let now = t0 + Duration::from_millis(DWELL_QUIET_MS + 16 * k);
        let (f, _, _) = tick_cat_at(&mut wd, now, &c, g, None, true);
        free = f;
    }
    let ambient: Vec<_> = free
        .iter()
        .filter(|s| matches!(s.z, FreeZ::UnderText))
        .collect();
    assert_eq!(ambient.len(), 2, "both species heads peek: {ambient:?}");
    assert_eq!(
        wd.drain_kitty_sightings().count(),
        0,
        "no Kitty Log sighting for a species head"
    );
}

#[test]
fn a_second_feline_word_typed_while_the_companion_stays_up_is_claimed_too() {
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let (rows, cols) = (usize::from(g.rows), usize::from(g.cols));
    let mut term = Terminal::new(g.rows, g.cols);
    term.process(b"\x1b[4;6Hkitty");
    let caret_a = (3u16, 10u16);
    assert_eq!(
        (term.cursor().row, term.cursor().col),
        caret_a,
        "the caret parks one past word A"
    );
    let mut wd = WordDecorations::default();
    let mut ctl = WordDecorations::default();
    wd.rescan(&term, rows, cols, &lex, &c, 1, t0);
    ctl.rescan(&term, rows, cols, &lex, &c, 1, t0);
    let a_ident = feline(&wd).ident;
    for k in 0..6u64 {
        let now = t0 + Duration::from_millis(100 + 16 * k);
        tick_cat_at(&mut wd, now, &c, g, Some(caret_a), true);
        tick_cat_at(&mut ctl, now, &c, g, None, true);
    }
    assert_eq!(
        wd.companion_claim,
        Some(a_ident),
        "the first word is claimed while the caret sits on it"
    );
    let a_spent = t0 + Duration::from_millis(5000);
    tick_cat_at(&mut wd, a_spent, &c, g, Some(caret_a), true);
    tick_cat_at(&mut ctl, a_spent, &c, g, None, true);
    assert!(
        wd.persist[&a_ident].peek_done,
        "word A's one-shot has burned out"
    );
    term.process(b"\x1b[6;13Hkitty");
    let caret_b = (5u16, 17u16);
    assert_eq!(
        (term.cursor().row, term.cursor().col),
        caret_b,
        "the caret parks one past word B"
    );
    let b_born = a_spent + Duration::from_millis(16);
    wd.rescan(&term, rows, cols, &lex, &c, 2, b_born);
    ctl.rescan(&term, rows, cols, &lex, &c, 2, b_born);
    let b_ident = wd
        .occ
        .iter()
        .find(|o| o.class == Class::Feline && o.row == 5)
        .expect("the typed word B is on screen")
        .ident;
    for k in 0..3u64 {
        let now = b_born + Duration::from_millis(20 + 16 * k);
        tick_cat_at(&mut wd, now, &c, g, Some(caret_b), true);
        tick_cat_at(&mut ctl, now, &c, g, None, true);
    }
    assert_eq!(
        wd.companion_claim,
        Some(b_ident),
        "the claim FOLLOWED the caret to word B — pet-blind latching kept it on A"
    );
    let away = (1u16, 0u16);
    let mut control_peeked = false;
    for k in 0..46u64 {
        let now = b_born + Duration::from_millis(100 + 100 * k);
        let (held, _, _) = tick_cat_at(&mut wd, now, &c, g, Some(away), true);
        let (loose, _, _) = tick_cat_at(&mut ctl, now, &c, g, None, true);
        assert!(
            held.iter().all(|s| !matches!(s.z, FreeZ::UnderText)),
            "sample {k}: word B peeked an ambient cat out from under the resident companion"
        );
        control_peeked |= loose.iter().any(|s| matches!(s.z, FreeZ::UnderText));
    }
    assert!(
        control_peeked,
        "non-vacuity: with no companion up, word B peeks somewhere in this window"
    );
}

#[test]
fn a_word_under_the_pet_body_never_peeks() {
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let mut term = Terminal::new(g.rows, g.cols);
    term.process(b"\x1b[6;1Hkitty\x1b[4;1H");
    let caret = (3u16, 0u16);
    let mut ctl = scan_grid(&mut term, &c, g, t0, Some(caret));
    let mut control = Vec::new();
    for k in 0..6u64 {
        let now = t0 + Duration::from_millis(DWELL_QUIET_MS + 16 * k);
        control = tick_cat_companion(&mut ctl, now, &c, g, None, true).0;
    }
    let cats = ambient(&control);
    assert_eq!(cats.len(), 1, "the word peeks with nothing on glass");
    let (cat_x, cat_y, cat_w, cat_h, _) = window(&cats[0]);
    let nat_h = (crate::kitty_pet::ART_ROWS * f32::from(g.cell_h)).round() as i32;
    let nat_w = (nat_h as f32 * crate::kitty_pet::ART_ASPECT).round() as i32;
    let feet = cat_y + i32::from(cat_h);
    let x0 = cat_x + i32::from(cat_w) / 2 - nat_w / 2;
    let companion = CompanionOnGlass::at_body(None, (x0, x0 + nat_w, feet - nat_h, feet));
    let mut wd = scan_grid(&mut term, &c, g, t0, Some(caret));
    for k in 0..6u64 {
        let now = t0 + Duration::from_millis(DWELL_QUIET_MS + 16 * k);
        let (free, _, _) = tick_cat_companion(&mut wd, now, &c, g, Some(companion), true);
        assert!(
            ambient(&free).is_empty(),
            "frame {k}: the word peeked its cat straight under the pet's body"
        );
    }
}

#[test]
fn a_retype_after_clear_plays_a_second_cat_even_after_expiry() {
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let (rows, cols) = (g.rows as usize, g.cols as usize);
    for (label, wait) in [
        ("right after the clear", Duration::ZERO),
        ("after grace expiry", GRACE_TTL + Duration::from_secs(1)),
    ] {
        let t0 = Instant::now();
        let mut term = Terminal::new(g.rows, g.cols);
        let mut wd = WordDecorations::default();
        term.process(b"kitty");
        wd.rescan(&term, rows, cols, &lex, &c, 1, t0);
        let first = feline(&wd).clone();
        let (cats, _, _) = tick_cat(&mut wd, t0 + Duration::from_millis(100), &c, g);
        assert!(
            !cats.is_empty(),
            "{label}: PRECONDITION — the first kitty plays"
        );
        let done_t = t0 + Duration::from_millis(5000);
        tick_cat(&mut wd, done_t, &c, g);
        assert!(
            wd.persist[&first.ident].peek_done,
            "{label}: PRECONDITION — the first one-shot completes"
        );
        let done_key = first.ident ^ wd.persist[&first.ident].ctx_fp;

        term.process(b"\x1b[2J\x1b[H");
        let cleared = done_t + Duration::from_millis(16);
        wd.rescan(&term, rows, cols, &lex, &c, 2, cleared);
        assert!(wd.occ.iter().all(|o| o.class != Class::Feline));
        let mut seq = 3;
        if !wait.is_zero() {
            wd.rescan(&term, rows, cols, &lex, &c, seq, cleared + wait);
            seq += 1;
            assert!(
                !wd.persist.contains_key(&first.ident),
                "{label}: PRECONDITION — the episode really has grace-expired"
            );
            assert!(
                wd.done_marks.contains_key(&done_key),
                "{label}: PRECONDITION — …while its session done mark survives, \
                 which is the asymmetry the bug lived in"
            );
        }

        term.process(b"kitty");
        let retyped_at = cleared + wait + Duration::from_millis(16);
        wd.note_typed_edit(retyped_at, None);
        wd.rescan(&term, rows, cols, &lex, &c, seq, retyped_at);
        let second = feline(&wd).clone();
        assert_eq!(
            second.ident, first.ident,
            "{label}: PRECONDITION — the retype re-keys onto the same ledger row"
        );
        assert_eq!(
            second.appeared, retyped_at,
            "{label}: the retyped kitty is a FRESH episode, not an adoption"
        );
        assert!(!second.inert, "{label}: the retype must not be born-done");
        let (cats, _, _) = tick_cat(&mut wd, retyped_at + Duration::from_millis(100), &c, g);
        assert!(
            !cats.is_empty(),
            "{label}: a genuinely retyped kitty plays a SECOND cat"
        );
    }
}

const WITNESS_BASE: &[u8] = b"\r\n\r\nkitty\r\n\r\n$ ok";
const WITNESS_GAP: &[u8] = b"\x1b[3;1H------";
const PARK_ON_WORD: &[u8] = b"\x1b[3;7H";
const PARK_FAR_AWAY: &[u8] = b"\x1b[6;11H";
const WITNESS_RESTORE: &[u8] = b"\x1b[3;1Hkitty \x1b[3;6H";

fn play_out_and_taint(
    wd: &mut WordDecorations,
    term: &mut Terminal,
    park: &[u8],
    t0: Instant,
    label: &str,
) -> u64 {
    let (lex, c, g) = (lex(), cfg(), geom20());
    let (rows, cols) = (usize::from(g.rows), usize::from(g.cols));
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    term.process(WITNESS_BASE);
    wd.rescan(term, rows, cols, &lex, &c, 1, t0);
    let first = feline(wd).clone();
    assert!(!first.inert, "{label}: PRECONDITION — the first cat arms");
    let (cats, _, _) = tick_cat(wd, at(100), &c, g);
    assert!(!cats.is_empty(), "{label}: PRECONDITION — it draws");
    tick_cat(wd, at(5000), &c, g);
    let ep = wd.persist[&first.ident];
    assert!(
        ep.peek_done,
        "{label}: PRECONDITION — the one-shot completes"
    );
    assert!(
        wd.done_marks.contains_key(&(first.ident ^ ep.ctx_fp)),
        "{label}: PRECONDITION — the guard is stamped in place"
    );
    term.process(WITNESS_GAP);
    term.process(park);
    wd.rescan(term, rows, cols, &lex, &c, 2, at(5016));
    assert!(
        wd.persist[&first.ident].continuity_tainted,
        "{label}: PRECONDITION — the overwrite taints, so every arm argues \
         about the same branch"
    );
    first.ident
}

fn restore_verdict(
    wd: &mut WordDecorations,
    term: &mut Terminal,
    ident: u64,
    back_at: Instant,
    label: &str,
) -> (bool, bool) {
    let (lex, c, g) = (lex(), cfg(), geom20());
    term.process(WITNESS_RESTORE);
    wd.rescan(
        term,
        usize::from(g.rows),
        usize::from(g.cols),
        &lex,
        &c,
        3,
        back_at,
    );
    let back = feline(wd).clone();
    assert_eq!(
        back.ident, ident,
        "{label}: PRECONDITION — the restore re-keys identically"
    );
    assert!(
        wd.last_caret.is_some_and(|cell| caret_on_span(
            cell,
            back.row,
            back.start_col,
            back.end_col
        )),
        "{label}: PRECONDITION — the restored caret is parked on the word, \
         so only the keystroke can separate the verdicts"
    );
    let (cats, _, _) = tick_cat(wd, back_at + Duration::from_millis(100), &c, g);
    (back.inert, cats.is_empty())
}

#[test]
fn an_unrelated_edit_does_not_license_a_repaint_replay() {
    let g = geom20();
    let t0 = Instant::now();
    let mut verdicts = Vec::new();
    for (label, park) in [("retype", PARK_ON_WORD), ("unrelated edit", PARK_FAR_AWAY)] {
        let mut term = Terminal::new(g.rows, g.cols);
        let mut wd = WordDecorations::default();
        let ident = play_out_and_taint(&mut wd, &mut term, park, t0, label);
        let key_at = t0 + Duration::from_millis(5036);
        let back_at = key_at + Duration::from_millis(12);
        wd.note_typed_edit(key_at, None);
        assert!(
            wd.typed_edit_witness(back_at),
            "{label}: PRECONDITION — the time/pane half of the witness is \
             live in BOTH arms, so only the PLACE can decide"
        );
        verdicts.push((
            label,
            restore_verdict(&mut wd, &mut term, ident, back_at, label),
        ));
    }

    assert_eq!(
        verdicts[0],
        ("retype", (false, false)),
        "CONVERSE: a key committed ON the word is a retype and must still \
         play its cat"
    );
    assert_eq!(
        verdicts[1],
        ("unrelated edit", (true, true)),
        "a key committed five rows away in another widget must not license \
         a redraw's replay — that is the owner's 'new kitties appear when \
         the screen draws'"
    );
}

#[test]
fn a_split_keystroke_is_attributed_to_the_pane_it_was_typed_in() {
    let g = geom20();
    let t0 = Instant::now();
    const PANE_A: u64 = 1;
    const PANE_B: u64 = 2;

    let mut verdicts = Vec::new();
    for (label, park_a, park_b) in [
        ("typed ON the word in B", PARK_FAR_AWAY, PARK_ON_WORD),
        ("typed FAR from the word in B", PARK_ON_WORD, PARK_FAR_AWAY),
    ] {
        let mut wd = WordDecorations::default();
        let mut term_a = Terminal::new(g.rows, g.cols);
        let mut term_b = Terminal::new(g.rows, g.cols);
        wd.bind_pane(PANE_A, (0, 0));
        play_out_and_taint(&mut wd, &mut term_a, park_a, t0, label);
        wd.bind_pane(PANE_B, (400, 0));
        let ident_b = play_out_and_taint(&mut wd, &mut term_b, park_b, t0, label);

        wd.bind_pane(PANE_A, (0, 0));
        let key_at = t0 + Duration::from_millis(5036);
        let back_at = key_at + Duration::from_millis(12);
        wd.note_typed_edit(key_at, Some(PANE_B));
        assert_eq!(
            wd.typed_edit.expect("the key was recorded").cell,
            Some(caret_of(&term_b)),
            "{label}: the recorded caret must be PANE B's — the pane the \
             keystroke went to — not the bound pane's"
        );
        assert!(
            !wd.typed_edit_witness(back_at),
            "{label}: PRECONDITION — while A is bound, B's keystroke is no \
             witness for A's scan"
        );

        wd.bind_pane(PANE_B, (400, 0));
        assert!(
            wd.typed_edit_witness(back_at),
            "{label}: PRECONDITION — the time/pane half IS live for B in \
             both arms, so only the recorded PLACE can decide"
        );
        verdicts.push((
            label,
            restore_verdict(&mut wd, &mut term_b, ident_b, back_at, label),
        ));
    }

    assert_eq!(
        verdicts[0],
        ("typed ON the word in B", (false, false)),
        "a retype committed IN PANE B, on B's own word, must re-arm and \
         draw B's cat — reading pane A's caret refuses it"
    );
    assert_eq!(
        verdicts[1],
        ("typed FAR from the word in B", (true, true)),
        "and a key committed five rows away IN PANE B must not license B's \
         redraw replay just because pane A's caret happens to sit on pane \
         A's copy of the word"
    );
}

fn caret_of(term: &Terminal) -> (u16, u16) {
    let cp = term.cursor();
    (cp.row, cp.col)
}

#[test]
fn a_collapsed_split_still_witnesses_the_survivors_keystrokes() {
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let (rows, cols) = (g.rows as usize, g.cols as usize);
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();

    let mut survivor = Terminal::new(g.rows, g.cols);
    survivor.process(b"kitty");
    wd.bind_pane(1, (0, 0));
    wd.rescan(&survivor, rows, cols, &lex, &c, 1, t0);
    let parked_caret = wd.last_caret.expect("the survivor's caret was scanned");
    wd.bind_pane(2, (400, 0));

    wd.retain_panes(|k| k == 1);
    assert_eq!(wd.bound, None, "PRECONDITION: the live pane departed");
    assert!(wd.parked.contains_key(&1), "PRECONDITION: pane 1 is parked");

    survivor.process(b"\r\n\r\nkitty");
    wd.rescan(
        &survivor,
        rows,
        cols,
        &lex,
        &c,
        2,
        t0 + Duration::from_millis(16),
    );
    let live_caret = wd.last_caret.expect("the single-pane rescan re-stamped it");
    assert_ne!(
        live_caret, parked_caret,
        "PRECONDITION: the parked copy really is stale, so the two answers \
         below are distinguishable"
    );

    let key_at = t0 + Duration::from_millis(32);
    wd.note_typed_edit(key_at, Some(1));
    assert_eq!(
        wd.typed_edit.expect("recorded").cell,
        Some(live_caret),
        "the survivor's caret is the LIVE one — its parked slot froze when \
         the split was still up"
    );
    assert!(
        wd.typed_edit_witness(key_at),
        "and an unbound single-grid scan must still witness the key, or a \
         window that was once split never re-arms again"
    );
    wd.set_scan_session(Some(1));
    assert!(
        wd.typed_edit_witness(key_at),
        "declaring the survivor's own session must keep witnessing it"
    );
}

#[test]
fn declaring_an_unsplit_session_retires_a_stale_pane_binding() {
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let (rows, cols) = (g.rows as usize, g.cols as usize);
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();

    const PANE: u64 = 1;
    const TAB: u64 = 2;

    let mut term = Terminal::new(g.rows, g.cols);
    term.process(b"kitty");
    wd.bind_pane(PANE, (400, 0));
    wd.rescan(&term, rows, cols, &lex, &c, 1, t0);

    let key_at = t0 + Duration::from_millis(16);
    wd.note_typed_edit(key_at, Some(TAB));
    assert!(
        !wd.typed_edit_witness(key_at),
        "PRECONDITION: while the split tab's pane is bound, the OTHER tab's \
         key is correctly no witness for it"
    );

    wd.set_scan_session(Some(TAB));
    assert_eq!(
        wd.bound, None,
        "the stale binding is retired, not merely outranked"
    );
    assert!(
        wd.parked.contains_key(&PANE),
        "and the split tab's pane keeps its episodes, parked — going back \
         must not find its cats wiped"
    );
    assert_eq!(
        wd.pane_px,
        (0, 0),
        "an unbound engine's grid origin is the window's own"
    );
    assert!(
        wd.needs_rescan(1),
        "the live fields describe a grid that is no longer on glass, so the \
         declared session owes the engine a fresh scan"
    );
    assert!(
        wd.typed_edit_witness(key_at),
        "and the unsplit tab's own keystroke is a witness for its own scan \
         — the stale binding was refusing every one of them"
    );
}

#[test]
fn an_edit_in_one_tab_does_not_license_another_tabs_replay_when_unsplit() {
    let g = geom20();
    let t0 = Instant::now();
    const TAB_A: u64 = 11;
    const TAB_B: u64 = 12;

    let mut verdicts = Vec::new();
    for (label, key_tab) in [("same tab", TAB_A), ("another tab", TAB_B)] {
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(g.rows, g.cols);
        wd.set_scan_session(Some(TAB_A));
        let ident = play_out_and_taint(&mut wd, &mut term, PARK_ON_WORD, t0, label);

        let key_at = t0 + Duration::from_millis(5036);
        let back_at = key_at + Duration::from_millis(12);
        wd.note_typed_edit(key_at, Some(key_tab));
        assert_eq!(
            wd.typed_edit_witness(back_at),
            key_tab == TAB_A,
            "{label}: PRECONDITION — a key witnesses the scan of its OWN \
             tab and no other; this is the discriminator the verdicts below \
             must be reading"
        );
        verdicts.push((
            label,
            restore_verdict(&mut wd, &mut term, ident, back_at, label),
        ));
    }

    assert_eq!(
        verdicts[0],
        ("same tab", (false, false)),
        "a retype in the tab that is on glass must still re-arm and draw — \
         this is a scope, not a deletion"
    );
    assert_eq!(
        verdicts[1],
        ("another tab", (true, true)),
        "but a key committed in ANOTHER tab must not license this one's \
         repaint replay: once per kitty word, per tab"
    );
}

#[test]
fn identical_words_in_two_tabs_each_get_their_own_cat() {
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let (rows, cols) = (g.rows as usize, g.cols as usize);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);

    const TAB_A: u64 = 31;
    const TAB_B: u64 = 32;

    let text = b"\r\n\r\nkitty\r\n\r\n$ ok".as_slice();
    let mut tab_a = Terminal::new(g.rows, g.cols);
    tab_a.process(text);
    let mut tab_b = Terminal::new(g.rows, g.cols);
    tab_b.process(text);

    let mut wd = WordDecorations::default();

    wd.set_scan_session(Some(TAB_A));
    wd.rescan(&tab_a, rows, cols, &lex, &c, 1, t0);
    let a_ident = feline(&wd).ident;
    let (cats, _, _) = tick_cat(&mut wd, at(100), &c, g);
    assert!(!cats.is_empty(), "tab A's kitty plays its cat");
    tick_cat(&mut wd, at(5000), &c, g);
    assert!(
        wd.persist[&a_ident].peek_done,
        "PRECONDITION: and the one-shot is spent, which is what makes the \
         two arms below distinguishable"
    );

    wd.rescan(&tab_a, rows, cols, &lex, &c, 2, at(5016));
    let (cats, _, _) = tick_cat(&mut wd, at(5116), &c, g);
    assert!(
        cats.is_empty(),
        "one tab, one cat: a plain repaint of the same word must stay silent"
    );

    wd.set_scan_session(Some(TAB_B));
    wd.rescan(&tab_b, rows, cols, &lex, &c, 1, at(5116));
    let b_ident = feline(&wd).ident;
    assert_eq!(
        b_ident, a_ident,
        "PRECONDITION: the two words are INDISTINGUISHABLE by identity — \
         the scope this test pins is the episode map, not the key"
    );
    let (cats, _, _) = tick_cat(&mut wd, at(5216), &c, g);
    assert!(
        !cats.is_empty(),
        "tab B's kitty is tab B's own first sighting and owes its own cat — \
         a shared map hands it tab A's spent episode and it never appears"
    );

    wd.set_scan_session(Some(TAB_A));
    wd.rescan(&tab_a, rows, cols, &lex, &c, 3, at(5232));
    let (cats, _, _) = tick_cat(&mut wd, at(5332), &c, g);
    assert!(
        cats.is_empty(),
        "returning to tab A finds its own spent episode where it left it"
    );
}

#[test]
fn an_unbound_scans_pending_ignition_is_not_session_zeros_to_cancel() {
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    assert_eq!(
        wd.scan_scope(),
        None,
        "PRECONDITION: a fresh engine names neither a pane nor a session"
    );
    for (owner, cx) in [(1u64, 100i32), (2, 900), (3, 1700)] {
        grant_pane_ignition(
            &mut wd.ignitions,
            None,
            wd.pane_px,
            owner,
            t0,
            (cx, 100),
            0.0,
        );
    }
    let pending = wd
        .ignitions
        .iter()
        .find(|r| r.start > t0)
        .map(|r| r.start)
        .expect("PRECONDITION: the third grant is deferred past the window");

    wd.set_scan_session(Some(0));
    wd.prune_ignitions(t0 + Duration::from_millis(10));
    assert!(
        wd.ignitions
            .iter()
            .any(|r| r.pane.is_none() && r.start == pending),
        "session 0 must not be able to cancel the unbound scan's queued \
         nova: `None` is a key no session can hold"
    );
}

#[test]
fn typed_kitty_after_dense_cat_output_is_not_slot_starved() {
    let lex = lex();
    let c = cfg();
    let g = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 6,
        cols: 24,
    };
    let t0 = Instant::now();
    let mut term = Terminal::new(g.rows, g.cols);
    let mut wd = WordDecorations::default();

    term.process(b"cat cat cat cat cat cat\r\ncat cat cat cat cat cat");
    wd.rescan(&term, g.rows as usize, g.cols as usize, &lex, &c, 1, t0);
    tick_cat(&mut wd, t0 + Duration::from_millis(16), &c, g);
    assert_eq!(
        wd.persist
            .values()
            .filter(|e| e.shown_as == Some(KittyShownAs::Cat))
            .count(),
        MAX_CATS,
        "the dense screen arms every slot"
    );

    term.process(b"\x1b[2J\x1b[Hkitty");
    let t1 = t0 + Duration::from_millis(1000);
    wd.rescan(&term, g.rows as usize, g.cols as usize, &lex, &c, 2, t1);
    let typed = feline(&wd).clone();
    let (cats, _, _) = tick_cat(&mut wd, t1 + Duration::from_millis(100), &c, g);
    assert_eq!(
        wd.persist[&typed.ident].shown_as,
        Some(KittyShownAs::Cat),
        "scrolled-off history must not starve the typed word's slot"
    );
    assert!(
        !cats.is_empty(),
        "the typed kitty draws even right after cat-word-dense output"
    );

    tick_cat(&mut wd, t1 + Duration::from_millis(6000), &c, g);
    assert!(
        wd.persist
            .values()
            .all(|e| e.shown_as != Some(KittyShownAs::Cat) || e.peek_done),
        "scrolled-off mid-peek episodes complete by wall clock"
    );
}

#[test]
fn occlusion_mid_dwell_pauses_the_peek_and_resumes_when_rows_clear() {
    let (rows, cols) = (6usize, 20usize);
    let lex = lex();
    let c = cfg();
    let g = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: rows as u16,
        cols: cols as u16,
    };
    let t0 = Instant::now();
    let mut term = Terminal::new(rows as u16, cols as u16);
    term.process(b"\r\n\r\n\r\nkitty");
    let mut snap = aterm_core::render::RenderInput::default();
    term.cell_frame_into(&mut snap, rows, cols);
    let bg = rgb3_to_u32(snap.cells[3][0].bg);
    let mut wd = WordDecorations::default();
    wd.rescan_from_cells_with_geom_at_cursor(
        &snap.cells,
        &snap.line_sizes,
        rows,
        cols,
        &lex,
        &c,
        1,
        t0,
        g,
        bg,
        None,
    );
    let ident = feline(&wd).ident;
    let (cats, _, _) = tick_cat(&mut wd, t0 + Duration::from_millis(600), &c, g);
    assert!(!cats.is_empty(), "the kitty is peeking mid-dwell");

    let mut template = snap.cells[3][0];
    template.ch = ' ';
    template.wide = false;
    for r in [1usize, 2, 4, 5] {
        snap.cells[r].resize(cols, template);
        for cell in &mut snap.cells[r] {
            cell.ch = 'X';
        }
    }
    let t1 = t0 + Duration::from_millis(1000);
    wd.rescan_from_cells_with_geom_at_cursor(
        &snap.cells,
        &snap.line_sizes,
        rows,
        cols,
        &lex,
        &c,
        2,
        t1,
        g,
        bg,
        None,
    );
    let (cats, _, _) = tick_cat(&mut wd, t1 + Duration::from_millis(16), &c, g);
    assert!(cats.is_empty(), "an occluded cat emits nothing");
    assert!(
        wd.persist[&ident].peek_pause.is_some(),
        "occlusion latches the clearance pause"
    );

    let (cats, _, _) = tick_cat(&mut wd, t1 + Duration::from_millis(8000), &c, g);
    assert!(cats.is_empty());
    assert!(
        !wd.persist[&ident].peek_done,
        "a paused peek never silently completes while invisible"
    );

    for r in [1usize, 2, 4, 5] {
        for cell in &mut snap.cells[r] {
            cell.ch = ' ';
        }
    }
    let t3 = t1 + Duration::from_millis(9000);
    wd.rescan_from_cells_with_geom_at_cursor(
        &snap.cells,
        &snap.line_sizes,
        rows,
        cols,
        &lex,
        &c,
        3,
        t3,
        g,
        bg,
        None,
    );
    let (cats, _, _) = tick_cat(&mut wd, t3 + Duration::from_millis(16), &c, g);
    assert!(
        !cats.is_empty(),
        "the cat resumes its dwell when the rows clear"
    );
    assert!(!wd.persist[&ident].peek_done);
    let (cats, _, _) = tick_cat(&mut wd, t3 + Duration::from_millis(6000), &c, g);
    assert!(cats.is_empty(), "the resumed peek runs to completion");
    assert!(
        wd.persist[&ident].peek_done,
        "after resuming, the one-shot completes normally"
    );
}

#[test]
fn bottom_rows_keep_occurrence_slots_on_a_match_saturated_screen() {
    let (rows, cols) = (5usize, 160usize);
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let mut term = Terminal::new(rows as u16, cols as u16);
    let dense = b"cat ".repeat(39);
    for _ in 0..4 {
        term.process(&dense);
        term.process(b"cat\r\n");
    }
    term.process(b"kitty");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, rows, cols, &lex, &c, 1, t0);

    assert_eq!(wd.occ.len(), 121, "kept suffix is rows 1..=4");
    assert!(
        wd.occ.iter().all(|o| o.row >= 1),
        "whole TOP rows are dropped, never bottom ones"
    );
    let prompt_kitty = wd
        .occ
        .iter()
        .find(|o| o.row == 4)
        .expect("the prompt row keeps its occurrence slot");
    assert_eq!(prompt_kitty.start_col, 0);
    assert_eq!(prompt_kitty.appeared, t0);
    assert!(
        wd.occ
            .windows(2)
            .all(|w| (w[0].row, w[0].start_col) < (w[1].row, w[1].start_col)),
        "row-major order is preserved within the kept rows"
    );
    assert!(
        wd.scan_row_occupied[0],
        "skipped dense rows still record truthful occupancy for the taint sweep"
    );

    wd.rescan(
        &term,
        rows,
        cols,
        &lex,
        &c,
        2,
        t0 + Duration::from_millis(16),
    );
    assert_eq!(wd.occ.len(), 121);
    let again = wd
        .occ
        .iter()
        .find(|o| o.row == 4)
        .expect("prompt slot survives the second pass");
    assert_eq!(
        again.appeared, t0,
        "the same screen re-adopts the same episode (no truncation churn)"
    );

    let mut tall = Terminal::new(8, cols as u16);
    for _ in 0..7 {
        tall.process(&dense);
        tall.process(b"cat\r\n");
    }
    tall.process(b"kitty");
    wd.rescan(&tall, 8, cols, &lex, &c, 3, t0 + Duration::from_millis(32));
    let (ink_spans, live_row) = wd.pet_ink();
    assert_eq!(live_row, Some(7));
    for (r, &span) in ink_spans.iter().enumerate().take(7) {
        assert_eq!(
            span,
            (0, 159),
            "row {r} above or below the saturation cutoff keeps its true ink span"
        );
    }
    assert_eq!(ink_spans[7], (0, 5));
}

#[test]
fn fold_free_covers_every_field_including_z_art_and_clip() {
    let art = |seed: u8| {
        aterm_render::fx::bitmap(
            aterm_core::render::FxTexels::Rgba8Straight,
            10,
            12,
            vec![seed; 10 * 12 * 4],
        )
        .expect("art")
    };
    let s = FreeSprite {
        x: -3,
        y: 5,
        w: 10,
        h: 12,
        art: art(1),
        clip: PxRect::EVERYWHERE,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z: FreeZ::UnderText,
    };
    let h0 = aterm_hash::FNV64_OFFSET;
    let base = fold_free(h0, &s);
    assert_ne!(
        base,
        fold_free(
            h0,
            &FreeSprite {
                z: FreeZ::OverText,
                ..s.clone()
            }
        ),
        "a z flip must move the fingerprint"
    );
    assert_ne!(
        base,
        fold_free(
            h0,
            &FreeSprite {
                art: art(2),
                ..s.clone()
            }
        ),
        "another art must move the fingerprint"
    );
    assert_ne!(
        base,
        fold_free(
            h0,
            &FreeSprite {
                clip: PxRect::new(0, 0, 4, 4),
                ..s.clone()
            }
        ),
        "a clip must move the fingerprint"
    );
    assert_ne!(
        base,
        fold_free(h0, &FreeSprite { x: 3, ..s.clone() }),
        "the signed origin folds by bit pattern"
    );
    assert_ne!(
        base,
        fold_free(h0, &FreeSprite { alpha: 254, ..s }),
        "alpha folds"
    );
}

#[test]
fn a_settled_keyword_kitty_breathes_without_resizing() {
    let now = Instant::now();
    let g = geom20();
    let build = || WordDecorations {
        occ: vec![cat_occ(2, 2, 6, 0, now)],
        cols: 20,
        have_scanned: true,
        ..WordDecorations::default()
    };
    let sample = |wd: &mut WordDecorations, c: &DecoConfig, ms: u64| {
        let (fr, _, _) = tick_cat(wd, now + Duration::from_millis(ms), c, g);
        fr.iter()
            .find(|s| matches!(s.z, FreeZ::UnderText))
            .map(|s| {
                let (x, y, w, h, _) = window(s);
                (x, y, w, h, s.art.w(), s.art.h())
            })
    };

    let c = cfg();
    let mut wd = build();
    for t in [100u64, 300, 440, 700] {
        tick_cat(&mut wd, now + Duration::from_millis(t), &c, g);
    }
    let frames: Vec<_> = [900u64, 1150, 1400, 1650, 1900, 2150]
        .into_iter()
        .filter_map(|ms| sample(&mut wd, &c, ms))
        .collect();
    assert!(
        frames.len() >= 5,
        "the fixture must sit in the dwell: {} frames",
        frames.len()
    );
    let origins: std::collections::BTreeSet<(i32, i32)> =
        frames.iter().map(|f| (f.0, f.1)).collect();
    assert!(
        origins.len() > 1,
        "a settled word-cat must be alive, not a sticker: {origins:?}"
    );
    let sizes: std::collections::BTreeSet<(u16, u16, u16, u16)> =
        frames.iter().map(|f| (f.2, f.3, f.4, f.5)).collect();
    assert_eq!(
        sizes.len(),
        1,
        "the idle pose translates and never resizes: {sizes:?}"
    );
}

#[test]
fn fresh_episode_latches_phase_clock_at_birth() {
    let lex = lex();
    let c = cfg();
    let g = EffectGeom {
        rows: 8,
        ..geom20()
    };
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(8, 20);
    term.process(b"\r\n\r\nnice kitty\r\n\r\nsame kitten");
    wd.rescan(&term, 8, 20, &lex, &c, 1, t0);
    for o in wd.occ.iter().filter(|o| o.spec.graphic.is_some()) {
        assert_eq!(
            wd.persist[&o.ident].phase_start,
            Some(t0),
            "the phase clock latches at birth, before any tick"
        );
    }
    let (fr, _, _) = tick_cat_at(&mut wd, t0 + Duration::from_millis(100), &c, g, None, true);
    let rows_at = |fr: &[FreeSprite]| -> std::collections::BTreeSet<i32> {
        fr.iter()
            .map(|s| (s.y + i32::from(s.h) - 1).div_euclid(20))
            .collect()
    };
    assert!(
        !fr.is_empty() && rows_at(&fr).len() == 2,
        "BOTH cats rise together from the first ticked frame (no \
         stagger), got {fr:?}"
    );
}

#[test]
fn freeze_thaw_mid_rise_resumes_at_same_phase() {
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut term = Terminal::new(4, 20);
    term.process(b"\r\n\r\na calm kitty");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 4, 20, &lex, &c, 1, t0);
    let mut ctl = WordDecorations::default();
    ctl.rescan(&term, 4, 20, &lex, &c, 1, t0);
    tick_cat_at(&mut wd, t0, &c, g, None, true);
    tick_cat_at(&mut ctl, t0, &c, g, None, true);
    let (q_a, _, _) = tick_cat_at(&mut wd, at(200), &c, g, None, true);
    let (q_c, _, _) = tick_cat_at(&mut ctl, at(200), &c, g, None, true);
    assert_eq!(q_a, q_c, "twin engines agree pre-freeze");
    assert!(!q_a.is_empty(), "mid-rise quads present");
    wd.freeze(at(200));
    let (q_frozen, _, fp_frozen) = {
        let mut out = Vec::new();
        let mut ink = Vec::new();
        let mut fr = Vec::new();
        let mut nv = Vec::new();
        let fp = wd.tick(
            at(1000),
            &c,
            g,
            None,
            None,
            true,
            &mut out,
            &mut ink,
            &mut fr,
            &mut nv,
        );
        (fr, out, fp)
    };
    assert!(
        q_frozen.is_empty() && fp_frozen == 0,
        "frozen ticks emit nothing"
    );
    wd.thaw(at(5200));
    let (q_resumed, _, _) = tick_cat_at(&mut wd, at(5200), &c, g, None, true);
    assert_eq!(
        q_resumed, q_a,
        "thaw resumes at the SAME phase (nothing completed invisibly)"
    );
    let o = wd.occ[0].clone();
    let total = peek_total_ms(o.ident, o.genome.gkey, false, (2200, 3598));
    let (q_mid, _, _) = tick_cat_at(&mut wd, at(5000 + CAT_RISE_MS + 300), &c, g, None, true);
    assert!(!q_mid.is_empty(), "the dwell continues after the thaw");
    let (q_done, _, _) = tick_cat_at(&mut wd, at(5000 + total + 50), &c, g, None, true);
    assert!(
        q_done.is_empty(),
        "the shifted one-shot completes exactly once"
    );
}

#[test]
fn one_shot_peek_conformance_real_engine() {
    let m = aterm_models_effects::one_shot_peek_model();
    let mut st = m.init_state();
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(6, 20);
    term.process(b"\r\n\r\na calm kitty");
    let blank = Terminal::new(6, 20);
    wd.rescan(&term, 6, 20, &lex, &c, 1, t0);
    let o = wd
        .occ
        .iter()
        .find(|o| o.class == Class::Feline)
        .unwrap()
        .clone();
    let variant =
        special_variant_v4(o.genome.magic).unwrap_or_else(|| cat_variant_v4(o.genome.gkey));
    let expected_h = u32::from(cat_geometry(&o, g, variant).hart);
    let dwell = peek_dwell_ms(o.ident, o.genome.gkey, false, (2200, 3598));
    let total = peek_total_ms(o.ident, o.genome.gkey, false, (2200, 3598));
    tick_cat_at(&mut wd, t0, &c, g, None, true);
    assert!(m.fire("Start", &mut st));
    let (q, _, _) = tick_cat_at(&mut wd, at(100), &c, g, None, true);
    assert!(!q.is_empty(), "rising");
    let frozen_q = q.clone();
    wd.freeze(at(200));
    wd.thaw(at(3200));
    let (q, _, _) = tick_cat_at(&mut wd, at(3100 + 200), &c, g, None, true);
    assert!(
        q.iter().map(shown_h).sum::<u32>() >= frozen_q.iter().map(shown_h).sum::<u32>(),
        "the thawed rise continues from where it froze"
    );
    let sh = |ms: u64| at(3000 + ms);
    let (q_dwell, _, _) = tick_cat_at(&mut wd, sh(CAT_RISE_MS + 300), &c, g, None, true);
    assert!(
        q_dwell.iter().map(shown_h).sum::<u32>() >= expected_h,
        "dwelling at the full rest reveal"
    );
    assert!(m.fire("Step", &mut st));
    let (q_mid, _, _) = tick_cat_at(&mut wd, sh(CAT_RISE_MS + dwell / 2), &c, g, None, true);
    assert!(
        !q_mid.is_empty(),
        "the mid-dwell present shows the body sprite"
    );
    wd.rescan(&blank, 6, 20, &lex, &c, 2, sh(CAT_RISE_MS + dwell / 2));
    wd.rescan(&term, 6, 20, &lex, &c, 3, sh(CAT_RISE_MS + dwell / 2 + 500));
    let back = sh(CAT_RISE_MS + dwell - 200);
    let (q, _, _) = tick_cat_at(&mut wd, back, &c, g, None, true);
    assert!(
        q.iter().map(shown_h).sum::<u32>() >= expected_h,
        "the re-hit continues the SAME dwell (no re-Rise)"
    );
    assert!(m.check_invariant("NoRepeek", &st));
    let (q, _, _) = tick_cat_at(
        &mut wd,
        sh(CAT_RISE_MS + dwell + anticipation_ms(o.genome.gkey) + CAT_DESCEND_MS / 2),
        &c,
        g,
        None,
        true,
    );
    assert!(
        !q.is_empty() && q.iter().map(shown_h).sum::<u32>() < expected_h,
        "descending (shrinking reveal)"
    );
    assert!(m.fire("Step", &mut st));
    let (q, _, _) = tick_cat_at(&mut wd, sh(total + 20), &c, g, None, true);
    assert!(q.is_empty(), "Done");
    assert!(m.fire("Finish", &mut st));
    wd.rescan(&blank, 6, 20, &lex, &c, 4, sh(total + 100));
    let expired = sh(total + 100 + 15_000);
    wd.rescan(&blank, 6, 20, &lex, &c, 5, expired);
    assert!(wd.persist.is_empty(), "grace expiry swept the episode");
    wd.rescan(
        &term,
        6,
        20,
        &lex,
        &c,
        6,
        expired + Duration::from_millis(100),
    );
    let (q, _, _) = tick_cat_at(
        &mut wd,
        expired + Duration::from_millis(200),
        &c,
        g,
        None,
        true,
    );
    assert!(q.is_empty(), "the re-appearance is born-done: no entrance");
    assert!(m.fire("Repeek", &mut st), "healthy Repeek is a no-op");
    assert!(m.check_invariant("NoRepeek", &st));
    assert!(m.check_invariant("PhaseBounded", &st));
    assert_eq!(st[&"phase"], 4, "the model (like the cat) ends Done");
    assert_eq!(st[&"rises"], 1, "exactly one entrance, ever");
}

#[test]
fn feline_magic_off_disables_rare_cats() {
    let now = Instant::now();
    let mut o = cat_occ(2, 2, 6, GKEY_ADULT_HEAD, now);
    o.genome.magic = 8;
    let mut wd = WordDecorations {
        occ: vec![o],
        cols: 20,
        have_scanned: true,
        ..WordDecorations::default()
    };
    let mut c = cfg();
    c.feline_magic = false;
    let g = geom20();
    let (_, out, _) = tick_cat(&mut wd, now + Duration::from_millis(120), &c, g);
    assert!(
        !out.iter().any(|d| matches!(d.glyph, DecoGlyph::Star4)),
        "magic=false ⇒ no Nebula aura"
    );
}

#[test]
fn shipping_cat_sprites_preserve_authored_aspect_and_uniform_age_scale() {
    let now = Instant::now();
    let render = |gkey: u64, magic: u64| {
        let mut occurrence = cat_occ(2, 2, 6, gkey, now);
        occurrence.genome.magic = magic;
        let mut wd = WordDecorations {
            occ: vec![occurrence],
            cols: 20,
            have_scanned: true,
            ..WordDecorations::default()
        };
        let (sprites, _, _) = tick_cat(
            &mut wd,
            now + Duration::from_millis(DWELL_QUIET_MS),
            &cfg(),
            geom20(),
        );
        assert_eq!(sprites.len(), 1, "shipping bake must land one sprite");
        sprites[0].clone()
    };
    let assert_aspect = |sprite: &FreeSprite, variant: CatGlyphId| {
        let expected = f32::from(GLYPHS[variant as usize].aspect_x1000) / 1000.0;
        let actual = f32::from(sprite.w) / f32::from(sprite.h);
        assert!(
            (actual - expected).abs() <= 0.04,
            "{variant:?}: shipping sprite {actual:.3} != authored {expected:.3}"
        );
        assert_eq!(
            (sprite.art.w(), sprite.art.h()),
            (sprite.w, sprite.h),
            "a peek draws its art 1:1"
        );
    };

    let head_variant = cat_variant_v4(GKEY_ADULT_HEAD);
    let head = render(GKEY_ADULT_HEAD, (200 << 24) | 100);
    assert_aspect(&head, head_variant);

    let maneki = render(GKEY_ADULT_HEAD, 0);
    assert_aspect(&maneki, CatGlyphId::SpecManeki);
    assert!(
        maneki.w < maneki.h,
        "the tall authored Maneki must not be stretched into the old wide head box"
    );

    let kitten_key = genome::gray_encode(0) << 13;
    let elder_key = genome::gray_encode(3) << 13;
    let kitten = render(kitten_key, (200 << 24) | 100);
    let elder = render(elder_key, (200 << 24) | 100);
    assert_aspect(&kitten, cat_variant_v4(kitten_key));
    assert_aspect(&elder, cat_variant_v4(elder_key));
    assert!(
        elder.w > kitten.w && elder.h > kitten.h,
        "age must scale width and height together: kitten={kitten:?}, elder={elder:?}"
    );
}

/// A word past 65,535 px (4096 columns of 20 px) keeps its cat over it: the cat's left edge
/// is `i32`, as the sprite it places is, where a `u16` edge wrapped the cat to the far side of
/// the frame. A word at the grid's start is the control.
#[test]
fn a_word_cat_past_the_u16_space_sits_over_its_word() {
    let geom = EffectGeom {
        cell_w: 20,
        cell_h: 40,
        rows: 3,
        cols: 4096,
    };
    for (start, end) in [(10u16, 12u16), (4090, 4092)] {
        let g = cat_geometry_for(start, end, 0, geom, cat_variant_v4(0));
        let (word_x0, word_x1) = (i32::from(start) * 20, (i32::from(end) + 1) * 20);
        assert!(
            g.x + i32::from(g.w) > word_x0 && g.x < word_x1,
            "the cat at x {} (w {}) is off its word {word_x0}..{word_x1}",
            g.x,
            g.w
        );
    }
}

#[test]
fn rotation_alignment_plays_new_twin_and_pins_survivor() {
    let model = aterm_models_effects::sparkle_reflow_cardinality_model();
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let mut twins = Terminal::new(6, 64);
    twins.process(b"dear kitty friend\r\n\r\ndear kitty friend");
    wd.rescan(&twins, 6, 64, &lex, &c, 1, t0);
    let cats: Vec<Occurrence> = wd
        .occ
        .iter()
        .filter(|o| o.class == Class::Feline)
        .cloned()
        .collect();
    assert_eq!(cats.len(), 2);
    let seed = cats[0].seed;
    let (id0, id1) = (mix(seed), mix(seed ^ ORDINAL_MIX));
    assert_eq!((cats[0].ident, cats[1].ident), (id0, id1));
    wd.persist.get_mut(&id0).expect("top twin").nova_done = true;
    wd.persist.get_mut(&id1).expect("surviving twin").nova_done = true;
    let mut rotated = Terminal::new(6, 64);
    rotated.process(b"\r\n\r\ndear kitty friend\r\n\r\ndear kitty friend");
    let t1 = t0 + Duration::from_secs(1);
    let births_before = wd.birth_seq;
    wd.rescan(&rotated, 6, 64, &lex, &c, 2, t1);
    let mut state = model.init_state();
    assert!(model.fire("RotatePair", &mut state));
    let cats2: Vec<Occurrence> = wd
        .occ
        .iter()
        .filter(|o| o.class == Class::Feline)
        .cloned()
        .collect();
    assert_eq!(cats2.len(), 2);
    assert_eq!(cats2[0].row, 2);
    assert_eq!(cats2[0].appeared, t0, "the survivor keeps its episode");
    assert_eq!(cats2[1].row, 4);
    assert_eq!(cats2[1].appeared, t1, "the new bottom twin plays");
    assert!(!wd.persist[&id1].born_done, "fresh, not born-done");
    assert_eq!(
        wd.birth_seq - births_before,
        state[&"fresh"] as u64,
        "real rotation births project onto RotatePair"
    );
    let transferred_spent = cats2
        .iter()
        .filter(|o| o.appeared == t0 && wd.persist[&o.ident].nova_done)
        .count() as i64;
    let fresh_armed = cats2
        .iter()
        .filter(|o| o.appeared == t1 && !wd.persist[&o.ident].nova_done)
        .count() as i64;
    assert_eq!(transferred_spent, state[&"transferred"]);
    assert_eq!(fresh_armed, state[&"armed"]);
    assert!(model.check_invariant("NoFalseTransfers", &state));
    assert!(
        wd.persist.contains_key(&id0) && wd.persist.contains_key(&id1),
        "both live idents hold episodes"
    );
}

#[test]
fn global_insertion_transfers_dense_triple_without_births() {
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let mut original = Terminal::new(7, 64);
    original
        .process(b"\x1b[1;1Hdear kitty friend\x1b[3;1Hdear kitty friend\x1b[5;1Hdear kitty friend");
    wd.rescan(&original, 7, 64, &lex, &c, 1, t0);
    assert_eq!(wd.occ.len(), 3);
    for o in &wd.occ {
        wd.persist.get_mut(&o.ident).expect("old live").nova_done = true;
    }

    let mut inserted = Terminal::new(7, 64);
    inserted
        .process(b"\x1b[2;1Hdear kitty friend\x1b[4;1Hdear kitty friend\x1b[6;1Hdear kitty friend");
    let births_before = wd.birth_seq;
    wd.rescan(
        &inserted,
        7,
        64,
        &lex,
        &c,
        2,
        t0 + Duration::from_millis(16),
    );
    assert_eq!(wd.birth_seq, births_before, "all three episodes transfer");
    assert_eq!(wd.occ.len(), 3);
    assert!(wd.occ.iter().all(|o| o.appeared == t0));
    assert!(wd.occ.iter().all(|o| wd.persist[&o.ident].nova_done));
}

#[test]
fn alignment_skip_semantics_old_episode_goes_unmatched() {
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let mut a = Terminal::new(24, 64);
    a.process(b"dear kitty friend\r\n\r\n\r\n\r\n\r\ndear kitty friend");
    wd.rescan(&a, 24, 64, &lex, &c, 1, t0);
    let mut b = Terminal::new(24, 64);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&b"\r\n".repeat(5));
    bytes.extend_from_slice(b"dear kitty friend");
    bytes.extend_from_slice(&b"\r\n".repeat(15));
    bytes.extend_from_slice(b"dear kitty friend");
    b.process(&bytes);
    let t1 = t0 + Duration::from_secs(1);
    wd.rescan(&b, 24, 64, &lex, &c, 2, t1);
    let cats: Vec<Occurrence> = wd
        .occ
        .iter()
        .filter(|o| o.class == Class::Feline)
        .cloned()
        .collect();
    assert_eq!(cats.len(), 2);
    assert_eq!(cats[0].row, 5);
    assert_eq!(cats[0].appeared, t0, "5→5: the row-5 episode transfers");
    assert_eq!(cats[1].row, 20);
    assert_eq!(
        cats[1].appeared, t1,
        "20 is FRESH — the unmatched row-0 episode must NOT adopt it"
    );
}

#[test]
fn reset_flush_writes_marks_and_reentry_is_born_done() {
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(4, 20);
    term.process(b"\r\n\r\nnice kitty");
    wd.rescan(&term, 4, 20, &lex, &c, 1, t0);
    tick_cat_at(&mut wd, t0, &c, g, None, true);
    let (q, _, _) = tick_cat_at(&mut wd, t0 + Duration::from_millis(100), &c, g, None, true);
    assert!(!q.is_empty());
    wd.reset();
    assert_eq!(wd.done_marks.len(), 1, "the flush pass wrote the mark");
    wd.rescan(&term, 4, 20, &lex, &c, 2, t0 + Duration::from_secs(1));
    assert!(
        wd.persist.values().all(|e| e.born_done),
        "born-done re-entry"
    );
    let (q, _, _) = tick_cat_at(&mut wd, t0 + Duration::from_millis(1100), &c, g, None, true);
    assert!(q.is_empty(), "a born-done cat emits zero quads");
    assert!(
        !wd.is_active(t0 + Duration::from_millis(1100)),
        "inert: no wakes"
    );
    wd.hard_reset();
    assert!(wd.done_marks.is_empty());
    wd.rescan(&term, 4, 20, &lex, &c, 3, t0 + Duration::from_secs(2));
    tick_cat_at(&mut wd, t0 + Duration::from_millis(2000), &c, g, None, true);
    let (q, _, _) = tick_cat_at(&mut wd, t0 + Duration::from_millis(2100), &c, g, None, true);
    assert!(!q.is_empty(), "hard_reset makes re-ignition reachable");
}

#[test]
fn transfer_then_expire_writes_mark_under_departure_ident() {
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let mut wd = WordDecorations::default();
    let mut twins = Terminal::new(6, 64);
    twins.process(b"dear kitty friend\r\n\r\ndear kitty friend");
    wd.rescan(&twins, 6, 64, &lex, &c, 1, t0);
    tick_cat_at(&mut wd, t0 + Duration::from_millis(100), &c, g, None, true);
    let mut lone = Terminal::new(6, 64);
    lone.process(b"\r\n\r\ndear kitty friend");
    wd.rescan(&lone, 6, 64, &lex, &c, 2, at(1));
    let survivor = feline(&wd).clone();
    assert_eq!(survivor.appeared, t0, "the survivor kept its episode");
    let blank = Terminal::new(6, 64);
    wd.rescan(&blank, 6, 64, &lex, &c, 3, at(15));
    assert!(wd.persist.is_empty());
    assert!(!wd.done_marks.is_empty(), "expiry wrote the done marks");
    wd.rescan(&lone, 6, 64, &lex, &c, 4, at(16));
    assert!(
        wd.persist[&survivor.ident].born_done,
        "the re-appearance is suppressed by the transferred mark"
    );
}

#[test]
fn resize_settle_births_are_inert_and_unmarked() {
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut wd = WordDecorations::default();
    let empty40 = Terminal::new(4, 40);
    wd.rescan(&empty40, 4, 40, &lex, &c, 1, t0);
    let mut narrower = Terminal::new(4, 32);
    narrower.process(b"\r\n\r\nnice kitty");
    wd.rescan(&narrower, 4, 32, &lex, &c, 2, at(100));
    assert!(
        wd.persist.values().any(|e| e.born_settled),
        "the width-step birth is born-settled"
    );
    let (q, _, _) = tick_cat_at(&mut wd, at(200), &c, g, None, true);
    assert!(q.is_empty(), "born-settled: the entrance is skipped");
    let blank = Terminal::new(4, 32);
    wd.rescan(&blank, 4, 32, &lex, &c, 3, at(15_000));
    assert!(wd.persist.is_empty());
    assert!(
        wd.done_marks.is_empty(),
        "born-settled episodes are never written to done_marks"
    );
    wd.rescan(&narrower, 4, 32, &lex, &c, 4, at(16_000));
    assert!(
        wd.persist.values().all(|e| !e.born_settled && !e.born_done),
        "stable-cols births play"
    );
    tick_cat_at(&mut wd, at(16_050), &c, g, None, true);
    let (q, _, _) = tick_cat_at(&mut wd, at(16_250), &c, g, None, true);
    assert!(!q.is_empty(), "the post-settle birth rises");
}

#[test]
fn kitty_sighting_once_per_episode_across_occlusion() {
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(4, 20);
    term.process(b"\r\n\r\na happy kitty naps");
    let blank = Terminal::new(4, 20);
    wd.rescan(&term, 4, 20, &lex, &c, 1, t0);
    tick_cat_at(&mut wd, t0, &c, g, None, true);
    assert_eq!(
        wd.drain_kitty_sightings().count(),
        0,
        "reveal 0: no quads yet"
    );
    tick_cat_at(&mut wd, t0 + Duration::from_millis(100), &c, g, None, true);
    let s: Vec<KittySighting> = wd.drain_kitty_sightings().collect();
    assert_eq!(s.len(), 1, "one sighting on the first landed present");
    assert_eq!(s[0].shown_as, KittyShownAs::Cat);
    assert!(!s[0].langs.is_empty(), "language attribution rides along");
    tick_cat_at(&mut wd, t0 + Duration::from_millis(300), &c, g, None, true);
    assert_eq!(wd.drain_kitty_sightings().count(), 0, "once per episode");
    wd.rescan(&blank, 4, 20, &lex, &c, 2, at(1));
    wd.rescan(&term, 4, 20, &lex, &c, 3, at(2));
    tick_cat_at(&mut wd, at(2), &c, g, None, true);
    assert_eq!(
        wd.drain_kitty_sightings().count(),
        0,
        "a grace re-hit never recounts"
    );
    wd.rescan(&blank, 4, 20, &lex, &c, 4, at(15));
    assert!(wd.persist.is_empty());
    wd.rescan(&term, 4, 20, &lex, &c, 5, at(16));
    tick_cat_at(
        &mut wd,
        at(16) + Duration::from_millis(100),
        &c,
        g,
        None,
        true,
    );
    assert_eq!(
        wd.drain_kitty_sightings().count(),
        0,
        "a born-done re-appearance logs nothing (§1.1 inertness)"
    );
}

fn cfg_nova() -> DecoConfig {
    DecoConfig {
        profanity_style: ProfanityStyle::Nova,
        ..cfg()
    }
}

#[allow(clippy::type_complexity, reason = "test-local 5-stream harness tuple")]
fn tick_nova(
    wd: &mut WordDecorations,
    now: Instant,
    cfg: &DecoConfig,
    geom: EffectGeom,
    cursor: Option<(u16, u16)>,
    sel: Option<SelView<'_>>,
) -> (
    Vec<GlowQuad>,
    Vec<WordDecoration>,
    Vec<InkCell>,
    Vec<FreeSprite>,
    u64,
) {
    let mut out = Vec::new();
    let mut ink = Vec::new();
    let mut fr = Vec::new();
    let mut nova = Vec::new();
    let fp = wd.tick(
        now,
        cfg,
        geom,
        cursor.map(CompanionOnGlass::at_cell),
        sel,
        true,
        &mut out,
        &mut ink,
        &mut fr,
        &mut nova,
    );
    (nova, out, ink, fr, fp)
}

fn ignition_starts(wd: &WordDecorations) -> Vec<Option<Instant>> {
    wd.occ
        .iter()
        .filter(|o| o.class == Class::Profanity)
        .map(|o| wd.persist.get(&o.ident).and_then(|e| e.nova_start))
        .collect()
}

fn granted_novas(now: Instant, words: &[(u16, u64, u64, bool)]) -> WordDecorations {
    let mut wd = WordDecorations {
        cols: 20,
        have_scanned: true,
        ..WordDecorations::default()
    };
    for &(row, seed, magic, dec_line) in words {
        let o = Occurrence {
            row,
            start_col: 4,
            end_col: 7,
            seed,
            ident: mix(seed),
            genome: Genome { gkey: 0, magic },
            dec_line,
            spec: class_default_spec(Class::Profanity, &cfg_nova()),
            ..occ(Class::Profanity, now)
        };
        let mut e = Episode::fresh(now, o.genome, seed, row, 0);
        e.nova_start = Some(now);
        wd.persist.insert(o.ident, e);
        wd.occ.push(o);
    }
    wd
}

fn select_span(row: i32, c0: u16, c1: u16) -> aterm_core::selection::TextSelection {
    use aterm_core::selection::{SelectionSide, SelectionType};
    let mut s = aterm_core::selection::TextSelection::new();
    s.start_selection(row, c0, SelectionSide::Left, SelectionType::Simple);
    s.update_selection(row, c1, SelectionSide::Right);
    s.complete_selection();
    s
}

#[test]
fn nova_one_flash_per_episode_across_occlusion() {
    let lex = lex();
    let c = cfg_nova();
    let g = geom20();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(2, 20);
    term.process(b"oh fuck");
    wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
    let (q, _, _, _, _) = tick_nova(&mut wd, t0, &c, g, None, None);
    assert!(q.is_empty(), "the Dip is ink-only (no quads)");
    assert_eq!(ignition_starts(&wd), vec![Some(t0)]);
    assert!(wd.is_active(t0), "the granted window arms the scheduler");
    let (q, _, _, _, _) = tick_nova(&mut wd, at(180), &c, g, None, None);
    assert!(!q.is_empty(), "the flash crown emits additive quads");
    let (q1, _, _, _, fp1) = tick_nova(&mut wd, at(500), &c, g, None, None);
    assert!(!q1.is_empty(), "ring quads");
    let (_, _, _, _, fp2) = tick_nova(&mut wd, at(516), &c, g, None, None);
    assert_ne!(fp1, fp2, "an animating nova must never be present-skipped");
    let (_, out, _, _, _) = tick_nova(&mut wd, at(700), &c, g, None, None);
    assert!(
        out.iter()
            .filter(|d| matches!(d.blend, DecoBlend::Add))
            .count()
            > 1,
        "ballistic debris motes on the Add stream, got {out:?}"
    );
    let (q, out, _, _, _) = tick_nova(&mut wd, at(2600), &c, g, None, None);
    assert!(q.is_empty(), "the fade emits nothing on the nova stream");
    assert_eq!(out.len(), 1, "the ember spark is fading");
    assert!(matches!(out[0].blend, DecoBlend::Add));
    assert!(wd.is_active(at(2600)), "the ember fade is animating");
    let (q, out, _, _, fpa) = tick_nova(&mut wd, at(3600), &c, g, None, None);
    assert!(q.is_empty() && out.is_empty(), "the ember faded to nothing");
    let (_, _, _, _, fpb) = tick_nova(&mut wd, at(3616), &c, g, None, None);
    assert_eq!(fpa, fpb, "the post-fade steady state is fp-stable");
    assert!(
        !wd.is_active(at(3616)),
        "a spent nova disarms the scheduler"
    );
    let blank = Terminal::new(2, 20);
    wd.rescan(&blank, 2, 20, &lex, &c, 2, at(4000));
    wd.rescan(&term, 2, 20, &lex, &c, 3, at(5000));
    tick_nova(&mut wd, at(5000), &c, g, None, None);
    let (q, _, _, _, _) = tick_nova(&mut wd, at(5300), &c, g, None, None);
    assert!(
        q.is_empty(),
        "a grace re-hit must not strobe (nova_done survives)"
    );
    assert!(!wd.is_active(at(5300)));
    wd.rescan(&blank, 2, 20, &lex, &c, 4, at(16_000));
    assert!(wd.persist.is_empty(), "grace expiry sweeps the episode");
    wd.rescan(&term, 2, 20, &lex, &c, 5, at(17_000));
    tick_nova(&mut wd, at(17_000), &c, g, None, None);
    assert_eq!(
        ignition_starts(&wd),
        vec![None],
        "born-done: the limiter is never asked"
    );
    let (q, out, ink, _, _) = tick_nova(&mut wd, at(17_300), &c, g, None, None);
    assert!(q.is_empty(), "born-done never re-ignites");
    assert!(out.is_empty(), "born-done emits no residual decos");
    assert!(
        !ink.is_empty(),
        "born-done still shows the settled ink bytes"
    );
    assert!(!wd.is_active(at(17_300)), "born-done is inert (zero wakes)");
}

#[test]
fn done_mark_lru_real_order_and_constant_work_conform() {
    const CAP: usize = 3;
    const MAX_LINK_WRITES: u8 = 6;
    let model = aterm_models_effects::done_mark_lru_model();
    let mut state = model.init_state();
    let mut marks = DoneMarkLru::default();

    for key in [10u64, 20, 30] {
        let mutation = marks.insert_with_cap(key, CAP);
        assert!(model.fire("Insert", &mut state));
        assert_eq!(mutation.evicted, None);
        assert!(mutation.link_writes <= MAX_LINK_WRITES);
        assert_eq!(marks.len() as i64, state[&"resident"]);
        marks.assert_valid();
    }
    assert_eq!(marks.keys_oldest_first(), vec![10, 20, 30]);

    assert!(marks.touch(10));
    assert!(model.fire("Touch", &mut state));
    assert_eq!(marks.keys_oldest_first(), vec![20, 30, 10]);
    marks.assert_valid();

    let mutation = marks.insert_with_cap(40, CAP);
    assert!(model.fire("ReplaceOldest", &mut state));
    assert_eq!(mutation.evicted, Some(20));
    assert!(mutation.link_writes <= MAX_LINK_WRITES);
    assert_eq!(marks.keys_oldest_first(), vec![30, 10, 40]);
    assert_eq!(marks.len() as i64, state[&"resident"]);
    assert_eq!(state[&"selections"], 1);
    assert!(model.check_invariant("ConstantSelection", &state));
    marks.assert_valid();

    let mut buggy = aterm_models_effects::done_mark_lru_model();
    for cst in &mut buggy.consts {
        if cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
    let mut bad = buggy.init_state();
    for _ in 0..CAP {
        assert!(buggy.fire("Insert", &mut bad));
    }
    assert!(buggy.fire("ReplaceOldest", &mut bad));
    assert_eq!(bad[&"selections"], CAP as i64);
    assert!(!buggy.check_invariant("ConstantSelection", &bad));
}

#[test]
fn done_mark_lru_full_capacity_replacement_is_bounded_and_growth_free() {
    const MAX_LINK_WRITES: u8 = 6;
    let mut marks = DoneMarkLru::default();
    for key in 0..DONE_MARKS_CAP as u64 {
        let mutation = marks.insert(key);
        assert_eq!(mutation.evicted, None);
        assert!(mutation.link_writes <= MAX_LINK_WRITES);
    }
    assert_eq!(marks.len(), DONE_MARKS_CAP);
    let capacities = (
        marks.index.capacity(),
        marks.nodes.capacity(),
        marks.free.capacity(),
    );
    for offset in 0..4096u64 {
        let key = DONE_MARKS_CAP as u64 + offset;
        let mutation = marks.insert(key);
        assert_eq!(mutation.evicted, Some(offset));
        assert!(mutation.link_writes <= MAX_LINK_WRITES);
        assert_eq!(marks.len(), DONE_MARKS_CAP);
    }
    assert!(
        marks.index.capacity() <= capacities.0,
        "replacement unexpectedly grew the warmed hash allocation"
    );
    assert_eq!(marks.nodes.capacity(), capacities.1);
    assert_eq!(marks.free.capacity(), capacities.2);
    assert_eq!(marks.nodes.len(), DONE_MARKS_CAP);
    assert!(marks.free.is_empty());
    marks.assert_valid();
}

fn reservation_episode(owner: u64, now: Instant) -> Episode {
    let mut episode = Episode::fresh(
        now,
        Genome {
            gkey: owner,
            magic: mix(owner),
        },
        owner,
        0,
        0,
    );
    episode.burst_kind = Some(BurstKind::Nova);
    episode.burst_roll = true;
    episode
}

fn reserve_overlapping(wd: &mut WordDecorations, owner: u64, now: Instant) -> Instant {
    wd.persist.insert(owner, reservation_episode(owner, now));
    let start = grant_ignition(&mut wd.ignitions, None, owner, now, (100, 100), 88.0)
        .expect("a live owner must fit the structural reservation cap");
    wd.persist
        .get_mut(&owner)
        .expect("owner resident")
        .nova_start = Some(start);
    start
}

fn assert_reservation_projection(
    wd: &WordDecorations,
    now: Instant,
    state: &std::collections::BTreeMap<&'static str, i64>,
) {
    let pending = wd
        .ignitions
        .iter()
        .filter(|reservation| reservation.start > now)
        .count() as i64;
    let recent = wd
        .ignitions
        .iter()
        .filter(|reservation| reservation.start <= now && reservation.start + IGNITION_WINDOW > now)
        .count() as i64;
    assert_eq!(wd.persist.len() as i64, state[&"live"]);
    assert_eq!(pending, state[&"pending"]);
    assert_eq!(recent, state[&"recent"]);
    assert_eq!(wd.ignitions.len(), (pending + recent) as usize);
    assert!(wd.ignitions.len() <= MAX_IGNITION_RESERVATIONS);
}

#[test]
fn ignition_reservation_lifecycle_real_queue_conforms() {
    let model = aterm_models_effects::ignition_reservation_lifecycle_model();
    let t0 = Instant::now();
    let mut state = model.init_state();
    let mut wd = WordDecorations::default();

    assert_eq!(reserve_overlapping(&mut wd, 1, t0), t0);
    assert!(model.fire("ReserveNow", &mut state));
    assert_eq!(reserve_overlapping(&mut wd, 2, t0), t0 + IGNITION_WINDOW);
    assert!(model.fire("ReserveFuture", &mut state));
    assert_eq!(
        reserve_overlapping(&mut wd, 3, t0),
        t0 + IGNITION_WINDOW + IGNITION_WINDOW
    );
    assert!(model.fire("ReserveFuture", &mut state));
    assert_reservation_projection(&wd, t0, &state);

    let initial = wd.ignitions.clone();

    wd.persist.remove(&2);
    assert!(model.fire("CancelFuture", &mut state));
    wd.prune_ignitions(t0);
    assert_reservation_projection(&wd, t0, &state);
    assert!(wd.ignitions.iter().all(|r| r.owner != 2));

    wd.persist.remove(&1);
    assert!(model.fire("DepartFired", &mut state));
    wd.prune_ignitions(t0);
    assert_reservation_projection(&wd, t0, &state);
    assert!(wd.ignitions.iter().any(|r| r.owner == 1));

    let t1 = t0 + IGNITION_WINDOW;
    assert!(model.fire("ExpireRecent", &mut state));
    wd.prune_ignitions(t1);
    assert_reservation_projection(&wd, t1, &state);

    assert_eq!(reserve_overlapping(&mut wd, 4, t1), t1);
    assert!(model.fire("ReserveNow", &mut state));
    assert_reservation_projection(&wd, t1, &state);

    let t2 = t1 + IGNITION_WINDOW;
    wd.prune_ignitions(t2);
    assert!(model.fire("ExpireRecent", &mut state));
    assert!(model.fire("FirePending", &mut state));
    assert_reservation_projection(&wd, t2, &state);
    for invariant in [
        "FutureOwned",
        "PendingBound",
        "RecentBound",
        "ReservationBound",
    ] {
        assert!(model.check_invariant(invariant, &state), "{invariant}");
    }

    let mut buggy = aterm_models_effects::ignition_reservation_lifecycle_model();
    for cst in &mut buggy.consts {
        if cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
    let mut bad = buggy.init_state();
    for action in ["ReserveNow", "ReserveFuture", "ReserveFuture"] {
        assert!(buggy.fire(action, &mut bad));
    }
    assert!(buggy.fire("CancelFuture", &mut bad));
    assert!(buggy.fire("CancelFuture", &mut bad));
    let mut expiry_only = initial;
    expiry_only.retain(|reservation| reservation.start + IGNITION_WINDOW > t0);
    let ownerless_pending = expiry_only
        .iter()
        .filter(|reservation| reservation.start > t0)
        .count() as i64;
    assert_eq!(bad[&"live"], 1);
    assert_eq!(bad[&"pending"], ownerless_pending);
    assert!(!buggy.check_invariant("FutureOwned", &bad));
}

#[test]
fn ignition_reservation_rekey_real_queue_conforms() {
    let model = aterm_models_effects::ignition_reservation_rekey_model();
    let mut state = model.init_state();
    let t0 = Instant::now();
    let t1 = t0 + IGNITION_WINDOW;
    let t2 = t1 + IGNITION_WINDOW;
    let rekey_at = t0 + Duration::from_millis(100);
    let lexicon = lex();
    let config = cfg_nova();
    let mut wd = WordDecorations::default();
    let mut original = Terminal::new(2, 48);
    original.process(b"oh fuck");
    let mut moved = Terminal::new(2, 48);
    moved.process(b"status fuck");
    wd.rescan(&original, 2, 48, &lexicon, &config, 1, t0);
    let old_owner = wd.occ[0].ident;
    let mut probe = WordDecorations::default();
    probe.rescan(&moved, 2, 48, &lexicon, &config, 1, t0);
    let expected_new_owner = probe.occ[0].ident;
    assert_ne!(old_owner, expected_new_owner, "fixture must rekey");

    let mut blocker = 1u64;
    while blocker == old_owner || blocker == expected_new_owner {
        blocker += 1;
    }

    assert_eq!(reserve_overlapping(&mut wd, blocker, t0), t0);
    let original_start = grant_ignition(&mut wd.ignitions, None, old_owner, t0, (100, 100), 88.0)
        .expect("live target fits structural cap");
    assert_eq!(original_start, t1);
    wd.persist
        .get_mut(&old_owner)
        .expect("target episode")
        .nova_start = Some(original_start);
    assert!(model.fire("GrantDelayed", &mut state));

    wd.rescan(&moved, 2, 48, &lexicon, &config, 2, rekey_at);
    let new_owner = wd.occ[0].ident;
    assert_eq!(new_owner, expected_new_owner);
    assert!(model.fire("Rekey", &mut state));
    assert_eq!(wd.persist[&new_owner].nova_start, Some(t1));
    assert!(
        wd.ignitions
            .iter()
            .any(|reservation| reservation.owner == new_owner && reservation.start == t1),
        "the future limiter slot must move with its episode"
    );

    wd.prune_ignitions(rekey_at);
    assert!(model.fire("Prune", &mut state));
    assert_eq!(wd.ignitions.len(), 2);

    let mut competitor = blocker + 1;
    while wd.persist.contains_key(&competitor) {
        competitor += 1;
    }
    assert_eq!(reserve_overlapping(&mut wd, competitor, rekey_at), t2);
    assert!(model.fire("CompetingGrant", &mut state));
    for invariant in [
        "RekeyOwnsReservation",
        "DelayedSlotSurvivesPrune",
        "NoOverlappingFlash",
        "PhaseBounded",
    ] {
        assert!(model.check_invariant(invariant, &state), "{invariant}");
    }

    let mut buggy = aterm_models_effects::ignition_reservation_rekey_model();
    for cst in &mut buggy.consts {
        if cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
    let mut bad_state = buggy.init_state();
    let mut bad = WordDecorations::default();
    assert_eq!(reserve_overlapping(&mut bad, 1, t0), t0);
    assert_eq!(reserve_overlapping(&mut bad, 2, t0), t1);
    assert!(buggy.fire("GrantDelayed", &mut bad_state));
    let episode = bad.persist.remove(&2).expect("old aligned episode");
    bad.persist.insert(22, episode);
    assert!(buggy.fire("Rekey", &mut bad_state));
    assert_eq!(bad.persist[&22].nova_start, Some(t1));

    bad.prune_ignitions(t0);
    assert!(buggy.fire("Prune", &mut bad_state));
    assert!(
        bad.ignitions
            .iter()
            .all(|reservation| reservation.owner != 2)
    );
    assert_eq!(reserve_overlapping(&mut bad, 3, t0), t1);
    assert!(buggy.fire("CompetingGrant", &mut bad_state));
    assert_eq!(bad.persist[&22].nova_start, bad.persist[&3].nova_start);
    assert!(!buggy.check_invariant("RekeyOwnsReservation", &bad_state));
    assert!(!buggy.check_invariant("DelayedSlotSurvivesPrune", &bad_state));
    assert!(!buggy.check_invariant("NoOverlappingFlash", &bad_state));
}

#[test]
fn vanished_classic_nova_flood_has_constant_reservation_storage() {
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    assert_eq!(reserve_overlapping(&mut wd, 1, t0), t0);

    assert_eq!(reserve_overlapping(&mut wd, 2, t0), t0 + IGNITION_WINDOW);
    wd.persist.remove(&2);
    wd.prune_ignitions(t0);
    let capacity = wd.ignitions.capacity();

    for owner in 3..10_003u64 {
        assert_eq!(
            reserve_overlapping(&mut wd, owner, t0),
            t0 + IGNITION_WINDOW
        );
        wd.persist.remove(&owner);
        wd.prune_ignitions(t0);
        assert_eq!(wd.ignitions.len(), 1);
    }
    assert_eq!(wd.ignitions.capacity(), capacity);
    assert_eq!(wd.ignitions[0].owner, 1);
}

#[test]
fn limiter_delays_third_of_three_ignitions_deterministically() {
    let lex = lex();
    let c = cfg_nova();
    let g = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 16,
        cols: 20,
    };
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(16, 20);
    term.process(b"fuck\r\n\r\n\r\n\r\n\r\n\r\nfuck");
    wd.rescan(&term, 16, 20, &lex, &c, 1, t0);
    tick_nova(&mut wd, t0, &c, g, None, None);
    assert_eq!(
        ignition_starts(&wd),
        vec![Some(t0), Some(t0)],
        "two disjoint ignitions fit one rolling second"
    );
    term.process(b"\r\n\r\n\r\n\r\n\r\n\r\nfuck");
    let t1 = t0 + Duration::from_millis(200);
    wd.rescan(&term, 16, 20, &lex, &c, 2, t1);
    tick_nova(&mut wd, t1, &c, g, None, None);
    assert_eq!(
        ignition_starts(&wd),
        vec![Some(t0), Some(t0), Some(t0 + IGNITION_WINDOW)],
        "the 3rd of 3 ignitions inside 200 ms is delayed to t0 + 1 s"
    );
    assert!(wd.is_active(t1));
    let tf = t0 + IGNITION_WINDOW + Duration::from_millis(300);
    let (q, _, _, _, _) = tick_nova(&mut wd, tf, &c, g, None, None);
    assert!(
        q.iter().any(|quad| quad.row >= 10),
        "the delayed row-12 nova rings after its shifted Dip start"
    );
}

#[test]
fn limiter_tightens_to_one_per_second_on_overlap() {
    let lex = lex();
    let c = cfg_nova();
    let g = geom20();
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(2, 20);
    term.process(b"fuck fuck");
    wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
    tick_nova(&mut wd, t0, &c, g, None, None);
    assert_eq!(
        ignition_starts(&wd),
        vec![Some(t0), Some(t0 + IGNITION_WINDOW)],
        "overlapping regions admit only 1 ignition per rolling second"
    );
}

#[test]
fn limiter_reused_owner_label_does_not_alias_fired_episode() {
    let t0 = Instant::now();
    let t1 = t0 + IGNITION_WINDOW;
    let mut ignitions = Vec::new();
    assert_eq!(
        grant_ignition(&mut ignitions, None, 7, t0, (100, 100), 88.0),
        Some(t0)
    );
    assert_eq!(
        grant_ignition(
            &mut ignitions,
            None,
            7,
            t0 + Duration::from_millis(100),
            (100, 100),
            88.0,
        ),
        Some(t1),
        "a later episode with the same owner label still respects history"
    );
    assert_eq!(ignitions.len(), 2);
}

#[test]
fn flash_limiter_conformance_real_limiter_projects_onto_model() {
    let tick = Duration::from_millis(250);
    let t0 = Instant::now();

    let m = aterm_models_effects::flash_limiter_model();
    let mut st = m.init_state();
    let mut igns = Vec::new();
    let a = grant_ignition(&mut igns, None, 1, t0, (100, 100), 88.0).expect("capacity");
    assert_eq!(a, t0, "an empty window grants immediately");
    assert!(m.fire("Ignite", &mut st), "the model admits ignition 1");
    let b = grant_ignition(&mut igns, None, 2, t0, (900, 100), 88.0).expect("capacity");
    assert_eq!(b, t0, "two disjoint ignitions fit one rolling second");
    assert!(m.fire("Ignite", &mut st), "the model admits ignition 2");
    assert!(m.check_invariant("IgnitionBound", &st));
    assert!(m.check_invariant("RegionFlashPairs", &st));
    assert!(
        !m.action_enabled("Ignite", &st),
        "the model refuses a 3rd in-window ignition"
    );
    let c = grant_ignition(
        &mut igns,
        None,
        3,
        t0 + Duration::from_millis(200),
        (100, 700),
        88.0,
    )
    .expect("capacity");
    assert_eq!(
        c,
        t0 + IGNITION_WINDOW,
        "the real limiter delays to t0 + 1 s"
    );
    for k in 1u32..=4 {
        assert!(m.fire("Shift", &mut st));
        assert_eq!(
            m.action_enabled("Ignite", &st),
            t0 + tick * k >= c,
            "model re-admission at tick {k} must match the granted slot"
        );
    }
    assert!(
        m.fire("Ignite", &mut st),
        "the delayed ignition fires at its slot"
    );
    assert!(m.check_invariant("IgnitionBound", &st));
    assert!(m.check_invariant("RegionFlashPairs", &st));

    let mut mo = aterm_models_effects::flash_limiter_model();
    for cst in &mut mo.consts {
        if cst.0 == "Overlap" {
            cst.1 = 1;
        }
    }
    let mut st = mo.init_state();
    let mut igns = Vec::new();
    let a = grant_ignition(&mut igns, None, 1, t0, (100, 100), 88.0).expect("capacity");
    assert_eq!(a, t0);
    assert!(mo.fire("Ignite", &mut st));
    assert!(
        !mo.action_enabled("Ignite", &st),
        "overlap tightens the model to ≤ 1 per rolling second"
    );
    let b = grant_ignition(&mut igns, None, 2, t0, (150, 100), 88.0).expect("capacity");
    assert_eq!(
        b,
        t0 + IGNITION_WINDOW,
        "overlap: 2nd ignition delayed a full window"
    );
    for k in 1u32..=4 {
        assert!(mo.fire("Shift", &mut st));
        assert_eq!(
            mo.action_enabled("Ignite", &st),
            t0 + tick * k >= b,
            "overlap re-admission at tick {k} must match the granted slot"
        );
    }
    assert!(mo.fire("Ignite", &mut st));
    assert!(mo.check_invariant("IgnitionBound", &st));
    assert!(mo.check_invariant("RegionFlashPairs", &st));
}

#[test]
fn flash_limiter_negative_control_overlap_blind_limiter_is_buggy_trace() {
    let mut healthy = aterm_models_effects::flash_limiter_model();
    let mut buggy = aterm_models_effects::flash_limiter_model();
    for cst in &mut healthy.consts {
        if cst.0 == "Overlap" {
            cst.1 = 1;
        }
    }
    for cst in &mut buggy.consts {
        if cst.0 == "Overlap" || cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
    let t0 = Instant::now();
    let mut igns = Vec::new();
    let a = grant_ignition(&mut igns, None, 1, t0, (100, 100), 0.0).expect("capacity");
    let b = grant_ignition(&mut igns, None, 2, t0, (150, 100), 0.0).expect("capacity");
    assert_eq!(
        (a, b),
        (t0, t0),
        "the overlap-blind limiter admits two overlapping ignitions in one second"
    );
    let mut st = buggy.init_state();
    assert!(buggy.fire("Ignite", &mut st));
    assert!(
        buggy.fire("Ignite", &mut st),
        "Buggy = 1 admits the second overlapping in-window ignition"
    );
    assert!(
        !healthy.check_invariant("IgnitionBound", &st),
        "2 overlapping ignitions in one second must violate IgnitionBound"
    );
    assert!(
        !healthy.check_invariant("RegionFlashPairs", &st),
        "4 transition pairs on the shared region must violate RegionFlashPairs"
    );
}

#[test]
fn selection_defers_ignition_and_attenuates_active_novas() {
    let lex = lex();
    let c = cfg_nova();
    let g = geom20();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(2, 20);
    term.process(b"oh fuck");
    wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
    let sel = select_span(0, 0, 19);
    let sv = SelView {
        sel: &sel,
        display_offset: 0,
    };
    tick_nova(&mut wd, t0, &c, g, None, Some(sv));
    assert_eq!(
        ignition_starts(&wd),
        vec![None],
        "no ignition while selected"
    );
    tick_nova(&mut wd, at(500), &c, g, None, Some(sv));
    assert_eq!(ignition_starts(&wd), vec![None]);
    tick_nova(&mut wd, at(800), &c, g, None, None);
    assert_eq!(ignition_starts(&wd), vec![Some(at(800))]);
    let (q_free, _, _, _, _) = tick_nova(&mut wd, at(1300), &c, g, None, None);
    assert!(
        !q_free.is_empty(),
        "ring live at t = 500 ms into the window"
    );
    let (q_sel, _, _, _, _) = tick_nova(&mut wd, at(1316), &c, g, None, Some(sv));
    assert!(
        q_sel.iter().all(|quad| quad.row != 0),
        "quads over the selected row attenuate away, got {q_sel:?}"
    );
    assert!(q_sel.len() < q_free.len());
}

#[test]
fn reduced_motion_nova_is_a_static_glint() {
    let lex = lex();
    let mut c = cfg_nova();
    c.reduced_motion = true;
    let g = geom20();
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(2, 20);
    term.process(b"oh fuck");
    wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
    let (q1, out1, ink1, _, fp1) = tick_nova(&mut wd, t0, &c, g, None, None);
    assert!(q1.is_empty(), "no quads under reduced motion, ever");
    assert_eq!(out1.len(), 1, "one static ember spark from frame 0");
    assert!(matches!(out1[0].blend, DecoBlend::Add));
    assert_eq!(
        ignition_starts(&wd),
        vec![None],
        "no ignition is ever granted"
    );
    assert!(!wd.is_active(t0), "a static glint never animates");
    let (q2, out2, ink2, _, fp2) =
        tick_nova(&mut wd, t0 + Duration::from_secs(3), &c, g, None, None);
    assert!(q2.is_empty());
    assert_eq!(out1, out2);
    assert_eq!(ink1, ink2, "the ember-tinted static gradient holds");
    assert_eq!(fp1, fp2);
    let occ = &wd.occ[0];
    let feats = nova_features(occ.genome.gkey);
    let (_, ember) = nova::ember_pair(nova::palette(feats.palette), None);
    if nova_magic(occ.genome.magic).is_none() {
        assert_eq!(out1[0].color, ember);
    }
}

#[test]
fn nova_dip_freezes_sweep_and_embers_ink() {
    let lex = lex();
    let c = cfg_nova();
    let g = geom20();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(2, 20);
    term.process(b"oh fuck");
    wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
    tick_nova(&mut wd, t0, &c, g, None, None);
    let (_, _, ink_a, _, _) = tick_nova(&mut wd, at(600), &c, g, None, None);
    let (_, _, ink_b, _, _) = tick_nova(&mut wd, at(700), &c, g, None, None);
    assert_eq!(ink_a, ink_b, "the sweep is frozen during the nova window");
    let (_, _, ink_dip, _, _) = tick_nova(&mut wd, at(60), &c, g, None, None);
    assert_eq!(ink_dip.len(), ink_a.len());
    for (d, s) in ink_dip.iter().zip(&ink_a) {
        let dimmed = dim_rgb(rgb3_to_u32(s.color), nova::dip_envelope(60));
        assert_eq!(rgb3_to_u32(d.color), dimmed, "dip = envelope × settled ink");
    }
    let c_v1 = cfg();
    let mut wd2 = WordDecorations::default();
    wd2.rescan(&term, 2, 20, &lex, &c_v1, 1, t0);
    let mut sweep_a = Vec::new();
    let mut sweep_b = Vec::new();
    let mut o = Vec::new();
    tick_ink(&mut wd2, at(600), &c_v1, &mut o, &mut sweep_a);
    tick_ink(&mut wd2, at(700), &c_v1, &mut o, &mut sweep_b);
    assert_ne!(
        sweep_a, sweep_b,
        "control: the v1 sweep moves between frames"
    );
    let (_, _, ink_ember, _, _) = tick_nova(&mut wd, at(2600), &c, g, None, None);
    assert_ne!(
        ink_ember, ink_a,
        "the ember tint differs from the live palette"
    );
}

#[test]
fn coupling_pulse_is_deterministic_and_luminance_bounded() {
    let lex = lex_with_emphasis();
    let g = geom20();
    let t0 = Instant::now();
    let end_ms = nova::RING_END_MS + nova::PULSE_MS + 50;
    let run = |style: ProfanityStyle| -> Vec<(u64, Vec<InkCell>)> {
        let cfgv = DecoConfig {
            profanity_style: style,
            ..cfg()
        };
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, 40);
        term.process(b"fuck ultrathink");
        wd.rescan(&term, 2, 40, &lex, &cfgv, 1, t0);
        tick_nova(&mut wd, t0, &cfgv, g, None, None);
        (0..end_ms)
            .step_by(10)
            .map(|ms| {
                let (_, _, ink, _, _) = tick_nova(
                    &mut wd,
                    t0 + Duration::from_millis(ms),
                    &cfgv,
                    g,
                    None,
                    None,
                );
                (ms, ink.into_iter().filter(|i| i.col >= 5).collect())
            })
            .collect()
    };
    let a = run(ProfanityStyle::Nova);
    let b = run(ProfanityStyle::Nova);
    assert_eq!(
        a, b,
        "coupling is a pure function of (center, t, span): replay-exact"
    );
    let control = run(ProfanityStyle::Sparkle);
    let mut pulsed_ms = Vec::new();
    for ((ms, fa), (_, fc)) in a.iter().zip(&control) {
        if fa == fc {
            continue;
        }
        pulsed_ms.push(*ms);
        for (ca, cc) in fa.iter().zip(fc) {
            let dl = (relative_luminance(rgb3_to_u32(ca.color))
                - relative_luminance(rgb3_to_u32(cc.color)))
            .abs();
            assert!(dl <= 0.05, "coupling pulse |ΔL| {dl:.4} > 5%");
        }
    }
    assert!(
        !pulsed_ms.is_empty(),
        "the ring crossing must actually pulse the neighbor"
    );
    let first_ms = pulsed_ms[0];
    let last_ms = *pulsed_ms.last().expect("non-empty pulsed_ms");
    assert!(
        first_ms >= nova::FLASH_END_MS && last_ms < nova::RING_END_MS + nova::PULSE_MS,
        "pulse instants {first_ms}..={last_ms} ms must lie inside [FLASH_END_MS, RING_END_MS + PULSE_MS)"
    );
    assert!(
        last_ms - first_ms < nova::PULSE_MS,
        "the pulse is bounded by nova::PULSE_MS ({} ms): {first_ms}..={last_ms} ms",
        nova::PULSE_MS
    );
}

#[test]
fn singularity_rides_the_host_tick() {
    const SING_MAGIC: u64 = 8 << 12;
    let now = Instant::now();
    let g = geom20();
    let run = |magic: u64, profanity_magic: bool| {
        let mut wd = granted_novas(now, &[(2, 0xF00D, magic, false)]);
        let mut c = cfg_nova();
        c.profanity_magic = profanity_magic;
        tick_nova(&mut wd, now + Duration::from_millis(400), &c, g, None, None)
    };
    let (_, out, _, _, _) = run(SING_MAGIC, true);
    assert!(
        out.iter()
            .any(|d| matches!(d.glyph, DecoGlyph::RingArc) && matches!(d.blend, DecoBlend::Over)),
        "the Singularity emits RingArc Over decos, got {out:?}"
    );
    assert!(
        out.iter()
            .filter(|d| matches!(d.glyph, DecoGlyph::RingArc))
            .count()
            <= nova::MAX_RING_ARC_CELLS
    );
    let (_, out, _, _, _) = run(SING_MAGIC, false);
    assert!(!out.iter().any(|d| matches!(d.glyph, DecoGlyph::RingArc)));
}

#[test]
fn excess_concurrent_novas_skip_to_ember() {
    let now = Instant::now();
    let g = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 30,
        cols: 20,
    };
    let words: Vec<_> = (0..4u16)
        .map(|k| (k * 7, 0xF00D + u64::from(k), (100 << 12) | 100, false))
        .collect();
    let mut wd = granted_novas(now, &words);
    let at = now + Duration::from_millis(400);
    tick_nova(&mut wd, at, &cfg_nova(), g, None, None);
    let spent: Vec<bool> = wd
        .occ
        .iter()
        .map(|o| wd.persist[&o.ident].nova_done)
        .collect();
    assert_eq!(
        spent,
        vec![false, false, false, true],
        "the 4th concurrent nova (row-major last) skips straight to Ember"
    );
}

#[test]
fn dec_rows_anchor_nova_via_row_advance() {
    let now = Instant::now();
    let g = geom20();
    let run = |dec_line: bool| {
        let mut wd = granted_novas(now, &[(2, 0xF00D, (100 << 12) | 100, dec_line)]);
        let (q, _, _, _, _) = tick_nova(
            &mut wd,
            now + Duration::from_millis(400),
            &cfg_nova(),
            g,
            None,
            None,
        );
        assert!(
            !q.is_empty(),
            "novas render on DEC rows too (dec={dec_line})"
        );
        q.iter()
            .map(|quad| f64::from(quad.x) + f64::from(quad.w) / 2.0)
            .sum::<f64>()
            / q.len() as f64
    };
    let normal = run(false);
    let dec = run(true);
    assert!(
        (normal - 60.0).abs() < 8.0,
        "single-width anchor ≈ 60 px, got {normal}"
    );
    assert!(
        (dec - 120.0).abs() < 8.0,
        "DECDWL anchor rides the row advance, got {dec}"
    );
}

fn cfg_rainbow(chance: u8) -> DecoConfig {
    DecoConfig {
        profanity_style: ProfanityStyle::Rainbow,
        supernova_chance: chance,
        ..cfg()
    }
}

fn custom_table(frag: &str) -> (SpecTable, String) {
    let entries = crate::spec::parse_custom_toml(frag).expect("fragment parses");
    crate::spec::build_custom(&entries)
}

fn lex_with_customs(frag: &str) -> (DecoConfig, Lexicon) {
    let (table, lex_frag) = custom_table(frag);
    let lexicon = Lexicon::with_languages_and_override(&["en"], Some(&lex_frag))
        .expect("synthesized override parses");
    let mut c = cfg();
    c.spec_table = table;
    (c, lexicon)
}

#[test]
fn custom_on_builtin_profanity_fires_with_class_disabled() {
    let (mut c, lex) = lex_with_customs(
        "[[sparkle_words.custom]]\nwords = [\"fuck\"]\nink = { colorway = \"rainbow\" }\n",
    );
    c.profanity = false;
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(2, 20);
    term.process(b"oh fuck");
    wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
    assert_eq!(wd.occ.len(), 1, "the override bypasses the class gate");
    assert_eq!(wd.occ[0].class, Class::Profanity, "still a profanity match");
    assert!(
        wd.occ[0].custom,
        "the custom spec wins over the class default"
    );
    let (_, _, ink, _, _) = tick_nova(&mut wd, t0, &c, geom20(), None, None);
    assert!(!ink.is_empty(), "the custom rainbow renders");
    let mut off = cfg();
    off.profanity = false;
    let mut wd2 = WordDecorations::default();
    wd2.rescan(&term, 2, 20, &lex, &off, 1, t0);
    assert!(wd2.occ.is_empty(), "profanity off + no override = no scan");
}

#[test]
fn graphic_only_custom_word_with_ink_off_shows_a_cat() {
    let (mut c, lex) = lex_with_customs(
        "[[sparkle_words.custom]]\nwords = [\"ultrathink\"]\ngraphic = { collection = \"cats\" }\n",
    );
    c.ink_enabled = false;
    c.emphasis = true;
    let t0 = Instant::now();
    let g = geom20();
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(6, 20);
    term.process(b"\r\n\r\n\r\nultrathink");
    wd.rescan(&term, 6, 20, &lex, &c, 1, t0);
    assert_eq!(
        wd.occ.len(),
        1,
        "graphic-only custom word scans with ink off"
    );
    assert!(wd.occ[0].spec.graphic.is_some() && wd.occ[0].spec.ink.is_none());
    let (_, _, ink, _, _) = tick_nova(&mut wd, t0, &c, g, None, None);
    assert!(ink.is_empty(), "no ink axis, no ink");
    let (_, _, _, cats, _) = tick_nova(&mut wd, t0 + Duration::from_millis(300), &c, g, None, None);
    assert!(!cats.is_empty(), "the cat graphic rises on a custom word");
}

#[test]
fn custom_short_possessive_and_cjk_surfaces_scan_and_style() {
    for (word, text, cols, ink_cells) in [
        ("gg", "gg well played", 20usize, Some(2usize)),
        ("ultrathink", "ultrathink's power", 40, None),
        ("\u{732b}\u{795e}", "これは猫神です", 30, None),
    ] {
        let (c, lex) = lex_with_customs(&format!(
            "[[sparkle_words.custom]]\nwords = [\"{word}\"]\nink = {{ colorway = \"rainbow\" }}\n"
        ));
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let mut term = Terminal::new(2, cols as u16);
        term.process(text.as_bytes());
        wd.rescan(&term, 2, cols, &lex, &c, 1, t0);
        assert_eq!(wd.occ.len(), 1, "{text:?}: the custom surface scans");
        assert!(wd.occ[0].custom, "{text:?}: the override spec resolved");
        assert!(
            matches!(
                wd.occ[0].spec.ink,
                Some(InkSpec {
                    colorway: Colorway::Rainbow { .. },
                    ..
                })
            ),
            "{text:?}: styled as the custom rainbow"
        );
        if let Some(n) = ink_cells {
            let (_, _, ink, _, _) = tick_nova(&mut wd, t0, &c, geom20(), None, None);
            assert_eq!(ink.len(), n, "{text:?}: every lead cell inks");
        }
    }
}

#[test]
fn rainbow_freeze_is_byte_stable_on_both_themes_and_done_born() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let build = |bg: [u8; 3], fg: [u8; 3], inert: bool| {
        let mut o = occ(Class::Profanity, t0);
        o.spec = class_default_spec(Class::Profanity, &cfg_rainbow(0));
        o.ink_cells = 4;
        o.ink_bg = bg;
        o.inert = inert;
        let mut wd = WordDecorations {
            occ: vec![o],
            cols: 20,
            have_scanned: true,
            ..WordDecorations::default()
        };
        wd.ink_base_fg = vec![fg; 4];
        wd.ink_cols = vec![2, 3, 4, 5];
        wd
    };
    let c = cfg_rainbow(0);
    for (bg, fg, label) in [
        ([0u8, 0, 0], [208u8, 208, 208], "dark"),
        ([255, 255, 255], [40, 40, 40], "light"),
    ] {
        let mut wd = build(bg, fg, false);
        let (_, _, ink0, _, _) = tick_nova(&mut wd, t0, &c, geom20(), None, None);
        assert_eq!(ink0.len(), 4, "{label}: rainbow ink lands");
        let (_, _, ink_mid, _, _) = tick_nova(&mut wd, at(1200), &c, geom20(), None, None);
        assert_ne!(ink0, ink_mid, "{label}: the drift actually animates");
        assert!(wd.is_active(at(1200)), "{label}: animating while t < drift");
        let (_, _, ink_a, _, fp_a) = tick_nova(&mut wd, at(2600), &c, geom20(), None, None);
        let (_, _, ink_b, _, fp_b) = tick_nova(&mut wd, at(2700), &c, geom20(), None, None);
        assert_eq!(ink_a, ink_b, "{label}: settled bytes identical");
        assert_eq!(
            fp_a, fp_b,
            "{label}: settled fp identical (frame fold gated)"
        );
        assert!(!ink_a.is_empty(), "{label}: settled ink is non-empty");
        assert!(
            !wd.is_active(at(2600)),
            "{label}: is_active false with settled ink (the gate-hit path)"
        );
        assert_eq!(
            ink0, ink_a,
            "{label}: EXACT full cycle — frozen phase == t = 0 phase"
        );
        let mut wd_done = build(bg, fg, true);
        let (_, _, ink_d, _, _) = tick_nova(&mut wd_done, at(50), &c, geom20(), None, None);
        assert_eq!(ink_d, ink_a, "{label}: done-born == naturally-settled");
        assert!(!wd_done.is_active(at(50)), "{label}: born-done never arms");
    }
    let mut dark = build([0, 0, 0], [208, 208, 208], false);
    let mut light = build([255, 255, 255], [208, 208, 208], false);
    let (_, _, ink_dark, _, _) = tick_nova(&mut dark, at(2600), &c, geom20(), None, None);
    let (_, _, ink_light, _, _) = tick_nova(&mut light, at(2600), &c, geom20(), None, None);
    assert_ne!(
        ink_dark, ink_light,
        "theme-aware s/v: light bg takes deep candy tones"
    );
}

#[test]
fn supernova_roll_decorrelates_and_transfers_across_alignment() {
    let lex = lex();
    let t0 = Instant::now();
    let c = cfg_rainbow(100);
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(6, 20);
    term.process(b"oh fuck");
    wd.rescan(&term, 6, 20, &lex, &c, 1, t0);
    let id0 = wd.occ[0].ident;
    assert!(wd.persist[&id0].burst_roll, "chance = 100 always escalates");
    term.process(b"\x1b[2J\x1b[3;1Hoh fuck");
    wd.rescan(&term, 6, 20, &lex, &c, 2, t0 + Duration::from_millis(100));
    assert_eq!(wd.occ[0].appeared, t0, "the episode transferred");
    assert!(
        wd.persist[&wd.occ[0].ident].burst_roll,
        "the decision transfers across alignment"
    );
    let c0 = cfg_rainbow(0);
    let mut wd0 = WordDecorations::default();
    wd0.rescan(&term, 6, 20, &lex, &c0, 1, t0);
    assert!(!wd0.persist[&wd0.occ[0].ident].burst_roll);
    let c50 = cfg_rainbow(50);
    let mut seen = [false; 2];
    for e in 0..30u64 {
        wd.hard_reset();
        wd.rescan(&term, 6, 20, &lex, &c50, 10 + e, t0);
        seen[usize::from(wd.persist[&wd.occ[0].ident].burst_roll)] = true;
    }
    assert!(
        seen[0] && seen[1],
        "independent outcomes across rebirths of the same context"
    );
}

#[test]
fn scrolled_off_supernova_window_still_defers_classic_grants() {
    let (table, lex_frag) =
        custom_table("[[sparkle_words.custom]]\nwords = [\"boom\"]\nburst = { kind = \"nova\" }\n");
    let lexicon =
        Lexicon::with_languages_and_override(&["en"], Some(&lex_frag)).expect("override parses");
    let mut c = cfg_rainbow(100);
    c.spec_table = table;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let g = EffectGeom {
        rows: 8,
        ..geom20()
    };
    let mut term = Terminal::new(8, 20);
    term.process(b"fuck");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 8, 20, &lexicon, &c, 1, t0);
    tick_nova(&mut wd, t0, &c, g, None, None);
    let fuck = wd.occ[0].ident;
    assert_eq!(wd.persist[&fuck].nova_start, Some(t0), "supernova granted");
    let mut term2 = Terminal::new(8, 20);
    term2.process(b"\r\nboom");
    wd.rescan(&term2, 8, 20, &lexicon, &c, 2, at(300));
    assert!(
        wd.persist.contains_key(&fuck),
        "the scrolled-off episode lives on grace"
    );
    let boom = wd
        .occ
        .iter()
        .find(|o| o.spec.burst.map(|b| b.kind) == Some(BurstKind::Nova))
        .expect("the classic word scanned")
        .ident;
    tick_nova(&mut wd, at(300), &c, g, None, None);
    assert!(
        wd.persist[&boom].nova_start.is_none(),
        "classic grant deferred behind the INVISIBLE supernova window"
    );
    assert!(
        wd.is_active(at(300)),
        "the deferred grant keeps the scheduler armed past the window"
    );
    tick_nova(&mut wd, at(1200), &c, g, None, None);
    assert!(
        wd.persist[&boom].nova_start.is_none(),
        "still deferred mid-window while scrolled off"
    );
    let mut term3 = Terminal::new(8, 20);
    term3.process(b"fuck\r\nboom");
    wd.rescan(&term3, 8, 20, &lexicon, &c, 3, at(1500));
    let (q, ..) = tick_nova(&mut wd, at(1500), &c, g, None, None);
    assert!(!q.is_empty(), "the supernova resumes emission on revisit");
    assert!(
        wd.persist[&boom].nova_start.is_none(),
        "the classic still waits with the word back on-screen"
    );
    tick_nova(
        &mut wd,
        at(supernova::SUPER_TOTAL_MS + 100),
        &c,
        g,
        None,
        None,
    );
    assert!(
        wd.persist[&fuck].nova_done,
        "the window expired into nova_done"
    );
    assert!(
        wd.persist[&boom].nova_start.is_some(),
        "granted once the supernova window ended"
    );
}

#[test]
fn fbomb_combo_five_typed_fucks_go_nuclear_then_decay() {
    let lexicon = lex();
    let c = cfg_rainbow(1);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(4, 40);
    let mut epoch = 1u64;
    for (i, ch) in b"fuck fuck fuck fuck fuck".iter().enumerate() {
        term.process(&[*ch]);
        wd.note_typed_edit(at(100 * (i as u64 + 1)), None);
        wd.rescan(&term, 4, 40, &lexicon, &c, epoch, at(100 * (i as u64 + 1)));
        epoch += 1;
    }
    assert_eq!(wd.occ.len(), 5);
    assert_eq!(wd.combo_births.len(), 5, "five typed f-bombs charged");
    let ep5 = &wd.persist[&wd.occ[4].ident];
    assert!(
        ep5.burst_roll,
        "level 5 detonates even at a 1% configured chance"
    );
    assert_eq!(
        ep5.burst_tier,
        supernova::SuperTier::Nuke,
        "level 5 is EXTREME: the mushroom cloud, guaranteed"
    );
    let ep4 = &wd.persist[&wd.occ[3].ident];
    assert_eq!(ep4.burst_tier, supernova::SuperTier::Nuke,);
    for (i, ch) in b" fuck".iter().enumerate() {
        term.process(&[*ch]);
        wd.note_typed_edit(at(2400 + 31_000 + 100 * (i as u64 + 1)), None);
        wd.rescan(
            &term,
            4,
            40,
            &lexicon,
            &c,
            epoch,
            at(2400 + 31_000 + 100 * (i as u64 + 1)),
        );
        epoch += 1;
    }
    assert_eq!(wd.occ.len(), 6);
    assert_eq!(
        wd.combo_births.len(),
        1,
        "the streak decayed; only the fresh f-bomb remains"
    );
}

#[test]
fn fbomb_combo_output_fucks_do_not_charge() {
    let lexicon = lex();
    let c = cfg_rainbow(100);
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(8, 40);
    term.process(b"fuck fuck fuck fuck fuck\r\n");
    wd.rescan(&term, 8, 40, &lexicon, &c, 1, t0);
    assert_eq!(wd.occ.len(), 5);
    assert!(
        wd.combo_births.is_empty(),
        "output f-bombs ride the ambient level but never feed it"
    );
    for occ in &wd.occ {
        assert!(
            wd.persist[&occ.ident].burst_roll,
            "chance 100 still rolls at the cold level"
        );
    }
}

#[test]
fn fbomb_combo_cools_across_freeze_thaw() {
    let lexicon = lex();
    let c = cfg_rainbow(1);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(4, 40);
    let mut epoch = 1u64;
    for (i, ch) in b"fuck fuck fuck".iter().enumerate() {
        term.process(&[*ch]);
        wd.note_typed_edit(at(100 * (i as u64 + 1)), None);
        wd.rescan(&term, 4, 40, &lexicon, &c, epoch, at(100 * (i as u64 + 1)));
        epoch += 1;
    }
    assert_eq!(wd.combo_births.len(), 3);
    wd.freeze(at(2_000));
    wd.thaw(at(62_000));
    for (i, ch) in b" fuck".iter().enumerate() {
        term.process(&[*ch]);
        wd.note_typed_edit(at(62_100 + 100 * (i as u64 + 1)), None);
        wd.rescan(
            &term,
            4,
            40,
            &lexicon,
            &c,
            epoch,
            at(62_100 + 100 * (i as u64 + 1)),
        );
        epoch += 1;
    }
    assert_eq!(
        wd.combo_births.len(),
        1,
        "60 frozen seconds age the streak out; only the fresh f-bomb remains"
    );
}

#[test]
fn fbomb_combo_nuke_cloud_reaches_the_free_lane() {
    let lexicon = lex();
    let c = cfg_rainbow(100);
    let g = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 20,
        cols: 40,
    };
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(20, 40);
    term.process(b"\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n");
    for (i, ch) in b"fuck fuck fuck fuck fuck".iter().enumerate() {
        term.process(&[*ch]);
        wd.note_typed_edit(at(60 * (i as u64 + 1)), None);
        wd.rescan(
            &term,
            20,
            40,
            &lexicon,
            &c,
            i as u64 + 1,
            at(60 * (i as u64 + 1)),
        );
    }
    let ep5 = &wd.persist[&wd.occ[4].ident];
    assert_eq!(ep5.burst_tier, supernova::SuperTier::Nuke);
    let mut cloud_frames = 0u32;
    for ms in (1500..20_000).step_by(100) {
        let (.., fr, _) = tick_nova(&mut wd, at(ms), &c, g, None, None);
        if !fr.is_empty() {
            cloud_frames += 1;
        }
    }
    assert!(
        cloud_frames >= 10,
        "the guaranteed Nuke never put its cloud on the free lane \
         (saw {cloud_frames} sprite-bearing frames)"
    );
}

#[test]
fn nuke_window_holds_the_mutex_for_its_full_cloud() {
    let (table, lex_frag) =
        custom_table("[[sparkle_words.custom]]\nwords = [\"boom\"]\nburst = { kind = \"nova\" }\n");
    let lexicon =
        Lexicon::with_languages_and_override(&["en"], Some(&lex_frag)).expect("override parses");
    let mut c = cfg_rainbow(100);
    c.spec_table = table;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let g = EffectGeom {
        rows: 8,
        ..geom20()
    };
    let mut term = Terminal::new(8, 20);
    term.process(b"fuck\r\nboom");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 8, 20, &lexicon, &c, 1, t0);
    let fuck = wd.occ[0].ident;
    let boom = wd.occ[1].ident;
    {
        let ep = wd.persist.get_mut(&fuck).expect("fresh episode");
        ep.burst_roll = true;
        ep.burst_tier = supernova::SuperTier::Nuke;
    }
    tick_nova(&mut wd, t0, &c, g, None, None);
    assert_eq!(wd.persist[&fuck].nova_start, Some(t0), "cloud granted");
    tick_nova(
        &mut wd,
        at(supernova::SUPER_TOTAL_MS + 200),
        &c,
        g,
        None,
        None,
    );
    assert!(
        !wd.persist[&fuck].nova_done,
        "the cloud's 2400-3600 ms tail must not be truncated"
    );
    assert!(
        wd.persist[&boom].nova_start.is_none(),
        "the mutex holds through the full cloud"
    );
    tick_nova(
        &mut wd,
        at(crate::nuke::NUKE_TOTAL_MS + 100),
        &c,
        g,
        None,
        None,
    );
    assert!(
        wd.persist[&fuck].nova_done,
        "the cloud expired at ITS window"
    );
    assert!(
        wd.persist[&boom].nova_start.is_some(),
        "the classic grant lands after the cloud"
    );

    let mut term = Terminal::new(8, 20);
    term.process(b"fuck\r\nboom");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 8, 20, &lexicon, &c, 1, t0);
    let fuck = wd.occ[0].ident;
    let boom = wd.occ[1].ident;
    {
        let ep = wd.persist.get_mut(&fuck).expect("fresh episode");
        ep.burst_roll = true;
        ep.burst_tier = supernova::SuperTier::Flash;
    }
    tick_nova(&mut wd, t0, &c, g, None, None);
    assert_eq!(wd.persist[&fuck].nova_start, Some(t0), "flash granted");
    tick_nova(&mut wd, at(1_200), &c, g, None, None);
    assert!(
        !wd.persist[&fuck].nova_done,
        "a Flash presents the classic window, mid-window it is still live"
    );
    tick_nova(
        &mut wd,
        at(supernova::SUPER_TOTAL_MS + 100),
        &c,
        g,
        None,
        None,
    );
    assert!(
        wd.persist[&fuck].nova_done,
        "a Flash expires at the classic 2400 ms window"
    );
    assert!(
        wd.persist[&boom].nova_start.is_some(),
        "the classic grant lands as soon as the Flash frees the mutex"
    );
}

fn burst_mutex_projection(
    wd: &WordDecorations,
    now: Instant,
) -> std::collections::BTreeMap<&'static str, i64> {
    let (mut supers, mut classics) = (0i64, 0i64);
    for ep in wd
        .persist
        .values()
        .chain(wd.parked.values().flat_map(|p| p.persist.values()))
    {
        if burst_mutex_end(ep).is_some_and(|end| now < end) {
            match ep.burst_kind {
                Some(BurstKind::SuperNova) => supers += 1,
                _ => classics += 1,
            }
        }
    }
    let funded = supers * supernova::S_MAX_BOUND as i64 + classics * 392;
    [
        ("supers", supers),
        ("classics", classics),
        ("funded", funded),
    ]
    .into_iter()
    .collect()
}

#[test]
fn real_burst_grants_conform_to_the_supernova_mutex_model() {
    let model = aterm_models_effects::supernova_burst_mutex_model();
    let check = |wd: &mut WordDecorations,
                 now: Instant,
                 c: &DecoConfig,
                 g: EffectGeom,
                 what: &str|
     -> std::collections::BTreeMap<&'static str, i64> {
        let mut state = burst_mutex_projection(wd, now);
        tick_nova(wd, now, c, g, None, None);
        let after = burst_mutex_projection(wd, now);
        for (action, var) in [("IgniteSuper", "supers"), ("IgniteClassic", "classics")] {
            for _ in state[var]..after[var] {
                assert!(
                    model.action_enabled(action, &state),
                    "{what}: the engine granted {action} at {state:?}, which the \
                     mutex model does not enable"
                );
                assert!(model.fire(action, &mut state), "{what}: {action}");
            }
        }
        assert_eq!(state, after, "{what}: a tick only grants (time retires)");
        let env = model.eval_env(&after);
        for inv in &model.invariants {
            assert!(
                model.check_invariant_in(inv, &env),
                "{what}: {} violated by {after:?}",
                inv.name
            );
        }
        after
    };

    let (table, lex_frag) =
        custom_table("[[sparkle_words.custom]]\nwords = [\"boom\"]\nburst = { kind = \"nova\" }\n");
    let lexicon =
        Lexicon::with_languages_and_override(&["en"], Some(&lex_frag)).expect("override parses");
    let mut c = cfg_rainbow(100);
    c.spec_table = table;
    let g = EffectGeom {
        rows: 8,
        ..geom20()
    };
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);

    let mut wd = WordDecorations::default();
    wd.bind_pane(1, (0, 0));
    let mut term_a = Terminal::new(8, 20);
    term_a.process(b"fuck\r\nfuck it\r\nboom");
    wd.rescan(&term_a, 8, 20, &lexicon, &c, 1, t0);
    let s = check(&mut wd, t0, &c, g, "pane 1 first tick");
    assert_eq!(
        (s["supers"], s["classics"]),
        (1, 0),
        "one supernova holds the mutex"
    );
    let win = |wd: &WordDecorations| {
        wd.persist
            .values()
            .chain(wd.parked.values().flat_map(|p| p.persist.values()))
            .filter_map(burst_mutex_end)
            .max()
            .expect("a live window")
    };
    let first_end = win(&wd);

    wd.bind_pane(2, (400, 0));
    let mut term_b = Terminal::new(8, 20);
    term_b.process(b"oh fuck\r\nboom");
    wd.rescan(&term_b, 8, 20, &lexicon, &c, 1, at(300));
    let s = check(&mut wd, at(300), &c, g, "pane 2 behind a PARKED supernova");
    assert_eq!(
        (s["supers"], s["classics"]),
        (1, 0),
        "both of pane 2's grants defer"
    );

    let t1 = first_end + Duration::from_millis(100);
    let s = check(&mut wd, t1, &c, g, "pane 2 after the parked window");
    assert_eq!(
        (s["supers"], s["classics"]),
        (1, 0),
        "pane 2's supernova granted"
    );
    let second_end = win(&wd);

    let t2 = second_end + Duration::from_millis(100);
    let s = check(&mut wd, t2, &c, g, "pane 2's classic after its supernova");
    assert_eq!(
        (s["supers"], s["classics"]),
        (0, 1),
        "the mutex released the nova"
    );

    let state = |supers: i64, classics: i64| {
        [
            ("supers", supers),
            ("classics", classics),
            ("funded", supers * 900 + classics * 392),
        ]
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert!(
        !model.action_enabled("IgniteSuper", &state(1, 0)),
        "a second supernova"
    );
    assert!(
        !model.action_enabled("IgniteSuper", &state(0, 1)),
        "a supernova beside a classic"
    );
    assert!(
        !model.action_enabled("IgniteClassic", &state(1, 0)),
        "a classic beside a supernova"
    );
}

#[test]
fn supernova_wash_splits_around_selection() {
    let lex = lex();
    let c = cfg_rainbow(100);
    let g = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 8,
        cols: 24,
    };
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(8, 24);
    term.process(b"\r\n\r\n\r\nfuck");
    wd.rescan(&term, 8, 24, &lex, &c, 1, t0);
    tick_nova(&mut wd, t0, &c, g, None, None);
    let sel = select_span(1, 8, 11);
    let sv = SelView {
        sel: &sel,
        display_offset: 0,
    };
    let (q, ..) = tick_nova(
        &mut wd,
        t0 + Duration::from_millis(500),
        &c,
        g,
        None,
        Some(sv),
    );
    assert!(!q.is_empty(), "mid-detonation wash present");
    let wash1: Vec<&GlowQuad> = q
        .iter()
        .filter(|x| x.row == 1 && i32::from(x.w) > 4 * 10)
        .collect();
    assert!(
        wash1.len() >= 2,
        "the full-width wash row split into remainders: {wash1:?}"
    );
    assert!(
        wash1.len() <= 3,
        "≤ 3 wash quads/row post-split: {}",
        wash1.len()
    );
    for x in q.iter().filter(|x| x.row == 1) {
        let (x0, x1) = (i32::from(x.x), i32::from(x.x) + i32::from(x.w));
        assert!(
            x1 <= 80 || x0 >= 120,
            "no quad overlaps the selected px span [80, 120): {x:?}"
        );
    }
    for row in 0..g.rows {
        for px in 0..u32::from(g.cols) * u32::from(g.cell_w) {
            let y0 = u32::from(row) * u32::from(g.cell_h);
            for py in y0..y0 + u32::from(g.cell_h) {
                let mut sum = [0u32; 3];
                for quad in q.iter().filter(|quad| {
                    quad.row == row
                        && px >= u32::from(quad.x)
                        && px < u32::from(quad.x) + u32::from(quad.w)
                        && py >= u32::from(quad.y)
                        && py < u32::from(quad.y) + u32::from(quad.h)
                }) {
                    sum[0] += (quad.color >> 16) & 0xff;
                    sum[1] += (quad.color >> 8) & 0xff;
                    sum[2] += quad.color & 0xff;
                }
                let peak = sum.into_iter().max().unwrap_or(0);
                assert!(
                    peak <= supernova::MAX_VIEWPORT_OVERLAY,
                    "post-split ({px}, {py}) accumulates {peak} channel levels"
                );
            }
        }
    }
}

#[test]
fn supernova_suppressed_under_reduced_motion() {
    let lex = lex();
    let mut c = cfg_rainbow(100);
    c.reduced_motion = true;
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let mut term = Terminal::new(2, 20);
    term.process(b"oh fuck");
    wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
    let (q, _, ink, _, _) = tick_nova(&mut wd, t0, &c, geom20(), None, None);
    assert!(q.is_empty(), "no supernova quads under reduced_motion");
    assert!(
        wd.persist[&wd.occ[0].ident].nova_start.is_none(),
        "no ignition granted"
    );
    assert!(!ink.is_empty(), "the static rainbow still inks");
    assert!(
        !wd.is_active(t0 + Duration::from_millis(1)),
        "static = idle"
    );
}

#[test]
fn full_play_then_reduce_resurrects_nothing() {
    let lex = lex();
    let g = geom20();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let reduce = |c: &DecoConfig| {
        let mut r = c.clone();
        r.reduced_motion = true;
        r
    };
    let c = cfg();
    let mut cat_term = Terminal::new(4, 20);
    cat_term.process(b"\r\n\r\na happy kitty naps");
    let mut wd = WordDecorations::default();
    wd.rescan(&cat_term, 4, 20, &lex, &c, 1, t0);
    tick_cat_at(&mut wd, t0, &c, g, None, true);
    let (fr, _, _) = tick_cat_at(&mut wd, at(100), &c, g, None, true);
    assert!(!fr.is_empty(), "the cat is rising");
    let (fr, out, _) = tick_cat_at(&mut wd, at(6_000), &c, g, None, true);
    assert!(fr.is_empty() && out.is_empty(), "the one-shot completed");
    let (fr, out, _) = tick_cat_at(&mut wd, at(6_050), &reduce(&c), g, None, true);
    assert!(
        fr.is_empty() && out.is_empty(),
        "reduced motion must not resurrect a done cat: {fr:?} {out:?}"
    );
    assert_eq!(wd.drain_kitty_sightings().count(), 0, "no zombie re-log");
    let (fr, out, _) = tick_cat_at(&mut wd, at(6_100), &c, g, None, true);
    assert!(
        fr.is_empty() && out.is_empty(),
        "…and stays gone on refocus"
    );

    let mut c_paw = cfg();
    c_paw.feline_style = FelineStyle::Paw;
    let mut wd = WordDecorations::default();
    wd.rescan(&cat_term, 4, 20, &lex, &c_paw, 1, t0);
    tick_cat_at(&mut wd, t0, &c_paw, g, None, true);
    let (fr, out, _) = tick_cat_at(&mut wd, at(100), &c_paw, g, None, true);
    assert!(
        fr.is_empty() && !out.iter().any(|d| matches!(d.glyph, DecoGlyph::Paw)),
        "paw style draws no graphic under v4"
    );
    assert_eq!(
        wd.drain_kitty_sightings().count(),
        0,
        "paw style logs no sighting"
    );
    let (_, out, _) = tick_cat_at(&mut wd, at(4_050), &reduce(&c_paw), g, None, true);
    assert!(out.is_empty(), "reduced motion resurrects nothing");

    let c_nova = cfg_nova();
    let mut term = Terminal::new(2, 20);
    term.process(b"oh fuck");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 2, 20, &lex, &c_nova, 1, t0);
    tick_nova(&mut wd, t0, &c_nova, g, None, None);
    let dur = u64::from(nova_features(wd.occ[0].genome.gkey).duration_ms);
    let (_, out, ..) = tick_nova(
        &mut wd,
        at(dur + RESIDUAL_FADE_MS + 100),
        &c_nova,
        g,
        None,
        None,
    );
    assert!(out.is_empty(), "the ember fade completed");
    let (q, out, ..) = tick_nova(
        &mut wd,
        at(dur + RESIDUAL_FADE_MS + 150),
        &reduce(&c_nova),
        g,
        None,
        None,
    );
    assert!(
        q.is_empty() && out.is_empty(),
        "no static-glint resurrection: {out:?}"
    );

    let c_sp = cfg();
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 2, 20, &lex, &c_sp, 1, t0);
    let mut out = Vec::new();
    tick_deco(&mut wd, t0, &c_sp, &mut out);
    tick_deco(
        &mut wd,
        at(c_sp.anim_ms + RESIDUAL_FADE_MS + 100),
        &c_sp,
        &mut out,
    );
    assert!(out.is_empty(), "the residual self-terminated");
    tick_deco(
        &mut wd,
        at(c_sp.anim_ms + RESIDUAL_FADE_MS + 150),
        &reduce(&c_sp),
        &mut out,
    );
    assert!(out.is_empty(), "no static-spark resurrection: {out:?}");
}

#[test]
fn a_peek_landing_records_one_positioned_cue() {
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let mut term = Terminal::new(4, 20);
    term.process(b"\r\n\r\na happy kitty naps");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 4, 20, &lex, &c, 1, t0);
    let mut cues: Vec<PeekCue> = Vec::new();
    for k in 0..60u64 {
        let _ = tick_cat_at(
            &mut wd,
            t0 + Duration::from_millis(k * 16),
            &c,
            g,
            None,
            true,
        );
        cues.extend(wd.drain_peek_cues());
    }
    assert_eq!(cues.len(), 1, "once per episode, at the landing");
    let cue = cues[0];
    assert_eq!((cue.row, cue.col), (2, 8), "the word's viewport cell");
    let (x0, x1, y0, y1) = cue.head_px;
    assert!(x1 > x0 && y1 > y0, "a real dest rect: {:?}", cue.head_px);
    assert!(
        y0 >= 0 && y1 <= i32::from(g.rows) * i32::from(g.cell_h),
        "clamped to the viewport like the sprite it mirrors"
    );
}

#[test]
fn reduced_from_birth_shows_a_static_pose_once_and_un_reducing_spends_it() {
    let lex = lex();
    let c = cfg();
    let mut c_red = cfg();
    c_red.reduced_motion = true;
    let g = geom20();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut term = Terminal::new(4, 20);
    term.process(b"\r\n\r\na happy kitty naps");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 4, 20, &lex, &c_red, 1, t0);
    let (fr1, out1, fp1) = tick_cat_at(&mut wd, t0, &c_red, g, None, true);
    assert!(
        !fr1.is_empty() || !out1.is_empty(),
        "the static pose lands on the first focused tick"
    );
    assert_eq!(wd.drain_kitty_sightings().count(), 1, "logs once");
    let cat = wd
        .occ
        .iter()
        .find(|o| o.spec.graphic.is_some())
        .expect("the cat word scanned")
        .ident;
    assert!(
        wd.persist[&cat].phase_start.is_some(),
        "the phase clock is latched (at birth)"
    );
    let (fr2, out2, fp2) = tick_cat_at(&mut wd, at(60_000), &c_red, g, None, true);
    assert_eq!(fr1, fr2, "the pose persists (flags never advance)");
    assert_eq!(out1, out2);
    assert_eq!(fp1, fp2, "byte-stable static pose");
    assert_eq!(wd.drain_kitty_sightings().count(), 0, "exactly once");
    assert!(!wd.is_active(at(60_000)), "static = zero wakes");
    assert!(
        !wd.persist[&cat].peek_done,
        "flags hold while reduced (the pose persists)"
    );

    let (fr, _, _) = tick_cat_at(&mut wd, at(61_000), &c, g, None, true);
    assert!(
        fr.is_empty(),
        "zero free sprites — no entrance replay on un-reduce: {fr:?}"
    );
    assert!(
        wd.persist[&cat].peek_done,
        "the elapsed static window latches peek_done"
    );
    assert!(!wd.is_active(at(61_000)), "done = zero wakes");
    let (fr, _, _) = tick_cat_at(&mut wd, at(62_000), &c, g, None, true);
    assert!(fr.is_empty(), "the peek stays spent (done forever)");
}

#[test]
fn capture_during_freeze_preserves_episodes_byte_for_byte() {
    let lex = lex();
    let c = cfg();
    let g = geom20();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut term = Terminal::new(4, 20);
    term.process(b"\r\n\r\na happy kitty naps");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 4, 20, &lex, &c, 1, t0);
    tick_cat_at(&mut wd, t0, &c, g, None, true);
    let snap = |wd: &WordDecorations| {
        let mut v: Vec<_> = wd
            .persist
            .iter()
            .map(|(id, e)| {
                (
                    *id,
                    e.appeared,
                    e.phase_start,
                    e.genome,
                    e.peek_started,
                    e.peek_done,
                    e.sweep_started,
                    e.sweep_done,
                    e.logged,
                    e.burst_roll,
                )
            })
            .collect();
        v.sort_by_key(|s| s.0);
        v
    };
    let before = snap(&wd);
    assert!(!before.is_empty(), "the fixture built episodes");
    let marks_before: std::collections::BTreeSet<u64> =
        wd.done_marks.index.keys().copied().collect();
    wd.freeze(at(100));
    let mut blank = Terminal::new(4, 20);
    wd.rescan(&blank, 4, 20, &lex, &c, 2, at(20_000));
    assert!(wd.needs_rescan(2), "the pending rescan stays pending");
    assert_eq!(snap(&wd), before, "episodes byte-for-byte (term rescan)");
    let mut snap_input = aterm_core::render::RenderInput::default();
    blank.cell_frame_into(&mut snap_input, 4, 20);
    wd.rescan_from_cells(
        &snap_input.cells,
        &snap_input.line_sizes,
        4,
        20,
        &lex,
        &c,
        2,
        at(20_000),
    );
    assert!(wd.needs_rescan(2), "the snapshot entry point no-ops too");
    assert_eq!(snap(&wd), before, "episodes byte-for-byte (cells rescan)");
    assert_eq!(
        wd.done_marks
            .index
            .keys()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        marks_before,
        "no grace-expiry done marks — the frozen rescan added nothing"
    );
    let (fr, out, fp) = tick_cat_at(&mut wd, at(20_000), &c, g, None, true);
    assert!(
        fr.is_empty() && out.is_empty() && fp == 0,
        "frozen tick no-ops"
    );
    assert_eq!(snap(&wd), before);
    wd.thaw(at(20_000));
    wd.rescan(&term, 4, 20, &lex, &c, 2, at(20_000));
    assert!(!wd.needs_rescan(2));
    let (fr, _, _) = tick_cat_at(&mut wd, at(20_050), &c, g, None, true);
    assert!(!fr.is_empty(), "the one-shot resumed mid-rise after thaw");
    assert!(wd.is_active(at(20_050)));
    let cat = wd
        .occ
        .iter()
        .find(|o| o.spec.graphic.is_some())
        .expect("the cat word rescanned");
    assert!(!wd.persist[&cat.ident].peek_done && !cat.inert);
}

#[test]
fn live_classic_nova_defers_the_supernova_grant() {
    let (table, lex_frag) =
        custom_table("[[sparkle_words.custom]]\nwords = [\"boom\"]\nburst = { kind = \"nova\" }\n");
    let lexicon =
        Lexicon::with_languages_and_override(&["en"], Some(&lex_frag)).expect("override parses");
    let mut c = cfg_rainbow(100);
    c.spec_table = table;
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let g = EffectGeom {
        rows: 8,
        ..geom20()
    };
    let mut term = Terminal::new(8, 20);
    term.process(b"boom");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 8, 20, &lexicon, &c, 1, t0);
    tick_nova(&mut wd, t0, &c, g, None, None);
    let boom = wd.occ[0].ident;
    assert_eq!(wd.persist[&boom].nova_start, Some(t0), "classic nova live");
    let dur = u64::from(nova_features(wd.occ[0].genome.gkey).duration_ms);
    term.process(b"\x1b[6;1Hoh fuck");
    wd.rescan(&term, 8, 20, &lexicon, &c, 2, at(100));
    let fuck = wd
        .occ
        .iter()
        .find(|o| o.spec.burst.map(|b| b.kind) == Some(BurstKind::SuperNova))
        .expect("the supernova word scanned")
        .ident;
    assert!(wd.persist[&fuck].burst_roll, "chance = 100 rolled");
    tick_nova(&mut wd, at(100), &c, g, None, None);
    assert!(
        wd.persist[&fuck].nova_start.is_none(),
        "deferred behind the live classic (mid-window)"
    );
    tick_nova(&mut wd, at(dur - 50), &c, g, None, None);
    assert!(
        wd.persist[&fuck].nova_start.is_none(),
        "still deferred at the classic window's tail"
    );
    tick_nova(&mut wd, at(dur + 100), &c, g, None, None);
    assert!(
        wd.persist[&fuck].nova_start.is_some(),
        "granted once the classic window ended"
    );
}

#[test]
fn selection_mid_eclipse_punches_through_the_veil() {
    let lex = lex();
    let c = cfg_rainbow(100);
    let g = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 8,
        cols: 24,
    };
    let t0 = Instant::now();
    let mut term = Terminal::new(8, 24);
    term.process(b"\x1b[48;2;255;255;255m\r\n\r\n\r\nfuck\x1b[0m");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 8, 24, &lex, &c, 1, t0);
    tick_nova(&mut wd, t0, &c, g, None, None);
    let sel = select_span(1, 8, 11);
    let sv = SelView {
        sel: &sel,
        display_offset: 0,
    };
    let (_, out, ..) = tick_nova(
        &mut wd,
        t0 + Duration::from_millis(500),
        &c,
        g,
        None,
        Some(sv),
    );
    let shade: Vec<&WordDecoration> = out
        .iter()
        .filter(|d| matches!(d.glyph, DecoGlyph::Shade))
        .collect();
    assert!(!shade.is_empty(), "mid-detonation veil present");
    assert!(
        !shade
            .iter()
            .any(|d| d.row == 1 && (8..=11).contains(&d.col)),
        "no Shade stamp lands on a selected cell"
    );
    assert!(
        shade.iter().any(|d| d.row == 1),
        "the selected ROW keeps its unselected veil cells (punch-through, not row deletion)"
    );
    let td = t0 + Duration::from_millis(1800);
    let (_, out_free, ..) = tick_nova(&mut wd, td, &c, g, None, None);
    let mote = out_free
        .iter()
        .find(|d| matches!(d.blend, DecoBlend::Over) && !matches!(d.glyph, DecoGlyph::Shade))
        .copied()
        .expect("light-theme debris motes ride the Over deco stream");
    let sel_d = select_span(i32::from(mote.row), mote.col, mote.col);
    let sv_d = SelView {
        sel: &sel_d,
        display_offset: 0,
    };
    let (_, out_sel, ..) = tick_nova(&mut wd, td, &c, g, None, Some(sv_d));
    assert!(
        !out_sel
            .iter()
            .any(|d| matches!(d.blend, DecoBlend::Over) && sv_d.cell_selected(d.row, d.col)),
        "no Over-blend debris mote renders over a selected cell"
    );
    assert!(
        out_sel.iter().any(|d| matches!(d.blend, DecoBlend::Over)),
        "unselected debris survives the filter (punch-through, not deletion)"
    );
}

#[test]
fn burst_chance_pct_gates_starburst_rolls() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut term = Terminal::new(2, 20);
    term.process(b"go zap");
    let mut out = Vec::new();
    let (c0, lex0) = lex_with_customs(
        "[[sparkle_words.custom]]\nwords = [\"zap\"]\nburst = { kind = \"starburst\", chance = 0 }\n",
    );
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 2, 20, &lex0, &c0, 1, t0);
    assert!(
        !wd.persist[&wd.occ[0].ident].burst_roll,
        "chance = 0 never rolls"
    );
    tick_deco(&mut wd, at(50), &c0, &mut out);
    assert!(out.is_empty(), "chance = 0 starburst never fires: {out:?}");
    assert!(!wd.is_active(at(50)), "…and never arms");
    let mut c0r = c0.clone();
    c0r.reduced_motion = true;
    tick_deco(&mut wd, at(100), &c0r, &mut out);
    assert!(
        out.is_empty(),
        "no reduced-motion static spark for a rolled-off burst"
    );
    let (c100, lex100) = lex_with_customs(
        "[[sparkle_words.custom]]\nwords = [\"zap\"]\nburst = { kind = \"starburst\", chance = 100 }\n",
    );
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 2, 20, &lex100, &c100, 1, t0);
    assert!(
        wd.persist[&wd.occ[0].ident].burst_roll,
        "chance = 100 always rolls"
    );
    tick_deco(&mut wd, at(50), &c100, &mut out);
    assert!(!out.is_empty(), "chance = 100 always fires");
}

#[test]
fn ink_disabled_light_bg_still_takes_the_eclipse_branch() {
    let lex = lex();
    let mut c = cfg_rainbow(100);
    c.ink_enabled = false;
    let t0 = Instant::now();
    let g = geom20();
    let mut term = Terminal::new(2, 20);
    term.process(b"\x1b[48;2;255;255;255moh fuck\x1b[0m");
    let mut wd = WordDecorations::default();
    wd.rescan(&term, 2, 20, &lex, &c, 1, t0);
    let o = wd
        .occ
        .iter()
        .find(|o| o.class == Class::Profanity)
        .expect("the word scanned");
    assert_eq!(o.ink_cells, 0, "ink disabled: nothing captured for ink");
    assert_eq!(
        o.ink_bg,
        [255, 255, 255],
        "…but the lead-cell bg is still captured for the theme branch"
    );
    tick_nova(&mut wd, t0, &c, g, None, None);
    let (q, out, ..) = tick_nova(&mut wd, t0 + Duration::from_millis(500), &c, g, None, None);
    let veil = out
        .iter()
        .filter(|d| matches!(d.blend, DecoBlend::Over))
        .count();
    assert!(veil > 50, "the Over-blend dark veil is present, got {veil}");
    let wash = q
        .iter()
        .filter(|x| i32::from(x.w) > 8 * i32::from(g.cell_w))
        .count();
    assert_eq!(wash, 0, "no full-viewport additive wash on a light bg");
}

#[test]
fn rainbow_guard_samples_four_points_not_just_mid() {
    let t0 = Instant::now();
    let c = cfg_rainbow(0);
    let bg = [0x80u8, 0x80, 0x80];
    let fg = [255u8, 255, 255];
    let strength = c.ink_strength;
    let bg32 = rgb3_to_u32(bg);
    let sample = |gkey: u64, u: f32| {
        let h = hsv2rgb(
            (rainbow_base_hue(gkey) + u * 300.0).rem_euclid(360.0),
            0.85,
            1.0,
        );
        contrast_ratio(mix_rgb(rgb3_to_u32(fg), h, strength), bg32)
    };
    let gkey = (0..10_000u64)
        .find(|g| {
            sample(*g, 0.5) >= MIN_INK_CONTRAST
                && [0.0f32, 1.0 / 3.0, 2.0 / 3.0, 1.0]
                    .iter()
                    .any(|u| sample(*g, *u) < MIN_INK_CONTRAST)
        })
        .expect("a mid-blind gkey exists");
    let mut o = occ(Class::Profanity, t0);
    o.spec = class_default_spec(Class::Profanity, &c);
    o.genome = Genome { gkey, magic: 100 };
    o.start_col = 2;
    o.end_col = 5;
    o.ink_cells = 4;
    o.ink_bg = bg;
    let mut wd = WordDecorations {
        occ: vec![o],
        cols: 20,
        have_scanned: true,
        ..WordDecorations::default()
    };
    wd.ink_base_fg = vec![fg; 4];
    wd.ink_cols = (2..6).collect();
    let settle = t0 + Duration::from_millis(2_600);
    let (_, _, ink, _, _) = tick_nova(&mut wd, settle, &c, geom20(), None, None);
    assert_eq!(ink.len(), 4);
    for cell in &ink {
        assert!(
            contrast_ratio(rgb3_to_u32(cell.color), bg32) >= MIN_INK_CONTRAST,
            "guarded cell washes out: {cell:?}"
        );
    }
    let denom = 3.0f32;
    let raw: Vec<[u8; 3]> = (0..4u16)
        .map(|i| {
            let u = f32::from(i) / denom;
            u32_to_rgb3(mix_rgb(
                rgb3_to_u32(fg),
                hsv2rgb(
                    (rainbow_base_hue(gkey) + u * 300.0).rem_euclid(360.0),
                    0.85,
                    1.0,
                ),
                strength,
            ))
        })
        .collect();
    assert_ne!(
        ink.iter().map(|i| i.color).collect::<Vec<_>>(),
        raw,
        "the guard pulled strength below the raw full-strength mix"
    );
}

#[test]
fn kitty_emits_only_its_body_in_front_of_the_cursor() {
    let g = geom20();
    let mut wd = WordDecorations::default();
    let mut free = Vec::new();
    let footprint = wd
        .kitty_cursor_footprint(still_layout(g, (3, 5)))
        .expect("visible footprint");

    wd.kitty_cursor(
        KittyCursorFrame {
            alpha: 200,
            ..still_frame(g, (3, 5))
        },
        &mut free,
    );
    assert_eq!(free.len(), 1, "cursor kitty emits exactly its body");
    let s = free[0].clone();
    assert_eq!(
        footprint,
        CatFootprint {
            x: s.x,
            y: s.y,
            w: s.w,
            h: s.h,
        },
        "palette footprint and emitted sprite geometry must be identical"
    );
    assert!(matches!(s.z, FreeZ::OverText));
    assert_eq!(s.clip, PxRect::EVERYWHERE, "the head is never cut");
    assert_eq!(s.alpha, 200);
    assert_eq!(s.art.w(), s.w, "1:1 width");
    assert_eq!(s.art.h(), s.h, "1:1 height");
    assert!(s.w > 0 && s.h > 0);
    let cursor_right = 6 * i32::from(g.cell_w);
    assert!(
        s.x > cursor_right && s.x < cursor_right + i32::from(g.cell_w),
        "the companion leads the cursor cell by less than a cell: x={}",
        s.x
    );
    let mid = s.y + i32::from(s.h) / 2;
    assert!(
        (mid - 70).abs() <= 1,
        "centred on the cursor row, got mid={mid}"
    );
    assert!(!wd.cat_baker.is_empty(), "baked into the tile cache");

    let mut none = Vec::new();
    wd.kitty_cursor(
        KittyCursorFrame {
            alpha: 0,
            ..still_frame(g, (3, 5))
        },
        &mut none,
    );
    assert!(none.is_empty());
}

#[test]
fn singing_notes_stream_ring_capped_tinted_and_drive_scaled() {
    use crate::kitty_sing::{MAX_NOTES, NOTE_TINTS, NoteKind, NoteSprite};
    let g = geom20();
    let mut wd = WordDecorations::default();
    wd.set_kitty_sprite_source(KittySpriteSource::Custom {
        source_fp: 1,
        w: 4,
        h: 4,
        rgba: Arc::from(vec![255u8; 4 * 4 * 4]),
    });
    let mut notes = [None; MAX_NOTES];
    for (i, slot) in notes.iter_mut().enumerate() {
        *slot = Some(NoteSprite {
            dx: 0.2 + i as f32 * 0.03,
            dy: -0.2 - i as f32 * 0.05,
            alpha: 200,
            kind: NoteKind::Eighth,
            tint: NOTE_TINTS[i % NOTE_TINTS.len()],
        });
    }
    let frame = |sing: f32, notes| KittyCursorFrame {
        sing,
        notes,
        ..still_frame(g, (3, 2))
    };
    let mut free = Vec::new();
    wd.kitty_cursor(frame(1.0, notes), &mut free);
    let body = free.last().expect("the body emits last (topmost)").clone();
    let emitted = &free[..free.len() - 1];
    assert_eq!(
        emitted.len(),
        MAX_NOTES,
        "a full ring emits exactly the cap, never more"
    );
    assert!(
        emitted.iter().all(|s| s.tint != 0x00FF_FFFF),
        "notes carry their rainbow tint (the body stays neutral)"
    );
    assert!(
        emitted.iter().all(|s| s.alpha == 200),
        "full drive passes the note envelope through"
    );
    assert!(
        emitted.iter().all(|s| s.y < body.y + i32::from(body.h)),
        "notes rise from the mouth, never below the cat"
    );

    let mut fading = Vec::new();
    wd.kitty_cursor(frame(0.5, notes), &mut fading);
    let faded = &fading[..fading.len() - 1];
    assert_eq!(faded.len(), MAX_NOTES);
    assert!(
        faded.iter().all(|s| s.alpha == 100),
        "the crossfade rides the emitted alpha"
    );

    let mut plain = Vec::new();
    wd.kitty_cursor(frame(0.0, [None; MAX_NOTES]), &mut plain);
    assert_eq!(plain.len(), 1, "no note work at rest — just the body");
}

#[test]
fn the_cursor_escort_fits_its_slot_at_every_age() {
    let g = geom20();
    let mut wd = WordDecorations::default();
    let emit = |wd: &mut WordDecorations, age| {
        wd.cat_baker_ready = false;
        let mut sprites = Vec::new();
        wd.kitty_cursor(
            KittyCursorFrame {
                look: KittyLook {
                    age,
                    ..KittyLook::default()
                },
                ..still_frame(g, (2, 4))
            },
            &mut sprites,
        );
        assert_eq!(sprites.len(), 1, "the escort drew");
        sprites[0].h
    };
    for age in [
        CatAge::Kitten,
        CatAge::Adolescent,
        CatAge::Adult,
        CatAge::Elder,
    ] {
        let h = emit(&mut wd, age);
        assert!(
            i32::from(h) <= 2 * i32::from(g.cell_h),
            "{age:?} escort ({h} px) must fit the 2-cell bake slot"
        );
    }
    assert_ne!(
        emit(&mut wd, CatAge::Kitten),
        emit(&mut wd, CatAge::Elder),
        "a clamped-flat age axis would defeat the ceiling above"
    );
}

#[test]
fn one_typed_animal_word_grows_exactly_one_cat_centred_on_its_span() {
    let lex = lex();
    let c = cfg();
    let mut still = cfg();
    still.reduced_motion = true;
    let g = geom20();
    let (rows, cols) = (g.rows as usize, g.cols as usize);
    let cw = i32::from(g.cell_w);
    let at = Instant::now() + Duration::from_millis(DWELL_QUIET_MS);

    let measure = |word: &str, cfg: &DecoConfig, slack: i32| {
        let t0 = Instant::now();
        let mut term = Terminal::new(g.rows, g.cols);
        term.process(format!("the {word} sat").as_bytes());
        let mut wd = WordDecorations::default();
        wd.rescan(&term, rows, cols, &lex, cfg, 1, t0);
        let occ = feline(&wd).clone();
        assert_eq!(
            (occ.start_col, occ.end_col),
            (4, 4 + word.chars().count() as u16 - 1),
            "PRECONDITION: {word:?} sits where the fixture typed it"
        );

        let (free, _, _) = tick_cat(&mut wd, at, cfg, g);
        assert_eq!(
            free.len(),
            1,
            "{word:?}: one animal word, ONE cat — got {free:?}"
        );
        let cat = free[0].clone();
        assert!(
            cat.x > 0 && cat.x + i32::from(cat.w) < i32::from(g.cols) * cw,
            "PRECONDITION: {word:?}'s head clears both viewport edges, so no \
             clamp is standing in for the centring law: {cat:?}"
        );

        let mid = (i32::from(occ.start_col) + i32::from(occ.end_col) + 1) * cw / 2;
        let variant =
            special_variant_v4(occ.genome.magic).unwrap_or_else(|| cat_variant_v4(occ.genome.gkey));
        let center = f32::from(GLYPHS[variant as usize].center_x) / f32::from(FIXED_ONE);
        let drawn_center = cat.x + (center * f32::from(cat.w)).round() as i32;
        assert!(
            (drawn_center - mid).abs() <= slack,
            "{word:?}: the head's authored centre must land on the word's \
             midpoint (span {}..={} ⇒ mid {mid} px, drawn {drawn_center} px, \
             slack {slack}); {cat:?}",
            occ.start_col,
            occ.end_col
        );
    };

    for word in ["cat", "cats"] {
        measure(word, &still, 0);
        measure(word, &c, 1);
    }
}

#[test]
fn custom_kitty_footprint_matches_emission_at_interior_right_and_top_edges() {
    let g = geom20();
    let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
    let rgba: Arc<[u8]> = Arc::from(vec![0xff; 4 * 2 * 4]);
    let mut out = Vec::new();
    for (w, h) in [(0, usize::MAX), (usize::MAX, 0)] {
        crate::kitty_cursor::resample_nearest_into(&rgba, 4, 2, w, h, &mut out);
        assert!(out.is_empty());
    }
    crate::kitty_cursor::resample_nearest_into(&rgba, 4, 2, 4, 2, &mut out);
    assert_eq!(out, rgba.as_ref());
    let mut wd = WordDecorations::default();
    wd.set_kitty_sprite_source(KittySpriteSource::Custom {
        source_fp: 0x101,
        w: 4,
        h: 2,
        rgba,
    });
    let emit = |wd: &mut WordDecorations, cursor: (u16, u16)| {
        let expected = wd
            .kitty_cursor_footprint(still_layout(g, cursor))
            .expect("custom sprite remains placeable");
        let mut free = Vec::new();
        wd.kitty_cursor(still_frame(g, cursor), &mut free)
            .expect("custom sprite emits");
        assert_eq!(free.len(), 1, "custom art is exactly one body");
        let body = free[0].clone();
        assert_eq!(
            expected,
            CatFootprint {
                x: body.x,
                y: body.y,
                w: body.w,
                h: body.h,
            },
            "palette/custody geometry is the body actually sent to the renderer"
        );
        body
    };

    let interior = emit(&mut wd, (3, 5));
    assert_eq!((interior.w, interior.h), (72, 36));
    assert!(interior.x >= 0 && interior.x + i32::from(interior.w) <= grid_w);

    let right = emit(&mut wd, (3, g.cols - 1));
    assert_eq!(
        right.x + i32::from(right.w),
        grid_w,
        "the configured body clamps exactly at the right edge"
    );
    assert!(right.x >= 0);

    let top_right = emit(&mut wd, (0, g.cols - 1));
    assert_eq!(top_right.x + i32::from(top_right.w), grid_w);
    assert!(
        top_right.y >= 0 && top_right.y < i32::from(g.cell_h),
        "row-zero custom art lands whole beside its row, never clipped above the grid, \
         misplaced or dropped: {top_right:?}"
    );

    wd.set_kitty_sprite_source(KittySpriteSource::BuiltIn);
    let homage = emit(&mut wd, (3, 5));
    assert_ne!(
        (homage.w, homage.h),
        (interior.w, interior.h),
        "clearing the custom art restores the built-in homage"
    );
}

#[test]
fn custom_kitty_hot_reload_rebases_repaints_and_invalid_replacement_is_dark() {
    use crate::kitty_cursor::{CatFoldDirection, CatFoldFrame, CursorCatPlacementFrame};

    let g = geom20();
    let t0 = Instant::now();
    let red: Arc<[u8]> = Arc::from([0xff, 0x10, 0x20, 0xff].repeat(8));
    let green: Arc<[u8]> = Arc::from([0x20, 0xee, 0x60, 0xff].repeat(8));
    let mut wd = WordDecorations::default();
    wd.set_kitty_sprite_source(KittySpriteSource::Custom {
        source_fp: 0x201,
        w: 4,
        h: 2,
        rgba: Arc::clone(&red),
    });

    let layout = |cursor| still_layout(g, cursor);
    let emit = |wd: &mut WordDecorations, cursor: (u16, u16), motion: CursorCatPlacementFrame| {
        let placement = wd
            .resolve_kitty_cursor_placement(layout(cursor), motion)
            .expect("custom kitty placement");
        let mut free = Vec::new();
        let fp = wd
            .kitty_cursor_at_placement(still_frame(g, cursor), placement, &mut free)
            .expect("custom kitty emission");
        (placement, free[0].clone(), fp)
    };
    let art_pixel =
        |sprite: &FreeSprite| <[u8; 4]>::try_from(&sprite.art.data()[..4]).expect("art texel");

    let old_cursor = (3, g.cols - 1);
    let (_, red_body, red_fp) = emit(&mut wd, old_cursor, settled_at(t0));
    let red_version = wd.cat_baker.version();
    assert_eq!(art_pixel(&red_body), [0xff, 0x10, 0x20, 0xff]);

    let started = t0 + Duration::from_millis(16);
    let folding = |progress| CursorCatPlacementFrame {
        sampled_at: started + Duration::from_secs_f32(progress * 0.28),
        fold: Some(CatFoldFrame {
            started,
            direction: CatFoldDirection::Forward,
            progress,
        }),
        facing_left: false,
    };
    let (moving, _, _) = emit(&mut wd, (4, 0), folding(0.2));
    assert!(moving.in_motion(), "negative control: old art is mid-fold");

    wd.set_kitty_sprite_source(KittySpriteSource::Custom {
        source_fp: 0x202,
        w: 4,
        h: 2,
        rgba: Arc::clone(&green),
    });
    assert_eq!(wd.kitty_sprite_source_fingerprint(), Some(0x202));
    assert!(Arc::ptr_eq(
        wd.kitty_sprite_rgba().expect("replacement source"),
        &green
    ));
    let (replaced, green_body, green_fp) = emit(&mut wd, (4, 0), folding(0.3));
    assert!(
        !replaced.in_motion(),
        "the replacement cannot inherit the old asset's off-glass fold"
    );
    assert_eq!(
        (green_body.x, green_body.y, green_body.w, green_body.h),
        (
            replaced.sample.x,
            replaced.sample.y,
            replaced.sample.w,
            replaced.sample.h
        )
    );
    assert_eq!(
        art_pixel(&green_body),
        [0x20, 0xee, 0x60, 0xff],
        "same-size hot reload addresses replacement texels"
    );
    assert!(wd.cat_baker.version() > red_version);
    assert_ne!(green_fp, red_fp, "the repaint key moves with the new art");

    wd.set_kitty_sprite_source(KittySpriteSource::Custom {
        source_fp: 0x203,
        w: 4,
        h: 2,
        rgba: Arc::from(vec![0u8; 4 * 2 * 4 - 1]),
    });
    assert_eq!(wd.kitty_sprite_source_fingerprint(), None);
    assert!(!wd.has_custom_kitty_sprite());
    assert!(wd.kitty_sprite_rgba().is_none());
    assert!(wd.kitty_cursor_footprint(layout((4, 0))).is_none());
    let mut stale = Vec::new();
    assert!(
        wd.kitty_cursor(still_frame(g, (4, 0)), &mut stale,)
            .is_none()
    );
    assert!(
        stale.is_empty(),
        "invalid reload cannot retain the old body"
    );
}

#[test]
fn cursor_cat_body_clamps_after_right_shift_stretch_and_lead() {
    let g = geom20();
    let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
    let mut wd = WordDecorations::default();
    let mut free = Vec::new();
    wd.kitty_cursor(
        KittyCursorFrame {
            pose: crate::kitty_cursor::CatPose {
                scale_x: 1.17,
                lead: 0.22,
                ..crate::kitty_cursor::CatPose::STILL
            },
            ..still_frame(g, (3, g.cols - 1))
        },
        &mut free,
    );
    assert_eq!(free.len(), 1, "the cursor kitty emits only its body");
    let body = free[0].clone();
    assert_eq!(
        body.x + i32::from(body.w),
        grid_w,
        "shift, stretch, and pose lead must still clamp to the grid edge"
    );
    assert!(body.x >= 0, "body never crosses the left edge");
}

#[test]
fn cursor_cat_fold_launches_from_the_drawn_body_and_lands_without_a_snap() {
    use crate::kitty_cursor::{CatFoldDirection, CatFoldFrame, CursorCatPlacementFrame};

    let g = geom20();
    let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let emit = |wd: &mut WordDecorations, cursor, motion| emit_placed(wd, g, cursor, motion);

    let old_cursor = (3, g.cols - 1);
    let (_, old) = emit(&mut wd, old_cursor, settled_at(t0));
    assert_eq!(old.x + i32::from(old.w), grid_w, "seed at right edge");

    let started = t0 + Duration::from_millis(16);
    let motion = |progress: f32, sampled_at: Instant| CursorCatPlacementFrame {
        sampled_at,
        fold: Some(CatFoldFrame {
            started,
            direction: CatFoldDirection::Forward,
            progress,
        }),
        facing_left: false,
    };
    let new_cursor = (4, 0);
    let (launch_place, launch) = emit(&mut wd, new_cursor, motion(0.0, started));
    assert_eq!(
        (launch.x, launch.y),
        (old.x, old.y),
        "the first fold sample is C0-continuous with the body actually drawn"
    );
    assert_eq!(
        launch_place.sample,
        wd.kitty_cursor_footprint(still_layout(g, new_cursor))
            .expect("logical target"),
        "palette sampling follows the live target, never the offstage body"
    );

    let advanced = (4, 3);
    let (entry_place, entry_body) = emit(
        &mut wd,
        advanced,
        motion(0.85, started + Duration::from_millis(238)),
    );
    assert_eq!(entry_place.sample.x, 4 * i32::from(g.cell_w) + 7);
    let mut at = started + crate::kitty_cursor::CURSOR_FOLD_DURATION;
    let (_, mut landed) = emit(&mut wd, advanced, settled_at(at));
    assert!(
        (landed.x - entry_body.x).abs() < 2 * i32::from(g.cell_w),
        "the landing glides from the entry body, no end snap: {entry_body:?} -> {landed:?}"
    );
    for _ in 0..40 {
        at += Duration::from_millis(16);
        let (_, body) = emit(&mut wd, advanced, settled_at(at));
        landed = body;
    }
    let mut direct = Vec::new();
    wd.kitty_cursor(still_frame(g, advanced), &mut direct);
    assert_eq!(
        landed, direct[0],
        "the seam converges on the exact settled frame"
    );
}

#[test]
fn reverse_fold_mirrors_reenters_and_rebaselines_on_coordinate_breaks() {
    use crate::kitty_cursor::{CatFoldDirection, CatFoldFrame, CursorCatPlacementFrame};

    let g = geom20();
    let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();

    let emit =
        |wd: &mut WordDecorations, geom, cursor, motion| emit_placed(wd, geom, cursor, motion);
    let _ = emit(&mut wd, g, (3, 0), settled_at(t0));
    let started = t0 + Duration::from_millis(16);
    let fold = |progress| CursorCatPlacementFrame {
        sampled_at: started + Duration::from_secs_f32(progress * 0.28),
        fold: Some(CatFoldFrame {
            started,
            direction: CatFoldDirection::Reverse,
            progress,
        }),
        facing_left: true,
    };
    let (_, left_off) = emit(&mut wd, g, (2, g.cols - 1), fold(0.499));
    assert!(left_off.flip_x);
    assert!(left_off.x + i32::from(left_off.w) <= 0);
    let (_, right_off) = emit(&mut wd, g, (2, g.cols - 1), fold(0.5));
    assert!(right_off.flip_x);
    assert!(right_off.x >= grid_w);

    let reentered_at = started + Duration::from_millis(150);
    let reentered = |progress| CursorCatPlacementFrame {
        sampled_at: reentered_at + Duration::from_secs_f32(progress * 0.28),
        fold: Some(CatFoldFrame {
            started: reentered_at,
            direction: CatFoldDirection::Forward,
            progress,
        }),
        facing_left: false,
    };
    let (_, restarted) = emit(&mut wd, g, (3, 0), reentered(0.0));
    assert_eq!(
        (restarted.x, restarted.y),
        (right_off.x, right_off.y),
        "a reentrant fold is C0-continuous with the last emitted body"
    );

    let zero_width = EffectGeom { cell_w: 0, ..g };
    assert!(
        wd.resolve_kitty_cursor_placement(still_layout(zero_width, (3, 0)), reentered(0.1))
            .is_none()
    );
    let (reappeared, body) = emit(&mut wd, g, (3, 0), reentered(0.2));
    assert!(!reappeared.in_motion());
    assert!(body.x >= 0 && body.x + i32::from(body.w) <= grid_w);

    wd.set_kitty_sprite_source(KittySpriteSource::BuiltIn);
    let (replaced, _) = emit(&mut wd, g, (3, 0), reentered(0.3));
    assert!(!replaced.in_motion());

    let resized = EffectGeom {
        cols: g.cols + 1,
        ..g
    };
    let (place, body) = emit(&mut wd, resized, (3, 0), reentered(0.4));
    assert!(
        !place.in_motion(),
        "geometry changes settle instead of interpolating incomparable spaces"
    );
    assert!(body.x >= 0 && body.x + i32::from(body.w) <= 210);

    let torn_at = reentered_at + Duration::from_millis(160);
    let torn_fold = |progress| CursorCatPlacementFrame {
        sampled_at: torn_at + Duration::from_secs_f32(progress * 0.28),
        fold: Some(CatFoldFrame {
            started: torn_at,
            direction: CatFoldDirection::Forward,
            progress,
        }),
        facing_left: false,
    };
    let (moving, _) = emit(&mut wd, resized, (3, 0), torn_fold(0.1));
    assert!(moving.in_motion());
    wd.rebase_kitty_cursor_placement();
    let (rebased, body) = emit(&mut wd, resized, (3, 0), torn_fold(0.2));
    assert!(!rebased.in_motion());
    assert!(body.x >= 0 && body.x + i32::from(body.w) <= 210);

    let session_fold_at = torn_at + Duration::from_millis(80);
    let session_fold = |progress| CursorCatPlacementFrame {
        sampled_at: session_fold_at + Duration::from_secs_f32(progress * 0.28),
        fold: Some(CatFoldFrame {
            started: session_fold_at,
            direction: CatFoldDirection::Reverse,
            progress,
        }),
        facing_left: true,
    };
    let (moving, _) = emit(&mut wd, resized, (2, g.cols), session_fold(0.1));
    assert!(
        moving.in_motion(),
        "negative control: a fresh fold animates"
    );
    wd.set_scan_session(Some(0xCA7));
    let (switched, _) = emit(&mut wd, resized, (2, g.cols), session_fold(0.2));
    assert!(!switched.in_motion(), "a new session settles old travel");
}

#[test]
fn sparkle_word_reset_preserves_cursor_companion_placement_and_atlas() {
    let g = geom20();
    let now = Instant::now();
    let mut wd = WordDecorations::default();
    wd.begin_host_frame();
    let layout = still_layout(g, (3, 7));
    let placement = wd
        .resolve_kitty_cursor_placement(layout, settled_at(now))
        .expect("placeable companion");
    let mut free = Vec::new();
    wd.kitty_cursor_at_placement(still_frame(g, layout.cursor), placement, &mut free)
        .expect("companion emits");
    let (cache_before, version_before) = (wd.cat_baker.len(), wd.cat_baker.version());
    assert!(cache_before > 0, "the companion baked its tile");
    let key_before = wd.kitty_cursor_placement.key;
    let target_before = wd.kitty_cursor_placement.last_target;
    wd.have_scanned = true;
    wd.active_until = Some(now + Duration::from_secs(1));

    wd.hard_reset_words();

    assert!(!wd.have_scanned, "word scan state retires immediately");
    assert!(
        wd.active_until.is_none(),
        "word cadence retires immediately"
    );
    assert_eq!(wd.kitty_cursor_placement.key, key_before);
    assert_eq!(wd.kitty_cursor_placement.last_target, target_before);
    assert_eq!(
        (wd.cat_baker.len(), wd.cat_baker.version()),
        (cache_before, version_before),
        "a word-only toggle must not rebake the companion's tiles"
    );

    wd.hard_reset();
    assert!(
        wd.kitty_cursor_placement.key.is_none(),
        "negative control: a full lifecycle reset retires placement"
    );
    assert!(
        wd.cat_baker.is_empty(),
        "negative control: a full reset evicts the tile cache"
    );
}

#[test]
fn cursor_cat_fold_conformance_real_placement_and_teleport_mutant() {
    use crate::kitty_cursor::{CatFoldDirection, CatFoldFrame, CursorCatPlacementFrame};

    let model = aterm_models_effects::cursor_cat_fold_model();
    let g = geom20();
    let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
    let t0 = Instant::now();

    let emit = |wd: &mut WordDecorations, cursor, motion| emit_placed(wd, g, cursor, motion);

    let sprite_place = |sprite: FreeSprite| {
        let wholly_left = sprite.x + i32::from(sprite.w) <= 0;
        let wholly_right = sprite.x >= grid_w;
        let off = wholly_left || wholly_right;
        let side = if wholly_left {
            0
        } else if wholly_right {
            1
        } else {
            i64::from(sprite.x * 2 + i32::from(sprite.w) >= grid_w)
        };
        (side, i64::from(off))
    };

    let project = |placement: KittyCursorPlacement,
                   sprite: FreeSprite,
                   origin_side: i64,
                   seen_off: bool,
                   folds: i64| {
        let (side, off_glass) = sprite_place(sprite);
        let (phase, direction) = match placement.leg {
            KittyCursorPlacementLeg::Settled => (0, 0),
            KittyCursorPlacementLeg::Exit { direction, .. } => (
                if off_glass == 1 { 2 } else { 1 },
                match direction {
                    CatFoldDirection::Forward => 1,
                    CatFoldDirection::Reverse => 2,
                },
            ),
            KittyCursorPlacementLeg::Enter { direction, .. } => (
                if off_glass == 1 { 3 } else { 4 },
                match direction {
                    CatFoldDirection::Forward => 1,
                    CatFoldDirection::Reverse => 2,
                },
            ),
        };
        let mut state = model.init_state();
        state.insert("phase", phase);
        state.insert("direction", direction);
        state.insert("origin_side", origin_side);
        state.insert("side", side);
        state.insert("off_glass", off_glass);
        state.insert("off_samples", i64::from(seen_off));
        state.insert("folds", folds);
        state
    };

    let assert_step = |before: &std::collections::BTreeMap<&'static str, i64>,
                       after: &std::collections::BTreeMap<&'static str, i64>,
                       action: &'static str,
                       label: &str| {
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            before,
            after,
            Some(action),
            label,
        );
        assert!(ok, "{label}: {why}");
        let mut expected = before.clone();
        assert!(model.fire(action, &mut expected), "{label}: action enabled");
        assert_eq!(&expected, after, "{label}: real projection matches model");
    };

    let motion = |started: Instant, direction, progress: f32| CursorCatPlacementFrame {
        sampled_at: started + Duration::from_secs_f32(progress * 0.28),
        fold: Some(CatFoldFrame {
            started,
            direction,
            progress,
        }),
        facing_left: matches!(direction, CatFoldDirection::Reverse),
    };

    let mut wd = WordDecorations::default();
    let (seed_placement, seed) = emit(&mut wd, (3, g.cols - 1), settled_at(t0));
    let mut state = project(seed_placement, seed, 1, false, 0);
    assert_eq!(state, model.init_state(), "right-edge seed is model Init");

    let forward_at = t0 + Duration::from_millis(16);
    for (progress, action, origin, seen_off, label) in [
        (0.0, "StartForward", 1, false, "forward launch on old edge"),
        (0.499, "LeaveOff", 1, true, "forward body wholly right-off"),
        (0.5, "CrossSide", 1, true, "forward body wholly left-off"),
        (0.9, "EnterGlass", 1, true, "forward arrival on new edge"),
    ] {
        let (placement, sprite) = emit(
            &mut wd,
            (4, 0),
            motion(forward_at, CatFoldDirection::Forward, progress),
        );
        let next = project(placement, sprite, origin, seen_off, 1);
        assert_step(&state, &next, action, label);
        state = next;
    }
    let (placement, sprite) = emit(
        &mut wd,
        (4, 0),
        settled_at(forward_at + crate::kitty_cursor::CURSOR_FOLD_DURATION),
    );
    let next = project(placement, sprite, 0, false, 1);
    assert_step(&state, &next, "Finish", "forward settles on left edge");
    state = next;

    let reverse_at =
        forward_at + crate::kitty_cursor::CURSOR_FOLD_DURATION + Duration::from_millis(16);
    for (progress, action, origin, seen_off, label) in [
        (0.0, "StartReverse", 0, false, "reverse launch on old edge"),
        (0.499, "LeaveOff", 0, true, "reverse body wholly left-off"),
        (0.5, "CrossSide", 0, true, "reverse body wholly right-off"),
        (0.9, "EnterGlass", 0, true, "reverse arrival on new edge"),
    ] {
        let (placement, sprite) = emit(
            &mut wd,
            (3, g.cols - 1),
            motion(reverse_at, CatFoldDirection::Reverse, progress),
        );
        let next = project(placement, sprite, origin, seen_off, 2);
        assert_step(&state, &next, action, label);
        state = next;
    }
    let (placement, sprite) = emit(
        &mut wd,
        (3, g.cols - 1),
        CursorCatPlacementFrame {
            sampled_at: reverse_at + crate::kitty_cursor::CURSOR_FOLD_DURATION,
            fold: None,
            facing_left: true,
        },
    );
    let next = project(placement, sprite, 1, false, 2);
    assert_step(&state, &next, "Finish", "reverse settles on right edge");

    let mut mutant_wd = WordDecorations::default();
    let mut free = Vec::new();
    let frame = |cursor| still_frame(g, cursor);
    mutant_wd
        .kitty_cursor(frame((3, g.cols - 1)), &mut free)
        .expect("old edge emits");
    let old_edge = free.last().expect("old body").clone();
    free.clear();
    mutant_wd
        .kitty_cursor(frame((4, 0)), &mut free)
        .expect("new edge emits");
    let new_edge = free.last().expect("new body").clone();
    assert_eq!(sprite_place(old_edge), (1, 0));
    assert_eq!(sprite_place(new_edge), (0, 0));

    let mut before = model.init_state();
    assert!(model.fire("StartForward", &mut before));
    let mut teleported = before.clone();
    teleported.insert("phase", 4);
    teleported.insert("side", 0);
    teleported.insert("off_glass", 0);
    teleported.insert("off_samples", 0);
    let (healthy_ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &before,
        &teleported,
        Some("LeaveOff"),
        "direct edge teleport mutant",
    );
    assert!(
        !healthy_ok,
        "healthy model rejects the old direct placement"
    );
    assert!(!model.check_invariant("OffGlassBeforeSideChange", &teleported));

    let mut buggy = aterm_models_effects::cursor_cat_fold_model();
    for cst in &mut buggy.consts {
        if cst.0 == "Buggy" {
            cst.1 = 1;
        }
    }
    let (buggy_ok, why) = aterm_spec::verify::validate_transition_tiered(
        &buggy,
        &[],
        &before,
        &teleported,
        Some("LeaveOff"),
        "Buggy=1 direct edge teleport",
    );
    assert!(buggy_ok, "Buggy=1 must reproduce the mutant: {why}");
}

#[test]
fn right_edge_clamp_rises_the_companion_instead_of_covering_the_text() {
    let g = geom20();
    let grid_w = i32::from(g.cols) * i32::from(g.cell_w);
    let wd = WordDecorations::default();
    let fp_at = |cursor: (u16, u16)| {
        wd.kitty_cursor_footprint(still_layout(g, cursor))
            .expect("visible footprint")
    };
    let interior = fp_at((3, 5));
    let edge = fp_at((3, g.cols - 1));
    assert_eq!(edge.x + i32::from(edge.w), grid_w, "grid-edge clamp holds");
    let cursor_right = i32::from(g.cols) * i32::from(g.cell_w);
    assert!(
        edge.x < cursor_right,
        "the clamped sprite horizontally overlaps the text cells"
    );
    assert_eq!(
        edge.y,
        interior.y - i32::from(g.cell_h) / 2,
        "clamped footprint rises ch/2 above the centred rest"
    );

    let mut wd = WordDecorations::default();
    let emit = |wd: &mut WordDecorations, cursor: (u16, u16)| {
        let mut free = Vec::new();
        wd.kitty_cursor(still_frame(g, cursor), &mut free);
        assert_eq!(free.len(), 1, "one companion body at {cursor:?}");
        free[0].clone()
    };
    let interior_body = emit(&mut wd, (3, 5));
    let edge_body = emit(&mut wd, (3, g.cols - 1));
    assert_eq!(
        edge_body.y,
        interior_body.y - i32::from(g.cell_h) / 2,
        "the emitted body follows the edge-rise footprint"
    );

    let top_interior = fp_at((0, 5));
    let top_edge = fp_at((0, g.cols - 1));
    assert_eq!(
        top_edge.y, top_interior.y,
        "a top-row flight is never lifted off-grid by the rise"
    );
}

#[test]
fn margin_staircase_glides_instead_of_stepping_per_key() {
    let g = geom20();
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let emit = |wd: &mut WordDecorations, cursor, at| emit_placed(wd, g, cursor, settled_at(at)).1;

    let mut at = t0;
    let mut prev = emit(&mut wd, (3, 15), at);
    let mut max_step = 0i32;
    for ccol in 16..=19u16 {
        at += Duration::from_millis(16);
        let body = emit(&mut wd, (3, ccol), at);
        max_step = max_step.max((body.y - prev.y).abs());
        prev = body;
    }
    assert!(
        max_step <= 4,
        "typing into the margin must glide, not step: max per-frame |dy| = {max_step} px"
    );

    let mut settled = prev;
    for _ in 0..40 {
        at += Duration::from_millis(16);
        settled = emit(&mut wd, (3, 19), at);
    }
    let mut direct = Vec::new();
    wd.kitty_cursor(still_frame(g, (3, 19)), &mut direct)
        .expect("direct emitter draws");
    assert_eq!(
        (settled.x, settled.y),
        (direct[0].x, direct[0].y),
        "the glide lands on the same bytes the raw placement law draws"
    );
}

#[test]
fn a_seat_lift_glides_instead_of_jumping_a_row() {
    let g = geom20();
    let cursor = (3, 5);
    let raw = |wd: &WordDecorations, lift: f32| {
        wd.kitty_cursor_footprint(KittyCursorLayout {
            lift,
            ..still_layout(g, cursor)
        })
        .expect("raw law")
        .y
    };
    let run = |as_bob: bool| {
        let mut wd = WordDecorations::default();
        let mut at = Instant::now();
        let mut emit = |lifted: f32, at| {
            let (bob, lift) = if as_bob {
                (-lifted, 0.0)
            } else {
                (0.0, lifted)
            };
            let layout = KittyCursorLayout {
                bob,
                lift,
                ..still_layout(g, cursor)
            };
            let placement = wd
                .resolve_kitty_cursor_placement(layout, settled_at(at))
                .expect("placeable companion");
            let mut free = Vec::new();
            wd.kitty_cursor_at_placement(
                KittyCursorFrame {
                    bob,
                    lift,
                    ..still_frame(g, cursor)
                },
                placement,
                &mut free,
            )
            .expect("companion emitted");
            free.last().expect("the body emits last").y
        };
        let mut prev = emit(0.0, at);
        let (mut max_step, mut landed) = (0, Vec::new());
        for lifted in [1.0f32, 0.0] {
            for _ in 0..40 {
                at += Duration::from_millis(8);
                let y = emit(lifted, at);
                max_step = max_step.max((y - prev).abs());
                prev = y;
            }
            landed.push(prev);
        }
        (max_step, landed)
    };
    let wd = WordDecorations::default();
    let (step, landed) = run(false);
    assert!(
        step <= 3,
        "a seat lift moved the body {step} px in one present (cell {} px)",
        g.cell_h
    );
    assert_eq!(
        landed,
        vec![raw(&wd, 1.0), raw(&wd, 0.0)],
        "it lands on the law"
    );
    assert_eq!(
        run(true).0,
        i32::from(g.cell_h),
        "control: a lift in the bob jumps a whole row"
    );
}

/// Row 0 has no row above it, and the row-1 sky seat lifts the head into row 0: no pose,
/// bob, fold leg or song note may start a pixel above the grid top, where a pane divider
/// cuts it, the window pad hides it, or (after the tab-strip splice) the strip covers it.
#[test]
fn the_flying_head_on_the_top_rows_stays_inside_the_grid() {
    use crate::kitty_cursor::{CatFoldDirection, CatFoldFrame, CatPose, CursorCatPlacementFrame};
    use crate::kitty_sing::{MAX_NOTES, NoteKind, NoteSprite};

    let stretched = CatPose {
        scale_x: 0.9,
        scale_y: 1.25,
        ..CatPose::STILL
    };
    let mut notes = [None; MAX_NOTES];
    notes[0] = Some(NoteSprite {
        kind: NoteKind::Eighth,
        dx: 0.5,
        dy: -1.0,
        alpha: 255,
        tint: 0x00FF_AA44,
    });
    let small = EffectGeom {
        cell_w: 9,
        cell_h: 19,
        rows: 24,
        cols: 80,
    };
    for g in [geom20(), small] {
        let ch = i32::from(g.cell_h);
        for (row, lift) in [(0u16, 0.0f32), (1, 0.0), (1, 1.0)] {
            let cursor = (row, 5);
            let caret_top = i32::from(row) * ch;
            for bob in [-0.09f32, 0.0, 0.09] {
                let at = format!(
                    "{}x{} caret {cursor:?} lift {lift} bob {bob}",
                    g.cell_w, g.cell_h
                );
                let layout = KittyCursorLayout {
                    bob,
                    lift,
                    ..still_layout(g, cursor)
                };
                let fp = WordDecorations::default()
                    .kitty_cursor_footprint(layout)
                    .expect("a footprint");
                assert!(
                    fp.y >= 0,
                    "{at}: the footprint starts {} px above the grid",
                    -fp.y
                );
                assert!(
                    fp.y < caret_top + ch && fp.y + i32::from(fp.h) > caret_top,
                    "{at}: the head stays beside the caret's row: {fp:?}"
                );
                for pose in [CatPose::STILL, stretched] {
                    let mut wd = WordDecorations::default();
                    let t0 = Instant::now();
                    let fold = |progress: f32, ms: u64| CursorCatPlacementFrame {
                        sampled_at: t0 + Duration::from_millis(ms),
                        fold: Some(CatFoldFrame {
                            started: t0 + Duration::from_millis(16),
                            direction: CatFoldDirection::Forward,
                            progress,
                        }),
                        facing_left: false,
                    };
                    let frame = KittyCursorFrame {
                        bob,
                        lift,
                        pose,
                        sing: 1.0,
                        notes,
                        ..still_frame(g, cursor)
                    };
                    for (leg, motion) in [
                        ("settled", settled_at(t0)),
                        ("exit", fold(0.25, 24)),
                        ("enter", fold(0.75, 32)),
                    ] {
                        let placement = wd
                            .resolve_kitty_cursor_placement(layout, motion)
                            .expect("placeable companion");
                        let mut free = Vec::new();
                        wd.kitty_cursor_at_placement(frame, placement, &mut free)
                            .expect("companion emitted");
                        assert_eq!(free.len(), 2, "{at} {leg}: fixture: the note and the body");
                        for s in &free {
                            assert!(
                                s.y >= 0,
                                "{at} {leg} scale_y {}: a {}x{} sprite starts {} px above the grid",
                                pose.scale_y,
                                s.w,
                                s.h,
                                -s.y
                            );
                        }
                    }
                }
            }
        }
        let open = WordDecorations::default()
            .kitty_cursor_footprint(KittyCursorLayout {
                lift: 1.0,
                ..still_layout(g, (3, 5))
            })
            .expect("a footprint");
        assert_eq!(
            open.y,
            3 * ch + ch / 2 - i32::from(open.h) / 2 - ch,
            "control: with room above, the sky seat still rises a whole row"
        );
    }
}

#[test]
fn a_fold_started_mid_glide_exits_from_the_glide_position() {
    use crate::kitty_cursor::{CatFoldDirection, CatFoldFrame, CursorCatPlacementFrame};

    let g = geom20();
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let emit = |wd: &mut WordDecorations, cursor, motion| emit_placed(wd, g, cursor, motion).1;
    let settled = settled_at;

    let _ = emit(&mut wd, (3, 14), settled(t0));
    let _ = emit(&mut wd, (3, 15), settled(t0 + Duration::from_millis(16)));
    let mid = emit(&mut wd, (3, 16), settled(t0 + Duration::from_millis(32)));
    let raw = wd
        .kitty_cursor_footprint(still_layout(g, (3, 16)))
        .expect("raw law");
    assert_ne!(
        (mid.x, mid.y),
        (raw.x, raw.y),
        "negative control: the body is genuinely mid-glide, behind the raw law"
    );

    let started = t0 + Duration::from_millis(48);
    let launch = emit(
        &mut wd,
        (4, 0),
        CursorCatPlacementFrame {
            sampled_at: started,
            fold: Some(CatFoldFrame {
                started,
                direction: CatFoldDirection::Forward,
                progress: 0.0,
            }),
            facing_left: false,
        },
    );
    assert_eq!(
        (launch.x, launch.y),
        (mid.x, mid.y),
        "the Exit leg is C0-continuous with the glide position"
    );
}

#[test]
fn compat_settled_emission_is_the_raw_footprint_and_ignores_the_glide() {
    let g = geom20();
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let resolver_emit =
        |wd: &mut WordDecorations, cursor, at| emit_placed(wd, g, cursor, settled_at(at)).1;

    let _ = resolver_emit(&mut wd, (3, 16), t0);
    let _ = resolver_emit(&mut wd, (3, 17), t0 + Duration::from_millis(16));
    let ledger_settle = wd.kitty_cursor_placement.settle;
    let ledger_sprite = wd.kitty_cursor_placement.last_sprite;
    assert!(ledger_settle.is_some(), "a resolver glide is live");

    let raw = wd
        .kitty_cursor_footprint(still_layout(g, (3, 17)))
        .expect("raw law");
    let compat = |wd: &mut WordDecorations| {
        let mut free = Vec::new();
        wd.kitty_cursor(still_frame(g, (3, 17)), &mut free)
            .expect("compat emitter draws");
        free.last().expect("one body").clone()
    };
    let first = compat(&mut wd);
    let second = compat(&mut wd);
    assert_eq!(
        (first.x, first.y),
        (raw.x, raw.y),
        "the compat path draws the raw footprint, not the glide"
    );
    assert_eq!(first, second, "the compat path is byte-stable");
    assert_eq!(
        wd.kitty_cursor_placement.settle, ledger_settle,
        "the compat path never writes the follower's ledger"
    );
    assert_eq!(
        wd.kitty_cursor_placement.last_sprite, ledger_sprite,
        "the compat path never writes the continuity ledger"
    );
}

#[test]
fn invalid_kitty_source_fails_closed_instead_of_using_builtin_art() {
    let g = geom20();
    let mut wd = WordDecorations::default();
    wd.set_kitty_sprite_source(KittySpriteSource::Disabled);
    assert_eq!(wd.kitty_sprite_source_fingerprint(), None);
    assert!(wd.kitty_cursor_footprint(still_layout(g, (3, 5))).is_none());
    let mut free = Vec::new();
    assert!(
        wd.kitty_cursor(still_frame(g, (3, 5)), &mut free,)
            .is_none()
    );
    assert!(free.is_empty());
}

/// looks10 L10: the flying head's follower trails the caret through a
/// burst (it glides with `PLACE_TAU`), and at the right margin its seat
/// glides up out of the caret's row; either way the drawn head is kept
/// off the caret's cell and every lettered cell, pushed ahead with air
/// between it and the caret or lifted clear, and the follower carries on
/// from where it was drawn. Control: `HEAD_KEEPS_OFF_OFF` draws it where
/// the follower put it, over the caret.
#[test]
fn the_flying_head_keeps_off_the_caret_and_the_letters() {
    let g = geom20();
    let (cw, ch) = (i32::from(g.cell_w), i32::from(g.cell_h));
    let t0 = Instant::now();
    // Row 3 is lettered from column 0 up to the caret.
    let ink_to = |end: u16| move |row: u16, col: u16| Some(row == 3 && col < end);
    let over = |rect: CatFootprint, caret: (u16, u16), end: u16| -> Vec<(i32, i32)> {
        let mut hits = Vec::new();
        let (x0, x1) = (rect.x, rect.x + i32::from(rect.w));
        let (y0, y1) = (rect.y, rect.y + i32::from(rect.h));
        for r in 0..i32::from(g.rows) {
            for c in 0..i32::from(g.cols) {
                let lettered = r == 3 && c < i32::from(end);
                let is_caret = (r, c) == (i32::from(caret.0), i32::from(caret.1));
                if (lettered || is_caret)
                    && x0 < (c + 1) * cw
                    && x1 > c * cw
                    && y0 < (r + 1) * ch
                    && y1 > r * ch
                {
                    hits.push((r, c));
                }
            }
        }
        hits
    };
    let run = |keep: bool| {
        HEAD_KEEPS_OFF_OFF.with(|c| c.set(!keep));
        let mut wd = WordDecorations::default();
        // A burst: the caret steps a cell every 16 ms from column 4 to 9.
        let mut worst = Vec::new();
        let mut gaps = Vec::new();
        for (i, col) in (4..=9_u16).enumerate() {
            let at = t0 + Duration::from_millis(16 * i as u64);
            let caret = (3, col);
            let mut placement = wd
                .prepare_kitty_cursor_placement(
                    still_layout(g, caret),
                    settled_at(at),
                    crate::kitty_cursor::CatPose::STILL,
                )
                .expect("placeable companion");
            placement.keep_off_letters(g, caret, ink_to(col));
            let mut free = Vec::new();
            wd.kitty_cursor_at_placement(still_frame(g, caret), placement, &mut free)
                .expect("companion emitted");
            let body = free.last().expect("body").clone();
            let rect = CatFootprint {
                x: body.x,
                y: body.y,
                w: body.w,
                h: body.h,
            };
            assert_eq!(placement.drawn_footprint(), Some(rect));
            worst.extend(over(rect, caret, col));
            gaps.push(rect.x - (i32::from(col) + 1) * cw);
        }
        HEAD_KEEPS_OFF_OFF.with(|c| c.set(false));
        (worst, gaps)
    };
    let (kept, gaps) = run(true);
    assert!(kept.is_empty(), "the head lay on {kept:?}");
    let gap = WordDecorations::kitty_cursor_lead_px(g.cell_w) / HEAD_CARET_GAP_DEN;
    assert!(
        gaps.iter().all(|&d| d >= gap),
        "air between the caret and the head: {gaps:?} (at least {gap})"
    );
    let (trailed, _) = run(false);
    assert!(
        !trailed.is_empty(),
        "control: the follower trails the head into the caret"
    );

    // The right margin: no room ahead, so the head is lifted clear of the
    // caret's row; the follower's state moves with it.
    let caret = (3, g.cols - 1);
    let mut wd = WordDecorations::default();
    let mut placement = wd
        .prepare_kitty_cursor_placement(
            still_layout(g, caret),
            settled_at(t0),
            crate::kitty_cursor::CatPose::STILL,
        )
        .expect("placeable companion");
    let rect = placement.drawn_footprint().expect("drawn");
    assert!(
        !over(rect, caret, caret.1).is_empty(),
        "the margin's seat lies on the caret before it is kept off"
    );
    let settle = placement.drawn.and_then(|d| d.settle);
    placement.keep_off_letters(g, caret, ink_to(caret.1));
    let lifted = placement.drawn_footprint().expect("drawn");
    assert!(over(lifted, caret, caret.1).is_empty(), "{lifted:?}");
    assert!(lifted.y + i32::from(lifted.h) <= i32::from(caret.0) * ch);
    assert_eq!(lifted.x, rect.x, "lifted straight up, not across");
    if let (Some(before), Some(after)) = (settle, placement.drawn.and_then(|d| d.settle)) {
        assert_eq!(after.1 - before.1, (lifted.y - rect.y) as f32);
    }
}

/// looks10 round 3 (the judges' low): the head kept off the caret's cell
/// still touched it. Glided flush beside the caret it laid its whiskers on
/// it (`split.bfly`), and lifted at the right margin it sat a pixel over
/// the caret's top, perched on it (`burst.t0`). The head now keeps a ring
/// of air round the caret's cell, the same share of its lead it keeps
/// beside it, and a head that only grazes the ring steps out of it by the
/// least move, never across the caret. Control: `HEAD_CARET_RING_OFF`
/// keeps only the cell, as round 2 did.
#[test]
fn the_flying_head_keeps_air_round_the_caret() {
    let g = geom20();
    let (cw, ch) = (i32::from(g.cell_w), i32::from(g.cell_h));
    let gap = WordDecorations::kitty_cursor_lead_px(g.cell_w) / HEAD_CARET_GAP_DEN;
    assert!(gap >= 2, "a ring of {gap} px");
    let t0 = Instant::now();
    // Row 3 is lettered from column 0 up to the caret.
    let ink_to = |end: u16| move |row: u16, col: u16| Some(row == 3 && col < end);
    // The air between the caret's cell and the head, along x and along y
    // (negative where they overlap on that axis).
    let air = |rect: CatFootprint, caret: (u16, u16)| -> (i32, i32) {
        let (c0, c1) = (i32::from(caret.1) * cw, (i32::from(caret.1) + 1) * cw);
        let (r0, r1) = (i32::from(caret.0) * ch, (i32::from(caret.0) + 1) * ch);
        let (x1, y1) = (rect.x + i32::from(rect.w), rect.y + i32::from(rect.h));
        ((rect.x - c1).max(c0 - x1), (rect.y - r1).max(r0 - y1))
    };
    // The head drawn at `at` (None: where the follower put it) for the
    // caret, given its rect and the caret cell's left, right and top
    // edges, then kept off; the head before and after.
    type At = fn(CatFootprint, (i32, i32, i32)) -> (i32, i32);
    let keep = |caret: (u16, u16), at: Option<At>, ring: bool| {
        HEAD_CARET_RING_OFF.with(|c| c.set(!ring));
        let mut wd = WordDecorations::default();
        let mut placement = wd
            .prepare_kitty_cursor_placement(
                still_layout(g, caret),
                settled_at(t0),
                crate::kitty_cursor::CatPose::STILL,
            )
            .expect("placeable companion");
        if let (Some(at), Some(draw)) = (at, placement.drawn.as_mut()) {
            let cell = (
                i32::from(caret.1) * cw,
                (i32::from(caret.1) + 1) * cw,
                i32::from(caret.0) * ch,
            );
            let (x, y) = at(draw.rect, cell);
            draw.rect.x = x;
            draw.rect.y = y;
        }
        let before = placement.drawn_footprint().expect("drawn");
        placement.keep_off_letters(g, caret, ink_to(caret.1));
        let after = placement.drawn_footprint().expect("drawn");
        HEAD_CARET_RING_OFF.with(|c| c.set(false));
        (before, after)
    };
    let caret = (3, 5);
    // Flush beside the caret, astride its row's top.
    let flush: At = |r, (_, right, top)| (right, top - i32::from(r.h) / 2);
    let (before, kept) = keep(caret, Some(flush), true);
    assert_eq!(
        air(before, caret).0,
        0,
        "the head starts flush beside the caret: {before:?}"
    );
    assert_eq!(
        air(kept, caret).0,
        gap,
        "stepped out beside it, not lifted: {kept:?}"
    );
    assert_eq!(kept.y, before.y);
    let (_, glued) = keep(caret, Some(flush), false);
    assert_eq!(
        air(glued, caret).0,
        0,
        "control: round 2 leaves it on the caret's side"
    );
    // A pixel over the caret's top, straight over it.
    let perched: At = |r, (left, right, top)| {
        (
            (left + right - i32::from(r.w)) / 2,
            top - i32::from(r.h) - 1,
        )
    };
    let (before, kept) = keep(caret, Some(perched), true);
    assert!(before.y >= 0 && air(before, caret).1 == 1, "{before:?}");
    assert_eq!(
        kept.x, before.x,
        "a grazing head is not thrown across the caret"
    );
    assert_eq!(air(kept, caret).1, gap + 1, "{kept:?}");
    let (_, sat) = keep(caret, Some(perched), false);
    assert_eq!(air(sat, caret).1, 1, "control: round 2 leaves it perched");
    // The right margin: the follower's seat lies on the caret; there is no
    // room ahead, so it is lifted, now over the ring.
    let caret = (3, g.cols - 1);
    let (before, lifted) = keep(caret, None, true);
    assert!(
        air(before, caret).0 < 0 && air(before, caret).1 < 0,
        "{before:?}"
    );
    assert_eq!(lifted.x, before.x, "lifted straight up");
    assert_eq!(air(lifted, caret).1, gap + 1, "{lifted:?}");
    let (_, low) = keep(caret, None, false);
    assert_eq!(
        air(low, caret).1,
        1,
        "control: round 2 lifts it a pixel over the caret"
    );
}

/// A HEAD A BOB LAYS ON A LETTERED ROW STEPS BACK OFF IT (composed r2's
/// judges: Nyan rainbow's earned head at fold rest sat beside the caret
/// under a full line at `f1100` and `f1400` and over the line at `f1250`,
/// one capture in three, a hop the companion laws call blinking). Its
/// bob put the top of its box a pixel into the lettered row above, and
/// the keep-off lifted it clear over the whole row; it now steps back off
/// by the least vertical move, within the caret ring's width. The fold:
/// row 3 lettered across, the caret on row 4 after two letters, the head
/// beside it on row 4, its top 1 to 3 px into row 3. Control:
/// `HEAD_GRAZE_LIFTS` throws it over row 3.
#[test]
fn a_head_a_bob_lays_on_a_lettered_row_steps_back_off_it() {
    let g = geom20();
    let ch = i32::from(g.cell_h);
    let gap = WordDecorations::kitty_cursor_lead_px(g.cell_w) / HEAD_CARET_GAP_DEN;
    let caret = (4, 2);
    let ink = |row: u16, col: u16| Some(row == 3 || (row == 4 && col < 2));
    let t0 = Instant::now();
    let kept = |rise: i32, lifts: bool| -> (CatFootprint, CatFootprint) {
        HEAD_GRAZE_LIFTS.with(|c| c.set(lifts));
        let mut wd = WordDecorations::default();
        let mut placement = wd
            .prepare_kitty_cursor_placement(
                still_layout(g, caret),
                settled_at(t0),
                crate::kitty_cursor::CatPose::STILL,
            )
            .expect("placeable companion");
        if let Some(draw) = placement.drawn.as_mut() {
            // Its top `rise` px into row 3.
            draw.rect.y = 4 * ch - rise;
        }
        let before = placement.drawn_footprint().expect("drawn");
        placement.keep_off_letters(g, caret, ink);
        let after = placement.drawn_footprint().expect("drawn");
        HEAD_GRAZE_LIFTS.with(|c| c.set(false));
        (before, after)
    };
    for rise in 1..=gap.min(3) {
        let (before, after) = kept(rise, false);
        assert!(
            before.x >= 3 * i32::from(g.cell_w),
            "fixture: the head sits beside the caret, off its ring: {before:?}"
        );
        assert_eq!(after.x, before.x, "a {rise} px graze: {after:?}");
        assert_eq!(
            after.y,
            4 * ch,
            "a {rise} px graze steps down off row 3, onto its own row's top: {after:?}"
        );
        let (_, lifted) = kept(rise, true);
        assert!(
            lifted.y + i32::from(lifted.h) <= 3 * ch,
            "control: a {rise} px graze lifts the head over row 3: {lifted:?}"
        );
    }
}

#[test]
fn settled_squash_scales_symmetrically_around_sprite_centre_without_follower_lag() {
    let g = geom20();
    let cursor = (3, 5);
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();

    let emit_pose = |wd: &mut WordDecorations, pose: crate::kitty_cursor::CatPose, at| {
        let layout = still_layout(g, cursor);
        let before = (
            wd.kitty_cursor_placement.settle,
            wd.kitty_cursor_placement.last_sprite,
            wd.kitty_cursor_placement.last_emitted_at,
        );
        let placement = wd
            .prepare_kitty_cursor_placement(layout, settled_at(at), pose)
            .expect("placeable companion");
        if before.2.is_some() {
            assert_eq!(
                (
                    wd.kitty_cursor_placement.settle,
                    wd.kitty_cursor_placement.last_sprite,
                    wd.kitty_cursor_placement.last_emitted_at
                ),
                before
            );
        }
        let mut free = Vec::new();
        wd.kitty_cursor_at_placement(
            KittyCursorFrame {
                pose,
                ..still_frame(g, cursor)
            },
            placement,
            &mut free,
        )
        .expect("companion emitted");
        let body = free.last().expect("body emitted").clone();
        assert_eq!(
            placement.drawn_footprint(),
            Some(CatFootprint {
                x: body.x,
                y: body.y,
                w: body.w,
                h: body.h,
            })
        );
        assert_eq!(
            (body.art.w(), body.art.h()),
            (placement.sample.w, placement.sample.h),
            "pose scales the destination, never the natural tile"
        );
        body
    };

    let natural = emit_pose(&mut wd, crate::kitty_cursor::CatPose::STILL, t0);
    let nat_cx = natural.x + i32::from(natural.w) / 2;
    let nat_cy = natural.y + i32::from(natural.h) / 2;

    let squashed = emit_pose(
        &mut wd,
        crate::kitty_cursor::CatPose {
            scale_x: 1.20,
            scale_y: 0.80,
            ..crate::kitty_cursor::CatPose::STILL
        },
        t0 + Duration::from_millis(16),
    );
    assert!(squashed.w > natural.w && squashed.h < natural.h);
    let sq_cx = squashed.x + i32::from(squashed.w) / 2;
    let sq_cy = squashed.y + i32::from(squashed.h) / 2;
    assert_eq!(
        (sq_cx, sq_cy),
        (nat_cx, nat_cy),
        "centre-scaling offset bypasses PLACE_TAU follower lag so impact stays centred"
    );

    let leaning = emit_pose(
        &mut wd,
        crate::kitty_cursor::CatPose {
            lead: 0.75,
            ..crate::kitty_cursor::CatPose::STILL
        },
        t0 + Duration::from_millis(32),
    );
    assert!(
        leaning.x > natural.x,
        "the prepared draw includes the pose's lead"
    );

    let at = t0 + Duration::from_millis(48);
    let before = (
        wd.kitty_cursor_placement.settle,
        wd.kitty_cursor_placement.last_sprite,
        wd.kitty_cursor_placement.last_emitted_at,
    );
    let layout = still_layout(g, (cursor.0, cursor.1 + 1));
    let placement = wd
        .prepare_kitty_cursor_placement(layout, settled_at(at), crate::kitty_cursor::CatPose::STILL)
        .expect("next placement");
    wd.begin_host_frame();
    assert!(wd.claim_companion_body());
    let mut free = Vec::new();
    assert!(
        wd.kitty_cursor_at_placement(still_frame(g, layout.cursor), placement, &mut free,)
            .is_none()
    );
    assert!(free.is_empty());
    assert_eq!(
        (
            wd.kitty_cursor_placement.settle,
            wd.kitty_cursor_placement.last_sprite,
            wd.kitty_cursor_placement.last_emitted_at
        ),
        before
    );
    wd.begin_host_frame();
    assert!(
        wd.kitty_cursor_at_placement(still_frame(g, layout.cursor), placement, &mut free,)
            .is_some(),
        "the same prepared placement can actually emit after admission"
    );

    let layout = still_layout(g, (cursor.0, cursor.1 + 2));
    let placement = wd
        .prepare_kitty_cursor_placement(
            layout,
            settled_at(t0 + Duration::from_millis(64)),
            crate::kitty_cursor::CatPose::STILL,
        )
        .expect("cold-palette placement");
    let before = (
        wd.kitty_cursor_placement.settle,
        wd.kitty_cursor_placement.last_sprite,
        wd.kitty_cursor_placement.last_emitted_at,
    );
    let cold_frame = || {
        let mut frame = still_frame(g, layout.cursor);
        frame.colors.accent ^= 1;
        frame
    };
    wd.begin_host_frame();
    wd.begin_baker_frame(g, true);
    let rgba = [255u8; 4 * 4 * 4];
    assert!(wd.cat_baker.host_tile(0xAA01, 4, 4, &rgba).is_some());
    assert!(wd.cat_baker.host_tile(0xAA02, 4, 4, &rgba).is_some());
    free.clear();
    assert!(
        wd.kitty_cursor_at_placement(cold_frame(), placement, &mut free)
            .is_none()
    );
    assert!(free.is_empty());
    assert_eq!(
        (
            wd.kitty_cursor_placement.settle,
            wd.kitty_cursor_placement.last_sprite,
            wd.kitty_cursor_placement.last_emitted_at
        ),
        before
    );
    wd.begin_host_frame();
    assert!(
        wd.kitty_cursor_at_placement(cold_frame(), placement, &mut free)
            .is_some()
    );
    let body = free.last().expect("cold tile admitted on the next frame");
    assert_eq!(
        placement.drawn_footprint(),
        Some(CatFootprint {
            x: body.x,
            y: body.y,
            w: body.w,
            h: body.h,
        })
    );
}

#[test]
fn expression_width_swap_preserves_reverse_fold_and_margin_glide() {
    use crate::kitty_cursor::{CatFoldDirection, CatFoldFrame, CursorCatPlacementFrame};

    let g = geom20();
    let t0 = Instant::now();
    let mut wd = WordDecorations::default();
    let wide_look = KittyLook {
        variant: CatGlyphId::SpecStretch,
        ..KittyLook::default()
    };
    let startled_look = KittyLook {
        variant: CatGlyphId::S121,
        ..KittyLook::default()
    };
    let emit_look = |wd: &mut WordDecorations,
                     cursor: (u16, u16),
                     look: KittyLook,
                     motion: CursorCatPlacementFrame| {
        let layout = KittyCursorLayout {
            look,
            ..still_layout(g, cursor)
        };
        let placement = wd
            .prepare_kitty_cursor_placement(layout, motion, crate::kitty_cursor::CatPose::STILL)
            .expect("placeable companion");
        let mut free = Vec::new();
        wd.kitty_cursor_at_placement(
            KittyCursorFrame {
                look,
                ..still_frame(g, cursor)
            },
            placement,
            &mut free,
        )
        .expect("companion emitted");
        let body = free.last().expect("body emitted").clone();
        assert_eq!(
            placement.drawn_footprint(),
            Some(CatFootprint {
                x: body.x,
                y: body.y,
                w: body.w,
                h: body.h,
            }),
            "the reserved fold/glide rectangle is the emitted body"
        );
        (placement, body)
    };

    let (_, seeded) = emit_look(&mut wd, (3, 0), wide_look, settled_at(t0));
    let fold_at = t0 + Duration::from_millis(16);
    let (fold_place, fold_launch) = emit_look(
        &mut wd,
        (2, g.cols - 1),
        startled_look,
        CursorCatPlacementFrame {
            sampled_at: fold_at,
            fold: Some(CatFoldFrame {
                started: fold_at,
                direction: CatFoldDirection::Reverse,
                progress: 0.0,
            }),
            facing_left: true,
        },
    );
    assert_ne!(
        seeded.w, fold_launch.w,
        "expression swap changes natural width"
    );
    assert!(
        fold_place.in_motion(),
        "expression width swap on Backspace must not cancel the reverse fold"
    );
    assert_eq!(
        (fold_launch.x, fold_launch.y),
        (seeded.x, seeded.y),
        "reverse fold launches C0-continuously from the last drawn body"
    );

    for direction in [CatFoldDirection::Forward, CatFoldDirection::Reverse] {
        let mut folded = WordDecorations::default();
        let _ = emit_look(&mut folded, (3, 5), wide_look, settled_at(t0));
        for progress in [0.499, 0.5] {
            let (_, body) = emit_look(
                &mut folded,
                (4, 0),
                startled_look,
                CursorCatPlacementFrame {
                    sampled_at: fold_at,
                    fold: Some(CatFoldFrame {
                        started: fold_at,
                        direction,
                        progress,
                    }),
                    facing_left: direction == CatFoldDirection::Reverse,
                },
            );
            let exits_right = (direction == CatFoldDirection::Forward) == (progress < 0.5);
            if exits_right {
                assert!(body.x > i32::from(g.cols) * i32::from(g.cell_w));
            } else {
                assert!(body.x + i32::from(body.w) < 0);
            }
        }
    }

    let settle_t0 = fold_at + crate::kitty_cursor::CURSOR_FOLD_DURATION;
    let (_, narrow_edge) = emit_look(
        &mut wd,
        (2, g.cols - 1),
        startled_look,
        settled_at(settle_t0),
    );
    let raw_wide = wd
        .kitty_cursor_footprint(KittyCursorLayout {
            look: wide_look,
            ..still_layout(g, (2, g.cols - 1))
        })
        .expect("wide footprint");
    assert!(
        (narrow_edge.x - raw_wide.x).abs() >= 8,
        "negative control: right-edge clamp shifts x across the width swap"
    );
    let (_, swapped_edge) = emit_look(
        &mut wd,
        (2, g.cols - 1),
        wide_look,
        settled_at(settle_t0 + Duration::from_millis(16)),
    );
    assert!(
        (swapped_edge.x - narrow_edge.x).abs() <= 6 && swapped_edge.x != raw_wide.x,
        "margin width swap must glide via PLACE_TAU instead of snapping: \
         narrow={} swapped={} raw={}",
        narrow_edge.x,
        swapped_edge.x,
        raw_wide.x
    );
}

#[test]
fn bow_cat_sighting_carries_trait_bow() {
    use crate::kitty_registry::{TRAIT_BOW, accessory_trait_bits};
    let now = Instant::now();
    let bow = (64u64 << 24) | 100;
    assert_eq!(accessory_trait_bits(genome::cat_accessory(bow)), TRAIT_BOW);
    let bare = cat_occ(2, 2, 6, GKEY_ADULT_HEAD, now).genome.magic;
    for (label, magic, bit) in [("bow", bow, TRAIT_BOW), ("bare", bare, 0)] {
        let mut o = cat_occ(2, 2, 6, GKEY_ADULT_HEAD, now);
        o.genome.magic = magic;
        let mut e = Episode::fresh(o.appeared, o.genome, o.seed, o.row, 0);
        e.phase_start = Some(o.appeared);
        let mut wd = WordDecorations {
            occ: vec![o.clone()],
            cols: 20,
            have_scanned: true,
            ..WordDecorations::default()
        };
        wd.persist.insert(o.ident, e);
        let mut s: Vec<KittySighting> = Vec::new();
        for k in 0..4u64 {
            let at = now + Duration::from_millis(DWELL_QUIET_MS + 16 * k);
            tick_cat_at(&mut wd, at, &cfg(), geom20(), None, true);
            s.extend(wd.drain_kitty_sightings());
        }
        assert_eq!(s.len(), 1, "{label}: one sighting");
        assert_eq!(
            s[0].traits & TRAIT_BOW,
            bit,
            "{label}: the accessory rides the sighting trait bits, got {:#010b}",
            s[0].traits
        );
    }
}

#[test]
fn typed_curse_birth_records_one_bonk_cue() {
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let mut term = Terminal::new(4, 32);
    let mut wd = WordDecorations::default();
    term.process(b"fuck");
    wd.note_typed_edit(t0, None);
    wd.rescan(&term, 4, 32, &lex, &c, 1, t0);
    let mut out = Vec::new();
    tick_deco(&mut wd, t0, &c, &mut out);
    let cues: Vec<CurseCue> = wd.drain_curse_cues().collect();
    assert_eq!(cues.len(), 1, "one typed curse ⇒ one cue");
    assert_eq!(cues[0].kind, CurseCueKind::Typed);
    assert_eq!(
        (cues[0].row, cues[0].col),
        (0, 0),
        "the cue carries the word's lead cell for stereo pan"
    );
    tick_deco(&mut wd, t0 + Duration::from_millis(16), &c, &mut out);
    assert_eq!(wd.drain_curse_cues().count(), 0);
    wd.rescan(&term, 4, 32, &lex, &c, 2, t0 + Duration::from_millis(32));
    tick_deco(&mut wd, t0 + Duration::from_millis(48), &c, &mut out);
    assert_eq!(wd.drain_curse_cues().count(), 0);
}

#[test]
fn multilingual_typed_curses_bonk_zh_ja_ko_en() {
    let lex = Lexicon::with_languages(&["all"]);
    let c = cfg();
    let t0 = Instant::now();
    for word in ["fuck", "他妈的", "くそ", "씨발"] {
        let mut term = Terminal::new(2, 32);
        term.process(word.as_bytes());
        let mut wd = WordDecorations::default();
        wd.note_typed_edit(t0, None);
        wd.rescan(&term, 2, 32, &lex, &c, 1, t0);
        let mut out = Vec::new();
        tick_deco(&mut wd, t0, &c, &mut out);
        let cues: Vec<CurseCue> = wd.drain_curse_cues().collect();
        assert_eq!(cues.len(), 1, "{word:?} must record one typed cue");
        assert_eq!(cues[0].kind, CurseCueKind::Typed, "{word:?}");
    }
}

#[test]
fn ignore_lists_suppress_the_bonk_with_the_decoration() {
    let lex = Lexicon::with_languages(&["all"]);
    let t0 = Instant::now();
    for (word, ignored) in [("fuck", "fuck"), ("씨발", "씨발")] {
        let mut c = cfg();
        c.ignore.insert(ignored.to_string());
        let mut term = Terminal::new(2, 32);
        term.process(word.as_bytes());
        let mut wd = WordDecorations::default();
        wd.rescan(&term, 2, 32, &lex, &c, 1, t0);
        assert!(
            wd.occ.iter().all(|o| o.class != Class::Profanity),
            "{word:?} must not decorate under its ignore entry"
        );
        let mut out = Vec::new();
        tick_deco(&mut wd, t0, &c, &mut out);
        assert_eq!(
            wd.drain_curse_cues().count(),
            0,
            "{word:?} must not bonk under its ignore entry"
        );
    }
}

#[test]
fn typing_dmg_never_bonks_but_the_marked_vietnamese_form_does() {
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let mut term = Terminal::new(2, 32);
    let mut wd = WordDecorations::default();
    let mut out = Vec::new();
    for (i, byte) in b"dmg".iter().copied().enumerate() {
        term.process(&[byte]);
        wd.note_typed_edit(t0, None);
        wd.rescan(&term, 2, 32, &lex, &c, i as u64 + 1, t0);
        tick_deco(&mut wd, t0 + Duration::from_millis(i as u64), &c, &mut out);
        assert_eq!(
            wd.drain_curse_cues().count(),
            0,
            "prefix {:?} of dmg must never bonk",
            &b"dmg"[..=i]
        );
    }
    let mut term = Terminal::new(2, 32);
    let mut wd = WordDecorations::default();
    term.process("đm".as_bytes());
    wd.note_typed_edit(t0, None);
    wd.rescan(&term, 2, 32, &lex, &c, 1, t0);
    tick_deco(&mut wd, t0, &c, &mut out);
    let cues: Vec<CurseCue> = wd.drain_curse_cues().collect();
    assert_eq!(cues.len(), 1, "typed đm must record one cue");
    assert_eq!(cues[0].kind, CurseCueKind::Typed);
}

#[test]
fn supernova_detonation_cues_at_the_grant_edge_once() {
    let lex = lex();
    let t0 = Instant::now();
    let g = geom20();
    for (profile, c) in [("supernova", cfg_rainbow(100)), ("nova", cfg_nova())] {
        let mut term = Terminal::new(6, 20);
        term.process(b"oh fuck");
        let mut wd = WordDecorations::default();
        wd.note_typed_edit(t0, None);
        wd.rescan(&term, 6, 20, &lex, &c, 1, t0);
        tick_nova(&mut wd, t0, &c, g, None, None);
        let cues: Vec<CurseCue> = wd.drain_curse_cues().collect();
        assert!(
            cues.iter().any(|q| q.kind == CurseCueKind::Typed),
            "{profile}: typed witness cue missing: {cues:?}"
        );
        assert_eq!(
            cues.iter()
                .filter(|q| q.kind == CurseCueKind::Detonated)
                .count(),
            1,
            "{profile}: exactly one detonation cue at the grant: {cues:?}"
        );
        tick_nova(&mut wd, t0 + Duration::from_millis(100), &c, g, None, None);
        assert_eq!(wd.drain_curse_cues().count(), 0, "{profile}");
        let mut term2 = Terminal::new(6, 20);
        term2.process(b"fuck\r\n$ ");
        let mut wd2 = WordDecorations::default();
        wd2.rescan(&term2, 6, 20, &lex, &c, 1, t0);
        tick_nova(&mut wd2, t0, &c, g, None, None);
        let cues2: Vec<CurseCue> = wd2.drain_curse_cues().collect();
        assert!(
            cues2.iter().all(|q| q.kind == CurseCueKind::Detonated),
            "{profile}: output content must never carry the typed kind: {cues2:?}"
        );
        assert_eq!(
            cues2.len(),
            1,
            "{profile}: the output detonation still cues: {cues2:?}"
        );
    }
}

#[test]
fn curse_cues_clear_at_tick_start_and_on_state_drop() {
    let lex = lex();
    let c = cfg();
    let t0 = Instant::now();
    let mut term = Terminal::new(4, 32);
    let mut wd = WordDecorations::default();
    term.process(b"fuck");
    wd.note_typed_edit(t0, None);
    wd.rescan(&term, 4, 32, &lex, &c, 1, t0);
    let mut out = Vec::new();
    tick_deco(&mut wd, t0, &c, &mut out);
    assert_eq!(wd.curse_cues.len(), 1);
    tick_deco(&mut wd, t0 + Duration::from_millis(16), &c, &mut out);
    assert_eq!(
        wd.drain_curse_cues().count(),
        0,
        "an undrained frame's cues never accumulate (tick-start clear)"
    );
    let mut wd2 = WordDecorations::default();
    wd2.note_typed_edit(t0, None);
    wd2.rescan(&term, 4, 32, &lex, &c, 1, t0);
    tick_deco(&mut wd2, t0, &c, &mut out);
    assert_eq!(wd2.curse_cues.len(), 1);
    wd2.reset();
    assert_eq!(
        wd2.drain_curse_cues().count(),
        0,
        "reset() must drop pending cues with the state"
    );
}

mod pane_binding {
    use super::*;

    fn rescan_pane(
        wd: &mut WordDecorations,
        text: &str,
        rows: usize,
        cols: usize,
        c: &DecoConfig,
        epoch: u64,
        now: Instant,
    ) {
        let mut term = Terminal::new(rows as u16, cols as u16);
        term.process(text.as_bytes());
        let mut snap = aterm_core::render::RenderInput::default();
        term.cell_frame_into(&mut snap, rows, cols);
        let bg = snap
            .cells
            .iter()
            .find_map(|line| line.first())
            .map_or(0, |cell| rgb3_to_u32(cell.bg));
        let geom = EffectGeom {
            cell_w: 10,
            cell_h: 20,
            rows: rows as u16,
            cols: cols as u16,
        };
        wd.rescan_from_cells_with_geom_at_cursor(
            &snap.cells,
            &snap.line_sizes,
            rows,
            cols,
            &lex(),
            c,
            epoch,
            now,
            geom,
            bg,
            None,
        );
    }

    fn feline_episode(wd: &WordDecorations) -> Option<&Episode> {
        let ident = wd
            .occ
            .iter()
            .find(|o| o.class == Class::Feline)
            .map(|o| o.ident)?;
        wd.persist.get(&ident)
    }

    #[test]
    fn bound_panes_keep_independent_damage_epochs() {
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        wd.bind_pane(1, (0, 0));
        assert!(wd.needs_rescan(1), "a pane's first frame always scans");
        rescan_pane(&mut wd, "\r\nhello kitty friend\r\n", 6, 20, &c, 1, t0);
        assert!(!wd.needs_rescan(1), "and not twice at the same epoch");

        wd.bind_pane(2, (0, 0));
        assert!(
            wd.needs_rescan(1),
            "pane 2 has scanned NOTHING — a shared last_epoch would starve \
             it of its first scan for as long as pane 1 stays quiet"
        );
        rescan_pane(&mut wd, "\r\nhello kitty friend\r\n", 6, 20, &c, 1, t0);

        wd.bind_pane(1, (0, 0));
        assert!(
            !wd.needs_rescan(1),
            "pane 1's own epoch survived pane 2's scan"
        );
    }

    #[test]
    fn unequal_pane_widths_do_not_pin_born_settled() {
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let text = "\r\nhello kitty friend\r\n";
        for round in 0..4u64 {
            wd.bind_pane(1, (0, 0));
            rescan_pane(&mut wd, text, 6, 40, &c, round + 1, t0);
            wd.bind_pane(2, (400, 0));
            rescan_pane(&mut wd, text, 6, 39, &c, round + 1, t0);
        }
        wd.bind_pane(1, (0, 0));
        assert!(
            wd.settle_until.is_none(),
            "pane 1's width never changed — the resize-settle window must \
             be shut, not re-armed by its sibling's different width"
        );
        let ep = feline_episode(&wd).expect("pane 1 has a feline episode");
        assert!(
            !ep.born_settled,
            "a stable pane still PLAYS its entrance in a split"
        );
    }

    #[test]
    fn identical_words_in_two_panes_each_get_their_own_cat() {
        let c = cfg();
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(400);
        let text = "\r\nhello kitty friend\r\n";
        let mut wd = WordDecorations::default();

        wd.bind_pane(1, (0, 0));
        rescan_pane(&mut wd, text, 6, 20, &c, 1, t0);
        assert_eq!(
            feline_episode(&wd).expect("pane 1 birth").appeared,
            t0,
            "pane 1's cat is born when pane 1 first saw the word"
        );

        wd.bind_pane(2, (200, 0));
        rescan_pane(&mut wd, text, 6, 20, &c, 1, t1);
        assert_eq!(
            feline_episode(&wd).expect("pane 2 birth").appeared,
            t1,
            "pane 2's identical word is its OWN birth — a shared persist map \
             would hand it pane 1's already-running (or already-spent) episode"
        );

        wd.bind_pane(1, (0, 0));
        assert_eq!(
            feline_episode(&wd).expect("pane 1 survives").appeared,
            t0,
            "and pane 1's episode is untouched by its sibling"
        );
    }

    #[test]
    fn pane_ticks_do_not_inflate_rescan_seq() {
        let c = cfg();
        let t0 = Instant::now();
        let text = "\r\nhello kitty friend\r\n";
        let mut wd = WordDecorations::default();
        for pane in 1..=3u64 {
            wd.bind_pane(pane, (0, 0));
            rescan_pane(&mut wd, text, 6, 20, &c, 1, t0);
        }
        wd.bind_pane(1, (0, 0));
        assert_eq!(
            wd.rescan_seq, 1,
            "one pane, one scan, one sequence step — siblings must not \
             consume pane 1's REKEY_MAX_SEQ_GAP budget"
        );
    }

    #[test]
    fn one_host_frame_bakes_at_most_two_tiles_across_panes() {
        let c = cfg();
        let g = geom20();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        for pane in 1..=3u64 {
            wd.bind_pane(pane, (i32::try_from(pane).expect("small") * 200, 0));
            rescan_pane(&mut wd, "\r\nhello kitty friend\r\n", 6, 20, &c, 1, t0);
        }
        let t1 = t0 + Duration::from_millis(600);
        wd.begin_host_frame();
        let clock0 = wd.cat_baker.frame_clock();
        let mut cats = 0usize;
        for pane in 1..=3u64 {
            wd.bind_pane(pane, (i32::try_from(pane).expect("small") * 200, 0));
            let (free, _, _) = tick_cat(&mut wd, t1, &c, g);
            cats += free.len();
        }
        assert!(cats > 0, "non-vacuous: the panes really did emit cats");
        assert_eq!(
            wd.cat_baker.frame_clock(),
            clock0 + 1,
            "ONE baker prologue — so ONE bake budget — for the whole frame"
        );
    }

    #[test]
    fn flash_limiter_stays_window_wide_across_panes() {
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        let grant = |wd: &mut WordDecorations, owner: u64| {
            let scope = wd.scan_scope();
            grant_pane_ignition(
                &mut wd.ignitions,
                scope,
                wd.pane_px,
                owner,
                t0,
                (100, 100),
                88.0,
            )
        };
        wd.bind_pane(1, (0, 0));
        let a = grant(&mut wd, 11).expect("capacity");
        wd.bind_pane(2, (400, 0));
        let b = grant(&mut wd, 22).expect("capacity");
        assert_eq!(
            (a, b),
            (t0, t0),
            "same pane-local centre, DIFFERENT panes — the overlap test \
             must compare window pixels or it tightens the cap to one"
        );
        wd.bind_pane(3, (800, 0));
        let c = grant(&mut wd, 33).expect("capacity");
        assert_eq!(
            c,
            t0 + IGNITION_WINDOW,
            "and the rolling-second cap of two is counted over ALL panes"
        );
    }

    #[test]
    fn a_panes_pending_ignition_survives_another_panes_tick() {
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        wd.bind_pane(1, (0, 0));
        for (owner, cx) in [(1u64, 100i32), (2, 900), (3, 1700)] {
            let scope = wd.scan_scope();
            grant_pane_ignition(
                &mut wd.ignitions,
                scope,
                wd.pane_px,
                owner,
                t0,
                (cx, 100),
                0.0,
            );
        }
        let pending = wd
            .ignitions
            .iter()
            .find(|r| r.start > t0)
            .map(|r| r.start)
            .expect("the third grant is deferred past the rolling window");

        wd.bind_pane(2, (400, 0));
        wd.prune_ignitions(t0 + Duration::from_millis(10));
        assert!(
            wd.ignitions
                .iter()
                .any(|r| r.pane == Some(1) && r.start == pending),
            "pane 2's tick must not cancel pane 1's queued nova"
        );
    }

    #[test]
    fn retain_panes_evicts_departed_panes() {
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        for pane in 1..=3u64 {
            wd.bind_pane(pane, (0, 0));
            rescan_pane(&mut wd, "\r\nhello kitty friend\r\n", 6, 20, &c, 1, t0);
        }
        wd.retain_panes(|s| s == 1);
        assert_eq!(
            wd.parked.keys().copied().collect::<Vec<_>>(),
            vec![1],
            "pane 2's parked state left with pane 2"
        );
        assert_eq!(wd.bound, None, "and pane 3's live binding with it");
        assert!(wd.occ.is_empty(), "a departed pane leaves no occurrences");
        wd.bind_pane(1, (0, 0));
        assert!(
            feline_episode(&wd).is_some(),
            "the still-visible pane kept its episodes"
        );
        assert!(
            !wd.needs_rescan(1),
            "and its scan — a survivor is not re-scanned because a sibling closed"
        );
    }

    #[test]
    fn retain_panes_evicts_departed_unsplit_session_and_restores_survivor() {
        let c = cfg();
        let t0 = Instant::now();
        let mut wd = WordDecorations::default();
        for tab in [10u64, 20] {
            wd.set_scan_session(Some(tab));
            rescan_pane(&mut wd, "\r\nhello kitty friend\r\n", 6, 20, &c, 1, t0);
        }
        assert_eq!(wd.scan_session, Some(20));
        assert!(wd.parked.contains_key(&10));

        wd.retain_panes(|s| s == 10);
        assert_eq!(
            wd.scan_session, None,
            "departed unsplit session is cleared from scan_session"
        );
        assert!(
            wd.occ.is_empty() && wd.persist.is_empty(),
            "departed unsplit session's live state is evicted"
        );

        wd.set_scan_session(Some(10));
        assert!(
            !wd.parked.contains_key(&20),
            "closed tab 20 must not be re-parked when switching to survivor 10"
        );
        assert!(
            feline_episode(&wd).is_some(),
            "surviving tab 10's parked episodes are restored into the live fields"
        );
    }
}
