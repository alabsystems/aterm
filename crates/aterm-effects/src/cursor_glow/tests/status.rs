// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fresh-ink pop, the typing wake and the `trail` status row.

use super::*;

/// Type one printable key, walking the SAME seam order the GUI host walks
/// every frame under the LICENSE law: stamp the press, feed the row probe
/// (the erase-poof lane's evidence, which survives the rip), then tick the
/// cursor onto its landing.
///
/// THE PRESS STAMP IS THE POINT. A fixture that ticks a cursor delta with
/// no fresh key hint is program output, and the license declines it — so
/// every ribbon fixture goes through here rather than poking `spawn`.
fn type_one_key(glow: &mut CursorGlow, cfg: &GlowConfig, g: Geom, at: Instant, row: u16, col: u16) {
    // The row as input time saw it: glyphs already typed, blanks after.
    let mut after = [' '; 40];
    after[..usize::from(col) + 1].fill('x');
    glow.note_typed_cells(at, 1);
    glow.observe_row(row, col + 1, &after, at);
    glow.tick(Some((row, col + 1)), at, cfg, g, &mut typed_scratch());
}

/// The status verb reports the same classic geometry as the renderer:
/// four licensed cells span half the arc (3 / 6) and occupy four bands.
#[test]
fn a_typed_burst_earns_a_rainbow_ribbon_the_status_verb_reports() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((2, 0)), t0, &c, g, &mut typed_scratch());
    for key in 0..4u32 {
        type_one_key(
            &mut glow,
            &c,
            g,
            t0 + Duration::from_millis(u64::from(key) * 120 + 1),
            2,
            key as u16,
        );
    }

    let tally = glow.admission_tally();
    assert_eq!((tally.licensed, tally.declined), (4, 0));
    assert_eq!(tally.last_decline_reason, None);
    assert_eq!(glow.spawns(), 4);
    let segments = glow.ribbon_segments();
    assert!(
        segments >= 4,
        "four keys leave at least four ribbon cells: {segments}"
    );
    assert!(
        glow.ribbon_hue_bands() >= 2,
        "a four-cell ribbon spans at least two status bands"
    );
    assert!(glow.is_active());
    assert!(
        glow.typing_momentum(t0 + Duration::from_millis(361)) > 0.0,
        "ordinary typing builds momentum without gating the ribbon"
    );
}

/// A whole word at fast, average, and slow human cadence retains one
/// classic rainbow. Status span and band counts are derived from the live
/// cell count, with no phase, fold, or endpoint-dwell allowance.
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

/// The NEGATIVE control the soundness essays demand: ambient program
/// output moves the cursor with no authored key behind it, so the status
/// verb must report a DECLINE and a DARK ribbon. A verb that reported a
/// ribbon here would be buying pixels with correctness.
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

/// The status ROW: fixed key order, every gate present, and the
/// `ribbon_active` verdict derived (never asserted) — one cell, or four
/// cells in one hue, is not a rainbow.
#[test]
fn the_status_row_names_every_gate_and_derives_the_ribbon_verdict() {
    let base = TrailStatus {
        style_raw: "rainbow kitty pet",
        style: GlowStyle::RainbowKitty,
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
    ] {
        assert!(line.contains(key), "missing {key:?} in {line}");
    }
    // The raw style string is quoted, so a trailing space or an empty
    // setting is legible as itself rather than as a broken row.
    assert!(
        line.contains(r#"style="rainbow kitty pet""#),
        "the raw config string is reported verbatim: {line}"
    );
    // ONE LINE — a status the caller can grep without reassembly.
    assert!(!line.contains('\n'), "the status row is one line: {line}");
    // Never a fabricated reason.
    let quiet = TrailStatus {
        tally: AdmissionTally::default(),
        ..base
    };
    assert!(quiet.line().contains(" last_decline_reason=none"));
    // The RIBBON VERDICT is derived from what is on glass, not asserted:
    // one lone cell is a glow under the caret, and four same-hue cells are
    // a monochrome bar. Neither is the flowing rainbow the docs promise.
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
    // **A FALLING CURTAIN IS STILL A RIBBON** (2026-09-14; the house rule
    // that a BOUND must not be returned as a FACT). `ribbon_segments` is
    // floored at the arc's DIMMEST stop, so from about +99 ms of every
    // 0.24 s curtain the claim is zero while the band is plainly on the
    // glass — 1065 quads at alpha 102 down to 9, measured. The row now
    // carries what is drawn and why the claim is under its floor, and the
    // headline verdict reads them.
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
    // A DARK BAND OUTSIDE A CURTAIN stays dark: the curtain clause may
    // not be a back door for the claim floor.
    assert!(
        !TrailStatus {
            ribbon_curtain_ms: None,
            ..curtain
        }
        .ribbon_active(),
        "`ribbon_drawn` alone must not carry the verdict"
    );
}

/// A minimal `trail status` row carrying one [`rk::Flow`] — the flow
/// laws assert the BYTES the socket prints, not the accessor alone, and
/// every other field on the row is beside the point for them.
fn row_for(flow: rk::Flow) -> TrailStatus<'static> {
    TrailStatus {
        style_raw: "rainbow kitty pet",
        style: GlowStyle::RainbowKitty,
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
        // The resident's own four columns (another round's, landed in the
        // same merge): a flow row says nothing about the cat.
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
    }
}

