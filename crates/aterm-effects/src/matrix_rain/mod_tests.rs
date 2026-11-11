// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::HashMap;
use std::time::Duration;

use super::*;

const fn cfg_on() -> RainConfig {
    RainConfig {
        enabled: true,
        output_material: false,
        seed: 7,
        ..RainConfig::DEFAULT
    }
}

const fn literal_cfg_on() -> RainConfig {
    RainConfig {
        output_material: true,
        ..cfg_on()
    }
}

const fn geom(rows: u16, cols: u16, cw: u16, ch: u16) -> EffectGeom {
    EffectGeom {
        cell_w: cw,
        cell_h: ch,
        rows,
        cols,
    }
}

const fn space_cell(bg: [u8; 3]) -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [0xD0, 0xD0, 0xD0],
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

const BG: u32 = 0x0011_1318;

const fn bg3() -> [u8; 3] {
    [0x11, 0x13, 0x18]
}

fn scan_empty(e: &mut MatrixRain, rows: usize, cols: usize) {
    let cells = vec![vec![space_cell(bg3()); cols]; rows];
    let sizes = vec![LineSize::SingleWidth; rows];
    e.rescan_from_cells(&cells, &sizes, &[], rows, cols, BG, 1);
}

const fn idle() -> RainTickInput<'static> {
    RainTickInput::EMPTY
}

const fn pour(e: &mut MatrixRain) {
    e.density_byte = 252;
    e.ring.push(e.tick, 252);
}

#[test]
fn emits_over_trimmed_blank_rows() {
    let (rows, cols) = (10usize, 40usize);
    let block = RenderCell {
        ch: '█',
        ..space_cell(bg3())
    };
    let cells = vec![vec![block; 12]];
    let sizes = vec![LineSize::SingleWidth; rows];
    let mut e = MatrixRain::new(RainConfig {
        enabled: true,
        density: 12,
        output_material: false,
        seed: 0xA7E2_11D3,
        default_bg: BG,
        ..RainConfig::default()
    });
    e.rescan_from_cells(&cells, &sizes, &[], rows, cols, BG, 1);
    pour(&mut e);
    let g = EffectGeom {
        cell_w: 9,
        cell_h: 18,
        rows: rows as u16,
        cols: cols as u16,
    };
    let (mut q, mut a) = (Vec::new(), Vec::new());
    let mut any = false;
    for _ in 0..20 {
        e.note_activity(e.tick + 1);
        step(&mut e, g, &idle(), 33, &mut q, &mut a);
        if !q.is_empty() {
            any = true;
        }
        assert!(
            q.iter().all(|quad| !(quad.row == 0 && (quad.x / 9) < 12)),
            "rain painted over the occupied block glyphs on row 0"
        );
    }
    assert!(any, "rain must emit over the trimmed blank rows");
}

fn step(
    e: &mut MatrixRain,
    g: EffectGeom,
    input: &RainTickInput<'_>,
    dt: u64,
    q: &mut Vec<RainCell>,
    a: &mut Vec<RainHalo>,
) -> u64 {
    e.advance_ms(dt);
    e.emit(g, input, q, a)
}

fn select_all(rows: i32, cols: u16) -> aterm_core::selection::TextSelection {
    use aterm_core::selection::{SelectionSide, SelectionType};
    let mut s = aterm_core::selection::TextSelection::new();
    s.start_selection(0, 0, SelectionSide::Left, SelectionType::Simple);
    s.update_selection(rows - 1, cols - 1, SelectionSide::Right);
    s.complete_selection();
    s
}

#[test]
fn deterministic_across_engines_and_clock_paths() {
    let g = geom(40, 60, 8, 16);
    let mut a = MatrixRain::new(cfg_on());
    let mut b = MatrixRain::new(cfg_on());
    scan_empty(&mut a, 40, 60);
    scan_empty(&mut b, 40, 60);
    let t0 = Instant::now();
    let (mut qa, mut aa) = (Vec::new(), Vec::new());
    let (mut qb, mut ab) = (Vec::new(), Vec::new());
    let mut rained = false;
    for i in 0..200u64 {
        if i % 3 == 0 {
            a.note_activity(i);
            b.note_activity(i);
        }
        if i % 7 == 0 {
            a.note_keystroke();
            b.note_keystroke();
        }
        let now = t0 + Duration::from_millis(i * 33);
        let fa = a.tick(now, g, &idle(), &mut qa, &mut aa);
        let fb = step(
            &mut b,
            g,
            &idle(),
            if i == 0 { 0 } else { 33 },
            &mut qb,
            &mut ab,
        );
        assert_eq!(fa, fb, "instant vs dt fingerprint diverged at step {i}");
        assert_eq!(qa, qb, "instant vs dt quads diverged at step {i}");
        assert_eq!(aa, ab, "instant vs dt halos diverged at step {i}");
        rained |= !qa.is_empty();
    }
    assert!(rained, "non-vacuous: the drive actually rained");
}

#[test]
fn budget_bound_holds_and_truncation_fires() {
    for (cw, ch) in [(4u16, 8u16), (20, 40)] {
        let cap = quad_cap(u32::from(cw), u32::from(ch));
        let g = geom(200, 500, cw, ch);
        let mut e = MatrixRain::new(RainConfig {
            density: 12,
            ..cfg_on()
        });
        scan_empty(&mut e, 200, 500);
        pour(&mut e);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        let mut hit_cap = false;
        for _ in 0..60 {
            e.note_keystroke();
            step(&mut e, g, &idle(), 83, &mut q, &mut a);
            assert!(q.len() <= cap, "quad budget exceeded: {} > {cap}", q.len());
            assert!(a.len() <= MAX_RAIN_ADD, "halo budget exceeded");
            if q.len() >= cap - 202 {
                hit_cap = true;
            }
        }
        assert!(hit_cap, "truncation branch never exercised at {cw}x{ch}");
    }
}

#[test]
fn step_stable_damage_shape() {
    let g = geom(50, 40, 8, 16);
    let mut e = MatrixRain::new(RainConfig {
        density: 12,
        ..cfg_on()
    });
    scan_empty(&mut e, 50, 40);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for i in 0..300u64 {
        e.note_activity(i + 1);
        step(&mut e, g, &idle(), 33, &mut q, &mut a);
    }
    assert_eq!(e.weather, RainWeather::Working);
    let mut seq = 1000u64;
    loop {
        let t = e.tick;
        if !q.is_empty()
            && !(t + 1).is_multiple_of(u64::from(e.mq))
            && !(t + 1).is_multiple_of(u64::from(e.dq))
            && !(t + 1).is_multiple_of(u64::from(e.sq))
        {
            break;
        }
        seq += 1;
        e.note_activity(seq);
        step(&mut e, g, &idle(), 33, &mut q, &mut a);
        assert!(seq < 1200, "never found an aligned raining tick");
    }
    let snap = |quads: &[RainCell]| {
        let mut m: HashMap<u16, HashMap<u16, RainCell>> = HashMap::new();
        for s in quads {
            m.entry(s.x / 8).or_default().insert(s.row, *s);
        }
        m
    };
    let t_first = snap(&q);
    let tick_before = e.tick;
    e.note_activity(seq + 100);
    step(&mut e, g, &idle(), 33, &mut q, &mut a);
    assert_eq!(e.tick, tick_before + 1, "exactly one engine tick elapsed");
    let t_second = snap(&q);
    let fp = FieldParams {
        seed32: e.seed32,
        rows: 50,
        tick_ms: e.tick_ms,
        speed: 5,
        trail: 5,
        mq: e.mq,
        dq: e.dq,
        sq: e.sq,
    };
    let empty = HashMap::new();
    for col in 0..40u16 {
        let p = col_params(&fp, u32::from(col)).p;
        let before = t_first.get(&col).unwrap_or(&empty);
        let after = t_second.get(&col).unwrap_or(&empty);
        let mut changed = 0usize;
        for r in 0..50u16 {
            if before.get(&r) != after.get(&r) {
                changed += 1;
            }
        }
        let bound = 3 + 15usize.div_ceil(p as usize);
        assert!(
            changed <= bound,
            "col {col}: {changed} changed cells > bound {bound} (p={p})"
        );
    }
}

