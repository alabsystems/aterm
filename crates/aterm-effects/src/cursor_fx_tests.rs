// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_caret_style_law_is_one_truth_table() {
    use CursorStyle::{Bolt, SteadyBar, SteadyBlock, SteadyUnderline};
    let rows = [
        (false, true, None, Some(SteadyBlock)),
        (false, true, Some(SteadyBlock), Some(SteadyBlock)),
        (false, true, Some(SteadyBar), Some(SteadyBlock)),
        (false, true, Some(SteadyUnderline), Some(SteadyBlock)),
        (false, false, None, None),
        (false, false, Some(SteadyBlock), Some(SteadyBlock)),
        (false, false, Some(SteadyBar), Some(SteadyBar)),
        (false, false, Some(SteadyUnderline), Some(SteadyUnderline)),
        (true, true, Some(SteadyBlock), Some(Bolt)),
        (true, false, None, Some(Bolt)),
    ];
    for (bolt, rainbow, momentum, want) in rows {
        assert_eq!(
            compose_caret_style_override(bolt, rainbow, momentum),
            want,
            "bolt={bolt} rainbow={rainbow} momentum={momentum:?}"
        );
    }
}

#[test]
fn the_momentum_engine_yields_the_caret_cell_to_the_rainbow() {
    assert!(!momentum_glow_allowed(true, true, true));
    assert!(momentum_glow_allowed(true, true, false));
    assert!(
        !momentum_glow_allowed(false, true, false),
        "the user's switch is honoured"
    );
    assert!(
        !momentum_glow_allowed(true, false, false),
        "and so are the host's gates"
    );
}

#[test]
fn every_effect_body_fades_to_the_terminal_cursor_through_one_seam() {
    let terminal_base = 0x0010_2030;
    let pinned_trail_base = 0x0000_FF00;
    let effect_fill = 0x00E0_C090;
    for owner in [
        BlockFillOwner::Rainbow,
        BlockFillOwner::Forge,
        BlockFillOwner::Phaser,
        BlockFillOwner::Bolt,
        BlockFillOwner::Comet,
        BlockFillOwner::Droplet,
        BlockFillOwner::BeamRod,
    ] {
        let identity = matches!(owner, BlockFillOwner::Forge | BlockFillOwner::Droplet);
        let candidate = BlockFill {
            owner,
            fill: effect_fill,
            base: (!identity).then_some(pinned_trail_base),
        };
        assert_eq!(
            project_block_fill(Some(candidate), terminal_base, 1.0),
            Some(candidate),
            "{} must preserve the full-amplitude endpoint",
            owner.label()
        );
        for envelope in [0.75, 0.5, 0.25, 0.001] {
            let projected = project_block_fill(Some(candidate), terminal_base, envelope)
                .expect("a positive envelope retains body custody");
            assert_eq!(
                projected.fill,
                aterm_render::blend_rgb(
                    terminal_base,
                    effect_fill,
                    (envelope * f32::from(u8::MAX)).round() as u8,
                ),
                "{} diverged from the shared fade at {envelope}",
                owner.label()
            );
            assert_eq!(projected.base, candidate.base, "provenance is preserved");
        }
        assert_eq!(
            project_block_fill(Some(candidate), terminal_base, 0.0),
            None,
            "{} must return exact-zero custody to the terminal",
            owner.label()
        );
    }
    assert_eq!(project_block_fill(None, terminal_base, 1.0), None);
    assert_eq!(
        project_block_fill(
            Some(BlockFill {
                owner: BlockFillOwner::Rainbow,
                fill: effect_fill,
                base: Some(pinned_trail_base),
            }),
            terminal_base,
            f32::NAN,
        ),
        None,
        "non-finite envelopes fail closed"
    );
}

