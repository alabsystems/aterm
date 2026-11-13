// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::kitty_cursor::{CatExit, CatFrame, CatPose, CatReaction};
use aterm_core::terminal::UnderlineStyle;

#[test]
fn single_and_composed_companions_share_continuous_shed_admission_and_alpha() {
    for (envelope, expected) in [(1.0, 200), (0.75, 150), (0.5, 100), (0.25, 50)] {
        assert!(
            shed_companion_presentable(true, envelope),
            "both render paths retain companion custody at {envelope}"
        );
        assert_eq!(shed_companion_alpha(200, envelope), expected);
    }
    assert!(!shed_companion_presentable(true, 0.0));
    assert!(!shed_companion_presentable(false, 1.0));
    assert_eq!(shed_companion_alpha(200, 0.0), 0);
    assert_eq!(shed_companion_alpha(200, f32::NAN), 0);
    assert!(shed_envelope_transitioning(true, 0.5));
    assert!(!shed_envelope_transitioning(true, 0.0));
    assert!(shed_envelope_transitioning(false, 0.0));
    assert!(!shed_envelope_transitioning(false, 1.0));
    assert!(!shed_envelope_transitioning(false, f32::NAN));
}

#[test]
fn hidden_caret_keeps_exact_resident_body_custody_without_a_stale_cell() {
    let body = (37, 66, 48, 82);
    let fading = cursor_companion_on_glass(CompanionDuty::Pet, None, None, Some(body))
        .expect("a visible fading pet still owns glass");
    assert_eq!(fading.cell, None, "DECTCEM supplies no honest caret cell");
    assert_eq!(
        fading.body_px,
        Some(body),
        "the emitted body is the yield box"
    );
    assert!(
        !fading.guards_caret,
        "the pet stands where its brain walked it and has no caret claim"
    );

    assert_eq!(
        cursor_companion_duty(false, 255, None),
        CompanionDuty::Idle,
        "the flying head may not guess a hidden cursor anchor"
    );
    assert!(
        cursor_companion_on_glass(CompanionDuty::Idle, None, None, None).is_none(),
        "an idle frame owns no glass"
    );

    let head = (26, 85, 73, 123);
    let duty = cursor_companion_duty(false, 255, Some((4, 9)));
    assert_eq!(duty, CompanionDuty::FlyingHead { cell: (4, 9) });
    let flying = cursor_companion_on_glass(duty, Some((4, 9)), Some(head), None)
        .expect("a visible classic kitty owns its caret");
    assert_eq!(flying.cell, Some((4, 9)));
    assert_eq!(flying.body_px, Some(head), "the head's REAL drawn rect");
    assert!(
        flying.guards_caret,
        "the escorting head guards the caret cell as well as its sprite"
    );
    let degraded = cursor_companion_on_glass(duty, Some((4, 9)), None, None)
        .expect("an unresolved footprint still claims the caret band");
    assert_eq!(degraded.body_px, None);
    assert!(degraded.guards_caret);

    let duty = cursor_companion_duty(true, 255, Some((4, 9)));
    assert_eq!(duty, CompanionDuty::Pet);
    let on_glass = cursor_companion_on_glass(duty, Some((4, 9)), Some(head), Some(body))
        .expect("the pet owns the frame");
    assert_eq!(on_glass.body_px, Some(body));
    assert!(!on_glass.guards_caret);
}