fn hue_gap(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

#[test]
fn theme_hue_on_a_neutral_fg_is_not_the_failure_tint() {
    let cfg = RainConfig {
        hue: RainHue::Theme,
        ..cfg_on()
    };
    assert!(
        hue_gap(rgb2hsv(cfg.theme_fg).0, FAIL_HUE_DEG) < 30.0,
        "negative control: the default fg's own hue already avoids the failure tint"
    );
    let hue = ramp_hue(&cfg);
    assert!(
        hue_gap(hue, FAIL_HUE_DEG) >= 30.0,
        "default-theme rain hue {hue:.1}° is within 30° of the failure tint"
    );
    let solarized = RainConfig {
        theme_fg: 0x0065_7B83,
        ..cfg
    };
    assert!(hue_gap(ramp_hue(&solarized), rgb2hsv(0x0065_7B83).0) < 0.5);
    let monochrome = RainConfig {
        theme_fg: 0x00C0_C0C0,
        theme_accent: 0x00FF_FFFF,
        ..cfg
    };
    let blue = ramp_hue(&monochrome);
    assert!(hue_gap(blue, rgb2hsv(THEME_HUE_FALLBACK).0) < 0.5);
    assert!(hue_gap(blue, FAIL_HUE_DEG) >= 30.0);
}

#[test]
fn luminance_invariant_on_both_stock_themes() {
    for (bg, fg, name) in [
        (0x0011_1318u32, 0x00D0_D0D0u32, "stock dark"),
        (0x00FF_FFFFu32, 0x001F_2328u32, "GitHub Light"),
    ] {
        let e = MatrixRain::new(RainConfig {
            default_bg: bg,
            theme_fg: fg,
            ..cfg_on()
        });
        let lb = relative_luminance(bg);
        let bound = (0.5 * (relative_luminance(fg) - lb)).abs();
        assert!(
            (RAIN_ALPHA_FLOOR..=RAIN_ALPHA_CAP).contains(&e.body_alpha),
            "{name}: body alpha out of range"
        );
        assert!(
            e.head_alpha >= e.body_alpha && e.head_alpha <= RAIN_ALPHA_CAP,
            "{name}: head alpha out of clamp"
        );
        for lvl in 0..=15u32 {
            let dl = (relative_luminance(srgb_over(e.ramp[lvl as usize], e.alpha_of(lvl), bg))
                - lb)
                .abs();
            assert!(
                dl < bound,
                "{name}: level {lvl} breaks the invariant ({dl} >= {bound})"
            );
        }
        let dh = (relative_luminance(srgb_over(e.ramp[15], e.head_alpha, bg)) - lb).abs();
        assert!(
            dh < bound,
            "{name}: head breaks the invariant ({dh} >= {bound})"
        );
    }
}

#[test]
fn rain_stays_under_real_faint_ink() {
    use aterm_core::terminal::color_resolve::dim_toward_bg;
    let pack =
        |c: aterm_types::Rgb| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b);
    let (mut lifted, mut receded) = (false, false);
    for name in [
        "Default",
        "Aterm Night",
        "Aterm Day",
        "Linen",
        "Default Light",
    ] {
        let scheme = aterm_types::scheme::builtin(name).expect("built-in");
        let (fg, bg) = (scheme.foreground, scheme.background);
        let faint = dim_toward_bg(fg, bg, aterm_types::DIM_FACTOR);
        let (fg, bg, faint) = (pack(fg), pack(bg), pack(faint));
        let lb = relative_luminance(bg);
        let plain = (0.5 * (relative_luminance(fg) - lb)).abs();
        let real = (relative_luminance(faint) - lb).abs();
        lifted |= real > plain * 1.5;
        receded |= real < plain * 0.7;
        let e = MatrixRain::new(RainConfig {
            default_bg: bg,
            theme_fg: fg,
            ..cfg_on()
        });
        for lvl in 0..=15u32 {
            let dl = (relative_luminance(srgb_over(e.ramp[lvl as usize], e.alpha_of(lvl), bg))
                - lb)
                .abs();
            assert!(dl < real, "{name}: level {lvl} at {dl} >= faint {real}");
        }
        let dh = (relative_luminance(srgb_over(e.ramp[15], e.head_alpha, bg)) - lb).abs();
        assert!(dh < real, "{name}: head at {dh} >= faint {real}");
    }
    assert!(
        lifted,
        "a light theme's faint ink sits past the plain blend"
    );
    assert_eq!(
        receded,
        aterm_core::terminal::color_resolve::FAINT_SRGB_BLEND,
        "a dark theme's faint ink sits short of the plain blend exactly where it is the \
         sRGB blend"
    );
}

#[test]
fn alpha_cap_is_pinned() {
    let e = MatrixRain::new(RainConfig {
        alpha_override: Some(200),
        head_alpha_override: Some(3),
        ..cfg_on()
    });
    assert_eq!(e.body_alpha, RAIN_ALPHA_CAP);
    assert_eq!(e.head_alpha, RAIN_ALPHA_CAP, "head clamps up to body");
}

#[test]
fn hidden_pane_drains_immediately() {
    let g = geom(60, 80, 8, 16);
    let mut e = MatrixRain::new(RainConfig {
        density: 12,
        ..cfg_on()
    });
    scan_empty(&mut e, 60, 80);
    pour(&mut e);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for i in 0..40u64 {
        e.note_activity(i + 1);
        step(&mut e, g, &idle(), 33, &mut q, &mut a);
    }
    assert!(!q.is_empty(), "engine should be raining before the drain");
    e.set_visibility(RainVisibility::Hidden);
    assert_eq!(step(&mut e, g, &idle(), 83, &mut q, &mut a), 0);
    assert!(q.is_empty(), "hidden pane drains immediately");
}

#[test]
fn fingerprint_liveness_and_stability() {
    let g = geom(40, 60, 8, 16);
    let mut e = MatrixRain::new(RainConfig {
        density: 12,
        ..cfg_on()
    });
    scan_empty(&mut e, 40, 60);
    pour(&mut e);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    let mut prev = step(&mut e, g, &idle(), 0, &mut q, &mut a);
    let mut seen_nonempty = 0;
    for _ in 0..30 {
        e.note_keystroke();
        let fp = step(&mut e, g, &idle(), 83, &mut q, &mut a);
        if !q.is_empty() {
            seen_nonempty += 1;
            assert_ne!(fp, prev, "animating fp must change every tick");
        }
        prev = fp;
    }
    assert!(seen_nonempty >= 10, "liveness test needs a raining field");
}

#[test]
fn halo_fingerprint_tracks_center_radii_and_mode() {
    use aterm_core::render::HaloMode;
    let base = RainHalo {
        row: 2,
        x: 16,
        y: 32,
        w: 24,
        h: 16,
        color: 0x0033_AA55,
        cx: 28,
        cy: 40,
        rx: 12,
        ry: 8,
        mode: HaloMode::Add,
    };
    let base_fp = fingerprint(&[], &[base], 10, 1);
    for variant in [
        RainHalo { cx: 29, ..base },
        RainHalo { cy: 41, ..base },
        RainHalo { rx: 13, ..base },
        RainHalo { ry: 9, ..base },
        RainHalo {
            mode: HaloMode::Over,
            ..base
        },
    ] {
        assert_ne!(
            fingerprint(&[], &[variant], 10, 1),
            base_fp,
            "halo change must move fingerprint: {variant:?}"
        );
    }
}

#[test]
fn even_quad_counts_do_not_cancel() {
    let quad = RainCell {
        row: 3,
        x: 8,
        y: 48,
        w: 8,
        h: 16,
        glyph: 0,
        tint: 0x0022_CC44,
        alpha: 90,
        flip_x: true,
    };
    let pair = [quad, quad];
    assert_ne!(fingerprint(&pair, &[], 10, 1), 0);
    assert_ne!(
        fingerprint(&pair, &[], 10, 1),
        fingerprint(&pair, &[], 11, 1),
        "the frame term must survive an even quad count"
    );
    assert_eq!(fingerprint(&[], &[], 10, 1), 0, "empty emission is fp 0");
}

#[test]
fn occupancy_mask_predicates() {
    let mut e = MatrixRain::new(cfg_on());
    let rows = 12usize;
    let cols = 8usize;
    let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
    cells[0][1].ch = 'x';
    cells[2][2].ch = '　';
    cells[2][3].wide = true;
    cells[2][4].underline = UnderlineStyle::Single;
    cells[2][5].bg = [0x30, 0x30, 0x30];
    cells[2][6].strikethrough = true;
    cells[2][7].overline = true;
    let mut sizes = vec![LineSize::SingleWidth; rows];
    sizes[1] = LineSize::DoubleWidth;
    let img_row: Vec<(usize, ImageRef)> = Vec::new();
    let mut images = vec![img_row; rows];
    images[3].push((
        3,
        ImageRef {
            image: std::sync::Arc::new(aterm_core::grid::extra::ImageData {
                bytes: Vec::new(),
                format: aterm_core::grid::extra::ImageFormat::Png,
                cols: 1,
                rows: 1,
                z_index: 0,
                band_lift_px: 0,
                cell_offset_px: (0, 0),
                scaling: aterm_core::grid::extra::ImageScaling::Fit,
                source_rect: None,
            }),
            cell_row: 0,
            cell_col: 0,
            kitty: None,
        },
    ));
    e.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 1);
    let band = [11u16];
    let inp = RainTickInput {
        hidden_band: &band,
        ..RainTickInput::default()
    };
    assert!(
        e.cell_eligible(0, 4, &inp),
        "distant plain space is eligible"
    );
    assert!(
        !e.cell_eligible(0, 0, &inp),
        "one-cell gap beside a glyph is semantic clearance"
    );
    assert!(!e.cell_eligible(0, 1, &inp), "glyph cell is masked");
    assert!(!e.cell_eligible(1, 0, &inp), "DECDWL row is wholly masked");
    assert!(!e.cell_eligible(2, 2, &inp), "wide lead is masked");
    assert!(!e.cell_eligible(2, 3, &inp), "wide continuation is masked");
    assert!(!e.cell_eligible(2, 4, &inp), "underlined cell is masked");
    assert!(!e.cell_eligible(2, 5, &inp), "non-default bg is masked");
    assert!(!e.cell_eligible(2, 6, &inp), "strikethrough cell is masked");
    assert!(!e.cell_eligible(2, 7, &inp), "overlined cell is masked");
    assert!(!e.cell_eligible(3, 3, &inp), "image span is masked");
    assert!(
        !e.cell_eligible(3, 4, &inp),
        "cell beside the image is halo clearance"
    );
    assert!(e.cell_eligible(4, 6, &inp), "distant cell remains eligible");
}

