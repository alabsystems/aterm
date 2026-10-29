// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Move-license parity tests across styles: licensed moves earn light, unlicensed
//! moves mint none, and neither wipes resident light.

use std::time::{Duration, Instant};

use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::cursor_trail::{CursorTrail, TrailConfig};
use aterm_render::GlowQuad;

fn fold(acc: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *acc = acc.wrapping_mul(1_000_003).wrapping_add(u64::from(*b));
    }
}

fn geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    }
}

fn cfg(style: GlowStyle) -> GlowConfig {
    GlowConfig {
        style,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        audible: true,
        radius: 0.6,
        ring: true,
        beam: !matches!(style, GlowStyle::Water | GlowStyle::RainbowKitty),
        ..Default::default()
    }
}

fn trail_cfg() -> TrailConfig {
    TrailConfig {
        enabled: true,
        color: 0x0050_FA7B,
        duration: Duration::from_millis(240),
        max_len: 18,
        intensity: 0.7,
        warmth: 0.0,
    }
}

/// Live light population from whichever engine (`v2_status` or `live_sparks`) owns the frame.
fn live_light(glow: &CursorGlow) -> usize {
    glow.v2_status().map_or_else(
        || glow.live_sparks(),
        |s| (s.cells + s.stars + s.meteors) as usize,
    )
}

/// Resident ribbon cell count from `v2_status` or `ribbon_segments`.
fn resident_ribbon(glow: &CursorGlow) -> usize {
    glow.v2_status()
        .map_or_else(|| glow.ribbon_segments(), |s| s.cells as usize)
}

/// Summary of a scripted typing and jump trajectory with or without license hints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TypedScriptOutcome {
    fingerprint: u64,
    licensed: u64,
    declined: u64,
    spawns: u64,
    peak_live_light: usize,
    peak_ribbon_segments: usize,
}

fn typed_script(style: GlowStyle, licensed: bool) -> TypedScriptOutcome {
    let g = geom();
    let c = cfg(style);
    let tc = trail_cfg();
    let mut glow = crate::common::quads_glow();
    let mut trail = CursorTrail::default();
    let mut out: Vec<GlowQuad> = Vec::new();
    let mut trail_out = Vec::new();
    let t0 = Instant::now();
    let mut acc: u64 = 0;
    let mut peak_live_light = 0;
    let mut peak_ribbon_segments = 0;

    let mut step =
        |glow: &mut CursorGlow, trail: &mut CursorTrail, at: Instant, cell: (u16, u16)| {
            let fp = glow.tick(Some(cell), at, &c, g, &mut out);
            trail.tick(Some(cell), at, &tc, &mut trail_out);
            let light = live_light(glow);
            let ribbon_segments = glow.ribbon_segments();
            peak_live_light = peak_live_light.max(light);
            peak_ribbon_segments = peak_ribbon_segments.max(ribbon_segments);
            fold(&mut acc, &fp.to_le_bytes());
            fold(&mut acc, &glow.spawns().to_le_bytes());
            fold(&mut acc, &(light as u64).to_le_bytes());
            fold(&mut acc, &(ribbon_segments as u64).to_le_bytes());
            fold(&mut acc, &glow.typing_momentum(at).to_bits().to_le_bytes());
            for q in &out {
                fold(&mut acc, format!("{q:?}").as_bytes());
            }
            for cell in &trail_out {
                fold(&mut acc, format!("{cell:?}").as_bytes());
            }
            for cue in glow.drain_sound_cues() {
                fold(&mut acc, format!("{cue:?}").as_bytes());
            }
        };

    step(&mut glow, &mut trail, t0, (2, 30));
    for k in 1..=6u64 {
        let at = t0 + Duration::from_millis(90 * k);
        if licensed {
            glow.note_synthetic_typed(at, 1);
            trail.note_synthetic_typed(at);
        }
        step(&mut glow, &mut trail, at, (2, 30 + k as u16));
    }
    for (k, cell) in [(7u64, (3u16, 0u16)), (8, (3, 1)), (9, (3, 2))] {
        let at = t0 + Duration::from_millis(90 * k);
        if licensed {
            glow.note_synthetic_typed(at, 1);
            trail.note_synthetic_typed(at);
        }
        step(&mut glow, &mut trail, at, cell);
    }
    let jump = t0 + Duration::from_millis(1_100);
    if licensed {
        glow.note_synthetic_move(jump);
        trail.note_synthetic_move(jump);
    }
    step(&mut glow, &mut trail, jump, (5, 30));
    for ms in [1_150u64, 1_400, 2_200, 4_000] {
        let at = t0 + Duration::from_millis(ms);
        step(&mut glow, &mut trail, at, (5, 30));
    }
    let tally = glow.admission_tally();
    TypedScriptOutcome {
        fingerprint: acc,
        licensed: tally.licensed,
        declined: tally.declined,
        spawns: glow.spawns(),
        peak_live_light,
        peak_ribbon_segments,
    }
}

