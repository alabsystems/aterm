// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fresh-ink pop, the typing wake and the `trail` status row.

use super::*;

fn type_one_key(glow: &mut CursorGlow, cfg: &GlowConfig, g: Geom, at: Instant, row: u16, col: u16) {
    let mut after = [' '; 40];
    after[..usize::from(col) + 1].fill('x');
    glow.note_typed_cells(at, 1);
    glow.observe_row(row, col + 1, &after, at);
    glow.tick(Some((row, col + 1)), at, cfg, g, &mut typed_scratch());
}

#[test]
fn a_word_typed_at_human_cadence_earns_the_full_rainbow_ribbon() {
    for ms in [100u64, 150, 250] {
        let g = geom();
        let c = cfg(GlowStyle::RainbowKitty, true);
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.tick(Some((2, 0)), t0, &c, g, &mut typed_scratch());

        const KEYS: u32 = 12;
        for key in 0..KEYS {
            type_one_key(
                &mut glow,
                &c,
                g,
                t0 + Duration::from_millis(u64::from(key) * ms + 1),
                2,
                key as u16,
            );
        }
        let end = t0 + Duration::from_millis(u64::from(KEYS - 1) * ms + 1);
        let tally = glow.admission_tally();
        assert_eq!(
            (tally.licensed, tally.declined),
            (u64::from(KEYS), 0),
            "{ms} ms/key"
        );
        assert_eq!(tally.last_decline_reason, None, "{ms} ms/key");
        assert_eq!(glow.spawns(), u64::from(KEYS), "{ms} ms/key");

        let segments = glow.ribbon_segments();
        assert!(
            segments >= 6,
            "{ms} ms/key: a word leaves a multi-cell ribbon, got {segments}"
        );
        assert!(
            glow.ribbon_hue_bands() >= 2,
            "{ms} ms/key: a word's ribbon spans at least two status bands"
        );
        assert!(glow.is_active(), "{ms} ms/key");
        assert!(
            glow.typing_momentum(end) > 0.25,
            "{ms} ms/key: ordinary typing builds momentum"
        );
    }
}

#[test]
fn unauthored_output_is_declined_and_the_status_verb_shows_it_dark() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((2, 0)), t0, &c, g, &mut typed_scratch());
    let at = t0 + Duration::from_millis(1);
    let mut redrawn = [' '; 40];
    redrawn[..8].fill('#');
    glow.observe_row(2, 1, &redrawn, at);
    glow.tick(Some((2, 1)), at, &c, g, &mut typed_scratch());
    let tally = glow.admission_tally();
    assert_eq!(tally.licensed, 0);
    assert_eq!(tally.declined, 1);
    assert_eq!(
        tally.last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        "the verb names the gate that stopped it"
    );
    assert_eq!(glow.spawns(), 0, "an unlicensed move lays no light");
    assert_eq!(glow.ribbon_segments(), 0);
    assert_eq!(glow.ribbon_hue_bands(), 0);
}