#[test]
fn closed_tui_frame_protects_default_background_interior() {
    let (rows, cols) = (14usize, 26usize);
    let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
    cells[1][1].ch = '╭';
    cells[1][22].ch = '╮';
    for cell in &mut cells[1][2..22] {
        cell.ch = '─';
    }
    for (offset, ch) in "Claude Code".chars().enumerate() {
        cells[1][5 + offset].ch = ch;
    }
    for row in cells.iter_mut().take(9).skip(2) {
        row[1].ch = '│';
        row[22].ch = '│';
    }
    cells[9][1].ch = '╰';
    cells[9][22].ch = '╯';
    for cell in &mut cells[9][2..22] {
        cell.ch = '─';
    }
    let sizes = vec![LineSize::SingleWidth; rows];
    let images = vec![Vec::new(); rows];
    let mut rain = MatrixRain::new(cfg_on());
    rain.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 1);
    let band = [13u16];
    let input = RainTickInput {
        hidden_band: &band,
        ..RainTickInput::default()
    };
    assert!(
        !rain.cell_eligible(5, 12, &input),
        "blank cells inside a closed titled panel are semantic UI"
    );
    assert!(
        rain.cell_eligible(11, 12, &input),
        "open terminal field below the panel remains rain-eligible"
    );

    for cell in &mut cells[9][2..22] {
        cell.ch = 'x';
    }
    rain.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 2);
    assert!(
        rain.cell_eligible(5, 12, &input),
        "corner glyphs around text must not close a framed surface"
    );

    cells[9][1].ch = ' ';
    cells[9][22].ch = ' ';
    for cell in &mut cells[9][2..22] {
        cell.ch = ' ';
    }
    rain.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 3);
    assert!(
        rain.cell_eligible(5, 12, &input),
        "an unclosed drawing must not hide ordinary terminal space"
    );
}

#[test]
fn frame_detector_handles_many_unmatched_left_corners_without_masking() {
    let (rows, cols) = (8usize, 1024usize);
    let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
    for col in (0..cols).step_by(2) {
        cells[1][col].ch = '╭';
    }
    let sizes = vec![LineSize::SingleWidth; rows];
    let images = vec![Vec::new(); rows];
    let mut rain = MatrixRain::new(cfg_on());
    rain.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 1);
    let band = [7u16];
    let input = RainTickInput {
        hidden_band: &band,
        ..RainTickInput::default()
    };
    assert!(
        rain.cell_eligible(4, 511, &input),
        "unmatched top-left glyphs are not a closed UI surface"
    );
}

#[test]
fn frame_region_overflow_fails_closed_for_the_rescan() {
    let (rows, cols) = ((MAX_FRAME_REGIONS + 1) * 3, 10usize);
    let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
    let draw = |cells: &mut [Vec<RenderCell>], index: usize| {
        let top = index * 3;
        cells[top][0].ch = '╭';
        cells[top][1].ch = '─';
        cells[top][2].ch = '╮';
        cells[top + 1][0].ch = '│';
        cells[top + 1][2].ch = '│';
        cells[top + 2][0].ch = '╰';
        cells[top + 2][1].ch = '─';
        cells[top + 2][2].ch = '╯';
    };
    for index in 0..MAX_FRAME_REGIONS {
        draw(&mut cells, index);
    }
    let sizes = vec![LineSize::SingleWidth; rows];
    let images = vec![Vec::new(); rows];
    let band = [(rows - 1) as u16];
    let input = RainTickInput {
        hidden_band: &band,
        ..RainTickInput::default()
    };
    let mut rain = MatrixRain::new(cfg_on());
    rain.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 1);
    assert!(
        rain.cell_eligible(100, 8, &input),
        "the bounded detector preserves open field at its exact capacity"
    );

    draw(&mut cells, MAX_FRAME_REGIONS);
    rain.rescan_from_cells(&cells, &sizes, &images, rows, cols, BG, 2);
    assert!(
        !rain.cell_eligible(100, 8, &input),
        "a prospective 65th region suppresses rain instead of leaking into UI"
    );
}

#[test]
fn material_bank_samples_output_not_typing() {
    let rows = 6usize;
    let cols = 8usize;
    let mut e = MatrixRain::new(literal_cfg_on());
    let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
    for (i, ch) in "0f3a".chars().enumerate() {
        cells[0][i].ch = ch;
    }
    for (i, ch) in "hunter2".chars().enumerate() {
        cells[3][i].ch = ch;
    }
    e.sample_material(&cells, rows, Some((3, 0)), &[]);
    assert_eq!(e.material.len(), 4, "only the hex row was sampled");
    let decoded = |engine: &MatrixRain| -> String {
        engine
            .material
            .iter()
            .map(|&slot| engine.material_chars[usize::from(slot)])
            .collect()
    };
    assert_eq!(decoded(&e), "0f3a", "literal source order is retained");
    assert_eq!(e.material_chars, vec!['0', '3', 'a', 'f']);

    e.sample_material(&cells, rows, None, &[3]);
    assert_eq!(e.material.len(), 4, "hidden band excludes the secret row");
    assert_eq!(decoded(&e), "0f3a");

    let mut bottom_secret = vec![vec![space_cell(bg3()); cols]; rows];
    for (i, ch) in "hunter2".chars().enumerate() {
        bottom_secret[rows - 1][i].ch = ch;
    }
    e.sample_material(&bottom_secret, rows, None, &[]);
    assert!(
        e.material.is_empty(),
        "unknown composer band ⇒ the bottom rows are never sampled"
    );

    e.sample_material(&cells, rows, Some((5, 0)), &[]);
    assert!(!e.material.is_empty());
    let mut off = e.cfg;
    off.output_material = false;
    e.set_config(off);
    assert!(
        e.material.is_empty(),
        "output_material=false takes effect on the live reload"
    );
    let mut back_on = e.cfg;
    back_on.output_material = true;
    e.set_config(back_on);
    assert!(
        e.needs_material_sample(),
        "classic-to-literal reload requests a host sample without damage"
    );
    e.sample_material(&cells, rows, Some((5, 0)), &[]);
    assert!(!e.needs_material_sample());

    let mut cfg = cfg_on();
    cfg.output_material = false;
    let mut e2 = MatrixRain::new(cfg);
    e2.sample_material(&cells, rows, Some((3, 0)), &[]);
    assert!(
        e2.material.is_empty(),
        "output_material=false stays classic"
    );
}

#[test]
fn literal_material_slots_are_bounded_stable_and_never_faked() {
    let rows = 4usize;
    let mut e = MatrixRain::new(literal_cfg_on());
    let mut cells = vec![vec![space_cell(bg3()); 96]; rows];
    for (cell, ch) in cells[0].iter_mut().zip('!'..='~') {
        cell.ch = ch;
    }
    e.sample_material(&cells, rows, Some((3, 0)), &[]);
    assert!(e.material_chars.len() <= MATERIAL_GLYPH_CAP);
    assert!(
        e.material
            .iter()
            .all(|&slot| usize::from(slot) < e.material_chars.len())
    );
    assert!(
        e.material_chars.iter().any(char::is_ascii_lowercase),
        "recent-slot selection must not sort/truncate lowercase away"
    );

    let mut small = vec![vec![space_cell(bg3()); 8]; rows];
    for (i, ch) in "Codex".chars().enumerate() {
        small[0][i].ch = ch;
    }
    e.sample_material(&small, rows, Some((3, 0)), &[]);
    let version = e.baker.version();
    let slots = e.material_chars.clone();
    for (i, ch) in "xedoC".chars().enumerate() {
        small[0][i].ch = ch;
    }
    e.sample_material(&small, rows, Some((3, 0)), &[]);
    assert_eq!(e.material_chars, slots, "same charset keeps stable slots");
    assert_eq!(
        e.baker.version(),
        version,
        "reordering/frequency alone never rebakes the atlas"
    );

    small[0][0].ch = '🐈';
    for cell in &mut small[0][1..] {
        cell.ch = ' ';
    }
    e.sample_material(&small, rows, Some((3, 0)), &[]);
    assert!(e.material.is_empty(), "unsupported glyph is omitted");
    assert!(
        e.material_chars.is_empty(),
        "unsupported glyph is never substituted"
    );
}

#[test]
fn literal_material_tail_matches_an_exhaustive_forward_walk() {
    fn forward_tail(
        cells: &[Vec<RenderCell>],
        rows: usize,
        cursor: Option<(u16, u16)>,
        hidden_band: &[u16],
    ) -> Vec<char> {
        let mut all: Vec<char> = Vec::new();
        for r in 0..rows {
            let row = r as u16;
            let cursor_banded =
                cursor.is_some_and(|(cr, _)| i32::from(cr.abs_diff(row)) <= CURSOR_BAND_ROWS);
            let remembered_banded = hidden_band.contains(&row);
            let fallback_banded =
                cursor.is_none() && hidden_band.is_empty() && r + HIDDEN_CURSOR_BAND_ROWS >= rows;
            if cursor_banded || remembered_banded || fallback_banded {
                continue;
            }
            let Some(row_cells) = cells.get(r) else {
                continue;
            };
            for cell in row_cells {
                if cell.ch != ' ' && !cell.wide && rom::material_bitmap(cell.ch).is_some() {
                    all.push(cell.ch);
                }
            }
        }
        if all.len() > MATERIAL_CAP {
            all.drain(..all.len() - MATERIAL_CAP);
        }
        all
    }

    let (rows, cols) = (16usize, 24usize);
    let pattern = ['A', 'b', '3', '{', '}', 'z', '7'];
    let mut cells = vec![vec![space_cell(bg3()); cols]; rows];
    for (r, row_cells) in cells.iter_mut().enumerate() {
        for (c, cell) in row_cells.iter_mut().enumerate() {
            match (r + c) % 7 {
                0 => {}
                1 => cell.ch = '\u{1F408}',
                2 => {
                    cell.ch = pattern[(r * cols + c) % pattern.len()];
                    cell.wide = true;
                }
                _ => cell.ch = pattern[(r * cols + c) % pattern.len()],
            }
        }
    }
    cells[9].truncate(5);
    cells.truncate(rows - 1);

    for (cursor, band) in [
        (Some((2u16, 0u16)), &[][..]),
        (Some((2, 0)), &[7u16, 8][..]),
        (None, &[][..]),
        (None, &[11u16][..]),
    ] {
        let mut e = MatrixRain::new(literal_cfg_on());
        e.sample_material(&cells, rows, cursor, band);
        let expected = forward_tail(&cells, rows, cursor, band);
        assert_eq!(
            e.material_scratch, expected,
            "bottom-up sampling must reproduce the forward walk's tail \
             (cursor {cursor:?}, band {band:?})"
        );
        let sampled: Vec<char> = e
            .material
            .iter()
            .map(|&slot| e.material_chars[usize::from(slot)])
            .collect();
        assert_eq!(
            sampled, expected,
            "the emitted tape follows the same source order"
        );
    }
}