/// A cold program move does not wipe resident light or comet cells earned by a licensed typed move.
#[test]
fn earned_light_survives_a_cold_program_move() {
    let g = geom();
    let tc = trail_cfg();
    for style in ALL_STYLES {
        let c = cfg(style);
        let mut glow = crate::common::quads_glow();
        let mut trail = CursorTrail::default();
        let mut out: Vec<GlowQuad> = Vec::new();
        let mut trail_out = Vec::new();
        let t0 = Instant::now();
        glow.tick(Some((2, 3)), t0, &c, g, &mut out);
        trail.tick(Some((2, 3)), t0, &tc, &mut trail_out);
        let typed = t0 + Duration::from_millis(8);
        glow.note_synthetic_typed(typed, 1);
        trail.note_synthetic_typed(typed);
        glow.tick(Some((2, 4)), typed, &c, g, &mut out);
        trail.tick(Some((2, 4)), typed, &tc, &mut trail_out);
        let earned = live_light(&glow);
        let earned_trail = trail_out.len();
        assert!(earned > 0, "{style:?}: the typed echo must earn light");
        assert!(earned_trail > 0, "{style:?}: the comet must exist");
        glow.tick(Some((4, 20)), typed, &c, g, &mut out);
        trail.tick(Some((4, 20)), typed, &tc, &mut trail_out);
        assert_eq!(
            live_light(&glow),
            earned,
            "{style:?}: earned light was wiped by a cold program move"
        );
        assert_eq!(
            trail_out.len(),
            earned_trail,
            "{style:?}: the earned comet was wiped by a cold program move"
        );
    }
}

/// The first move after a keypress consumes the one-shot license; a split-batch suffix stays dark while advancing anchors.
#[test]
fn split_batch_suffix_is_dark_but_keeps_resident_light_and_advances_anchors() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty);
    let tc = trail_cfg();
    let mut glow = crate::common::quads_glow();
    let mut trail = CursorTrail::default();
    let mut out: Vec<GlowQuad> = Vec::new();
    let mut trail_out = Vec::new();
    let t0 = Instant::now();

    glow.tick(Some((2, 3)), t0, &c, g, &mut out);
    trail.tick(Some((2, 3)), t0, &tc, &mut trail_out);

    let first = t0 + Duration::from_millis(8);
    glow.note_typed_cells(first, 1);
    trail.note_typed(first);
    glow.tick(Some((2, 4)), first, &c, g, &mut out);
    trail.tick(Some((2, 4)), first, &tc, &mut trail_out);

    let earned_spawns = glow.spawns();
    let earned_light = live_light(&glow);
    let earned_ribbon = resident_ribbon(&glow);
    let earned_trail: Vec<(usize, usize)> =
        trail_out.iter().map(|cell| (cell.row, cell.col)).collect();
    assert_eq!(earned_spawns, 1, "the first batch spends the press once");
    assert!(earned_light > 0, "the first batch must earn rainbow light");
    assert!(earned_ribbon > 0, "the first batch must lay the ribbon");
    assert!(
        !earned_trail.is_empty(),
        "the first batch must lay the comet"
    );
    assert_eq!(glow.cursor_anchor(), Some((2, 4)));
    assert_eq!(trail.cursor_anchor(), Some((2, 4)));
    assert!(!glow.move_licensed(first), "the glow license is one-shot");
    assert!(!trail.move_licensed(first), "the trail license is one-shot");

    let suffix = first + Duration::from_millis(1);
    glow.tick(Some((2, 5)), suffix, &c, g, &mut out);
    trail.tick(Some((2, 5)), suffix, &tc, &mut trail_out);

    assert_eq!(glow.spawns(), earned_spawns, "the suffix minted glow");
    assert_eq!(
        live_light(&glow),
        earned_light,
        "the suffix erased resident rainbow light"
    );
    assert_eq!(
        resident_ribbon(&glow),
        earned_ribbon,
        "the suffix cut the resident ribbon"
    );
    assert_eq!(
        trail_out
            .iter()
            .map(|cell| (cell.row, cell.col))
            .collect::<Vec<_>>(),
        earned_trail,
        "the suffix changed the resident comet bed"
    );
    assert_eq!(glow.cursor_anchor(), Some((2, 5)));
    assert_eq!(trail.cursor_anchor(), Some((2, 5)));

    let tally = glow.admission_tally();
    assert_eq!((tally.licensed, tally.declined), (1, 1));
    assert_eq!(
        tally.last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT)
    );
}