#[test]
fn the_status_row_names_every_gate_and_derives_the_ribbon_verdict() {
    let base = TrailStatus {
        style_raw: "rainbow kitty pet",
        style: Some(GlowStyle::RainbowKitty),
        unknown: false,
        config_enabled: true,
        effective: true,
        focused: true,
        motion_stage: "full",
        motion_mode: "auto",
        shed: 1.0,
        intensity: 0.7,
        sound_seam: true,
        tally: AdmissionTally {
            licensed: 8,
            declined: 1,
            last_decline_reason: Some(CursorGlow::DECLINE_NO_FRESH_HINT),
        },
        spawns: 8,
        ribbon_look: "underline",
        ribbon_segments: 4,
        ribbon_hue_bands: 4,
        ribbon_drawn: 4,
        ribbon_curtain_ms: None,
        field: 0.31,
        sparks: 4,
        momentum: 0.5,
        momentum_display: 0.44,
        momentum_glow: 0.62,
        glow_active: true,
        pet_active: true,
        pet_action: "purr",
        pet_content: 0.42,
        pet_pending: 1,
        pet_focus: "rest",
        pet_reason: "quiet",
        pet_anchor: None,
        pet_event_seq: 0,
        pet_pose: "pet_sit",
        pet_deep: false,
        pet_body: Some((12, 68, 30, 64)),
        head_body: Some((70, 98, 4, 30)),
        cat_active: false,
        block_fill: Some(BlockFill {
            owner: BlockFillOwner::Rainbow,
            fill: 0x00F0_2218,
            base: Some(0x00FF_0000),
        }),
        flow: rk::Flow {
            heat: 0.5,
            combo: 12,
            best: 31,
        },
        inserts: InsertTally::default(),
        in_flight: InFlightTally {
            licensed: 1,
            forgotten: 2,
            credits: 3,
            swallowed_no_echo: 4,
            park_returns: 5,
            park_flushed: 6,
        },
        comet_cells: 7,
        comet_alpha_max: 96,
    };
    let line = base.line();
    for key in [
        "trail style=",
        " resolved=rainbow-kitty",
        " config_enabled=true",
        " effective=true",
        " focused=true",
        " motion=auto",
        " motion_stage=full",
        " shed=1.00",
        " intensity=0.70",
        " licensed=8",
        " declined=1",
        " last_decline_reason=no-fresh-hint",
        " spawns=8",
        " ribbon_active=true",
        " ribbon_look=underline",
        " ribbon_segments=4",
        " ribbon_hue_bands=4",
        " sparks=4",
        " momentum=0.50",
        " momentum_display=0.44",
        " momentum_glow=0.62",
        " flow=0.50",
        " combo=12",
        " combo_best=31",
        " glow_active=true",
        " pet_active=true",
        " pet_action=purr",
        " pet_content=0.420",
        " pet_pending=1",
        " pet_body=12,68,30,64",
        " head_body=70,98,4,30",
        " cat_active=false",
        " block_fill=rainbow",
        " block_fill_rgb=f02218",
        " block_fill_base=ff0000",
        " block_fill_base_from=cursor_color",
        " inserts_delivered=0",
        " inserts_lit=0",
        " inserts_retracted=0",
        " last_insert_cells=0",
        " inflight_licensed=1",
        " inflight_forgotten=2",
        " credits=3",
        " swallowed_no_echo=4",
        " park_returns=5",
        " park_flushed=6",
        " issue=none",
        " comet_cells=7",
        " comet_alpha_max=96",
    ] {
        assert!(line.contains(key), "missing {key:?} in {line}");
    }
    assert!(
        line.contains(r#"style="rainbow kitty pet""#),
        "the raw config string is reported verbatim: {line}"
    );
    assert!(!line.contains('\n'), "the status row is one line: {line}");
    let off = TrailStatus {
        style_raw: "off",
        style: None,
        effective: false,
        intensity: 0.0,
        ..base
    }
    .line();
    assert!(off.contains(" resolved=off "), "{off}");
    let quiet = TrailStatus {
        tally: AdmissionTally::default(),
        ..base
    };
    assert!(quiet.line().contains(" last_decline_reason=none"));
    assert!(base.ribbon_active());
    assert!(
        !TrailStatus {
            ribbon_segments: 1,
            ribbon_hue_bands: 1,
            ribbon_drawn: 4,
            ribbon_curtain_ms: None,
            field: 0.5,
            ..base
        }
        .ribbon_active(),
        "one cell is a glow, not a trail"
    );
    assert!(
        !TrailStatus {
            ribbon_segments: 4,
            ribbon_hue_bands: 1,
            ribbon_drawn: 4,
            ribbon_curtain_ms: None,
            field: 0.5,
            ..base
        }
        .ribbon_active(),
        "four cells in one hue is a bar, not a rainbow"
    );
    let curtain = TrailStatus {
        ribbon_segments: 0,
        ribbon_hue_bands: 0,
        ribbon_drawn: 1065,
        ribbon_curtain_ms: Some(120),
        field: 0.5,
        ..base
    };
    assert!(
        curtain.ribbon_active(),
        "the verb says the band is gone while a curtain is drawing it"
    );
    let row = curtain.line();
    assert!(
        row.contains(" ribbon_drawn=1065") && row.contains(" ribbon_curtain_ms=120"),
        "the row must carry the fact and the reason beside the claim: {row}"
    );
    assert!(
        base.line().contains(" ribbon_curtain_ms=none"),
        "no curtain reads `none`, never a number: {}",
        base.line()
    );
    assert!(
        !TrailStatus {
            ribbon_drawn: 0,
            ..curtain
        }
        .ribbon_active(),
        "a spent curtain is not an active ribbon"
    );
    assert!(
        !TrailStatus {
            ribbon_curtain_ms: None,
            ..curtain
        }
        .ribbon_active(),
        "`ribbon_drawn` alone must not carry the verdict"
    );
}

fn row_for(flow: rk::Flow) -> TrailStatus<'static> {
    TrailStatus {
        style_raw: "rainbow kitty pet",
        style: Some(GlowStyle::RainbowKitty),
        unknown: false,
        config_enabled: true,
        effective: true,
        focused: true,
        motion_stage: "full",
        motion_mode: "auto",
        shed: 1.0,
        intensity: 0.7,
        sound_seam: true,
        tally: AdmissionTally::default(),
        spawns: 0,
        ribbon_look: "underline",
        ribbon_segments: 0,
        ribbon_hue_bands: 0,
        ribbon_drawn: 4,
        ribbon_curtain_ms: None,
        field: 0.0,
        sparks: 0,
        momentum: 0.0,
        momentum_display: 0.0,
        momentum_glow: 0.0,
        glow_active: false,
        pet_active: false,
        pet_action: "none",
        pet_content: 0.0,
        pet_pending: 0,
        pet_focus: "rest",
        pet_reason: "quiet",
        pet_anchor: None,
        pet_event_seq: 0,
        pet_pose: "pet_sit",
        pet_deep: false,
        pet_body: None,
        head_body: None,
        cat_active: false,
        block_fill: None,
        flow,
        inserts: InsertTally::default(),
        in_flight: InFlightTally::default(),
        comet_cells: 0,
        comet_alpha_max: 0,
    }
}