#[test]
fn literal_mode_never_emits_a_decorative_fallback() {
    let (rows, cols) = (30usize, 40usize);
    let g = geom(rows as u16, cols as u16, 8, 16);
    let mut e = MatrixRain::new(literal_cfg_on());
    scan_empty(&mut e, rows, cols);
    let blank = vec![vec![space_cell(bg3()); cols]; rows];
    e.sample_material(&blank, rows, Some((rows as u16 - 1, 0)), &[]);
    pour(&mut e);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for _ in 0..30 {
        step(&mut e, g, &idle(), 33, &mut q, &mut a);
    }
    assert!(q.is_empty(), "no real material means no glyph rain");
    assert!(
        !e.is_active(),
        "empty literal mode must not keep a timer armed"
    );

    let mut real = blank.clone();
    for (cell, ch) in real[0].iter_mut().zip("Codex42".chars()) {
        cell.ch = ch;
    }
    e.sample_material(&real, rows, Some((rows as u16 - 1, 0)), &[]);
    for _ in 0..30 {
        step(&mut e, g, &idle(), 33, &mut q, &mut a);
    }
    assert!(
        !q.is_empty(),
        "supported literal output arms the real field"
    );
    assert!(q.iter().all(|quad| !quad.flip_x));

    e.sample_material(&blank, rows, Some((rows as u16 - 1, 0)), &[]);
    step(&mut e, g, &idle(), 33, &mut q, &mut a);
    assert!(
        q.is_empty(),
        "clearing real output cannot reveal fake glyphs"
    );
}

#[test]
fn real_codex_and_claude_frames_feed_literal_output_not_composer() {
    const CLAUDE_FRAME: &[u8] = concat!(
        "\x1b[?1049h\x1b[?2026h\x1b[2J\x1b[H",
        "Claude Code v2.1.206",
        "\x1b[4;4HTips for getting started",
        "\x1b[7;4HWhat's new: directory path suggestions",
        "\x1b[11;1HSafe mode: customizations disabled",
        "\x1b[20;1H> write a test for lib.rs\x1b[20;26H",
        "\x1b[?2026l"
    )
    .as_bytes();
    const CODEX_FRAME: &[u8] = concat!(
        "\x1b[2J\x1b[HOpenAI Codex v0.144.0",
        "\x1b[4;3HWorking directory: /workspace/aterm",
        "\x1b[7;3HReviewing renderer and matrix output",
        "\x1b[10;3HTests completed successfully",
        "\x1b[20;1H> refactor the secret composer draft\x1b[20;35H"
    )
    .as_bytes();

    let sampled = |bytes: &[u8], expect_alt: bool| -> String {
        let rows = 20usize;
        let cols = 80usize;
        let mut term = aterm_core::terminal::Terminal::new(rows as u16, cols as u16);
        term.process(bytes);
        assert_eq!(term.is_alternate_screen(), expect_alt);
        let frame = term.cell_frame(rows, cols);
        let cursor = frame
            .cursor_visible
            .then_some((frame.cursor_row as u16, frame.cursor_col as u16));
        let mut rain = MatrixRain::new(literal_cfg_on());
        rain.sample_material(&frame.cells, rows, cursor, &[]);
        rain.material
            .iter()
            .map(|&slot| rain.material_chars[usize::from(slot)])
            .collect()
    };

    let claude = sampled(CLAUDE_FRAME, true);
    assert!(
        claude.contains("ClaudeCodev2.1.206"),
        "literal Claude header: {claude}"
    );
    assert!(claude.contains("directorypathsuggestions"));
    assert!(
        !claude.contains("writeatestforlib.rs"),
        "composer excluded: {claude}"
    );

    let codex = sampled(CODEX_FRAME, false);
    assert!(
        codex.contains("OpenAICodexv0.144.0"),
        "literal Codex header: {codex}"
    );
    assert!(codex.contains("Testscompletedsuccessfully"));
    assert!(
        !codex.contains("secretcomposerdraft"),
        "composer excluded: {codex}"
    );
}

#[test]
fn emission_never_rains_bottom_band_when_ring_unknown() {
    let g = geom(30, 40, 8, 16);
    let mut e = MatrixRain::new(cfg_on());
    scan_empty(&mut e, 30, 40);
    pour(&mut e);
    let input = RainTickInput {
        cursor: None,
        hidden_band: &[],
        ..RainTickInput::default()
    };
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for _ in 0..30 {
        step(&mut e, g, &input, 33, &mut q, &mut a);
    }
    assert!(!q.is_empty(), "field pours above the band");
    let floor = 30 - HIDDEN_CURSOR_BAND_ROWS as u16;
    assert!(
        q.iter().all(|quad| quad.row < floor),
        "no quad may land in the bottom-{HIDDEN_CURSOR_BAND_ROWS} fallback band"
    );
}

#[test]
fn bell_and_exit_survive_suspension_activity_does_not() {
    let g = geom(30, 40, 8, 16);
    let mut e = MatrixRain::new(cfg_on());
    scan_empty(&mut e, 30, 40);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    e.note_bell();
    e.note_exit_status(true);
    e.note_keystroke();
    for _ in 0..5 {
        e.advance_ms(83);
        e.step_suspended();
    }
    assert_eq!(e.bell_until_ms, 0, "not applied while suspended");
    step(&mut e, g, &idle(), 83, &mut q, &mut a);
    assert!(
        e.bell_until_ms > 0,
        "the bell alert survives the suspension"
    );
    assert!(e.fail_until_ms > 0, "the ember exit tint survives too");
    assert!(
        e.last_key_ms.is_none(),
        "activity notes were dropped, not deferred"
    );
}

#[test]
fn unfocused_drained_engine_disarms_despite_streaming() {
    let g = geom(30, 40, 8, 16);
    let mut e = MatrixRain::new(cfg_on());
    scan_empty(&mut e, 30, 40);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for i in 0..60u64 {
        e.note_activity(i + 1);
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
    }
    assert_eq!(e.weather, RainWeather::Working);
    e.set_visibility(RainVisibility::VisibleUnfocused);
    let mut inactive_at = None;
    for i in 0..120u64 {
        e.note_activity(1000 + i);
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        if !e.is_active() {
            inactive_at = Some(i);
            break;
        }
    }
    assert!(
        inactive_at.is_some(),
        "unfocused pane must disarm after its bounded drain"
    );
    assert!(q.is_empty(), "drained frame is empty");
    e.set_visibility(RainVisibility::Focused);
    assert!(e.is_active(), "refocus resumes an awake weather");
}

#[test]
fn can_emit_probe_tracks_visibility_and_motion() {
    let mut e = MatrixRain::new(cfg_on());
    assert!(e.can_emit(), "focused + full motion");
    e.set_reduced_motion(true);
    assert!(!e.can_emit(), "reduced motion cannot emit");
    e.set_reduced_motion(false);
    e.set_visibility(RainVisibility::Hidden);
    assert!(!e.can_emit(), "hidden pane past its (instant) drain");
    e.set_visibility(RainVisibility::Focused);
    assert!(e.can_emit(), "refocus resets the drain");
}

#[test]
fn exit_status_drives_wave_and_ember() {
    let g = geom(30, 40, 8, 16);
    let mut e = MatrixRain::new(cfg_on());
    scan_empty(&mut e, 30, 40);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    e.note_exit_status(false);
    step(&mut e, g, &idle(), 83, &mut q, &mut a);
    assert!(
        e.wave_pending || e.wave.is_some(),
        "success fires the finishing sweep"
    );

    let mut cfg = cfg_on();
    cfg.exit_tint = false;
    let mut e2 = MatrixRain::new(cfg);
    scan_empty(&mut e2, 30, 40);
    e2.note_exit_status(true);
    step(&mut e2, g, &idle(), 83, &mut q, &mut a);
    assert_eq!(e2.fail_until_ms, 0, "knob off ⇒ inert");
    assert!(!e2.wave_pending, "knob off ⇒ no sweep either");
}

#[test]
fn material_remaps_emitted_glyphs_unmirrored() {
    let g = geom(30, 40, 8, 16);
    let mut e = MatrixRain::new(literal_cfg_on());
    scan_empty(&mut e, 30, 40);
    pour(&mut e);
    e.material_chars = vec!['7'];
    e.rom = Some(rom::rasterize_material_master(&e.material_chars));
    e.baker.restart();
    e.material = vec![0];
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for _ in 0..30 {
        step(&mut e, g, &idle(), 33, &mut q, &mut a);
    }
    assert!(!q.is_empty(), "field emits under pour");
    for quad in &q {
        assert_eq!(quad.glyph, 0, "every rained glyph is the material tile");
        assert!(!quad.flip_x, "exact-form tiles are never mirrored");
    }

    e.material_chars = vec!['2', '7'];
    e.rom = Some(rom::rasterize_material_master(&e.material_chars));
    e.baker.restart();
    e.material = vec![0u8; 64];
    e.material.extend(std::iter::repeat_n(1u8, 64));
    let (mut q2, mut a2) = (Vec::new(), Vec::new());
    for _ in 0..30 {
        step(&mut e, g, &idle(), 33, &mut q2, &mut a2);
    }
    let front = q2.iter().any(|qd| qd.glyph == 0);
    let back = q2.iter().any(|qd| qd.glyph == 1);
    assert!(
        front && back,
        "both table halves must rain (front={front}, back={back}, n={})",
        q2.len()
    );
}