const ALL_STYLES: [GlowStyle; 9] = [
    GlowStyle::Lumen,
    GlowStyle::Phaser,
    GlowStyle::RainbowKitty,
    GlowStyle::Sparkle,
    GlowStyle::Fire,
    GlowStyle::Laser,
    GlowStyle::Beam,
    GlowStyle::Water,
    GlowStyle::Comet,
];

#[test]
fn a_delayed_backspace_extends_only_rainbow_kittys_move_licence() {
    let retreat = |style, delay, backspace| {
        let mut glow = crate::common::quads_glow();
        let mut trail = CursorTrail::default();
        let mut out = Vec::new();
        let mut trail_out = Vec::new();
        let c = cfg(style);
        let tc = trail_cfg();
        let t0 = Instant::now();
        glow.tick(Some((2, 10)), t0, &c, geom(), &mut out);
        trail.tick(Some((2, 10)), t0, &tc, &mut trail_out);
        let pressed = t0 + Duration::from_millis(8);
        if backspace {
            glow.note_backspace_erasing(pressed, Some(1));
            trail.note_typed(pressed);
        }
        let echoed = pressed + delay;
        assert_eq!(glow.move_licensed(echoed), trail.move_licensed(echoed));
        glow.tick(Some((2, 9)), echoed, &c, geom(), &mut out);
        trail.tick(Some((2, 9)), echoed, &tc, &mut trail_out);
        (glow.spawns(), trail_out.len(), glow.admission_tally())
    };

    let fresh = retreat(GlowStyle::Fire, Duration::from_millis(8), true);
    assert_eq!(fresh.0, 1);
    assert!(fresh.1 > 0);
    let cold = retreat(GlowStyle::Fire, Duration::from_secs(1), false);
    assert_eq!((cold.0, cold.1), (0, 0));
    assert_eq!(
        cold.2.last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT)
    );

    let rainbow = retreat(GlowStyle::RainbowKitty, Duration::from_secs(1), true);
    assert_eq!(rainbow.0, 1);
    assert_eq!(rainbow.1, 0);

    for style in ALL_STYLES
        .into_iter()
        .filter(|s| *s != GlowStyle::RainbowKitty)
    {
        let delayed = retreat(style, Duration::from_secs(1), true);
        assert_eq!(delayed.1, 0, "{style:?}: the trail's hint has expired");
        assert_eq!(delayed.0, 0, "{style:?}: retain the trail's move licence");
        assert_eq!(
            delayed.2.last_decline_reason,
            Some(CursorGlow::DECLINE_NO_FRESH_HINT)
        );
    }
}

/// Withholding key hints declines every move and mints no light across all styles.
#[test]
fn an_unlicensed_script_mints_nothing_the_licensed_one_lays() {
    for &style in &ALL_STYLES {
        let licensed = typed_script(style, true);
        let cold = typed_script(style, false);
        assert_eq!(
            cold,
            typed_script(style, false),
            "{style:?}: the unlicensed script is nondeterministic — this control \
             cannot mean anything until it is not"
        );
        assert_eq!(
            (licensed.licensed, licensed.declined),
            (10, 0),
            "{style:?}: the positive arm did not admit every authored move"
        );
        assert_eq!(
            (cold.licensed, cold.declined),
            (0, 10),
            "{style:?}: withholding hints did not exercise every denial"
        );
        assert_eq!(licensed.spawns, 10, "{style:?}: an admitted move was lost");
        assert_eq!(cold.spawns, 0, "{style:?}: a denied move minted light");
        assert!(
            licensed.peak_live_light > 0,
            "{style:?}: the licensed control never carried live light"
        );
        assert_eq!(
            cold.peak_live_light, 0,
            "{style:?}: the unlicensed control carried live light"
        );
        if style == GlowStyle::RainbowKitty {
            assert!(
                licensed.peak_ribbon_segments > 0,
                "the licensed rainbow control never laid its ribbon"
            );
            assert_eq!(
                cold.peak_ribbon_segments, 0,
                "the unlicensed rainbow control laid a ribbon"
            );
        }
        assert_ne!(
            licensed.fingerprint, cold.fingerprint,
            "{style:?}: withholding every key hint left the frames unchanged"
        );
    }
}