#[test]
fn the_row_reports_off_and_names_why_a_spelling_fell_back() {
    let dark = TrailStatus {
        effective: false,
        intensity: 0.0,
        ribbon_look: "none",
        ..row_for(rk::Flow::default())
    };
    let off = TrailStatus {
        style_raw: "off",
        style: None,
        unknown: false,
        ..dark
    }
    .line();
    assert!(
        off.contains(" resolved=off ") && off.contains(" issue=none"),
        "{off}"
    );
    for raw in ["rainbw", "pack:synthwav"] {
        let line = TrailStatus {
            style_raw: raw,
            style: Some(GlowStyle::RainbowKitty),
            unknown: true,
            ..row_for(rk::Flow::default())
        }
        .line();
        assert!(
            line.contains(" resolved=rainbow-kitty ") && line.contains(" issue=unknown"),
            "{line}"
        );
    }
    let honoured = TrailStatus {
        style_raw: "rainbow kitty",
        style: Some(GlowStyle::RainbowKitty),
        unknown: false,
        ..row_for(rk::Flow::default())
    }
    .line();
    assert!(
        honoured.contains(" resolved=rainbow-kitty ") && honoured.contains(" issue=none"),
        "{honoured}"
    );
    let comet = TrailStatus {
        style_raw: "comet",
        style: Some(GlowStyle::Comet),
        ribbon_look: "none",
        comet_cells: 12,
        comet_alpha_max: 140,
        ..row_for(rk::Flow::default())
    }
    .line();
    assert!(
        comet.contains(" resolved=comet ")
            && comet.contains(" ribbon_look=none ")
            && comet.contains(" comet_cells=12 comet_alpha_max=140"),
        "{comet}"
    );
}

