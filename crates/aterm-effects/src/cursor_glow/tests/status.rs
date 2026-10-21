// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fresh-ink pop, the typing wake and the `trail` status row.

use super::*;

/// Type one printable key in the GUI host's seam order: stamp the press, feed
/// the row probe (the erase-poof lane's evidence), then tick the cursor onto
/// its landing. The press stamp matters: a cursor delta with no fresh key hint
/// is program output, which the license declines.
fn type_one_key(glow: &mut CursorGlow, cfg: &GlowConfig, g: Geom, at: Instant, row: u16, col: u16) {
    // The row as input time saw it: glyphs already typed, blanks after.
    let mut after = [' '; 40];
    after[..usize::from(col) + 1].fill('x');
    glow.note_typed_cells(at, 1);
    glow.observe_row(row, col + 1, &after, at);
    glow.tick(Some((row, col + 1)), at, cfg, g, &mut typed_scratch());
}

/// A twelve-key word at fast, average and slow human cadence earns a multi-cell ribbon across at
/// least two status bands.
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

/// Negative control: ambient program output moves the cursor with no authored
/// key, so status reports a decline and a dark ribbon.
#[test]
fn unauthored_output_is_declined_and_the_status_verb_shows_it_dark() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((2, 0)), t0, &c, g, &mut typed_scratch());
    // A TUI redraw rewrote the row and walked the caret. No key was
    // pressed, so no license term is fresh.
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

/// The status row: fixed key order, every gate present, and `ribbon_active`
/// derived; one cell, or four cells in one hue, is not a rainbow.
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
        pet_body: Some((12, 68, 30, 64)),
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
    // The raw style string is quoted, so a trailing space or an empty
    // setting is legible as itself rather than as a broken row.
    assert!(
        line.contains(r#"style="rainbow kitty pet""#),
        "the raw config string is reported verbatim: {line}"
    );
    // One line: a status the caller can grep without reassembly.
    assert!(!line.contains('\n'), "the status row is one line: {line}");
    // A style that draws nothing says so, not the engine's placeholder enum.
    let off = TrailStatus {
        style_raw: "off",
        style: None,
        effective: false,
        intensity: 0.0,
        ..base
    }
    .line();
    assert!(off.contains(" resolved=off "), "{off}");
    // Never a fabricated reason.
    let quiet = TrailStatus {
        tally: AdmissionTally::default(),
        ..base
    };
    assert!(quiet.line().contains(" last_decline_reason=none"));
    // The ribbon verdict is derived from what is drawn: one lone cell is a
    // glow under the caret, four same-hue cells a monochrome bar.
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
    // A falling curtain is still a ribbon: `ribbon_segments` is floored at the arc's dimmest
    // stop, so late in a curtain the claim is zero while the band is still drawn. The row
    // carries what is drawn and why the claim is under its floor; the verdict reads both.
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
    // …and the end of the fall is the honest edge: nothing drawn, nothing
    // claimed.
    assert!(
        !TrailStatus {
            ribbon_drawn: 0,
            ..curtain
        }
        .ribbon_active(),
        "a spent curtain is not an active ribbon"
    );
    // A dark band outside a curtain stays dark: the curtain clause is not a
    // back door for the claim floor.
    assert!(
        !TrailStatus {
            ribbon_curtain_ms: None,
            ..curtain
        }
        .ribbon_active(),
        "`ribbon_drawn` alone must not carry the verdict"
    );
}

/// A minimal `trail status` row carrying one [`rk::Flow`]; the flow tests
/// assert the bytes the socket prints.
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
        // A flow row says nothing about the pet.
        pet_action: "none",
        pet_content: 0.0,
        pet_pending: 0,
        pet_focus: "rest",
        pet_reason: "quiet",
        pet_anchor: None,
        pet_event_seq: 0,
        pet_pose: "pet_sit",
        pet_body: None,
        cat_active: false,
        block_fill: None,
        flow,
        inserts: InsertTally::default(),
        in_flight: InFlightTally::default(),
        comet_cells: 0,
        comet_alpha_max: 0,
    }
}

/// `resolved=` names what the spelling resolved to: `off` resolves to NO
/// style and the row says `off` rather than the placeholder enum a disabled
/// config carries. A spelling naming nothing drawable (a typo, a bare
/// `pack:`, a pack that did not load) falls back to the default style, so the
/// row reads that default and `issue=` says why it was not honoured as
/// written (the control: a style honoured as written reads `issue=none`).
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