const fn frame_input(now: Instant) -> CursorFxInput {
    CursorFxInput {
        now,
        cur: Some((2, 3)),
        live_viewport: true,
        cursor_style: CursorStyle::SteadyBar,
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

#[test]
fn the_automatic_glow_colour_law_is_one_truth_table() {
    use crate::cursor_glow::{
        BEAM_DEFAULT_COLOR, COMET_DEFAULT_COLOR, LASER_DEFAULT_COLOR, SPARKLE_DEFAULT_COLOR,
    };
    let live = 0x00FF_40C0;
    for (style, pinned, want) in [
        (GlowStyle::Lumen, false, live),
        (GlowStyle::Lumen, true, live),
        (GlowStyle::RainbowKitty, false, live),
        (GlowStyle::Phaser, false, live),
        (GlowStyle::Beam, false, BEAM_DEFAULT_COLOR),
        (GlowStyle::Beam, true, live),
        (GlowStyle::Comet, false, COMET_DEFAULT_COLOR),
        (GlowStyle::Comet, true, live),
        (GlowStyle::Sparkle, false, SPARKLE_DEFAULT_COLOR),
        (GlowStyle::Sparkle, true, live),
        (GlowStyle::Laser, false, LASER_DEFAULT_COLOR),
        (GlowStyle::Laser, true, LASER_DEFAULT_COLOR),
    ] {
        assert_eq!(
            automatic_glow_color(style, live, pinned),
            want,
            "{style:?} with pinned={pinned}"
        );
    }
    let resolve = |style, color| {
        resolve_glow_config(
            GlowKnobs {
                enabled: true,
                color,
                accent: None,
                duration_ms: 260,
                length: 24,
                intensity: 1.0,
                radius: 0.6,
                ring: true,
            },
            GlowSpelling {
                style: Some(style),
                beam: true,
                ribbon_tall: false,
                ribbon_flat: false,
                classic_mono: false,
            },
            live,
            true,
            0x00C8_D3F5,
            0x001A_1B26,
            0.5,
        )
    };
    for style in [GlowStyle::Lumen, GlowStyle::Comet, GlowStyle::Laser] {
        let cfg = resolve(style, None);
        assert_eq!(cfg.color, automatic_glow_color(style, live, false));
        assert_eq!(cfg.accent, brighten(cfg.color, 1.5));
        assert_eq!(
            resolve(style, Some(0x0012_3456)).color,
            0x0012_3456,
            "{style:?}: an explicit cursor_trail_color beats the law"
        );
    }
}

const fn comet_cfgs(knob: f32) -> (GlowConfig, TrailConfig) {
    let glow = resolve_glow_config(
        GlowKnobs {
            enabled: true,
            color: None,
            accent: None,
            duration_ms: 260,
            length: 24,
            intensity: knob,
            radius: 0.6,
            ring: true,
        },
        GlowSpelling {
            style: Some(GlowStyle::Comet),
            beam: true,
            ribbon_tall: false,
            ribbon_flat: false,
            classic_mono: false,
        },
        0x0050_FA7B,
        true,
        0x00C8_D3F5,
        0x001A_1B26,
        0.5,
    );
    let trail = TrailConfig {
        enabled: true,
        duration: std::time::Duration::from_millis(260),
        max_len: 24,
        color: glow.color,
        intensity: 0.0,
        warmth: 0.0,
    };
    (glow, trail)
}

fn typed_comet_burst(knob: f32) -> (Option<u32>, u8, usize) {
    let (glow, trail) = comet_cfgs(knob);
    let mut fx = CursorFx::default();
    let t0 = Instant::now();
    let input = |now, col| CursorFxInput {
        cur: Some((2, col)),
        cursor_style: CursorStyle::SteadyBlock,
        trail_brightness: knob,
        ..frame_input(now)
    };
    fx.tick(&input(t0, 3), &glow, trail);
    let (mut fill, mut alpha_max, mut cells) = (None, 0u8, 0usize);
    for k in 0..20u16 {
        let now = t0 + std::time::Duration::from_millis(60 * (u64::from(k) + 1));
        fx.glow.note_synthetic_typed(now, 1);
        fx.trail.note_synthetic_typed(now);
        fx.cadence.on_keystroke(now);
        let frame = fx.tick(&input(now, 4 + k), &glow, trail);
        fill = frame
            .block_fill
            .filter(|owned| owned.owner == BlockFillOwner::Comet)
            .map(|owned| owned.fill);
        cells += fx.trail_scratch.len();
        let brightest = fx.trail_scratch.iter().map(|cell| cell.alpha).max();
        alpha_max = alpha_max.max(brightest.unwrap_or(0));
    }
    (fill, alpha_max, cells)
}

#[test]
fn the_brightness_dial_dims_the_comet_nucleus_and_its_bed() {
    let luma = |rgb: u32| (rgb >> 16 & 0xff) + (rgb >> 8 & 0xff) + (rgb & 0xff);
    let (full_fill, full_alpha, full_cells) = typed_comet_burst(1.0);
    let full_fill = full_fill.expect("control: a full dial's comet owns the block caret");
    assert!(
        full_cells > 0 && full_alpha > 0,
        "control: a full dial lays a bed under a fast burst"
    );
    let (quarter_fill, quarter_alpha, quarter_cells) = typed_comet_burst(0.25);
    let quarter_fill = quarter_fill.expect("a quarter dial still draws the nucleus");
    assert!(
        luma(quarter_fill) < luma(full_fill),
        "a quarter dial must frost the nucleus less: {quarter_fill:06x} vs {full_fill:06x}"
    );
    assert!(
        quarter_cells > 0 && quarter_alpha < full_alpha,
        "a quarter dial must lay a fainter bed: max alpha {quarter_alpha} vs {full_alpha}"
    );
    let (off_fill, _, off_cells) = typed_comet_burst(0.0);
    assert_eq!(off_fill, None, "a zero dial draws no nucleus");
    assert_eq!(off_cells, 0, "a zero dial lays no bed");
}

fn classic_cfg(enabled: bool) -> (GlowConfig, TrailConfig) {
    let glow = GlowConfig {
        ribbon_tall: false,
        enabled,
        style: GlowStyle::Classic,
        intensity: 0.7,
        audible: true,
        radius: 0.6,
        ring: true,
        beam: true,
        head_dx: 0.08,
        ..Default::default()
    };
    let trail = TrailConfig {
        enabled: false,
        duration: std::time::Duration::from_millis(260),
        max_len: 24,
        color: 0x0050_FA7B,
        intensity: 0.0,
        warmth: 0.0,
    };
    (glow, trail)
}

#[test]
fn the_momentum_glow_obeys_its_own_switch() {
    for momentum_glow in [true, false] {
        let mut fx = CursorFx::default();
        let t0 = Instant::now();
        for k in 0..24u64 {
            fx.momentum.on_key(
                t0 + std::time::Duration::from_millis(k * 40),
                MOMENTUM_GLOW_TAU_S,
            );
        }
        let now = t0 + std::time::Duration::from_millis(24 * 40);
        let (glow, trail) = classic_cfg(false);
        let frame = fx.tick(
            &CursorFxInput {
                momentum_glow,
                ..frame_input(now)
            },
            &glow,
            trail,
        );
        let momentum_owns = frame
            .block_fill
            .is_some_and(|owned| owned.owner == BlockFillOwner::Momentum);
        if momentum_glow {
            assert!(
                fx.momentum.is_active() || !fx.glow_scratch.is_empty(),
                "control: a fast hand lights the momentum glow with its switch on"
            );
        } else {
            assert!(
                fx.glow_scratch.is_empty() && !momentum_owns && frame.momentum_steady.is_none(),
                "the switch off paints no momentum halo and pins no caret"
            );
        }
    }
}

#[test]
fn a_cooling_momentum_glow_is_a_settling_body() {
    let lane = std::time::Duration::from_micros(16_667);
    let mut fx = CursorFx::default();
    let t0 = Instant::now();
    for k in 0..24u64 {
        fx.momentum.on_key(
            t0 + std::time::Duration::from_millis(k * 60),
            MOMENTUM_GLOW_TAU_S,
        );
    }
    let (glow, trail) = classic_cfg(false);
    let mut now = t0 + std::time::Duration::from_millis(23 * 60);
    loop {
        fx.tick(&frame_input(now), &glow, trail);
        if !fx.bodies_active() {
            break;
        }
        now += lane;
        assert!(
            now.saturating_duration_since(t0) < std::time::Duration::from_secs(10),
            "fixture: the glow's frames never stopped moving"
        );
    }
    assert!(
        !fx.glow_scratch.is_empty(),
        "negative control: the cadence let go with the halo on glass"
    );
    assert!(
        fx.bodies_settling() && fx.momentum.settling() && !fx.rainbow.settling(),
        "the cooling glow holds the lane"
    );
    let next = fx
        .bodies_next_change_deadline(now, lane)
        .expect("a settling glow names its next step");
    assert_eq!(Some(next), fx.momentum.next_change_deadline(now, lane));
    assert!(next >= now + lane, "never sooner than a frame out");
}

#[test]
fn effect_caret_fills_keep_the_page_floor_unless_they_wear_a_pinned_cursor() {
    use crate::color_math::{contrast_ratio, rgb2hsv};
    let owners = [
        BlockFillOwner::Rainbow,
        BlockFillOwner::Forge,
        BlockFillOwner::Phaser,
        BlockFillOwner::Bolt,
        BlockFillOwner::Comet,
        BlockFillOwner::Droplet,
        BlockFillOwner::BeamRod,
        BlockFillOwner::Momentum,
    ];
    for (ground, low, live) in [
        (0x0011_1318u32, 0x0051_0A00u32, 0x0050_FA7Bu32),
        (0x00FF_FFFF, 0x00B4_8CFF, 0x0009_69DA),
    ] {
        for owner in owners {
            let candidate = BlockFill {
                owner,
                fill: low,
                base: None,
            };
            let floored = floor_block_fill(candidate, ground, live, None, false);
            let bar = match owner {
                BlockFillOwner::Forge | BlockFillOwner::Phaser => {
                    CARET_PAGE_MIN_CONTRAST + EFFECT_CARET_PAGE_GUARD
                }
                _ => CARET_PAGE_MIN_CONTRAST,
            };
            assert_eq!(caret_page_floor(owner), bar, "{}", owner.label());
            assert!(
                contrast_ratio(floored.fill, ground) >= bar,
                "{} {low:06x} on {ground:06x} left at {:06x}",
                owner.label(),
                floored.fill
            );
            assert_eq!((floored.owner, floored.base), (owner, None));
            let (h0, _, _) = rgb2hsv(low);
            let (h1, _, _) = rgb2hsv(floored.fill);
            assert!((h0 - h1).abs() < 4.0, "{}: the hue moved", owner.label());
            let fine = BlockFill {
                fill: if ground == 0x00FF_FFFF {
                    0x0009_69DA
                } else {
                    0x00E0_C090
                },
                ..candidate
            };
            assert_eq!(floor_block_fill(fine, ground, live, None, false), fine);
        }
    }
    let pinned = 0x0000_00FF;
    for owner in [BlockFillOwner::Rainbow, BlockFillOwner::BeamRod] {
        let worn = BlockFill {
            owner,
            fill: pinned,
            base: Some(pinned),
        };
        assert_eq!(
            floor_block_fill(worn, 0x0011_1318, pinned, Some(pinned), false),
            worn
        );
        assert_ne!(
            floor_block_fill(worn, 0x0011_1318, 0x0050_FA7B, None, false).fill,
            pinned,
            "control: unpinned, the same {} fill is floored",
            owner.label()
        );
    }
    let bolt = BlockFill {
        owner: BlockFillOwner::Bolt,
        fill: 0x00B4_8CFF,
        base: Some(0x00B4_8CFF),
    };
    assert_ne!(
        floor_block_fill(bolt, 0x00FF_FFFF, 0x0000_00FF, Some(0x0000_00FF), false),
        bolt
    );
    let (latte, rosewater) = (0x00EF_F1F5, 0x00DC_8A78);
    let tint = BlockFill {
        owner: BlockFillOwner::Momentum,
        fill: rosewater,
        base: None,
    };
    assert_eq!(floor_block_fill(tint, latte, rosewater, None, false), tint);
    let forge = BlockFill {
        owner: BlockFillOwner::Forge,
        ..tint
    };
    assert!(
        contrast_ratio(
            floor_block_fill(forge, latte, rosewater, None, false).fill,
            latte
        ) >= CARET_PAGE_MIN_CONTRAST
    );
}

fn style_cfgs(style: GlowStyle, page: u32) -> (GlowConfig, TrailConfig) {
    let dark = aterm_render::theme_is_dark(page);
    let glow = resolve_glow_config(
        GlowKnobs {
            enabled: true,
            color: None,
            accent: None,
            duration_ms: 260,
            length: 24,
            intensity: 1.0,
            radius: 0.6,
            ring: true,
        },
        GlowSpelling {
            style: Some(style),
            beam: true,
            ribbon_tall: false,
            ribbon_flat: false,
            classic_mono: false,
        },
        if dark { 0x0050_FA7B } else { 0x0009_69DA },
        dark,
        if dark { 0x00C8_D3F5 } else { 0x0024_292F },
        page,
        0.5,
    );
    let trail = TrailConfig {
        enabled: matches!(style, GlowStyle::Comet),
        duration: std::time::Duration::from_millis(260),
        max_len: 24,
        color: glow.color,
        intensity: 0.0,
        warmth: 0.0,
    };
    (glow, trail)
}

#[test]
fn every_effect_caret_keeps_the_page_floor_while_typing_and_at_rest() {
    for page in [0x0011_1318u32, 0x0028_2A36, 0x00FF_FFFF, 0x00FD_F6E3] {
        let dark = aterm_render::theme_is_dark(page);
        for style in [
            GlowStyle::Laser,
            GlowStyle::Lumen,
            GlowStyle::Beam,
            GlowStyle::Sparkle,
            GlowStyle::Water,
            GlowStyle::Comet,
            GlowStyle::Fire,
            GlowStyle::Phaser,
            GlowStyle::RainbowKitty,
        ] {
            let (glow, trail) = style_cfgs(style, page);
            let mut fx = CursorFx::default();
            let t0 = Instant::now();
            let input = |now, col: u16| CursorFxInput {
                cur: Some((2, col)),
                cursor_style: CursorStyle::SteadyBlock,
                live_cursor: if dark { 0x0050_FA7B } else { 0x0009_69DA },
                default_bg: page,
                momentum_glow: false,
                ..frame_input(now)
            };
            let mut hottest = 0.0f32;
            for k in 0..25u16 {
                let now = if k < 24 {
                    let now = t0 + std::time::Duration::from_millis(50 * u64::from(k));
                    fx.glow.note_synthetic_typed(now, 1);
                    fx.trail.note_synthetic_typed(now);
                    fx.cadence.on_keystroke(now);
                    now
                } else {
                    t0 + std::time::Duration::from_secs(12)
                };
                let frame = fx.tick(&input(now, 4 + k.min(23)), &glow, trail);
                hottest = hottest.max(fx.glow.blaze());
                let owned = frame
                    .block_fill
                    .unwrap_or_else(|| panic!("{style:?} on {page:06x}: a body owns the caret"));
                let page_ratio = crate::color_math::contrast_ratio(owned.fill, page);
                let bar = if owned.owner == BlockFillOwner::Rainbow {
                    3.0
                } else {
                    CARET_PAGE_MIN_CONTRAST
                };
                assert!(
                    page_ratio >= bar,
                    "{style:?} on {page:06x} frame {k}: {} fill {:06x} at {page_ratio:.2}:1",
                    owned.owner.label(),
                    owned.fill
                );
            }
            assert!(
                hottest > 0.8,
                "{style:?} on {page:06x}: the burst never ran hot (blaze {hottest:.2})"
            );
        }
    }
}

#[test]
fn the_caret_floor_holds_against_the_cell_it_sits_on() {
    use crate::color_math::contrast_ratio;
    const PAGE: u32 = 0x0011_1318;
    let run = |style: GlowStyle, cell: Option<u32>| -> Vec<u32> {
        let (glow, trail) = style_cfgs(style, PAGE);
        let mut fx = CursorFx::default();
        let t0 = Instant::now();
        let input = |now, col: u16| CursorFxInput {
            cur: Some((2, col)),
            cursor_style: CursorStyle::SteadyBlock,
            default_bg: PAGE,
            cursor_cell_bg: cell,
            momentum_glow: false,
            ..frame_input(now)
        };
        (0..25u16)
            .map(|k| {
                let now = if k < 24 {
                    let now = t0 + std::time::Duration::from_millis(50 * u64::from(k));
                    fx.glow.note_synthetic_typed(now, 1);
                    fx.trail.note_synthetic_typed(now);
                    fx.cadence.on_keystroke(now);
                    now
                } else {
                    t0 + std::time::Duration::from_secs(12)
                };
                fx.tick(&input(now, 4 + k.min(23)), &glow, trail)
                    .block_fill
                    .unwrap_or_else(|| panic!("{style:?}: a body owns the caret"))
                    .fill
            })
            .collect()
    };
    let (reverse, band) = (0x00D0_D0D0u32, 0x003B_4261u32);
    let mut sunk = false;
    for style in [
        GlowStyle::Fire,
        GlowStyle::Laser,
        GlowStyle::Lumen,
        GlowStyle::Water,
        GlowStyle::RainbowKitty,
    ] {
        for cell in [reverse, band] {
            let bar = if style == GlowStyle::RainbowKitty {
                caret_page_floor(BlockFillOwner::Rainbow)
            } else {
                CARET_PAGE_MIN_CONTRAST
            };
            for (k, fill) in run(style, Some(cell)).into_iter().enumerate() {
                let ratio = contrast_ratio(fill, cell);
                assert!(
                    ratio >= bar,
                    "{style:?} on cell {cell:06x} frame {k}: {fill:06x} at {ratio:.2}:1"
                );
            }
        }
        sunk |= run(style, None)
            .into_iter()
            .any(|fill| contrast_ratio(fill, reverse) < 3.0);
    }
    assert!(
        sunk,
        "control: with the page standing in, some caret must sink on the reverse cell"
    );
    let (glow, trail) = style_cfgs(GlowStyle::RainbowKitty, PAGE);
    let pinned = 0x00A0_A0A0;
    let fill = |cell: Option<u32>| {
        CursorFx::default()
            .tick(
                &CursorFxInput {
                    cursor_style: CursorStyle::SteadyBlock,
                    default_bg: PAGE,
                    cursor_cell_bg: cell,
                    live_cursor: pinned,
                    cursor_color_pinned: true,
                    momentum_glow: false,
                    ..frame_input(Instant::now())
                },
                &glow,
                trail,
            )
            .block_fill
            .expect("the rainbow wears the pinned colour")
            .fill
    };
    let worn = fill(Some(reverse));
    assert_eq!(
        worn,
        fill(None),
        "a pinned colour is the user's on any cell"
    );
    assert!(
        contrast_ratio(worn, reverse) < CARET_PAGE_MIN_CONTRAST,
        "the pinned fill {worn:06x} must sit under the floor on the reverse cell"
    );
}

#[test]
fn the_light_momentum_glow_wears_the_carets_hue_beneath_the_letters() {
    use crate::color_math::rgb2hsv;
    const PAGE: u32 = 0x00FF_FFFF;
    const CARET: u32 = 0x0009_69DA;
    const STYLE: u32 = 0x00CF_222E;
    let deepest_hue = |style: Option<GlowStyle>| {
        let (mut glow, trail) = style_cfgs(style.unwrap_or(GlowStyle::Lumen), PAGE);
        glow.enabled = style.is_some();
        glow.color = STYLE;
        let mut fx = CursorFx::default();
        let now = Instant::now();
        for _ in 0..12 {
            fx.momentum.on_key(now, MOMENTUM_GLOW_TAU_S);
        }
        fx.tick(
            &CursorFxInput {
                cursor_style: CursorStyle::SteadyBlock,
                live_cursor: CARET,
                default_bg: PAGE,
                user_tinted: style.is_some(),
                accent_pinned: false,
                ..frame_input(now)
            },
            &glow,
            trail,
        );
        assert!(
            fx.glow_scratch.iter().all(|q| q.alpha > 0),
            "{style:?}: no additive light on white"
        );
        let deepest = fx
            .glow
            .under_quads()
            .iter()
            .max_by_key(|q| q.alpha)
            .unwrap_or_else(|| panic!("{style:?}: the glow lies beneath the letters"));
        let a = u32::from(deepest.alpha).max(1);
        let ch = |sh: u32| (((deepest.color >> sh) & 0xff) * 255 / a).min(255);
        rgb2hsv((ch(16) << 16) | (ch(8) << 8) | ch(0)).0
    };
    let (caret_hue, _, _) = rgb2hsv(CARET);
    let off = deepest_hue(None);
    assert!(
        (off - caret_hue).abs() < 15.0,
        "off: the glow wears the caret's hue ({off:.0} vs {caret_hue:.0})"
    );
    let lit = deepest_hue(Some(GlowStyle::Lumen));
    assert!(
        (lit - off).abs() > 90.0,
        "control: a lit style's own colour ({lit:.0}) is not the caret's ({off:.0})"
    );
}

#[test]
fn a_pinned_cursor_colour_is_the_caret_at_rest() {
    const PINNED: u32 = 0x00FF_3399;
    for page in [0x0011_1318u32, 0x00FF_FFFF, 0x00F6_F7F9] {
        for style in [
            GlowStyle::Comet,
            GlowStyle::Lumen,
            GlowStyle::Sparkle,
            GlowStyle::Beam,
            GlowStyle::Phaser,
            GlowStyle::RainbowKitty,
        ] {
            let run = |pinned: bool| -> (Vec<Option<u32>>, Option<u32>) {
                let (mut glow, mut trail) = style_cfgs(style, page);
                glow.color = PINNED;
                glow.accent = brighten(PINNED, 1.5);
                trail.color = PINNED;
                let mut fx = CursorFx::default();
                let t0 = Instant::now();
                let input = |now, col: u16| CursorFxInput {
                    cur: Some((2, col)),
                    cursor_style: CursorStyle::SteadyBlock,
                    live_cursor: PINNED,
                    cursor_color_pinned: pinned,
                    user_tinted: !pinned,
                    accent_pinned: false,
                    default_bg: page,
                    momentum_glow: false,
                    ..frame_input(now)
                };
                let fill = |frame: CursorFxFrame| frame.block_fill.map(|owned| owned.fill);
                let burst = (0..24u16)
                    .map(|k| {
                        let now = t0 + std::time::Duration::from_millis(50 * u64::from(k));
                        fx.glow.note_synthetic_typed(now, 1);
                        fx.trail.note_synthetic_typed(now);
                        fx.cadence.on_keystroke(now);
                        fill(fx.tick(&input(now, 4 + k), &glow, trail))
                    })
                    .collect();
                let rest = t0 + std::time::Duration::from_secs(12);
                (burst, fill(fx.tick(&input(rest, 27), &glow, trail)))
            };
            let (burst, rest) = run(true);
            let rest = rest.unwrap_or(PINNED);
            assert_eq!(
                rest, PINNED,
                "{style:?} on {page:06x}: the settled caret is {rest:06x}, not the pinned \
                 {PINNED:06x}"
            );
            assert!(
                burst.iter().flatten().any(|&fill| fill != PINNED),
                "{style:?} on {page:06x}: the burst animates around the pinned base"
            );
            let (_, unpinned) = run(false);
            assert!(
                unpinned.is_some_and(|fill| fill != PINNED),
                "control: {style:?} on {page:06x} rests at its own look's fill, \
                 {unpinned:06x?}"
            );
        }
    }
}

/// A COLOUR THE USER CHOSE IS NEVER SATURATED ON PAPER
/// ([`crate::color_math::light_caret_fill`]): a caret charged on a light page wears a chosen
/// colour (the trail colour, or a pinned cursor colour) darkened in its own hue and
/// saturation, so every frame's fill keeps the pin's hue and no more than its chroma, and the
/// charged caret still reads on the page. Sparkle pinned to `#b0b0c8` charged to navy
/// `#000079` on paper, chroma 0.475 against the pin's 0.094 (review9fix round 1's judge,
/// the gallery's `sparkle-pin` on GitHub Light and Default Light). Control:
/// [`CursorFx::chosen_unread`], which reads no colour as chosen.
#[test]
fn a_chosen_colour_charges_in_its_own_hue_and_chroma_on_paper() {
    use crate::color_math::{contrast_ratio, rgb2hsv};
    const PIN: u32 = 0x00B0_B0C8;
    let chroma = |c: u32| {
        let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
        r.max(g).max(b) - r.min(g).min(b)
    };
    let default_light = aterm_types::scheme::builtin("Default Light")
        .expect("Default Light ships")
        .to_theme_parts()
        .bg
        & 0x00FF_FFFF;
    let run = |style: GlowStyle, page: u32, cursor_pin: bool, unread: bool| -> Vec<u32> {
        let (mut glow, mut trail) = style_cfgs(style, page);
        glow.color = PIN;
        glow.accent = brighten(PIN, 1.5);
        trail.color = PIN;
        let mut fx = CursorFx {
            chosen_unread: unread,
            ..CursorFx::default()
        };
        let t0 = Instant::now();
        let input = |now, col: u16| CursorFxInput {
            cur: Some((2, col)),
            cursor_style: CursorStyle::SteadyBlock,
            live_cursor: if cursor_pin { PIN } else { 0x0009_69DA },
            cursor_color_pinned: cursor_pin,
            user_tinted: !cursor_pin,
            accent_pinned: false,
            default_bg: page,
            momentum_glow: false,
            ..frame_input(now)
        };
        (0..24u16)
            .filter_map(|k| {
                let now = t0 + std::time::Duration::from_millis(50 * u64::from(k));
                fx.glow.note_synthetic_typed(now, 1);
                fx.trail.note_synthetic_typed(now);
                fx.cadence.on_keystroke(now);
                fx.tick(&input(now, 4 + k), &glow, trail)
                    .block_fill
                    .map(|owned| owned.fill)
            })
            .collect()
    };
    let (pin_hue, _, _) = rgb2hsv(PIN);
    let mut control_saturated = false;
    for page in [0x00FF_FFFF, default_light] {
        for style in [
            GlowStyle::Sparkle,
            GlowStyle::Beam,
            GlowStyle::Lumen,
            GlowStyle::Comet,
            GlowStyle::Laser,
        ] {
            for cursor_pin in [false, true] {
                let fills = run(style, page, cursor_pin, false);
                let charged = fills.iter().copied().filter(|&f| f != PIN);
                assert!(
                    charged.clone().count() > 0,
                    "{style:?} on {page:06x} (cursor pin {cursor_pin}): the burst charges the \
                     caret: {fills:06x?}"
                );
                for fill in charged.clone() {
                    let (hue, _, _) = rgb2hsv(fill);
                    assert!(
                        chroma(fill) <= chroma(PIN) + 1 && (hue - pin_hue).abs() < 6.0,
                        "{style:?} on {page:06x} (cursor pin {cursor_pin}): the charged \
                         caret {fill:06x} (chroma {}, hue {hue:.0}) recolours the chosen \
                         {PIN:06x} (chroma {}, hue {pin_hue:.0})",
                        chroma(fill),
                        chroma(PIN)
                    );
                }
                let deepest = charged
                    .map(|f| contrast_ratio(f, page))
                    .fold(0.0f32, f32::max);
                assert!(
                    deepest >= CARET_PAGE_MIN_CONTRAST,
                    "{style:?} on {page:06x} (cursor pin {cursor_pin}): the charged caret \
                     reads at {deepest:.2}:1"
                );
                control_saturated |= run(style, page, cursor_pin, true)
                    .into_iter()
                    .any(|f| chroma(f) > 2 * chroma(PIN));
            }
        }
    }
    assert!(
        control_saturated,
        "control: concentrated as a style's own colour, some charged caret on paper wears \
         more than twice the pin's chroma"
    );
}

/// A COLOUR THE USER CHOSE IS NEVER RE-SATURATED BY THE TYPING-MOMENTUM GLOW
/// ([`MomentumGlowConfig::chosen_hue`], [`MomentumGlowConfig::chosen_base`]): beside a
/// chosen colour (Sparkle's or Classic's pinned trail colour, or a pinned cursor colour with
/// no trail), every pixel of the paper halo keeps the pin's hue and no more than its chroma,
/// and a caret it charges in the chosen colour keeps them on paper and on a dark page.
/// Beside Sparkle pinned to `#b0b0c8` (chroma 0.094) the paper halo was laid up to
/// `#c1c1ee`, chroma 0.176, its ink walked from the hue's vivid ink (review9fix round 2's
/// judge, the gallery's `sparkle-pin` on paper), and a pinned cursor colour's charged caret
/// went navy on paper and amber on a dark page. The three cases take the wiring's three
/// hues (Sparkle's paper gold, a style's rod, the cursor's own). Control:
/// [`CursorFx::momentum_chosen_unread`], the momentum glow alone reading no colour as
/// chosen, which lays each case's paper halo off the pin.
/// After-land lows W10: on a dark page Classic's one-colour face wears its momentum glow in
/// the pin, as every pinned style's lights are worn ([`CursorGlow::hold_classic_momentum`]).
/// Its warm white, unheld, lay on the pin's neutral ground as a grey disc round the caret on
/// Solarized Dark after an Enter (`classic-pink/solarized-dark/...e470`). Every light of a
/// burst beside the pink pin on Solarized Dark keeps the pin's hue; the control, the glow
/// unheld, lays warm (amber to white) lights there.
#[test]
fn classic_pink_s_momentum_glow_wears_the_pin() {
    use crate::color_math::rgb2hsv;
    const PIN: u32 = 0x00D0_3A8C;
    const SOLARIZED_DARK: u32 = 0x0000_2B36;
    let (pin_hue, _, _) = rgb2hsv(PIN);
    // The momentum glow's lights over a burst: the over stream's lights a twin with no
    // momentum glow, fed the same frames, does not lay. Of them, (lit, off the pin): a lit
    // one is off the pin when it wears no hue the eye names or one more than 40 degrees
    // from the pin's.
    let momentum_lights = |unheld: bool| -> (usize, usize) {
        let (mut glow, mut trail) = style_cfgs(GlowStyle::Classic, SOLARIZED_DARK);
        glow.color = PIN;
        glow.accent = brighten(PIN, 1.5);
        glow.classic_mono = true;
        trail.color = PIN;
        let mut fx = CursorFx::default();
        fx.glow.classic_momentum_unheld = unheld;
        let mut twin = CursorFx::default();
        let t0 = Instant::now();
        let (mut lit, mut off) = (0usize, 0usize);
        for k in 0..24u16 {
            let now = t0 + std::time::Duration::from_millis(50 * u64::from(k));
            for f in [&mut fx, &mut twin] {
                f.glow.note_synthetic_typed(now, 1);
                f.trail.note_synthetic_typed(now);
                f.cadence.on_keystroke(now);
            }
            fx.momentum
                .on_key(now, crate::cursor_momentum::MOMENTUM_GLOW_TAU_S);
            let input = |momentum_glow| CursorFxInput {
                cur: Some((2, 4 + k)),
                cursor_style: CursorStyle::SteadyBlock,
                live_cursor: 0x0093_A1A1,
                user_tinted: true,
                default_bg: SOLARIZED_DARK,
                momentum_glow,
                ..frame_input(now)
            };
            let _ = fx.tick(&input(true), &glow, trail);
            let _ = twin.tick(&input(false), &glow, trail);
            let mut own: Vec<GlowQuad> = twin.glow_scratch.clone();
            for q in &fx.glow_scratch {
                if let Some(i) = own.iter().position(|o| o == q) {
                    own.swap_remove(i);
                    continue;
                }
                for c in [q.color, q.color2] {
                    let (h, sat, v) = rgb2hsv(c & 0x00FF_FFFF);
                    if v < 16.0 / 255.0 {
                        continue;
                    }
                    lit += 1;
                    let d = (h - pin_hue).abs();
                    if sat < 0.25 || d.min(360.0 - d) > 40.0 {
                        off += 1;
                    }
                }
            }
        }
        (lit, off)
    };
    let (lit, off) = momentum_lights(false);
    assert!(lit > 0, "fixture: the burst lays a momentum glow");
    assert_eq!(off, 0, "{off} of {lit} momentum lights wear no pin's hue");
    let (_, control) = momentum_lights(true);
    assert!(
        control > 0,
        "control: the unheld momentum glow lays its warm white"
    );
}

#[test]
fn the_momentum_glow_wears_a_chosen_colour_in_its_own_hue_and_chroma() {
    use crate::color_math::rgb2hsv;
    const PIN: u32 = 0x00B0_B0C8;
    let chroma = |c: u32| {
        let (r, g, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
        r.max(g).max(b) - r.min(g).min(b)
    };
    let (pin_hue, _, _) = rgb2hsv(PIN);
    let default_light = aterm_types::scheme::builtin("Default Light")
        .expect("Default Light ships")
        .to_theme_parts()
        .bg
        & 0x00FF_FFFF;
    // What a burst draws beside the chosen colour: the halo's pixels (paper only: a dark
    // page's halo is additive, the momentum's own warm light), its quads laid as the
    // renderer lays them over a neutral white, so a tinted page's own hue (Default Light's
    // `#f6f7f9`) does not read as the halo's, and the caret fills the momentum glow owns.
    // The halo's quads are the ones a twin with no momentum glow, fed the same frames,
    // does not lay: the momentum glow's alone, not the style's own marks under the letters.
    let run = |style: Option<GlowStyle>, page: u32, unread: bool| -> (Vec<u32>, Vec<u32>) {
        let (mut glow, mut trail) = style_cfgs(style.unwrap_or(GlowStyle::Sparkle), page);
        let cursor_pin = style.is_none();
        if cursor_pin {
            glow.enabled = false;
        } else {
            glow.color = PIN;
            glow.accent = brighten(PIN, 1.5);
            // A pinned colour selects Classic's one-colour face (`resolve_glow_config`).
            glow.classic_mono = matches!(style, Some(GlowStyle::Classic));
            trail.color = PIN;
        }
        let mut fx = CursorFx {
            momentum_chosen_unread: unread,
            ..CursorFx::default()
        };
        let mut twin = CursorFx::default();
        let t0 = Instant::now();
        let geom = frame_input(t0).geom;
        let (w, h) = (usize::from(geom.win_w), usize::from(geom.win_h));
        let (mut halo, mut carets) = (Vec::new(), Vec::new());
        for k in 0..24u16 {
            let now = t0 + std::time::Duration::from_millis(50 * u64::from(k));
            for f in [&mut fx, &mut twin] {
                f.glow.note_synthetic_typed(now, 1);
                f.trail.note_synthetic_typed(now);
                f.cadence.on_keystroke(now);
            }
            fx.momentum
                .on_key(now, crate::cursor_momentum::MOMENTUM_GLOW_TAU_S);
            let input = |momentum_glow| CursorFxInput {
                cur: Some((2, 4 + k)),
                cursor_style: CursorStyle::SteadyBlock,
                live_cursor: if cursor_pin { PIN } else { 0x0009_69DA },
                cursor_color_pinned: cursor_pin,
                user_tinted: !cursor_pin,
                accent_pinned: false,
                default_bg: page,
                momentum_glow,
                ..frame_input(now)
            };
            if let Some(owned) = fx
                .tick(&input(true), &glow, trail)
                .block_fill
                .filter(|owned| owned.owner == BlockFillOwner::Momentum)
            {
                carets.push(owned.fill);
            }
            let _ = twin.tick(&input(false), &glow, trail);
            let (laid, style_own) = (fx.glow.under_quads(), twin.glow.under_quads());
            let (own, halo_quads) = laid.split_at(style_own.len().min(laid.len()));
            assert_eq!(
                own, style_own,
                "frame {k}: the momentum glow lays its halo after the style's own marks"
            );
            if aterm_render::theme_is_dark(page) {
                continue;
            }
            const NEUTRAL: u32 = 0x00FF_FFFF;
            let mut px = vec![NEUTRAL; w * h];
            for q in halo_quads {
                let qw = u32::from(q.w.max(1));
                let at = |i: u32, a: u32, b: u32| {
                    (a * (2 * qw - (2 * i + 1)) + b * (2 * i + 1) + qw) / (2 * qw)
                };
                for y in usize::from(q.y)..(usize::from(q.y) + usize::from(q.h)).min(h) {
                    for x in usize::from(q.x)..(usize::from(q.x) + usize::from(q.w)).min(w) {
                        let i = (x - usize::from(q.x)) as u32;
                        let (color, alpha) = if q.is_flat() {
                            (q.color, q.alpha)
                        } else {
                            let lerp = |sh: u32| {
                                at(i, (q.color >> sh) & 0xff, (q.color2 >> sh) & 0xff) << sh
                            };
                            (
                                lerp(16) | lerp(8) | lerp(0),
                                at(i, u32::from(q.alpha), u32::from(q.alpha2)) as u8,
                            )
                        };
                        let slot = &mut px[y * w + x];
                        *slot = aterm_render::over_premul(*slot, color, alpha);
                    }
                }
            }
            halo.extend(px.into_iter().filter(|&p| p != NEUTRAL));
        }
        (halo, carets)
    };
    let off_pin = |c: u32| {
        let (hue, _, _) = rgb2hsv(c);
        let hue_off = (hue - pin_hue).abs().min(360.0 - (hue - pin_hue).abs());
        chroma(c) > chroma(PIN) + 1 || (chroma(c) >= 8 && hue_off >= 6.0)
    };
    let styles = [Some(GlowStyle::Sparkle), Some(GlowStyle::Classic), None];
    let mut control_halo = [0usize; 3];
    let mut control_carets = 0;
    for page in [0x00FF_FFFF, default_light, 0x0010_1016] {
        for (si, style) in styles.into_iter().enumerate() {
            let at = format!("{style:?} on {page:06x}");
            // Classic's caret wears the theme's cursor colour, which nobody chose; its halo
            // wears the trail's.
            let caret_chosen = !matches!(style, Some(GlowStyle::Classic));
            let (halo, mut carets) = run(style, page, false);
            if !aterm_render::theme_is_dark(page) {
                assert!(!halo.is_empty(), "{at}: the burst lays a paper halo");
            }
            carets.retain(|_| caret_chosen);
            let off: Vec<String> = halo
                .iter()
                .chain(&carets)
                .filter(|&&c| off_pin(c))
                .map(|c| format!("{c:06x}"))
                .collect();
            assert!(
                off.is_empty(),
                "{at}: {} momentum pixels or carets leave the pin {PIN:06x}'s hue or pass \
                 its chroma: {:?}",
                off.len(),
                &off[..off.len().min(8)]
            );
            if style.is_none() {
                assert!(
                    carets.iter().any(|&c| c != PIN),
                    "{at}: the burst charges the pinned caret: {carets:06x?}"
                );
            }
            let (halo, mut carets) = run(style, page, true);
            carets.retain(|_| caret_chosen);
            control_halo[si] += halo.iter().filter(|&&c| off_pin(c)).count();
            control_carets += carets.iter().filter(|&&c| off_pin(c)).count();
        }
    }
    assert!(
        control_halo.iter().all(|&n| n > 0) && control_carets > 0,
        "control: reading no colour as chosen, each style's paper halo ({styles:?}) and the \
         charged caret leave the pin's hue or chroma (halo pixels, carets): \
         {control_halo:?}, {control_carets}"
    );
}

#[test]
fn the_rainbow_kitty_caret_reads_on_every_shipped_light_theme() {
    use crate::color_math::contrast_ratio;
    const BAR: f32 = 3.0;
    let mut light_pages = 0;
    let mut lifted_on_white = Vec::new();
    for name in aterm_types::scheme::builtin_names() {
        let parts = aterm_types::scheme::builtin(name)
            .unwrap_or_else(|| panic!("built-in theme {name} must resolve"))
            .to_theme_parts();
        let page = parts.bg & 0x00FF_FFFF;
        if aterm_render::theme_is_dark(page) {
            continue;
        }
        light_pages += 1;
        let (mut glow, trail) = style_cfgs(GlowStyle::RainbowKitty, page);
        glow.theme_fg = parts.fg & 0x00FF_FFFF;
        let mut fx = CursorFx::default();
        let t0 = Instant::now();
        let input = |now, col: u16| CursorFxInput {
            cur: Some((2, col)),
            cursor_style: CursorStyle::SteadyBlock,
            live_cursor: parts.cursor & 0x00FF_FFFF,
            default_bg: page,
            momentum_glow: false,
            ..frame_input(now)
        };
        let mut hottest = 0.0f32;
        for k in 0..25u16 {
            let now = if k < 24 {
                let now = t0 + std::time::Duration::from_millis(50 * u64::from(k));
                fx.glow.note_synthetic_typed(now, 1);
                fx.cadence.on_keystroke(now);
                now
            } else {
                t0 + std::time::Duration::from_secs(12)
            };
            let frame = fx.tick(&input(now, 4 + k.min(23)), &glow, trail);
            let blaze = fx.glow.blaze();
            hottest = hottest.max(blaze);
            let owned = frame
                .block_fill
                .unwrap_or_else(|| panic!("{name}: the rainbow owns the caret, frame {k}"));
            assert_eq!(owned.owner, BlockFillOwner::Rainbow, "{name} frame {k}");
            let ratio = contrast_ratio(owned.fill, page);
            assert!(
                ratio >= BAR,
                "{name} ({page:06x}) frame {k}: caret {:06x} at {ratio:.2}:1",
                owned.fill
            );
            if page == 0x00FF_FFFF && k < 24 && blaze > 0.5 {
                let lifted = crate::cursor_rainbow::lift_to_light_floor(
                    owned.fill,
                    crate::cursor_glow::RAINBOW_CARET_LIGHT_FLOOR,
                );
                lifted_on_white.push(contrast_ratio(lifted, page));
            }
        }
        assert!(
            hottest > 0.8,
            "{name}: the burst never ran hot (blaze {hottest:.2})"
        );
    }
    assert!(
        light_pages >= 3,
        "only {light_pages} shipped light themes found"
    );
    assert!(
        !lifted_on_white.is_empty() && lifted_on_white.iter().all(|&r| r < BAR),
        "control: the white lift must sink the caret under {BAR}:1 on white, got \
         {lifted_on_white:?}"
    );
}

#[test]
fn the_momentum_tint_cools_onto_the_plain_cursor_without_a_step() {
    let run = |page: u32, cursor: u32| {
        let glow = GlowConfig {
            theme_fg: 0x0024_292F,
            theme_bg: page,
            dark_theme: false,
            ..classic_cfg(false).0
        };
        let trail = classic_cfg(false).1;
        let mut fx = CursorFx::default();
        let t0 = Instant::now();
        for k in 0..24u64 {
            fx.momentum.on_key(
                t0 + std::time::Duration::from_millis(k * 40),
                MOMENTUM_GLOW_TAU_S,
            );
        }
        let input = |now| CursorFxInput {
            cursor_style: CursorStyle::SteadyBlock,
            live_cursor: cursor,
            default_bg: page,
            ..frame_input(now)
        };
        let (mut peak, mut last_hot) = (None, None);
        for i in 0..2000u64 {
            let now = t0 + std::time::Duration::from_millis(23 * 40 + 5 * i);
            let frame = fx.tick(&input(now), &glow, trail);
            match frame.block_fill {
                Some(owned) => {
                    assert_eq!(owned.owner, BlockFillOwner::Momentum);
                    peak.get_or_insert(owned.fill);
                    last_hot = Some(owned.fill);
                }
                None if last_hot.is_some() => break,
                None => {}
            }
        }
        (
            peak.expect("a fast hand heats the momentum tint"),
            last_hot.expect("the tint cools to cold inside the window"),
        )
    };
    let (_, edge) = run(0x00EF_F1F5, 0x00DC_8A78);
    let step = [16u32, 8, 0]
        .iter()
        .map(|sh| ((edge >> sh) & 0xff).abs_diff((0x00DC_8A78 >> sh) & 0xff))
        .max()
        .unwrap_or(0);
    assert!(
        step <= 2,
        "the last hot frame {edge:06x} must be the plain cursor DC8A78 (step {step})"
    );
    let (peak, _) = run(0x00FF_FFFF, 0x0009_69DA);
    assert!(
        crate::color_math::contrast_ratio(peak, 0x00FF_FFFF) >= CARET_PAGE_MIN_CONTRAST,
        "control: the hot tint {peak:06x} on GitHub Light keeps the page floor"
    );
}

#[test]
fn the_resting_forge_ember_is_floored_on_the_dark_page() {
    const PAGE: u32 = 0x0011_1318;
    let rest = |solved_for: u32| {
        let (glow, trail) = style_cfgs(GlowStyle::Fire, solved_for);
        let mut fx = CursorFx::default();
        let frame = fx.tick(
            &CursorFxInput {
                cursor_style: CursorStyle::SteadyBlock,
                default_bg: PAGE,
                ..frame_input(Instant::now())
            },
            &glow,
            trail,
        );
        let owned = frame.block_fill.expect("the forge owns the fire caret");
        assert_eq!(owned.owner, BlockFillOwner::Forge);
        let raw = fx.glow.forge_fill().expect("the forge always has a colour");
        (owned.fill, raw)
    };
    let (fill, raw) = rest(PAGE);
    assert!(
        crate::color_math::contrast_ratio(fill, PAGE) >= CARET_PAGE_MIN_CONTRAST,
        "the ember {fill:06x} must clear the page floor"
    );
    let (r, g, b) = ((fill >> 16) & 0xff, (fill >> 8) & 0xff, fill & 0xff);
    assert!(r > g && g >= b, "still an ember, got {fill:06x}");
    assert_eq!(
        fill, raw,
        "the forge band already rests at the floor, so the floor keeps its ember"
    );
    let (lifted, dull) = rest(0x00FF_FFFF);
    assert!(
        crate::color_math::contrast_ratio(dull, PAGE) < 2.0,
        "control: the light page's resting ember {dull:06x} is under 2:1 on the dark page"
    );
    assert!(
        crate::color_math::contrast_ratio(lifted, PAGE) >= CARET_PAGE_MIN_CONTRAST,
        "control: the tick floors that ember ({dull:06x} drew {lifted:06x})"
    );
    let (r, g, b) = ((lifted >> 16) & 0xff, (lifted >> 8) & 0xff, lifted & 0xff);
    assert!(
        r > g && g >= b,
        "the floored control is still an ember, got {lifted:06x}"
    );
}

#[test]
fn the_bolt_whitens_on_dark_and_deepens_on_light() {
    use crate::color_math::relative_luminance;
    let violet = crate::cursor_glow::LASER_DEFAULT_COLOR;
    assert_eq!(bolt_body(violet, 0.0, true, false), violet);
    let hot_dark = bolt_body(violet, 1.0, true, false);
    assert!(relative_luminance(hot_dark) > relative_luminance(violet));
    let hot_light = bolt_body(violet, 1.0, false, false);
    assert!(relative_luminance(hot_light) < relative_luminance(violet));
    assert!(
        crate::color_math::contrast_ratio(hot_light, 0x00FF_FFFF) >= 3.0,
        "the charged light bolt {hot_light:06x} keeps 3:1 on white"
    );
}

#[test]
fn the_frame_colour_step_applies_the_law() {
    use crate::cursor_glow::{
        BEAM_DEFAULT_COLOR, COMET_DEFAULT_COLOR, LASER_DEFAULT_COLOR, SPARKLE_DEFAULT_COLOR,
    };
    let live = 0x0050_FA7B;
    let stale = 0x0012_1212;
    let step = |style: GlowStyle, pinned: bool| {
        let (mut glow, mut trail) = classic_cfg(true);
        glow.style = style;
        glow.color = stale;
        trail.color = stale;
        follow_live_cursor(
            &mut glow,
            &mut trail,
            live,
            pinned,
            TrailColor::Automatic,
            false,
            None,
        );
        (glow, trail)
    };
    for (style, unpinned) in [
        (GlowStyle::Lumen, live),
        (GlowStyle::Comet, COMET_DEFAULT_COLOR),
        (GlowStyle::Beam, BEAM_DEFAULT_COLOR),
        (GlowStyle::Sparkle, SPARKLE_DEFAULT_COLOR),
        (GlowStyle::Laser, LASER_DEFAULT_COLOR),
    ] {
        for (pinned, want) in [
            (false, unpinned),
            (
                true,
                if style == GlowStyle::Laser {
                    LASER_DEFAULT_COLOR
                } else {
                    live
                },
            ),
        ] {
            let (glow, trail) = step(style, pinned);
            assert_eq!(glow.color, want, "{style:?} pinned={pinned}");
            assert_eq!(trail.color, glow.color, "{style:?} pinned={pinned}");
            assert_eq!(
                glow.accent,
                brighten(want, 1.5),
                "{style:?} pinned={pinned}"
            );
        }
    }
    for style in [
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::RainbowKitty,
        GlowStyle::Sparkle,
        GlowStyle::Fire,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Water,
        GlowStyle::Comet,
        GlowStyle::Classic,
    ] {
        assert_eq!(
            canonical_glow_color(style).is_some(),
            canonical_glow_color_name(style).is_some(),
            "{style:?}: a hue of its own has a name"
        );
        for pinned in [false, true] {
            let own = own_trail_color(style, pinned);
            assert_eq!(
                own.is_none(),
                glow_color_follows_live_cursor(style, pinned),
                "{style:?} pinned={pinned}"
            );
            if let Some((rgb, _)) = own {
                assert_eq!(Some(rgb), canonical_glow_color(style), "{style:?}");
            }
        }
    }
    let (mut glow, mut trail) = classic_cfg(true);
    glow.style = GlowStyle::Comet;
    glow.color = 0x00E0_218A;
    trail.color = 0x00E0_218A;
    let accent = glow.accent;
    follow_live_cursor(
        &mut glow,
        &mut trail,
        live,
        true,
        TrailColor::Pinned(0x00E0_218A),
        false,
        None,
    );
    assert_eq!((glow.color, trail.color), (0x00E0_218A, 0x00E0_218A));
    assert_eq!(
        glow.accent, accent,
        "a style drawing its colour keeps its accent"
    );
    for style in [
        GlowStyle::Fire,
        GlowStyle::Water,
        GlowStyle::Phaser,
        GlowStyle::RainbowKitty,
    ] {
        let (mut glow, mut trail) = classic_cfg(true);
        glow.style = style;
        glow.color = 0x00E0_218A;
        follow_live_cursor(
            &mut glow,
            &mut trail,
            live,
            false,
            TrailColor::Pinned(0x00E0_218A),
            false,
            None,
        );
        assert_eq!(glow.color, 0x00E0_218A, "{style:?}");
        assert_eq!(glow.accent, brighten(live, 1.5), "{style:?}");
    }
    let (mut glow, mut trail) = classic_cfg(true);
    glow.style = GlowStyle::Lumen;
    glow.accent = 0x0012_3456;
    follow_live_cursor(
        &mut glow,
        &mut trail,
        live,
        false,
        TrailColor::Automatic,
        true,
        None,
    );
    assert_eq!((glow.color, glow.accent), (live, 0x0012_3456));
    for style in [GlowStyle::Laser, GlowStyle::Classic, GlowStyle::Comet] {
        for pinned in [false, true] {
            let (mut glow, mut trail) = classic_cfg(true);
            glow.style = style;
            glow.color = stale;
            trail.color = stale;
            follow_live_cursor(
                &mut glow,
                &mut trail,
                live,
                pinned,
                TrailColor::Cursor,
                false,
                None,
            );
            assert_eq!((glow.color, trail.color), (live, live), "{style:?}");
            assert_eq!(glow.accent, brighten(live, 1.5), "{style:?}");
        }
    }
    assert_eq!(TrailColor::Cursor.knob(0x0112_3456), Some(0x0012_3456));
    assert_eq!(TrailColor::Automatic.knob(live), None);
    assert!(TrailColor::Cursor.chosen() && !TrailColor::Automatic.chosen());
}

#[test]
fn the_unset_accent_brightens_the_base_a_window_draws() {
    let (theme_cursor, live, chosen) = (0x0009_69DA, 0x0050_FA7B, 0x00E0_218A);
    let styles = [
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::RainbowKitty,
        GlowStyle::Sparkle,
        GlowStyle::Fire,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Water,
        GlowStyle::Comet,
        GlowStyle::Classic,
    ];
    let mut bases = std::collections::HashSet::new();
    for (style, pinned) in styles
        .into_iter()
        .flat_map(|style| [(style, false), (style, true)])
    {
        for setting in [
            TrailColor::Automatic,
            TrailColor::Cursor,
            TrailColor::Pinned(chosen),
        ] {
            let knobs = GlowKnobs {
                enabled: true,
                color: setting.knob(theme_cursor),
                accent: None,
                duration_ms: 260,
                length: 24,
                intensity: 1.0,
                radius: 0.6,
                ring: true,
            };
            let spelling = GlowSpelling {
                style: Some(style),
                beam: false,
                ribbon_tall: true,
                ribbon_flat: false,
                classic_mono: false,
            };
            let mut glow = resolve_glow_config(
                knobs,
                spelling,
                theme_cursor,
                true,
                0x00C8_D3F5,
                0x001A_1B26,
                0.5,
            );
            let (_, mut trail) = classic_cfg(true);
            follow_live_cursor(&mut glow, &mut trail, live, pinned, setting, false, None);
            let base = unset_accent_base(style, setting, pinned);
            let expected = match base {
                AccentBase::Cursor => live,
                AccentBase::Own(own, _) => own,
                AccentBase::Trail(color) => color,
            };
            assert_eq!(
                glow.accent,
                brighten(expected, 1.5),
                "{style:?} under {setting:?} pinned={pinned}: {base:?}"
            );
            bases.insert(std::mem::discriminant(&base));
        }
    }
    assert_eq!(bases.len(), 3, "every base is drawn by some style");
}