#[test]
fn the_trail_row_reports_the_flow_the_engine_is_in() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 1)), t0, &c, g, &mut out);
    assert!(glow.v2.engaged(), "flow is priced by v2's spine");

    let cold = glow.flow_status();
    assert_eq!(
        (cold.heat, cold.combo, cold.best),
        (0.0, 0, 0),
        "a hand that has not typed is not in flow"
    );

    let mut now = t0;
    let mut entries = 0u32;
    for k in 0..48u32 {
        now += Duration::from_millis(83);
        glow.note_typed(now);
        let col = 2 + (k % 30) as u16;
        glow.tick(Some((2, col)), now, &c, g, &mut out);
        if glow.take_flow_entry() {
            entries += 1;
        }
    }
    let hot = glow.flow_status();
    assert!(
        hot.combo >= rk::spine::FLOW_ENTRY_KEYS,
        "48 keys at 12 cps must open the theme; combo={}",
        hot.combo
    );
    assert_eq!(hot.heat, 1.0, "an open theme reads `flow=1.00`");
    assert_eq!(hot.best, hot.combo, "the run IS the best so far");
    assert_eq!(entries, 1, "one ENTRY latch for one crossing, ever");

    let row = row_for(hot).line();
    assert!(
        row.contains(" flow=1.00")
            && row.contains(&format!(" combo={}", hot.combo))
            && row.contains(&format!(" combo_best={}", hot.best)),
        "the row must carry the run it is in: {row}"
    );

    glow.note_backspace(now + Duration::from_millis(83));
    let broken = glow.flow_status();
    assert_eq!(
        (broken.heat, broken.combo),
        (0.0, 0),
        "one Backspace ends the run"
    );
    assert_eq!(
        broken.best, hot.best,
        "…and never un-earns the run already held"
    );
    let row = row_for(broken).line();
    assert!(
        row.contains(" flow=0.00")
            && row.contains(" combo=0")
            && row.contains(&format!(" combo_best={}", hot.best)),
        "a broken run reads as broken: {row}"
    );
    assert!(
        !glow.take_flow_entry(),
        "an EXIT latches nothing — the pet hears only entries"
    );

    let mut cold = CursorGlow::default();
    cold.tick(Some((2, 1)), t0, &c, g, &mut out);
    cold.note_typed(t0 + Duration::from_millis(83));
    assert_eq!(
        cold.flow_status().combo,
        0,
        "a key under the spine floor is not flow"
    );
}

#[test]
fn a_celebration_freezes_the_combo_so_a_held_key_cannot_farm_it() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 1)), t0, &c, g, &mut out);
    let mut now = t0;
    for k in 0..20u32 {
        now += Duration::from_millis(83);
        glow.note_typed(now);
        glow.tick(Some((2, 2 + (k % 30) as u16)), now, &c, g, &mut out);
    }
    let earned = glow.flow_status().combo;
    assert!(earned > 0, "the run is live before the celebration");
    for _ in 0..20u32 {
        now += Duration::from_millis(83);
        glow.celebrate(now, 1.0);
        glow.note_typed(now);
        glow.tick(Some((2, 5)), now, &c, g, &mut out);
    }
    assert_eq!(
        glow.flow_status().combo,
        earned,
        "a frozen combo neither climbs nor breaks"
    );
    now += Duration::from_secs_f32(rk::spine::FLOW_FREEZE_S) + Duration::from_millis(83);
    glow.note_typed(now);
    glow.tick(Some((2, 6)), now, &c, g, &mut out);
    assert_eq!(
        glow.flow_status().combo,
        earned + 1,
        "the freeze lifts after the last drive"
    );
}