#[test]
fn literal_material_flows_in_source_order_down_each_column() {
    let mut e = MatrixRain::new(literal_cfg_on());
    e.baker.begin_frame(8, 16);
    e.material_chars = vec!['a', 'b', 'c'];
    e.material = vec![0, 1, 2];
    let input = RainTickInput::default();
    let ctx = EmitCtx {
        fp: FieldParams {
            seed32: e.seed32,
            rows: 8,
            tick_ms: 33,
            speed: 5,
            trail: 5,
            mq: 4,
            dq: 12,
            sq: 15,
        },
        geom: geom(8, 8, 8, 16),
        input: &input,
        cap: 64,
        pen: 0,
        ramp: e.ramp,
        wave_row: None,
        glyph_epoch: 0,
        dither_epoch: 0,
    };
    let start = Some(e.material_start(3));
    let a = e.cell_quad(&ctx, 3, 2, 10, false, start);
    let b = e.cell_quad(&ctx, 3, 3, 10, false, start);
    let slot_a = usize::from(a.glyph);
    let slot_b = usize::from(b.glyph);
    assert_eq!(slot_b, (slot_a + 1) % 3);
    assert!(!a.flip_x && !b.flip_x, "literal text is never mirrored");
}

#[test]
fn semantic_pulses_are_bounded_prioritized_and_shape_real_tape_lanes() {
    let mut rain = MatrixRain::new(cfg_on());
    rain.material = (0..12).collect();

    rain.note_signal(RainSignal::Failure as u32, 99);
    rain.note_signal(RainSignal::Inspect as u32, 1);
    rain.apply_pending_notes();
    assert_eq!(rain.semantic_phase, RainSignal::Failure);
    assert_eq!(rain.semantic_energy, SEMANTIC_ENERGY_CAP);
    assert_eq!(rain.semantic_ticks_left, SEMANTIC_HOLD_TICKS);

    rain.note_signal(RainSignal::Inspect as u32, 4);
    rain.apply_pending_notes();
    assert_eq!(rain.semantic_phase, RainSignal::Inspect);
    let inspect_seq = rain.semantic_seq;
    rain.semantic_ticks_left = 1;
    rain.note_signal(RainSignal::Inspect as u32, 6);
    rain.apply_pending_notes();
    assert_eq!(
        rain.semantic_seq, inspect_seq,
        "repeated evidence extends one phase without reseeding its lanes"
    );
    assert_eq!(rain.semantic_ticks_left, SEMANTIC_HOLD_TICKS);
    let index = |rain: &MatrixRain, col, row, epoch| {
        rain.semantic_material_index(rain.material_start(col), col, row, epoch)
    };
    assert_eq!(
        index(&rain, 0, 3, 2),
        index(&rain, 1, 3, 2),
        "inspect forms a coherent two-column scout lane"
    );

    rain.note_signal(RainSignal::Modify as u32, 1);
    rain.apply_pending_notes();
    assert_ne!(
        index(&rain, 0, 2, 1),
        index(&rain, 1, 2, 1),
        "modify counterflows adjacent real-output sequences"
    );

    let before = rain.semantic_phase;
    rain.note_signal(u32::MAX, 8);
    rain.apply_pending_notes();
    assert_eq!(rain.semantic_phase, before, "unknown codes are inert");
}

#[test]
fn semantic_work_wakes_drained_weather_without_a_gap() {
    let g = geom(30, 40, 8, 16);
    let mut rain = MatrixRain::new(cfg_on());
    scan_empty(&mut rain, 30, 40);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for _ in 0..160 {
        step(&mut rain, g, &idle(), 83, &mut q, &mut a);
    }
    assert_eq!(rain.weather, RainWeather::Sleep);
    assert!(!rain.is_active(), "settled glass is wake-free");

    rain.note_signal(RainSignal::Inspect as u32, 3);
    step(&mut rain, g, &idle(), 83, &mut q, &mut a);
    assert_eq!(rain.weather, RainWeather::Working);
    assert!(
        rain.is_active(),
        "observable work re-arms the bounded field"
    );
    assert!(!q.is_empty(), "the wake produces a real frame, not a gap");
}

#[test]
fn live_selection_and_cursor_band_mask() {
    let g = geom(40, 60, 8, 16);
    let mut e = MatrixRain::new(RainConfig {
        density: 12,
        ..cfg_on()
    });
    scan_empty(&mut e, 40, 60);
    pour(&mut e);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for _ in 0..10 {
        e.note_keystroke();
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
    }
    assert!(!q.is_empty(), "field must be raining for the mask test");
    let sel = select_all(40, 60);
    let sel_input = RainTickInput {
        sel: Some(SelView {
            sel: &sel,
            display_offset: 0,
        }),
        ..idle()
    };
    e.note_keystroke();
    let fp = step(&mut e, g, &sel_input, 83, &mut q, &mut a);
    assert_eq!(fp, 0, "selection must suppress every quad");
    assert!(q.is_empty());
    let remembered = [20u16, 21];
    let cur_input = RainTickInput {
        cursor: Some((10, 5)),
        hidden_band: &remembered,
        ..idle()
    };
    e.note_keystroke();
    step(&mut e, g, &cur_input, 83, &mut q, &mut a);
    assert!(!q.is_empty());
    assert!(
        q.iter()
            .all(|s| !(8..=12).contains(&s.row) && !remembered.contains(&s.row)),
        "cursor and remembered multiline composer rows must be masked"
    );
    let band = [35u16, 36, 37, 38, 39];
    let hid_input = RainTickInput {
        hidden_band: &band,
        ..idle()
    };
    e.note_keystroke();
    step(&mut e, g, &hid_input, 83, &mut q, &mut a);
    assert!(
        q.iter().all(|s| s.row < 35),
        "hidden-cursor damaged band must be masked"
    );
}

#[test]
fn reading_gates_suppress_emission() {
    let g = geom(40, 60, 8, 16);
    let mut e = MatrixRain::new(RainConfig {
        density: 12,
        ..cfg_on()
    });
    scan_empty(&mut e, 40, 60);
    pour(&mut e);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for _ in 0..5 {
        e.note_keystroke();
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
    }
    assert!(!q.is_empty());
    let back = RainTickInput {
        display_offset: 3,
        ..idle()
    };
    assert_eq!(step(&mut e, g, &back, 83, &mut q, &mut a), 0);
    assert!(!e.is_active(), "reading + empty frame ⇒ disarmed");
    let alt = RainTickInput {
        is_alt_screen: true,
        ..idle()
    };
    e.note_alt_scroll();
    assert_eq!(step(&mut e, g, &alt, 83, &mut q, &mut a), 0);
    for _ in 0..40 {
        e.note_keystroke();
        step(&mut e, g, &alt, 83, &mut q, &mut a);
    }
    assert!(!q.is_empty(), "scroll-quiet window must expire");
    assert!(e.is_active(), "a live emit re-arms");
    let mut e2 = MatrixRain::new(RainConfig {
        density: 12,
        suppress_in_alt_screen: true,
        ..cfg_on()
    });
    scan_empty(&mut e2, 40, 60);
    pour(&mut e2);
    for _ in 0..10 {
        e2.note_keystroke();
        assert_eq!(step(&mut e2, g, &alt, 83, &mut q, &mut a), 0);
    }
}

#[test]
fn weather_transitions_with_dwell_hysteresis() {
    let g = geom(30, 40, 8, 16);
    let mut e = MatrixRain::new(cfg_on());
    scan_empty(&mut e, 30, 40);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for _ in 0..60 {
        e.note_keystroke();
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert_ne!(
            e.weather,
            RainWeather::Working,
            "keystrokes must cap at CALM"
        );
    }
    assert_eq!(e.weather, RainWeather::Calm);
    let mut e = MatrixRain::new(cfg_on());
    scan_empty(&mut e, 30, 40);
    let mut became_working_at = None;
    for i in 0..60u64 {
        e.note_activity(i + 1);
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        if e.weather == RainWeather::Working {
            became_working_at = Some(i * 83);
            break;
        }
    }
    let at = became_working_at.expect("stream must reach WORKING");
    assert!(at >= 900, "dwell violated: WORKING after only {at} ms");
    let since = e.weather_since_ms;
    let mut left_working_at = None;
    for _ in 0..120u64 {
        e.note_keystroke();
        step(&mut e, g, &idle(), 33, &mut q, &mut a);
        if e.weather != RainWeather::Working {
            left_working_at = Some(e.weather_since_ms);
            break;
        }
    }
    let left = left_working_at.expect("stream end must decay to CALM");
    assert!(
        left.saturating_sub(since) >= DWELL_MS,
        "dwell violated on the way down"
    );
    assert_eq!(e.weather, RainWeather::Calm);
}

#[test]
fn typing_echo_never_pours() {
    let g = geom(30, 40, 8, 16);
    let mut e = MatrixRain::new(cfg_on());
    scan_empty(&mut e, 30, 40);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    let mut seq = 1u64;
    e.note_activity(seq);
    step(&mut e, g, &idle(), 83, &mut q, &mut a);
    for i in 0..120u64 {
        seq += u64::from(CONTENT_CREDIT_CAP);
        e.note_keystroke();
        e.note_activity(seq);
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        assert_ne!(
            e.weather,
            RainWeather::Working,
            "echoed typing must never pour (step {i})"
        );
    }
    assert_eq!(e.weather, RainWeather::Calm, "typing keeps CALM alive");
    for _ in 0..40 {
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
    }
    assert!(e.wave.is_none(), "no turn wave from a typing pause");
    for _ in 0..60u64 {
        seq += 1;
        e.note_activity(seq);
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
    }
    assert_eq!(
        e.weather,
        RainWeather::Working,
        "clean agent output still pours"
    );
}