#[test]
fn pre_tick_setup_binds_focused_ink_before_read_and_applies_species() {
    use crate::word_decorations::DecoConfig;
    use aterm_core::terminal::Terminal;
    use aterm_lexicon::Lexicon;

    let cfg = DecoConfig::default();
    let lexicon = Lexicon::with_languages(&["en"]);
    let now = Instant::now();
    let mut decorations = WordDecorations::default();

    let mut inked = Terminal::new(6, 40);
    inked.process(b"\x1b[3;1Hxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx");
    decorations.bind_pane(11, (0, 0));
    decorations.rescan(&inked, 6, 40, &lexicon, &cfg, 1, now);
    assert_eq!(decorations.pet_ink().1, Some(2));

    let blank = Terminal::new(6, 40);
    decorations.bind_pane(22, (400, 0));
    decorations.rescan(&blank, 6, 40, &lexicon, &cfg, 1, now);
    assert_eq!(
        decorations.pet_ink().1,
        None,
        "negative control: the live slot belongs to the final blank pane"
    );

    let sense = PetSense {
        caret_drawn: true,
        now,
        caret: Some((2, 10)),
        rows: 6,
        cols: 40,
        cell_w: 10,
        cell_h: 20,
        reduced_motion: false,
        output_burst: false,
        pointer: None,
        wrapped: false,
    };
    let mut focused_pet = PetBrain::default();
    prepare_resident_pet_tick(
        &mut decorations,
        &mut focused_pet,
        PetSpecies::Dog,
        Some((11, (0, 0))),
    );
    let focused = focused_pet.tick_static_capture(sense);
    assert_eq!(focused_pet.species(), PetSpecies::Dog);
    assert_eq!(
        focused.row, 3.0,
        "focused-pane ink moves a cold pet onto the blank row below"
    );

    let mut sibling_pet = PetBrain::default();
    prepare_resident_pet_tick(
        &mut decorations,
        &mut sibling_pet,
        PetSpecies::Cat,
        Some((22, (400, 0))),
    );
    let sibling = sibling_pet.tick_static_capture(sense);
    assert_eq!(
        sibling.row, 2.0,
        "negative control: the sibling's blank map leaves the same pet on the caret row"
    );
}

const fn cell(ch: char, fg: [u8; 3], bg: [u8; 3]) -> RenderCell {
    RenderCell {
        ch,
        fg,
        bg,
        wide: false,
        emoji_presentation: false,
        text_presentation: false,
        bold: false,
        italic: false,
        underline: UnderlineStyle::None,
        strikethrough: false,
        overline: false,
        underline_color: None,
        overline_color: None,
        script: aterm_core::terminal::Script::Normal,
    }
}

#[test]
fn cursor_companion_samples_its_actual_multiline_footprint() {
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 2,
        cols: 3,
    };
    let footprint = CatFootprint {
        x: 0,
        y: 0,
        w: 30,
        h: 40,
    };
    let make = |neighbor, top_bg, bottom_bg| {
        vec![
            vec![cell('x', neighbor, top_bg); 3],
            vec![cell(' ', [240, 240, 240], bottom_bg); 3],
        ]
    };
    let key = |neighbor, top_bg, bottom_bg| {
        let cells = make(neighbor, top_bg, bottom_bg);
        cursor_cat_color_key(&cells, geom, footprint, 0, 0x00FF_FFFF, 0)
    };
    let red = key([255, 20, 20], [8, 8, 8], [8, 8, 8]);
    let blue = key([20, 80, 255], [8, 8, 8], [8, 8, 8]);
    let light = key([255, 20, 20], [248, 248, 248], [248, 248, 248]);
    let mixed = key([255, 20, 20], [4, 4, 4], [248, 248, 248]);
    assert_ne!(
        red.accent, blue.accent,
        "neighbor text changes the hue family"
    );
    assert_ne!(
        red.background, light.background,
        "backgrounds across the sprite footprint change contrast ink"
    );
    assert_eq!(
        mixed.background, 4,
        "a dark+light footprint must not collapse to its RGB-average band"
    );
}

#[test]
fn cursor_companion_ignores_alternating_backgrounds_outside_its_footprint() {
    let geom = EffectGeom {
        cell_w: 10,
        cell_h: 10,
        rows: 2,
        cols: 4,
    };
    let footprint = CatFootprint {
        x: 20,
        y: 0,
        w: 20,
        h: 20,
    };
    let cells = vec![
        vec![
            cell('x', [255, 0, 0], [255, 255, 255]),
            cell('x', [255, 0, 0], [255, 255, 255]),
            cell('x', [0, 0, 255], [4, 4, 4]),
            cell('x', [0, 0, 255], [4, 4, 4]),
        ];
        2
    ];
    let sampled = cursor_cat_color_key(&cells, geom, footprint, 0, 0, 0);
    assert!(
        sampled.dark(),
        "only the dark right-hand footprint is sampled"
    );
}