/// **THE ROW REPORTS THE FLOW THE ENGINE IS IN.**
///
/// The whole point of the feature on the wire: an agent reading
/// `trail status` can tell that the human is mid-flow — typing at speed,
/// uninterrupted — and hold its turn. So a run of fast keys must MOVE
/// `flow=` and `combo=` on the row the socket prints, and one Backspace
/// must zero them, all the way through the real engine and the real row.
///
/// Nothing here asserts light: flow draws nothing of its own, and this is
/// the sensor's law, not the theme's.
#[test]
fn the_trail_row_reports_the_flow_the_engine_is_in() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 1)), t0, &c, g, &mut out);
    assert!(glow.v2.engaged(), "flow is priced by v2's spine");

    // COLD: no keys, no flow. The row says so in its own words.
    let cold = glow.flow_status();
    assert_eq!(
        (cold.heat, cold.combo, cold.best),
        (0.0, 0, 0),
        "a hand that has not typed is not in flow"
    );

    // A RUN AT SPEED — 12 cps, one key per tick, the caret walking its
    // row. The spine needs ~a dozen keys to climb past `FLOW_KEY_DISP`,
    // and the combo counts only the keys after that.
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

    // The ROW, in the bytes the socket prints.
    let row = row_for(hot).line();
    assert!(
        row.contains(" flow=1.00")
            && row.contains(&format!(" combo={}", hot.combo))
            && row.contains(&format!(" combo_best={}", hot.best)),
        "the row must carry the run it is in: {row}"
    );

    // A DELETE ENDS IT — exit no. 1, on the key's own edge, and the row
    // says so on the very next reading.
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

    // A KILL ends it too (exit no. 2), and a key under the floor is the
    // third: a cold hand's first key can never open the theme.
    let mut cold = CursorGlow::default();
    cold.tick(Some((2, 1)), t0, &c, g, &mut out);
    cold.note_typed(t0 + Duration::from_millis(83));
    assert_eq!(
        cold.flow_status().combo,
        0,
        "a key under the spine floor is not flow"
    );
}

/// **AUTO-REPEAT CANNOT FARM THE COMBO.** A held key at repeat cadence IS
/// maximal momentum by definition — that is the documented bypass
/// [`CursorGlow::celebrate`] takes — so the ladder must FREEZE under it,
/// or leaning on one key would buy the state the feature exists to make
/// you earn.
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

/// THE FIELD THAT WOULD HAVE NAMED THE BUG.
///
/// `glow_active` / `pet_active` / `cat_active` all read false while the
/// rainbow block was eating OSC 12 — none of them is the gate that was
/// open. The row must therefore name the style that owns the caret, the
/// colour that owner built its body from, and WHERE that colour came from,
/// so "is the cursor colour reaching the glass?" is a question the sensor
/// answers instead of one a screen capture has to.
#[test]
fn the_status_row_names_who_owns_the_block_cursor() {
    let quiet = TrailStatus {
        style_raw: "lumen",
        style: GlowStyle::Lumen,
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
        // EVERY gate the row used to print, quiet…
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
    };
    let line = quiet.line();
    assert!(
        line.contains(" block_fill=none")
            && line.contains(" block_fill_rgb=none")
            && line.contains(" block_fill_base=none")
            && line.contains(" block_fill_base_from=none"),
        "nobody owns the caret ⇒ the terminal's own cursor colour paints it: {line}"
    );

    // …and the SAME quiet row with a body effect live. This is the exact
    // reading the two misdiagnoses lacked: three `*_active=false` gates
    // beside a named owner holding the caret.
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

    // THE DEFECT ITSELF, READABLE IN ONE ROW. The pre-fix phaser was handed
    // a red cursor colour and painted a near-white block anyway. `base` is
    // what the host handed the owner; `fill` is what the owner painted —
    // so the two disagreeing IS the bug, printed, with no capture needed.
    let broken = TrailStatus {
        block_fill: Some(BlockFill {
            owner: BlockFillOwner::Phaser,
            fill: 0x00F8_E1E1, // measured on the glass, pre-fix
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

    // The provenance is a PROPERTY OF THE OWNER, so a reader can tell a
    // style that deliberately owns its colour from one that is eating the
    // user's. Fire is orange because it is fire; the rainbow and the phaser
    // blooms must start from the cursor's own colour.
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
                // The host hands a base to everyone EXCEPT the styles that
                // own their colour outright.
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
        // A style-identity owner reports NO base — it was handed none —
        // and the painted colour still shows. That is a complete reading,
        // not a missing one.
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