#[test]
fn suspended_tick_starves_to_sleep_and_disarms() {
    let g = geom(30, 40, 8, 16);
    let mut e = MatrixRain::new(cfg_on());
    scan_empty(&mut e, 30, 40);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for i in 0..60u64 {
        e.note_activity(i + 1);
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
    }
    assert_eq!(e.weather, RainWeather::Working);
    assert!(e.is_active(), "an awake engine keeps the timer armed");
    for _ in 0..145 {
        e.note_keystroke();
        e.advance_ms(83);
        e.step_suspended();
    }
    assert_eq!(e.weather, RainWeather::Sleep, "notes starve to SLEEP");
    assert!(
        !e.is_active(),
        "a drained suspended engine must disarm (no perpetual wakes)"
    );
}

#[test]
fn turn_wave_fires_on_working_to_sleep() {
    let g = geom(30, 40, 8, 16);
    let mut cfg = cfg_on();
    cfg.idle_secs = 2;
    cfg.turn_wave = true;
    let mut e = MatrixRain::new(cfg);
    scan_empty(&mut e, 30, 40);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for i in 0..60u64 {
        e.note_activity(i + 1);
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
    }
    assert_eq!(e.weather, RainWeather::Working);
    let mut saw_wave = false;
    for _ in 0..80 {
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        saw_wave |= e.wave.is_some();
    }
    assert!(saw_wave, "turn wave must fire on a direct WORKING→SLEEP");
    assert_eq!(e.weather, RainWeather::Sleep);
}

#[test]
fn turn_wave_limiter_caps_ignitions() {
    let g = geom(30, 40, 8, 16);
    let mut e = MatrixRain::new(cfg_on());
    scan_empty(&mut e, 30, 40);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    let mut starts: Vec<u64> = Vec::new();
    let mut last_wave: Option<(u64, u32)> = None;
    for i in 0..30u64 {
        e.wave_pending = true;
        e.note_keystroke();
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
        if e.wave != last_wave
            && let Some((start, _)) = e.wave
            && last_wave.is_none_or(|(s, _)| s != start)
        {
            starts.push(i * 83);
        }
        last_wave = e.wave;
    }
    assert!(
        starts.len() >= 3,
        "delayed waves must still eventually grant"
    );
    for w in starts.windows(3) {
        assert!(
            w[2] - w[0] >= 900,
            "3 wave ignitions within a rolling second: {starts:?}"
        );
    }
}

#[test]
fn off_states_emit_nothing_and_disarm() {
    let g = geom(30, 40, 8, 16);
    for disabled in [false, true] {
        let mut e = MatrixRain::new(RainConfig {
            density: 12,
            enabled: !disabled,
            ..cfg_on()
        });
        scan_empty(&mut e, 30, 40);
        pour(&mut e);
        e.set_reduced_motion(!disabled);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for i in 0..10u64 {
            e.note_activity(i);
            assert_eq!(step(&mut e, g, &idle(), 83, &mut q, &mut a), 0);
            assert!(q.is_empty() && a.is_empty(), "disabled={disabled}");
            assert!(!e.is_active(), "disabled={disabled}");
        }
        if disabled {
            assert!(e.tile(0).is_none(), "no tile is baked while disabled");
        }
    }
}

#[test]
fn needs_rescan_follows_the_damage_epoch_and_grid_replacement() {
    let mut e = MatrixRain::new(cfg_on());
    assert!(e.needs_rescan(1), "unscanned engine always rescans");
    scan_empty(&mut e, 10, 10);
    assert!(!e.needs_rescan(1), "an equal epoch holds (the collision)");
    assert!(e.needs_rescan(2), "epoch change re-arms the rescan");
    e.note_grid_replaced();
    assert!(
        e.needs_rescan(1),
        "grid replacement re-arms the rescan at the SAME epoch"
    );
}

#[test]
fn tiny_pane_emits_without_panicking() {
    for rows in [1u16, 2] {
        let mut e = MatrixRain::new(cfg_on());
        scan_empty(&mut e, usize::from(rows), 20);
        pour(&mut e);
        let g = geom(rows, 20, 8, 16);
        let (mut q, mut a) = (Vec::new(), Vec::new());
        for _ in 0..120 {
            e.note_keystroke();
            e.advance_ms(83);
            e.emit(g, &idle(), &mut q, &mut a);
            for quad in &q {
                assert!(quad.row < rows, "emission stays inside the real rows");
            }
        }
    }
}

#[test]
fn note_grid_replaced_clears_the_bank_and_releases_the_editing_latch() {
    let mut e = MatrixRain::new(literal_cfg_on());
    scan_empty(&mut e, 10, 10);
    let mut cells = vec![vec![space_cell(bg3()); 10]; 10];
    for (i, ch) in "0f3a".chars().enumerate() {
        cells[0][i].ch = ch;
    }
    e.sample_material(&cells, 10, None, &[]);
    assert!(!e.literal_material_chars_for_test().is_empty());
    e.note_keystroke();
    assert!(e.material_editing_for_test(), "keystroke latches Editing");
    e.note_grid_replaced();
    assert!(
        e.literal_material_chars_for_test().is_empty(),
        "old grid's literal alphabet must not rain over the new session"
    );
    assert!(e.needs_material_sample(), "fresh sample re-armed");
    assert!(
        !e.material_editing_for_test(),
        "the latch belongs to the OLD grid's draft — the swap releases it"
    );
    e.sample_material(&cells, 10, None, &[]);
    assert!(
        !e.literal_material_chars_for_test().is_empty(),
        "the new grid's output refills the alphabet immediately"
    );
}

#[test]
fn progressive_bake_through_the_engine() {
    let mut e = MatrixRain::new(RainConfig {
        density: 12,
        ..cfg_on()
    });
    scan_empty(&mut e, 30, 40);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    let g = geom(30, 40, 8, 16);
    step(&mut e, g, &idle(), 0, &mut q, &mut a);
    let v1 = e.atlas_version();
    assert!(
        e.tile(0).is_some() && !e.baker.complete(),
        "the first tick bakes a first batch of tiles"
    );
    for _ in 0..8 {
        e.note_keystroke();
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
    }
    assert!(e.baker.complete());
    assert!(e.atlas_version() > v1);
    let settled = e.atlas_version();
    e.note_keystroke();
    step(&mut e, g, &idle(), 83, &mut q, &mut a);
    assert_eq!(e.atlas_version(), settled);
    step(&mut e, geom(30, 40, 10, 20), &idle(), 83, &mut q, &mut a);
    assert_ne!(e.atlas_version(), settled);
}

#[test]
fn quads_are_row_banded_and_tile_aligned() {
    let g = geom(40, 60, 8, 16);
    let mut e = MatrixRain::new(RainConfig {
        density: 12,
        ..cfg_on()
    });
    scan_empty(&mut e, 40, 60);
    pour(&mut e);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    for _ in 0..10 {
        e.note_keystroke();
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
    }
    assert!(!q.is_empty());
    for s in &q {
        assert_eq!((s.w, s.h), (8, 16));
        assert!(
            e.tile(s.glyph).is_some_and(|t| (t.w(), t.h()) == (8, 16)),
            "every cell draws a baked cell-sized tile 1:1"
        );
        assert_eq!(
            u32::from(s.y),
            u32::from(s.row) * 16,
            "one-row-band invariant"
        );
        assert!(s.row < 40 && s.x < 8 * 60);
        assert!(s.alpha <= RAIN_ALPHA_CAP);
    }
    for hq in &a {
        let band = (u32::from(hq.y) / 16) as u16;
        assert_eq!(hq.row, band, "halo quads honor the row band");
        assert_eq!(
            (u32::from(hq.y) + u32::from(hq.h) - 1) / 16,
            u32::from(band),
            "halo band never spills"
        );
    }
}

fn composer_material_gate_model() -> aterm_spec::derive::Model {
    use aterm_spec::ty_model;
    ty_model! {
        ComposerMaterialGate {
            const Buggy = 0;
            var editing = 0;
            var sampled = 0;
            action Key {
                editing = 1;
                sampled = 0;
            }
            action TurnStart {
                editing = 0;
            }
            action TrySample {
                sampled = if editing == 0 { 1 } else { Buggy };
            }
            invariant BooleanBounds: editing <= 1 && sampled <= 1;
            invariant NoUnsentDraftSample: editing + sampled <= 1;
        }
    }
}