#[test]
fn the_status_row_names_who_owns_the_block_cursor() {
    let quiet = TrailStatus {
        style_raw: "lumen",
        style: Some(GlowStyle::Lumen),
        unknown: false,
        config_enabled: true,
        effective: true,
        focused: true,
        motion_stage: "full",
        motion_mode: "auto",
        shed: 1.0,
        intensity: 0.7,
        sound_seam: true,
        tally: AdmissionTally::default(),
        spawns: 0,
        ribbon_look: "underline",
        ribbon_segments: 0,
        ribbon_hue_bands: 0,
        ribbon_drawn: 4,
        ribbon_curtain_ms: None,
        field: 0.0,
        sparks: 0,
        momentum: 0.0,
        momentum_display: 0.0,
        momentum_glow: 0.0,
        glow_active: false,
        pet_active: false,
        pet_action: "none",
        pet_content: 0.0,
        pet_pending: 0,
        pet_focus: "rest",
        pet_reason: "quiet",
        pet_anchor: None,
        pet_event_seq: 0,
        pet_pose: "pet_sit",
        pet_deep: false,
        pet_body: None,
        head_body: None,
        cat_active: false,
        block_fill: None,
        flow: rk::Flow::default(),
        inserts: InsertTally::default(),
        in_flight: InFlightTally::default(),
        comet_cells: 0,
        comet_alpha_max: 0,
    };
    let line = quiet.line();
    assert!(
        line.contains(" block_fill=none")
            && line.contains(" block_fill_rgb=none")
            && line.contains(" block_fill_base=none")
            && line.contains(" block_fill_base_from=none"),
        "nobody owns the caret ⇒ the terminal's own cursor colour paints it: {line}"
    );

    let owned = TrailStatus {
        block_fill: Some(BlockFill {
            owner: BlockFillOwner::Phaser,
            fill: 0x001A_22EE,
            base: Some(0x0000_00FF),
        }),
        ..quiet
    };
    let line = owned.line();
    assert!(
        line.contains(" glow_active=false")
            && line.contains(" pet_active=false")
            && line.contains(" cat_active=false"),
        "the old gates still read quiet: {line}"
    );
    assert!(
        line.contains(" block_fill=phaser")
            && line.contains(" block_fill_rgb=1a22ee")
            && line.contains(" block_fill_base=0000ff")
            && line.contains(" block_fill_base_from=cursor_color"),
        "…while the row names the style that owns the caret: {line}"
    );

    let broken = TrailStatus {
        block_fill: Some(BlockFill {
            owner: BlockFillOwner::Phaser,
            fill: 0x00F8_E1E1,
            base: Some(0x00FF_0000),
        }),
        ..quiet
    };
    let line = broken.line();
    assert!(
        line.contains(" block_fill_rgb=f8e1e1") && line.contains(" block_fill_base=ff0000"),
        "an owner that ignored its base must be legible as such: {line}"
    );

    assert!(
        TrailStatus {
            block_fill: Some(BlockFill {
                owner: BlockFillOwner::Comet,
                fill: 0x0000_00FF,
                base: Some(0x0000_00FF),
            }),
            ..quiet
        }
        .line()
        .contains(" block_fill_rgb=0000ff")
    );

    for (owner, from) in [
        (BlockFillOwner::Rainbow, "cursor_color"),
        (BlockFillOwner::Phaser, "cursor_color"),
        (BlockFillOwner::Comet, "trail_color"),
        (BlockFillOwner::Bolt, "trail_color"),
        (BlockFillOwner::BeamRod, "trail_color"),
        (BlockFillOwner::Forge, "style_identity"),
        (BlockFillOwner::Droplet, "style_identity"),
    ] {
        assert_eq!(
            owner.base_from().label(),
            from,
            "{} reports the wrong base provenance",
            owner.label()
        );
        let identity = owner.base_from() == BlockFillBase::StyleIdentity;
        let line = TrailStatus {
            block_fill: Some(BlockFill {
                owner,
                fill: 0x0012_3456,
                base: (!identity).then_some(0x00AB_CDEF),
            }),
            ..quiet
        }
        .line();
        assert!(
            line.contains(&format!(" block_fill={}", owner.label()))
                && line.contains(" block_fill_rgb=123456")
                && line.contains(&format!(" block_fill_base_from={from}")),
            "{} is not legible in the row: {line}",
            owner.label()
        );
        assert!(
            line.contains(if identity {
                " block_fill_base=none"
            } else {
                " block_fill_base=abcdef"
            }),
            "{} reports the wrong base: {line}",
            owner.label()
        );
    }
    assert!(!owned.line().contains('\n'));
}

