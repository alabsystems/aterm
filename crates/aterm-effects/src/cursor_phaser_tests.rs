// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::effect_util::ink;
use std::time::Duration;

const fn geom() -> Geom {
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
const fn cfg() -> PhaserConfig {
    PhaserConfig {
        enabled: true,
        intensity: 1.0,
        base: None,
    }
}

/// Disabled, or zero intensity (reduced motion / load shed) at full
/// strength: fully inert (no fill, no wings, fp 0, settled), equal to the
/// plain cursor.
#[test]
fn disabled_or_zero_intensity_is_inert() {
    for (why, config) in [
        (
            "disabled",
            PhaserConfig {
                enabled: false,
                intensity: 1.0,
                base: None,
            },
        ),
        (
            "zero intensity",
            PhaserConfig {
                enabled: true,
                intensity: 0.0,
                base: None,
            },
        ),
    ] {
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();
        let f = cp.tick(
            Some((1, 1)),
            Instant::now(),
            0.3,
            1.0,
            true,
            geom(),
            &config,
            &mut out,
        );
        assert!(f.fill.is_none(), "{why}: keeps the plain cursor");
        assert_eq!(f.fp, 0, "{why}");
        assert!(out.is_empty(), "{why}: no additive light");
        assert!(!cp.is_active(), "{why}: settled");
    }
}

/// A hidden cursor draws no wings, but the fill stays resolved (harmless —
/// the renderer draws no cursor to fill).
#[test]
fn hidden_cursor_draws_no_wings() {
    let mut cp = CursorPhaser::default();
    let mut out = Vec::new();
    let f = cp.tick(
        None,
        Instant::now(),
        0.3,
        0.5,
        true,
        geom(),
        &cfg(),
        &mut out,
    );
    assert!(out.is_empty(), "no additive light without a cursor cell");
    assert!(f.fill.is_some());
}

/// The block fill follows the beam hue: two well-separated hues at full
/// charge give clearly different fills, each close to the beam colour.
#[test]
fn fill_locks_to_the_beam_hue() {
    let g = geom();
    let c = cfg();
    let t = Instant::now();
    let mut out = Vec::new();
    let mut fill_at = |hue: f32| -> u32 {
        let mut cp = CursorPhaser::default();
        cp.tick(Some((1, 1)), t, hue, 1.0, true, g, &c, &mut out)
            .fill
            .unwrap()
    };
    let red = fill_at(0.0);
    let cyan = fill_at(0.5);
    let (rr, rg, rb) = ((red >> 16) & 0xff, (red >> 8) & 0xff, red & 0xff);
    let (cr_, cg, cb) = ((cyan >> 16) & 0xff, (cyan >> 8) & 0xff, cyan & 0xff);
    assert!(
        rr > rg + 60 && rr > rb + 60,
        "hue 0 fill is red: {red:#08x}"
    );
    assert!(
        cg > cr_ + 60 && cb > cr_ + 60,
        "hue 0.5 fill is cyan: {cyan:#08x}"
    );
}

/// With no host cursor colour ([`PhaserConfig::base`] `None`), the fill
/// blooms from the theme base (near-white on dark) toward the saturated beam
/// hue under charge; on a light theme it is the beam's hue itself, clean
/// and saturated, deep enough to read clearly on white at rest.
#[test]
fn fill_blooms_from_theme_base() {
    let g = geom();
    let c = cfg();
    let t = Instant::now();
    let mut out = Vec::new();
    let mut idle = CursorPhaser::default();
    let f_idle = idle
        .tick(Some((1, 1)), t, 0.0, 0.0, true, g, &c, &mut out)
        .fill
        .unwrap();
    let minch = |c: u32| {
        [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff]
            .into_iter()
            .min()
            .unwrap()
    };
    assert!(
        minch(f_idle) > 140,
        "idle block stays near white on dark, got {f_idle:#08x}"
    );
    let mut hot = CursorPhaser::default();
    let f_hot = hot
        .tick(Some((1, 1)), t, 0.0, 1.0, true, g, &c, &mut out)
        .fill
        .unwrap();
    let spread = |c: u32| {
        let (r, gg, b) = ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
        r.max(gg).max(b) - r.min(gg).min(b)
    };
    assert!(
        spread(f_hot) > spread(f_idle) + 60,
        "charge saturates the fill toward the beam hue"
    );
    for hue in [0.0, 0.33, 0.6, 0.8] {
        let mut light = CursorPhaser::default();
        let f_light = light
            .tick(Some((1, 1)), t, hue, 0.0, false, g, &c, &mut out)
            .fill
            .unwrap();
        let ratio = crate::color_math::contrast_ratio(f_light, 0x00FF_FFFF);
        let (_, sat, _) = crate::color_math::rgb2hsv(f_light);
        assert!(
            ratio >= crate::cursor_beam::LIGHT_EMITTER_CONTRAST - 0.05 && sat > 0.9,
            "hue {hue}: the idle light block is a clean deep hue, got {f_light:#08x} \
             ({ratio:.2}:1, saturation {sat:.2})"
        );
    }
}

/// ON PAPER THE EMITTER AND ITS BAR STEP OVER THE MUDDY HUES: every beam
/// phase gives a light block whose hue lies outside
/// [`crate::color_math::LIGHT_MUDDY_HUES`], and every hue on the light
/// wheel is one of the others, in order.
#[test]
fn the_light_emitter_steps_over_the_muddy_hues() {
    use crate::color_math::{LIGHT_MUDDY_HUES, light_spectrum_hue, rgb2hsv};
    let (b0, b1) = LIGHT_MUDDY_HUES;
    let mut last = -1.0;
    for i in 0..360 {
        let h = light_spectrum_hue(i as f32 / 360.0);
        assert!(
            !(b0..b1).contains(&h),
            "{i} degrees lands in the band at {h}"
        );
        assert!(h > last, "the light wheel keeps the hues in order");
        last = h;
        let fill = light_emitter_fill(i as f32 / 360.0, 0.0);
        let (deg, _, _) = rgb2hsv(fill);
        assert!(
            !(b0 * 360.0 + 1.0..b1 * 360.0 - 1.0).contains(&deg),
            "{i} degrees: the block {fill:06x} is a muddy hue"
        );
    }
    let yellow = crate::color_math::floor_contrast_keeping_hue(
        hsv2rgb(60.0 / 360.0, 1.0, 1.0),
        0x00FF_FFFF,
        crate::cursor_beam::LIGHT_EMITTER_CONTRAST,
    );
    let (r, g, b) = ((yellow >> 16) & 0xff, (yellow >> 8) & 0xff, yellow & 0xff);
    assert!(
        r.max(g) < 0x80 && b == 0,
        "control: yellow deepened to read on white is khaki, got {yellow:06x}"
    );
}

/// THE EMITTER KEEPS A MARGIN OVER THE PAGE (after-land L4): on Solarized
/// Dark the dark emitter, at every hue and charge and floored as the host
/// floors its caret ([`crate::cursor_fx::floor_block_fill`]), reads at
/// 4.6:1 or better, past the caret bar (4.5) by more than a renderer's
/// rounding. Negative control: floored to the bare bar (a bolt's floor,
/// [`crate::cursor_fx::CARET_PAGE_MIN_CONTRAST`]), its lowest reads under
/// 4.6, as the composed land's `phaser-dark.mid` read 4.51.
#[test]
fn the_dark_emitter_keeps_a_margin_over_the_page() {
    use crate::cursor_glow::{BlockFill, BlockFillOwner};
    let (g, c, t, page) = (geom(), cfg(), Instant::now(), 0x0000_2B36);
    let mut out = Vec::new();
    let (mut low, mut bare) = (f32::MAX, f32::MAX);
    for i in 0..48 {
        for k in 0..=8 {
            let fill = CursorPhaser::default()
                .tick(
                    Some((1, 1)),
                    t,
                    i as f32 / 48.0,
                    k as f32 / 8.0,
                    true,
                    g,
                    &c,
                    &mut out,
                )
                .fill
                .expect("the emitter owns its fill");
            let read = |owner| {
                let held = crate::cursor_fx::floor_block_fill(
                    BlockFill {
                        owner,
                        fill,
                        base: None,
                    },
                    page,
                    0x0093_A1A1,
                    None,
                    false,
                );
                crate::color_math::contrast_ratio(held.fill, page)
            };
            low = low.min(read(BlockFillOwner::Phaser));
            bare = bare.min(read(BlockFillOwner::Bolt));
        }
    }
    assert!(
        low >= 4.6,
        "the emitter's lowest on Solarized Dark reads {low:.2}:1"
    );
    assert!(
        bare < 4.6,
        "control: floored to the bare bar its lowest reads {bare:.2}:1"
    );
}

/// The host's cursor colour is the emitter block on either theme polarity.
#[test]
fn host_base_is_the_settled_block_on_either_polarity() {
    let g = geom();
    let mut out = Vec::new();
    let t = Instant::now();
    let settled = |base: u32, dark: bool, out: &mut Vec<GlowQuad>| {
        let c = PhaserConfig {
            base: Some(base),
            ..cfg()
        };
        out.clear();
        CursorPhaser::default()
            .tick(Some((1, 1)), t, 0.5, 0.0, dark, g, &c, out)
            .fill
            .unwrap()
    };
    let chans = |c: u32| ((c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
    for dark in [true, false] {
        let (rr, rg, rb) = chans(settled(0x00FF_0000, dark, &mut out));
        assert!(
            rr > 200 && rg < 130 && rb < 130,
            "a red cursor colour settles red (dark={dark})"
        );
        let (br, bg, bb) = chans(settled(0x0000_00FF, dark, &mut out));
        assert!(
            bb > 200 && br < 130 && bg < 130,
            "a blue cursor colour settles blue (dark={dark})"
        );
        assert_ne!(
            settled(0x00FF_0000, dark, &mut out),
            settled(0x0000_00FF, dark, &mut out),
            "two cursor colours must not paint one identical block (dark={dark})"
        );
    }
    let c = PhaserConfig {
        base: Some(0x00FF_0000),
        ..cfg()
    };
    out.clear();
    let hot = CursorPhaser::default()
        .tick(Some((1, 1)), t, 0.5, 1.0, true, g, &c, &mut out)
        .fill
        .unwrap();
    assert_ne!(hot, settled(0x00FF_0000, true, &mut out));
}

/// Full charge reaches further sideways and burns far brighter than the idle port glow.
#[test]
fn charge_widens_and_brightens_the_wings() {
    let g = geom();
    let c = cfg();
    let t = Instant::now();
    let run = |energy: f32| -> (u64, i32, i32) {
        let mut cp = CursorPhaser::default();
        let mut out = Vec::new();
        cp.tick(Some((3, 20)), t, 0.3, energy, true, g, &c, &mut out);
        out.clear();
        cp.tick(
            Some((3, 20)),
            t + Duration::from_millis(16),
            0.3,
            energy,
            true,
            g,
            &c,
            &mut out,
        );
        let x0 = out.iter().map(|q| q.x as i32).min().unwrap_or(0);
        let x1 = out.iter().map(|q| (q.x + q.w) as i32).max().unwrap_or(0);
        (ink(&out), x0, x1)
    };
    let (idle_ink, idle_x0, idle_x1) = run(0.0);
    let (hot_ink, hot_x0, hot_x1) = run(1.0);
    assert!(idle_ink > 0, "the idle port glow is still visibly lit");
    assert!(
        hot_ink > idle_ink * 2,
        "full charge far outshines idle ({hot_ink} vs {idle_ink})"
    );
    assert!(
        hot_x0 < idle_x0 && hot_x1 > idle_x1,
        "charged wings reach further ({hot_x0}..{hot_x1} vs {idle_x0}..{idle_x1})"
    );
}

/// The wings stay on the cursor's row (the beam axis), with no light above
/// or below, and never touch the cell's own interior columns (the fill owns
/// the cell).
#[test]
fn wings_hug_the_beam_axis() {
    let g = geom();
    let c = cfg();
    let mut cp = CursorPhaser::default();
    let mut out = Vec::new();
    let t = Instant::now();
    let (row, col) = (3u16, 20u16);
    cp.tick(Some((row, col)), t, 0.3, 1.0, true, g, &c, &mut out);
    assert!(!out.is_empty());
    let cell_l = col as u32 * g.cw as u32;
    let cell_r = cell_l + g.cw as u32;
    for q in &out {
        assert_eq!(q.row, row, "wings never leave the cursor row: {q:?}");
        assert!(
            (q.x as u32 + q.w as u32) <= cell_l || q.x as u32 >= cell_r,
            "side lobes only — the fill owns the cell: {q:?}"
        );
    }
}

/// Every emitted quad is single-row, inside the grid, with capped coverage, including at a
/// grid-edge cell where clamping applies.
#[test]
fn quads_respect_grid_and_cap() {
    let g = geom();
    let c = cfg();
    let mut cp = CursorPhaser::default();
    let mut out = Vec::new();
    let t = Instant::now();
    for cell in [(0u16, 0u16), (5, 39)] {
        cp.tick(Some(cell), t, 0.7, 1.0, true, g, &c, &mut out);
    }
    assert!(!out.is_empty());
    let gw = (g.cols * g.cw) as u32;
    let gh = (g.rows * g.ch) as u32;
    for q in &out {
        let band = q.row as u32 * g.ch as u32;
        assert!(
            q.y as u32 >= band && q.y as u32 + q.h as u32 <= band + g.ch as u32,
            "single-row: {q:?}"
        );
        assert!(
            q.x as u32 + q.w as u32 <= gw && q.y as u32 + q.h as u32 <= gh,
            "in grid: {q:?}"
        );
        for sh in [16u32, 8, 0] {
            assert!(
                (q.color >> sh) & 0xff <= COV_CAP as u32 + 1,
                "coverage capped: {q:?}"
            );
        }
    }
}

/// Charged ⇒ active (host keeps the tick armed); settled charge ⇒ inactive
/// so the idle port glow rides the blink cadence at no extra wakeup cost.
#[test]
fn settles_to_inactive_when_charge_dies() {
    let g = geom();
    let c = cfg();
    let mut cp = CursorPhaser::default();
    let mut out = Vec::new();
    let t = Instant::now();
    cp.tick(Some((1, 1)), t, 0.3, 0.8, true, g, &c, &mut out);
    assert!(cp.is_active(), "a charged emitter keeps the tick armed");
    cp.tick(
        Some((1, 1)),
        t + Duration::from_millis(16),
        0.3,
        0.0,
        true,
        g,
        &c,
        &mut out,
    );
    assert!(
        !cp.is_active(),
        "the settled emitter idles on the blink cadence"
    );
}

/// The idle port is static: sparse captures or an unrelated repaint after
/// seconds of silence must not integrate a hidden breath slice and flicker
/// the cursor edge.
#[test]
fn settled_present_gaps_are_byte_identical() {
    let g = geom();
    let c = cfg();
    let t = Instant::now();
    let mut cp = CursorPhaser::default();
    let mut out = Vec::new();

    cp.tick(Some((1, 12)), t, 0.3, 0.8, true, g, &c, &mut out);
    out.clear();
    let first = cp.tick(
        Some((1, 12)),
        t + Duration::from_secs(5),
        0.3,
        0.0,
        true,
        g,
        &c,
        &mut out,
    );
    let first_quads = out.clone();
    out.clear();
    let late = cp.tick(
        Some((1, 12)),
        t + Duration::from_secs(30),
        0.3,
        0.0,
        true,
        g,
        &c,
        &mut out,
    );

    assert_eq!(late.fill, first.fill);
    assert_eq!(late.fp, first.fp);
    assert_eq!(out, first_quads);
    assert!(!cp.is_active());
}

/// The per-frame quad budget is bounded by construction (4 layers × slabs × 2 sides), far
/// under the aurora's MAX_QUADS defence.
#[test]
fn quad_count_is_bounded() {
    let g = Geom {
        cw: 20,
        ch: 40,
        rows: 50,
        cols: 200,
        origin_x: 0,
        origin_y: 0,
        win_w: 4000,
        win_h: 2000,
        head: 0,
    };
    let mut cp = CursorPhaser::default();
    let mut out = Vec::new();
    cp.tick(
        Some((25, 100)),
        Instant::now(),
        0.3,
        1.0,
        true,
        g,
        &cfg(),
        &mut out,
    );
    assert!(out.len() < 200, "bounded wing geometry, got {}", out.len());
}