#[test]
fn same_session_change_is_a_wrap_and_a_session_switch_never_is() {
    let mut seen = None;
    assert!(!wrap_fact_edge(&mut seen, 7, 41));
    assert!(!wrap_fact_edge(&mut seen, 7, 41));
    assert!(wrap_fact_edge(&mut seen, 7, 42));
    assert!(!wrap_fact_edge(&mut seen, 7, 42));
    assert!(!wrap_fact_edge(&mut seen, 8, 0));
    assert!(wrap_fact_edge(&mut seen, 8, u64::MAX));
    assert!(wrap_fact_edge(&mut seen, 8, 3));
}

#[test]
fn pointer_maps_into_pane_cells_and_dies_at_the_edge() {
    assert_eq!(
        pet_pointer_cell((125.0, 90.0), (20, 40), (10, 20), (80, 24)),
        Some((10.5, 2.5))
    );
    assert_eq!(
        pet_pointer_cell((19.0, 90.0), (20, 40), (10, 20), (80, 24)),
        None
    );
    assert_eq!(
        pet_pointer_cell((20.0 + 800.0, 90.0), (20, 40), (10, 20), (80, 24)),
        None
    );
    assert_eq!(
        pet_pointer_cell((5.0, 5.0), (0, 0), (0, 20), (80, 24)),
        None
    );
}

#[test]
fn typing_echo_never_reads_as_a_burst() {
    assert!(!pet_output_burst(false, true, false, true, false));
    assert!(pet_output_burst(true, false, true, true, false));
    assert!(pet_output_burst(false, true, true, true, false));
    assert!(!pet_output_burst(true, true, true, false, false));
    assert!(!pet_output_burst(false, false, true, true, false));
    assert!(!pet_output_burst(false, true, true, true, true));
    assert!(!pet_output_burst(true, true, true, true, true));
}

#[test]
fn pet_hit_rect_win_offsets_the_body_by_the_effects_origin() {
    assert_eq!(pet_hit_rect_win(None, (7, 9)), None);
    assert_eq!(
        pet_hit_rect_win(Some((10, 30, 40, 60)), (7, 9)),
        Some((17, 37, 49, 69))
    );
    assert_eq!(
        pet_hit_rect_win(Some((0, 20, -12, 8)), (4, 30)),
        Some((4, 24, 18, 38))
    );
}

#[test]
fn pet_mode_summons_exit_plain() {
    let frame = |exit| CatFrame {
        alpha: 128,
        exit,
        fade_out: 0.5,
        look: KittyLook::default(),
        reaction: CatReaction::Cruise,
        discovery: false,
        collection_hello: false,
        bob: 0.0,
        sing: 0.4,
        pose: CatPose::STILL,
    };
    for rolled in [CatExit::StarWink, CatExit::HeartMeow, CatExit::Plain] {
        let mut f = frame(rolled);
        pin_pet_mode_exit(true, &mut f);
        assert_eq!(f.exit, CatExit::Plain, "pet-mode episodes exit Plain");
        let mut f = frame(rolled);
        pin_pet_mode_exit(false, &mut f);
        assert_eq!(f.exit, rolled, "earned flights keep their roll");
    }
}

#[test]
fn the_arrival_mapping_rations_the_theater_by_rung_and_difference() {
    use crate::kitty_registry::{SUFFICIENT_DIFFERENCE, coat_distance};
    use CompanionRung::{Favourite, Launch, Program};
    use PetArrival::{Ceremony, Quiet};
    let far = (0u8..16)
        .find(|&c| coat_distance(3, c) >= SUFFICIENT_DIFFERENCE)
        .expect("some coat is sufficiently different from coat 3");
    for (rung, authorised, worn, incoming, expected) in [
        (Favourite, Ceremony, None, 3, Quiet),
        (Launch, Ceremony, None, 3, Quiet),
        (Program, Quiet, None, 3, Quiet),
        (Program, Ceremony, None, 3, Ceremony),
        (Program, Ceremony, Some((3, 0)), 3, Quiet),
        (Program, Ceremony, Some((3, 0)), far, Ceremony),
    ] {
        assert_eq!(
            pet_arrival_for_sync(rung, authorised, worn, incoming),
            expected,
            "{rung:?} {authorised:?} worn={worn:?} incoming={incoming}"
        );
    }
}