#[test]
fn the_styles_that_read_the_flanking_rows_ask_for_them() {
    let g = geom();
    let ask_face = |style: GlowStyle, mono: bool| {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let c = GlowConfig {
            classic_mono: mono,
            ..cfg(style, true)
        };
        glow.tick(Some((2, 3)), Instant::now(), &c, g, &mut out);
        glow.reads_neighbor_rows()
    };
    let ask = |style: GlowStyle| ask_face(style, false);
    assert!(
        ask_face(GlowStyle::Classic, true),
        "the classic wake's mono face reads the flanking rows"
    );
    for style in [
        GlowStyle::Fire,
        GlowStyle::Comet,
        GlowStyle::Sparkle,
        GlowStyle::Laser,
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::Water,
        GlowStyle::Beam,
    ] {
        assert!(ask(style), "{style:?} reads the flanking rows");
    }
    assert!(
        !ask(GlowStyle::Classic),
        "control: the classic wake reads neither"
    );
    assert!(!CursorGlow::default().reads_neighbor_rows());
}

#[test]
fn classic_spawns_reach_the_count_and_the_ring() {
    let g = geom();
    let c = cfg(GlowStyle::Classic, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let t = Instant::now();
    glow.tick(Some((2, 4)), t, &c, g, &mut out);
    let (spawns, licensed) = (glow.spawns(), glow.admission_tally().licensed);
    glow.tick(Some((2, 5)), t + Duration::from_millis(40), &c, g, &mut out);
    assert_eq!(glow.spawns(), spawns + 1);
    assert_eq!(glow.admission_tally().licensed, licensed + 1);
    let last = glow.admission_log().last().expect("a ring row");
    assert_eq!(last.licence, AdmissionRecord::LICENCE_MOTION);
    assert_eq!((last.origin, last.target), ((2, 4), (2, 5)));
    glow.tick(Some((2, 5)), t + Duration::from_millis(80), &c, g, &mut out);
    assert_eq!(glow.spawns(), spawns + 1);
    assert_eq!(glow.admission_tally().licensed, licensed + 1);
}

/// A SPLIT PANE'S TEXT IS READ AT ITS WINDOW OFFSET: a split composes the
/// window's cells only after the cursor's step, so the host hands the
/// focused pane's own cells and where they sit; the frame's text (the dark
/// streak's air, the letter guard's letters) is then where the window draws
/// it. Negative control: the same cells read at the window's origin put the
/// letter three rows and ten columns off and leave its window cell blank.
#[test]
fn a_split_pane_s_text_is_read_at_its_window_offset() {
    use aterm_core::terminal::RenderCell;
    let mut cells = vec![vec![RenderCell::default(); 20]; 6];
    cells[1][3].ch = 'x';
    cells[1][3].fg = [0xD0, 0xD0, 0xD0];
    let t0 = Instant::now();
    let lettered = |glow: &CursorGlow, row: i32, col: i32| {
        matches!(glow.frame_text.pair(row, col), Some((Some(_), _)))
    };
    let mut glow = CursorGlow::default();
    glow.observe_frame_text_at(Some((3, 15)), &cells, ((2, 10), 0), t0);
    assert!(
        lettered(&glow, 3, 13),
        "the pane's letter at pane cell (1, 3) reads at window cell (3, 13)"
    );
    let span = glow.frame_text.rows[(3 - glow.frame_text.lo) as usize];
    assert_eq!(span, Some((13, 14)), "the row's text spans window columns");
    let mut control = CursorGlow::default();
    control.observe_frame_text(Some((3, 15)), &cells, t0);
    assert!(
        !lettered(&control, 3, 13),
        "control: read at the window's origin, window cell (3, 13) is blank"
    );
}