/// The row reports the engine's flow: a run of fast keys moves `flow=` and
/// `combo=` on the printed row, and one Backspace zeroes them, through the
/// real engine. An agent reading `trail status` can tell the human is
/// mid-flow. Flow draws nothing, so no light is asserted.
#[test]
fn the_trail_row_reports_the_flow_the_engine_is_in() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 1)), t0, &c, g, &mut out);
    assert!(glow.v2.engaged(), "flow is priced by v2's spine");

    // Cold: no keys, no flow.
    let cold = glow.flow_status();
    assert_eq!(
        (cold.heat, cold.combo, cold.best),
        (0.0, 0, 0),
        "a hand that has not typed is not in flow"
    );

    // A run at 12 cps, one key per tick. The spine needs about a dozen keys to
    // pass `FLOW_KEY_DISP`; the combo counts keys after that.
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

    // The row, in the bytes the socket prints.
    let row = row_for(hot).line();
    assert!(
        row.contains(" flow=1.00")
            && row.contains(&format!(" combo={}", hot.combo))
            && row.contains(&format!(" combo_best={}", hot.best)),
        "the row must carry the run it is in: {row}"
    );

    // A delete ends it on the key's own edge, visible on the next reading.
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

    // A cold hand's first key is under the spine floor and opens nothing.
    let mut cold = CursorGlow::default();
    cold.tick(Some((2, 1)), t0, &c, g, &mut out);
    cold.note_typed(t0 + Duration::from_millis(83));
    assert_eq!(
        cold.flow_status().combo,
        0,
        "a key under the spine floor is not flow"
    );
}

/// Auto-repeat cannot farm the combo: a held key is maximal momentum (the
/// bypass [`CursorGlow::celebrate`] takes), so the ladder freezes under it.
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
    // The celebration drives once per frame, as the host drives it.
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
    // …and it thaws: after the grace, real keys count again.
    now += Duration::from_secs_f32(rk::spine::FLOW_FREEZE_S) + Duration::from_millis(83);
    glow.note_typed(now);
    glow.tick(Some((2, 6)), now, &c, g, &mut out);
    assert_eq!(
        glow.flow_status().combo,
        earned + 1,
        "the freeze lifts after the last drive"
    );
}

/// The row names the style that owns the caret, the colour that owner built
/// its body from, and where that colour came from, so whether the cursor
/// colour reaches the glass is answered by the sensor, not a capture.
/// `glow_active` / `pet_active` / `cat_active` cannot answer it.
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
        // Every gate quiet…
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
        pet_body: None,
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

    // …and the same quiet row with a body effect live: a named owner holding
    // the caret beside three `*_active=false` gates.
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

    // A body painting a colour other than the one it was handed is readable
    // in one row: `base` is what the host handed, `fill` what was painted.
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

    // Zero-padded to six digits: a dark colour must not print as `ff` and
    // be mistaken for a red one.
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

    // Provenance is a property of the owner: fire owns its orange; the
    // rainbow and phaser blooms start from the cursor's own colour.
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
                // The host hands a base to every style except those that own their colour.
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
        // A style-identity owner reports no base (it was handed none), and
        // the painted colour still shows.
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
    // One line, still — the whole row stays greppable without reassembly.
    assert!(!owned.line().contains('\n'));
}

/// The engine asks its host for the rows above and below the caret's for
/// every style with an over-text rule that reads them, not only while the
/// rainbow kitty owns the frame: Fire (its over-ink ledger and light flame
/// cap), the beam family (its dark bed's split between bed and beam) and the
/// particle styles count an unread row as text or as air, which is wrong
/// either way over a blank row or old output, and the classic wake's mono
/// face (its bed's rail and its over-ink account). Negative control: the
/// classic wake's spectrum face reads neither, and a fresh engine has no
/// style.
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

/// The classic wake lights every observed move without the licensed spawn
/// path, so it reports its spawns itself: `trail status` counts them and the
/// diagnosis ring names them `motion`. A frame with no move adds nothing.
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
    // Negative control: the cursor stays put, nothing more is counted.
    glow.tick(Some((2, 5)), t + Duration::from_millis(80), &c, g, &mut out);
    assert_eq!(glow.spawns(), spawns + 1);
    assert_eq!(glow.admission_tally().licensed, licensed + 1);
}