#[test]
fn composer_material_gate_model_proves_and_catches_draft_sampling() {
    let model = composer_material_gate_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn real_material_sampling_gate_conforms_and_defers_refresh() {
    let model = composer_material_gate_model();
    let mut state = model.init_state();
    let mut rain = MatrixRain::new(literal_cfg_on());
    let mut cells = vec![vec![space_cell(bg3()); 8]; 6];
    cells[0][0].ch = 'R';

    rain.sample_material(&cells, 6, Some((5, 0)), &[]);
    assert!(model.fire("TrySample", &mut state));
    assert_eq!(i64::from(!rain.material_editing), state["sampled"]);
    assert_eq!(rain.literal_material_chars_for_test(), &['R']);

    rain.note_keystroke();
    assert!(model.fire("Key", &mut state));
    assert_eq!(i64::from(rain.material_editing), state["editing"]);
    cells[0][0].ch = 'z';
    rain.sample_material(&cells, 6, Some((5, 0)), &[]);
    assert!(model.fire("TrySample", &mut state));
    assert_eq!(i64::from(!rain.material_editing), state["sampled"]);
    assert_eq!(
        rain.literal_material_chars_for_test(),
        &['R'],
        "an unsent edit retains the previous real-output tape"
    );
    assert!(
        rain.needs_material_sample(),
        "the trusted refresh stays pending"
    );

    rain.note_signal(RainSignal::TurnStart as u32, 4);
    assert!(model.fire("TurnStart", &mut state));
    assert_eq!(i64::from(rain.material_editing), state["editing"]);
    rain.sample_material(&cells, 6, Some((5, 0)), &[]);
    assert!(model.fire("TrySample", &mut state));
    assert_eq!(i64::from(!rain.material_editing), state["sampled"]);
    assert_eq!(rain.literal_material_chars_for_test(), &['z']);
}

fn semantic_pulse_model() -> aterm_spec::derive::Model {
    use aterm_spec::ty_model;
    ty_model! {
        SemanticRainPulse {
            const Cap = 8;
            const Hold = 24;
            const Buggy = 0;
            var phase = 0;
            var energy = 0;
            var ttl = 0;
            var pending = 0;
            var pending_phase = 0;
            var pending_energy = 0;
            var pending_pri = 0;
            var turn = 0;
            var wave = 0;
            var fail = 0;
            var reduced = 0;
            action QueueAssistant when (reduced == 0) {
                pending = 1;
                pending_phase = if pending == 1 && pending_pri > 0 { pending_phase } else { 0 };
                pending_energy = if pending == 1 && pending_pri > 0 && pending_energy > 2 { pending_energy } else { 2 };
                pending_pri = if pending == 1 && pending_pri > 0 { pending_pri } else { 0 };
            }
            action QueueInspect when (reduced == 0) {
                pending = 1;
                pending_phase = if pending == 1 && pending_pri > 1 { pending_phase } else { 1 };
                pending_energy = if pending == 1 && pending_pri > 1 && pending_energy > 3 { pending_energy } else { 3 };
                pending_pri = if pending == 1 && pending_pri > 1 { pending_pri } else { 1 };
            }
            action QueueModify when (reduced == 0) {
                pending = 1;
                pending_phase = if pending == 1 && pending_pri > 1 { pending_phase } else { 2 };
                pending_energy = if pending == 1 && pending_pri > 1 && pending_energy > 5 { pending_energy } else { 5 };
                pending_pri = if pending == 1 && pending_pri > 1 { pending_pri } else { 1 };
            }
            action QueueExecute when (reduced == 0) {
                pending = 1;
                pending_phase = if pending == 1 && pending_pri > 1 { pending_phase } else { 3 };
                pending_energy = if pending == 1 && pending_pri > 1 && pending_energy > 6 { pending_energy } else { 6 };
                pending_pri = if pending == 1 && pending_pri > 1 { pending_pri } else { 1 };
            }
            action QueueNetwork when (reduced == 0) {
                pending = 1;
                pending_phase = if pending == 1 && pending_pri > 1 { pending_phase } else { 4 };
                pending_energy = if pending == 1 && pending_pri > 1 && pending_energy > 5 { pending_energy } else { 5 };
                pending_pri = if pending == 1 && pending_pri > 1 { pending_pri } else { 1 };
            }
            action QueueBranch when (reduced == 0) {
                pending = 1;
                pending_phase = if pending == 1 && pending_pri > 1 { pending_phase } else { 5 };
                pending_energy = if pending == 1 && pending_pri > 1 && pending_energy > 6 { pending_energy } else { 6 };
                pending_pri = if pending == 1 && pending_pri > 1 { pending_pri } else { 1 };
            }
            action QueueWaiting when (reduced == 0) {
                pending = 1;
                pending_phase = if pending == 1 && pending_pri > 2 { pending_phase } else { 6 };
                pending_energy = if pending == 1 && pending_pri > 2 && pending_energy > 2 { pending_energy } else { 2 };
                pending_pri = if pending == 1 && pending_pri > 2 { pending_pri } else { 2 };
            }
            action QueueSuccess when (reduced == 0) {
                pending = 1;
                pending_phase = if pending == 1 && pending_pri > 2 { pending_phase } else { 7 };
                pending_energy = if pending == 1 && pending_pri > 2 && pending_energy > 6 { pending_energy } else { 6 };
                pending_pri = if pending == 1 && pending_pri > 2 { pending_pri } else { 2 };
            }
            action QueueFailure when (reduced == 0) {
                pending = 1;
                pending_phase = 8;
                pending_energy = if Buggy == 1 { Cap + 1 } else { Cap };
                pending_pri = 3;
            }
            action QueueInterrupted when (reduced == 0) {
                pending = 1;
                pending_phase = 9;
                pending_energy = 7;
                pending_pri = 3;
            }
            action QueueTurnStart when (reduced == 0) {
                pending = 1;
                pending_phase = if pending == 1 && pending_pri > 0 { pending_phase } else { 10 };
                pending_energy = if pending == 1 && pending_pri > 0 && pending_energy > 4 { pending_energy } else { 4 };
                pending_pri = if pending == 1 && pending_pri > 0 { pending_pri } else { 0 };
                turn = 1;
            }
            action Apply when (pending == 1) {
                phase = pending_phase;
                energy = pending_energy;
                ttl = Hold;
                wave = if pending_phase == 7 { 1 } else { wave };
                fail = if pending_phase > 7 && pending_phase <= 9 { 1 } else { fail };
                pending = 0;
                pending_phase = 0;
                pending_energy = 0;
                pending_pri = 0;
                turn = 0;
            }
            action Tick when (ttl > 0) {
                phase = if ttl == 1 { 0 } else { phase };
                energy = if ttl == 1 { 0 } else { energy };
                ttl = ttl - 1;
            }
            action Reduce {
                reduced = 1;
                phase = 0;
                energy = 0;
                ttl = 0;
                pending = 0;
                pending_phase = 0;
                pending_energy = 0;
                pending_pri = 0;
                turn = 0;
            }
            action Restore { reduced = 0; }
            action Reset {
                phase = 0;
                energy = 0;
                ttl = 0;
                pending = 0;
                pending_phase = 0;
                pending_energy = 0;
                pending_pri = 0;
                turn = 0;
                wave = 0;
                fail = 0;
            }
            invariant PhaseBound: phase <= 10;
            invariant PendingPhaseBound: pending_phase <= 10;
            invariant EnergyBound: energy <= Cap;
            invariant PendingEnergyBound: pending_energy <= Cap;
            invariant TtlBound: ttl <= Hold;
            invariant PriorityBound: pending_pri <= 3;
            invariant BooleanBounds: pending <= 1 && turn <= 1 && wave <= 1 && fail <= 1 && reduced <= 1;
        }
    }
}

#[test]
fn semantic_pulse_derived_model_proves_and_catches_overflow() {
    let model = semantic_pulse_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn semantic_pulse_real_engine_conforms_across_all_phases() {
    let model = semantic_pulse_model();
    let cases = [
        (RainSignal::AssistantStream, 2, "QueueAssistant"),
        (RainSignal::Inspect, 3, "QueueInspect"),
        (RainSignal::Modify, 5, "QueueModify"),
        (RainSignal::Execute, 6, "QueueExecute"),
        (RainSignal::Network, 5, "QueueNetwork"),
        (RainSignal::Branch, 6, "QueueBranch"),
        (RainSignal::Waiting, 2, "QueueWaiting"),
        (RainSignal::Success, 6, "QueueSuccess"),
        (RainSignal::Failure, 8, "QueueFailure"),
        (RainSignal::Interrupted, 7, "QueueInterrupted"),
        (RainSignal::TurnStart, 4, "QueueTurnStart"),
    ];
    for (code, (signal, weight, action)) in cases.into_iter().enumerate() {
        assert_eq!(RainSignal::from_code(code as u32), Some(signal));
        let mut state = model.init_state();
        let mut rain = MatrixRain::new(cfg_on());
        rain.note_signal(code as u32, weight);
        assert!(model.fire(action, &mut state));
        let (pending, pending_energy) = rain.pending_signal.expect("queued signal");
        assert_eq!(i64::from(pending as u8), state["pending_phase"]);
        assert_eq!(i64::from(pending_energy), state["pending_energy"]);
        assert_eq!(i64::from(rain.pending_turn_start), state["turn"]);

        rain.apply_pending_notes();
        assert!(model.fire("Apply", &mut state));
        assert_eq!(i64::from(rain.semantic_phase as u8), state["phase"]);
        assert_eq!(i64::from(rain.semantic_energy), state["energy"]);
        assert_eq!(i64::from(rain.semantic_ticks_left), state["ttl"]);
        assert_eq!(i64::from(rain.wave_pending), state["wave"]);
        assert_eq!(i64::from(rain.fail_until_ms > rain.clock_ms), state["fail"]);
    }
    assert_eq!(RainSignal::from_code(cases.len() as u32), None);
}

#[test]
fn semantic_pulse_same_priority_replaces_phase_and_energy() {
    let model = semantic_pulse_model();
    let mut state = model.init_state();
    let mut rain = MatrixRain::new(cfg_on());

    rain.note_signal(RainSignal::Execute as u32, 6);
    assert!(model.fire("QueueExecute", &mut state));
    rain.note_signal(RainSignal::Inspect as u32, 3);
    assert!(model.fire("QueueInspect", &mut state));

    let (pending, energy) = rain.pending_signal.expect("replacement signal");
    assert_eq!(pending, RainSignal::Inspect);
    assert_eq!(energy, 3, "same-priority replacement does not retain heat");
    assert_eq!(i64::from(pending as u8), state["pending_phase"]);
    assert_eq!(i64::from(energy), state["pending_energy"]);

    rain.apply_pending_notes();
    assert!(model.fire("Apply", &mut state));
    assert_eq!(rain.semantic_phase, RainSignal::Inspect);
    assert_eq!(rain.semantic_energy, 3);
    assert_eq!(i64::from(rain.semantic_energy), state["energy"]);
}

#[test]
fn semantic_pulse_real_engine_priority_motion_reset_and_expiry_conform() {
    let model = semantic_pulse_model();
    let mut state = model.init_state();
    let mut rain = MatrixRain::new(cfg_on());

    rain.note_signal(RainSignal::Failure as u32, 8);
    assert!(model.fire("QueueFailure", &mut state));
    rain.note_signal(RainSignal::Inspect as u32, 3);
    assert!(model.fire("QueueInspect", &mut state));
    rain.note_signal(RainSignal::TurnStart as u32, 4);
    assert!(model.fire("QueueTurnStart", &mut state));
    let (pending, energy) = rain.pending_signal.expect("priority-coalesced signal");
    assert_eq!(pending, RainSignal::Failure);
    assert_eq!(i64::from(pending as u8), state["pending_phase"]);
    assert_eq!(i64::from(energy), state["pending_energy"]);
    assert_eq!(i64::from(rain.pending_turn_start), state["turn"]);

    rain.apply_pending_notes();
    assert!(model.fire("Apply", &mut state));
    for _ in 0..SEMANTIC_HOLD_TICKS {
        rain.step_engine_tick();
        assert!(model.fire("Tick", &mut state));
    }
    assert_eq!(rain.semantic_phase, RainSignal::AssistantStream);
    assert_eq!(rain.semantic_ticks_left, 0);

    rain.note_signal(RainSignal::Inspect as u32, 3);
    assert!(model.fire("QueueInspect", &mut state));
    rain.set_reduced_motion(true);
    assert!(model.fire("Reduce", &mut state));
    assert!(rain.pending_signal.is_none());
    rain.note_signal(RainSignal::Failure as u32, 8);
    assert!(rain.pending_signal.is_none(), "reduced motion drops pulses");
    rain.set_reduced_motion(false);
    assert!(model.fire("Restore", &mut state));

    rain.note_signal(RainSignal::Success as u32, 6);
    assert!(model.fire("QueueSuccess", &mut state));
    rain.apply_pending_notes();
    assert!(model.fire("Apply", &mut state));
    rain.reset();
    assert!(model.fire("Reset", &mut state));
    assert_eq!(i64::from(rain.semantic_phase as u8), state["phase"]);
    assert_eq!(i64::from(rain.semantic_energy), state["energy"]);
    assert_eq!(i64::from(rain.semantic_ticks_left), state["ttl"]);
}

fn rain_activity_credit_model() -> aterm_spec::derive::Model {
    use aterm_spec::ty_model;
    ty_model! {
        RainActivityCredit {
            const Cap = 32;
            const Buggy = 0;
            var credit = 0;
            var real = 0;
            var turn = 0;
            action ObserveBurst {
                credit = if Buggy == 1 { Cap + 1 } else { Cap };
            }
            action NoteTurn { turn = 1; }
            action ApplyEcho when (credit > 0 && turn == 0) {
                real = 0;
                credit = 0;
            }
            action ApplyTurn when (credit > 0 && turn == 1) {
                real = credit;
                credit = 0;
                turn = 0;
            }
            action ApplyClean when (credit > 0 && turn == 0) {
                real = credit;
                credit = 0;
            }
            action Rebase {
                credit = 0;
                real = 0;
                turn = 0;
            }
            invariant CreditBound: credit <= Cap;
            invariant RealBound: real <= Cap;
            invariant TurnBound: turn <= 1;
        }
    }
}

#[test]
fn rain_activity_credit_derived_model_proves_and_catches_overflow() {
    let m = rain_activity_credit_model();
    aterm_spec::verify::prove_and_catch_scalar(&m, m.name);
}

#[test]
fn rain_activity_credit_real_engine_conforms_to_model() {
    let m = rain_activity_credit_model();
    let mut st = m.init_state();
    let g = geom(30, 40, 8, 16);
    let mut e = MatrixRain::new(cfg_on());
    scan_empty(&mut e, 30, 40);
    let (mut q, mut a) = (Vec::new(), Vec::new());

    e.note_activity(100);
    e.note_keystroke();
    e.note_activity(10_000);
    assert!(m.fire("ObserveBurst", &mut st));
    assert_eq!(i64::from(e.pending_content_credit), st["credit"]);
    assert!(m.check_invariant("CreditBound", &st));

    step(&mut e, g, &idle(), 83, &mut q, &mut a);
    assert!(m.fire("ApplyEcho", &mut st));
    assert_eq!(e.pending_content_credit, 0);
    assert_eq!(i64::from(e.content_streak), st["real"]);
    assert_eq!(st["real"], 0);

    e.note_keystroke();
    e.note_signal(RainSignal::TurnStart as u32, 4);
    assert!(m.fire("NoteTurn", &mut st));
    e.note_activity(10_000 + u64::from(CONTENT_CREDIT_CAP));
    assert!(m.fire("ObserveBurst", &mut st));
    assert_eq!(i64::from(e.pending_content_credit), st["credit"]);
    step(&mut e, g, &idle(), 83, &mut q, &mut a);
    assert!(m.fire("ApplyTurn", &mut st));
    assert_eq!(i64::from(e.content_streak), st["real"]);
    assert_eq!(st["real"], i64::from(CONTENT_CREDIT_CAP));

    for _ in 0..13 {
        step(&mut e, g, &idle(), 83, &mut q, &mut a);
    }
    e.note_activity(10_000 + 2 * u64::from(CONTENT_CREDIT_CAP));
    assert!(m.fire("ObserveBurst", &mut st));
    step(&mut e, g, &idle(), 83, &mut q, &mut a);
    assert!(m.fire("ApplyClean", &mut st));
    assert_eq!(i64::from(e.content_streak), st["real"]);

    e.note_activity(2);
    assert!(m.fire("Rebase", &mut st));
    assert_eq!(i64::from(e.pending_content_credit), st["credit"]);
    assert_eq!(i64::from(e.content_streak), st["real"]);
    assert!(m.check_invariant("CreditBound", &st));
    assert!(m.check_invariant("RealBound", &st));
}

#[test]
fn rain_lifecycle_conformance_real_engine_projects_onto_model() {
    let m = aterm_models_effects::rain_lifecycle_model();
    let mut st = m.init_state();
    let g = geom(24, 60, 8, 16);
    let mut e = MatrixRain::new(cfg_on());
    scan_empty(&mut e, 24, 60);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    let check_inv = |st: &std::collections::BTreeMap<&'static str, i64>| {
        for name in [
            "NoUnlicensedRain",
            "CanReachIdle",
            "StateBounded",
            "DrainBounded",
        ] {
            assert!(m.check_invariant(name, st), "invariant {name} must hold");
        }
    };

    e.note_activity(1);
    e.note_activity(2);
    step(&mut e, g, &idle(), 100, &mut q, &mut a);
    assert_eq!(
        e.drain_ticks, 0,
        "a focused awake pane carries no drain fuel"
    );
    assert!(e.is_active());
    assert!(
        m.fire("Activity", &mut st),
        "the model admits the enable Activity"
    );
    assert_eq!(st["state"], 1, "enable enters Raining");
    assert_eq!(st["drained"], 0);
    check_inv(&st);

    e.set_visibility(RainVisibility::VisibleUnfocused);
    assert_eq!(e.drain_ticks, 0, "unfocus does not itself spend drain fuel");
    assert!(m.fire("StartDrain", &mut st));
    assert_eq!(st["state"], 2, "unfocus enters Draining");
    assert_eq!(st["drained"], 0);
    check_inv(&st);

    let mut prev = e.drain_ticks;
    let mut guard = 0;
    while e.drain_ticks < DRAIN_TICKS {
        step(&mut e, g, &idle(), u64::from(CALM_TICK_MS), &mut q, &mut a);
        let d = e.drain_ticks;
        for _ in prev..d {
            assert!(
                m.fire("DrainTick", &mut st),
                "the model admits each drain tick"
            );
        }
        assert_eq!(
            st["drained"],
            i64::from(d),
            "model fuel tracks real drain_ticks"
        );
        assert_eq!(
            st["state"],
            if d >= DRAIN_TICKS { 0 } else { 2 },
            "Draining until the fixed drain bound, then Idle"
        );
        check_inv(&st);
        prev = d;
        guard += 1;
        assert!(guard < 200, "the drain must terminate");
    }
    assert_eq!(
        st["state"], 0,
        "the drain lands Idle at exactly DRAIN_TICKS"
    );
    assert_eq!(st["drained"], i64::from(DRAIN_TICKS));

    assert_ne!(
        e.weather,
        RainWeather::Sleep,
        "still awake after a ~2.5 s drain (idle_secs default 8 s)"
    );
    e.set_visibility(RainVisibility::Focused);
    assert_eq!(e.drain_ticks, 0, "a licensed refocus clears the drain fuel");
    assert!(
        m.fire("Activity", &mut st),
        "the licensed resume is an Activity"
    );
    assert_eq!(st["state"], 1, "resume re-enters Raining");
    check_inv(&st);
    assert_eq!(st["rains"], st["acts"]);
}

#[test]
fn a_slept_drained_pane_does_not_relight_on_refocus() {
    let g = geom(24, 60, 8, 16);
    let mut e = MatrixRain::new(RainConfig {
        enabled: true,
        seed: 7,
        idle_secs: 2,
        ..RainConfig::default()
    });
    scan_empty(&mut e, 24, 60);
    let (mut q, mut a) = (Vec::new(), Vec::new());
    e.note_activity(1);
    e.note_activity(2);
    let mut guard = 0;
    while !(e.weather == RainWeather::Sleep && e.drain_ticks >= DRAIN_TICKS) {
        step(&mut e, g, &idle(), u64::from(CALM_TICK_MS), &mut q, &mut a);
        guard += 1;
        assert!(guard < 500, "the pane must sleep and drain");
    }
    assert!(!e.is_active(), "a slept, drained pane is inactive");
    e.set_visibility(RainVisibility::Hidden);
    assert_eq!(e.drain_ticks, DRAIN_TICKS);
    e.set_visibility(RainVisibility::Focused);
    assert_eq!(
        e.drain_ticks, DRAIN_TICKS,
        "a refocus of a SLEEPING pane keeps the drain spent — no phantom relight"
    );
    assert!(
        !e.is_active(),
        "the refocused slept pane stays inactive until real activity"
    );
}
