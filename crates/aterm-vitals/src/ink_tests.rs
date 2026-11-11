// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const DEFAULT_DARK_BAND: Rgb = [0x30, 0x31, 0x35];
const DEFAULT_FG: Rgb = [0xF8, 0xF8, 0xF2];
const DEFAULT_ACCENT: Rgb = [0x50, 0xFA, 0x7B];
const LIGHT_BAND: Rgb = [0xE2, 0xE3, 0xE5];
const LIGHT_FG: Rgb = [0x2B, 0x2D, 0x33];
const LIGHT_ACCENT: Rgb = [0x1A, 0x7F, 0x37];
const NORD_BAND: Rgb = [0x49, 0x4F, 0x5B];

#[test]
fn oklab_round_trips_srgb() {
    for c in [
        [0, 0, 0],
        [255, 255, 255],
        [0x30, 0x31, 0x35],
        [0xFC, 0xA9, 0x42],
    ] {
        let back = from_lab(to_lab(c));
        for k in 0..3 {
            assert!(back[k].abs_diff(c[k]) <= 1, "{c:?} → {back:?}");
        }
    }
}

#[test]
fn the_ramp_stops_are_the_designs_oklch_stops() {
    let want = [
        (true, 0, (0.340, 0.050, 258.0)),
        (true, 2, (0.620, 0.030, 215.0)),
        (false, 0, (0.860, 0.035, 245.0)),
        (false, 2, (0.645, 0.050, 262.0)),
    ];
    for (dark, i, (l, c, h)) in want {
        let got = ramp_stops(dark)[i];
        let ok = oklch(l, c, h);
        for k in 0..3 {
            assert!(got[k].abs_diff(ok[k]) <= 1, "{dark} {i}: {got:?} vs {ok:?}");
        }
    }
}

#[test]
fn the_ramps_lightness_is_monotone_away_from_the_ground() {
    for (ground, fg, acc) in [
        (DEFAULT_DARK_BAND, DEFAULT_FG, DEFAULT_ACCENT),
        (LIGHT_BAND, LIGHT_FG, LIGHT_ACCENT),
    ] {
        let inks = vitals_inks(ground, fg, acc);
        let mut last = to_lab(inks.load_at(0.0)).l;
        for i in 1..=20 {
            let l = to_lab(inks.load_at(i as f32 / 20.0)).l;
            if inks.dark {
                assert!(l >= last - 1e-3, "{i}: {l} < {last}");
            } else {
                assert!(l <= last + 1e-3, "{i}: {l} > {last}");
            }
            last = l;
        }
        assert!(contrast(inks.load_at(1.0), inks.track) >= 3.0);
    }
}

#[test]
fn the_light_ic_ramp_warms_step_by_step() {
    let chroma = |c: Rgb| {
        let l = to_lab(c);
        l.a.hypot(l.b)
    };
    let warms = |ramp: &[Rgb; 7]| {
        let c: Vec<f32> = ramp.iter().map(|&x| chroma(x)).collect();
        c[0] < 0.06 && c[1] < 0.06 && c[..6].iter().any(|&x| x > 0.12)
    };
    let grounds = [
        LIGHT_BAND,
        [0xFF; 3],
        [0xF6, 0xF8, 0xFA],
        [0xFD, 0xF6, 0xE3],
    ];
    for ground in grounds {
        let inks = vitals_inks(ground, LIGHT_FG, LIGHT_ACCENT);
        assert!(!inks.dark, "{ground:02X?}");
        assert!(warms(&inks.heat_ic), "{ground:02X?}: {:02X?}", inks.heat_ic);
    }
    let plain = heat_ramp(LIGHT_BAND, NON_TEXT, heat_stops(false), false);
    assert!(!warms(&plain), "{plain:02X?}");
}

#[test]
fn the_scope_ladder_steps_away_from_the_ground() {
    for (ground, fg, acc) in [
        (DEFAULT_DARK_BAND, DEFAULT_FG, DEFAULT_ACCENT),
        (LIGHT_BAND, LIGHT_FG, LIGHT_ACCENT),
    ] {
        let inks = vitals_inks(ground, fg, acc);
        let d = |c: Rgb| (to_lab(c).l - to_lab(ground).l).abs();
        assert!(d(inks.track) < d(inks.others), "{inks:?}");
        assert!(d(inks.others) < d(inks.aterm), "{inks:?}");
        assert!(d(inks.aterm) <= d(inks.tab) + 1e-3, "{inks:?}");
        assert!(contrast(inks.tide, inks.track) >= 3.0);
        assert!(contrast(inks.words, ground) >= 4.5);
        assert!(contrast(inks.stuck, ground) >= 3.0, "{inks:?}");
        assert!(contrast(inks.quiet, ground) >= 3.0, "{inks:?}");
    }
}

#[test]
fn words_meet_aa_and_rims_meet_f1_on_every_ground() {
    let cases = [
        (DEFAULT_DARK_BAND, DEFAULT_FG, DEFAULT_ACCENT),
        (LIGHT_BAND, LIGHT_FG, LIGHT_ACCENT),
        ([0x11, 0x13, 0x18], [0xD0, 0xD0, 0xD0], DEFAULT_ACCENT),
        ([0xF6, 0xF7, 0xF9], LIGHT_FG, LIGHT_ACCENT),
        ([0, 0, 0], [0xC0, 0xC0, 0xC0], DEFAULT_ACCENT),
        ([255, 255, 255], [0x60, 0x60, 0x60], LIGHT_ACCENT),
        ([0x00, 0x2B, 0x36], [0x83, 0x94, 0x96], [0x93, 0xA1, 0xA1]),
        ([0xFD, 0xF6, 0xE3], [0x65, 0x7B, 0x83], [0x58, 0x6E, 0x75]),
    ];
    for (ground, fg, acc) in cases {
        let inks = vitals_inks(ground, fg, acc);
        assert!(
            contrast(inks.words, ground) >= WORD_AA,
            "{ground:?} {inks:?}"
        );
        assert!(
            contrast(inks.words2, ground) >= WORD_AA,
            "{ground:?} {inks:?}"
        );
        assert!(
            contrast(inks.rim, ground) >= RIM_FLOOR,
            "{ground:?} {inks:?}"
        );
        assert!(
            contrast(inks.words2, ground) <= contrast(inks.words, ground) + 1e-3,
            "{ground:?} {inks:?}"
        );
    }
}

#[test]
fn the_state_inks_read_on_both_grounds() {
    for (ground, fg, acc) in [
        (DEFAULT_DARK_BAND, DEFAULT_FG, DEFAULT_ACCENT),
        (LIGHT_BAND, LIGHT_FG, LIGHT_ACCENT),
    ] {
        let inks = vitals_inks(ground, fg, acc);
        assert!(contrast(inks.warn, ground) >= 3.0, "{inks:?}");
        assert!(contrast(inks.critical, ground) >= 3.0, "{inks:?}");
        let d = |c: Rgb| (to_lab(c).l - to_lab(ground).l).abs();
        assert!(d(inks.groove) > 0.01 && d(inks.groove) < 0.06, "{inks:?}");
    }
}

#[test]
fn the_rising_sliver_differs_from_its_fill_at_every_level() {
    for ground in [DEFAULT_DARK_BAND, LIGHT_BAND] {
        let inks = vitals_inks(ground, DEFAULT_FG, DEFAULT_ACCENT);
        for i in 0..=20 {
            let fill = inks.load_at(i as f32 / 20.0);
            let s = inks.sliver(fill);
            let (a, b) = (to_lab(fill), to_lab(s));
            assert!((a.l - b.l).abs() >= 0.08, "{i}: {fill:?} vs {s:?}");
        }
    }
}

#[test]
fn heat_steps_follow_the_bins() {
    for (v, step) in [
        (0.0, 0),
        (0.049, 0),
        (0.05, 1),
        (0.199, 1),
        (0.20, 2),
        (0.399, 2),
        (0.40, 3),
        (0.599, 3),
        (0.60, 4),
        (0.799, 4),
        (0.80, 5),
        (0.949, 5),
        (0.95, 6),
        (1.0, 6),
        (1.7, 6),
        (-0.2, 0),
    ] {
        assert_eq!(heat_step(v), step, "{v}");
    }
}

#[test]
#[should_panic(expected = "not a reading")]
fn a_nan_is_never_a_heat_step() {
    let _ = heat_step(f32::NAN);
}

#[test]
fn dwc_heights_follow_the_table() {
    for (cell, want) in [
        (8, [2, 3, 4, 5, 6, 7, 8]),
        (9, [3, 4, 5, 6, 7, 8, 9]),
        (16, [4, 6, 8, 10, 12, 14, 16]),
        (17, [5, 7, 9, 11, 13, 15, 17]),
        (18, [6, 8, 10, 12, 14, 16, 18]),
    ] {
        let got: [u32; 7] = std::array::from_fn(|k| dwc_height(cell, k as u8));
        assert_eq!(got, want, "cell {cell}");
    }
    for cell in 7..=64 {
        assert!(dwc_height(cell, 0) >= 1, "cell {cell}");
        assert_eq!(dwc_height(cell, 6), cell, "cell {cell}");
        for k in 1..=6 {
            assert!(
                dwc_height(cell, k) > dwc_height(cell, k - 1),
                "cell {cell} step {k}"
            );
        }
    }
}

fn hex(s: &str) -> Rgb {
    let n = u32::from_str_radix(s.trim_start_matches('#'), 16).expect("a hex colour");
    [(n >> 16) as u8, (n >> 8) as u8, n as u8]
}

fn hexes<const N: usize>(s: &str) -> [Rgb; N] {
    let v: Vec<Rgb> = s.split_whitespace().map(hex).collect();
    v.try_into().expect("N colours")
}

fn near(name: &str, got: Rgb, want: Rgb) {
    for k in 0..3 {
        assert!(
            got[k].abs_diff(want[k]) <= 1,
            "{name}: {got:02X?}, the table has {want:02X?}"
        );
    }
}

#[test]
fn the_deck_inks_reproduce_the_reference_table() {
    struct Row {
        ground: &'static str,
        fg: &'static str,
        accent: &'static str,
        heat: &'static str,
        heat_text: &'static str,
        heat_ic: &'static str,
        grid_seam: &'static str,
        hollow: &'static str,
        scopes: &'static str,
        scopes_ic: &'static str,
        states: &'static str,
    }
    let rows = [
        Row {
            ground: "#303135",
            fg: "#F8F8F2",
            accent: "#50FA7B",
            heat: "#4C6277 #607886 #768D90 #9DA27D #C3B660 #E1CB57 #FFE148",
            heat_text: "#839BB2 #839CAB #859DA0 #9DA27D #C3B660 #E1CB57 #FFE148",
            heat_ic: "#657E89 #798F8F #979F80 #B5AF6C #D0BF5C #E7D054 #FFE148",
            grid_seam: "#474749 #4B4B4D",
            hollow: "#7A7A7A",
            scopes: "#696A6C #439955 #50FA7B",
            scopes_ic: "#898989 #62B871 #50FA7B",
            states: "#FF9A2E #E9C46A #FF6E6E #B4E7FC",
        },
        Row {
            ground: "#E2E3E5",
            fg: "#24292F",
            accent: "#1A7F37",
            heat: "#94ACC9 #8598B6 #818394 #94664F #9C420F #8C2901 #751A00",
            heat_text: "#516782 #556682 #626474 #885B44 #9C420F #8C2901 #751A00",
            heat_ic: "#82808D #84665B #834931 #802100 #690B00 #530000 #3D0000",
            grid_seam: "#C7C7C8 #C2C3C4",
            hollow: "#818181",
            scopes: "#A0A0A1 #619368 #01732C",
            scopes_ic: "#757575 #3A6B42 #00570E",
            states: "#CA6600 #865E00 #C3242D #20536E",
        },
    ];
    for row in rows {
        let inks = vitals_inks(hex(row.ground), hex(row.fg), hex(row.accent));
        let g = row.ground;
        let ramps = [
            ("heat", inks.heat, hexes::<7>(row.heat)),
            ("heat_text", inks.heat_text, hexes(row.heat_text)),
            ("heat_ic", inks.heat_ic, hexes(row.heat_ic)),
        ];
        for (name, got, want) in ramps {
            for i in 0..7 {
                near(&format!("{g} {name} {i}"), got[i], want[i]);
            }
        }
        let singles = [
            ("grid", inks.grid, hexes::<2>(row.grid_seam)[0]),
            ("seam", inks.seam, hexes::<2>(row.grid_seam)[1]),
            ("hollow", inks.hollow, hex(row.hollow)),
            ("machine", inks.machine, hexes::<3>(row.scopes)[0]),
            ("aterm", inks.aterm, hexes::<3>(row.scopes)[1]),
            ("tab", inks.tab, hexes::<3>(row.scopes)[2]),
            ("machine_ic", inks.machine_ic, hexes::<3>(row.scopes_ic)[0]),
            ("aterm_ic", inks.aterm_ic, hexes::<3>(row.scopes_ic)[1]),
            ("tab_ic", inks.tab_ic, hexes::<3>(row.scopes_ic)[2]),
            ("over", inks.over, hexes::<4>(row.states)[0]),
            ("warn_text", inks.warn_text, hexes::<4>(row.states)[1]),
            (
                "critical_text",
                inks.critical_text,
                hexes::<4>(row.states)[2],
            ),
            ("stuck", inks.stuck, hexes::<4>(row.states)[3]),
        ];
        for (name, got, want) in singles {
            near(&format!("{g} {name}"), got, want);
        }
        let from = if inks.dark { 3 } else { 4 };
        assert_eq!(inks.heat_text[from..], inks.heat[from..], "{g}");
        assert_eq!(inks.deck_floor_misses(), [], "{g}");
    }
    assert_eq!(
        ramp_stops(true),
        hexes::<5>("#273952 #4A6076 #728B92 #C4B65F #FFE148")
    );
    assert_eq!(
        ramp_stops(false),
        hexes::<5>("#BFD4E7 #99B2CF #7D8EAD #A85419 #8C2901")
    );
    assert_eq!(heat_stops(true), ramp_stops(true));
}

#[test]
fn the_deck_inks_hold_on_every_ground() {
    let accents = [
        DEFAULT_ACCENT,
        LIGHT_ACCENT,
        [0x26, 0x8B, 0xD2],
        [0xFF, 0x79, 0xC6],
        [0xD0, 0xD0, 0xD0],
    ];
    let grounds = [
        ([0x00, 0x00, 0x00], [0xC0, 0xC0, 0xC0]),
        ([0x11, 0x13, 0x18], [0xD0, 0xD0, 0xD0]),
        ([0x00, 0x2B, 0x36], [0x83, 0x94, 0x96]),
        ([0x1E, 0x1E, 0x2E], [0xCD, 0xD6, 0xF4]),
        ([0x28, 0x2A, 0x36], [0xF8, 0xF8, 0xF2]),
        (DEFAULT_DARK_BAND, DEFAULT_FG),
        ([0xFF, 0xFF, 0xFF], [0x24, 0x29, 0x2F]),
        ([0xF6, 0xF7, 0xF9], LIGHT_FG),
        ([0xFD, 0xF6, 0xE3], [0x65, 0x7B, 0x83]),
        ([0xEF, 0xF1, 0xF5], [0x4C, 0x4F, 0x69]),
        (LIGHT_BAND, LIGHT_FG),
        ([0xC8, 0xCA, 0xCE], [0x20, 0x20, 0x20]),
        ([0xC0, 0xC0, 0xC0], [0x20, 0x20, 0x20]),
    ];
    for (ground, fg) in grounds {
        for accent in accents {
            let inks = vitals_inks(ground, fg, accent);
            assert_eq!(inks.deck_floor_misses(), [], "{ground:02X?} {accent:02X?}");
        }
    }
}

#[test]
fn a_floor_out_of_reach_is_said_and_the_solver_still_answers() {
    let inks = vitals_inks(NORD_BAND, [0xEC, 0xEF, 0xF4], [0x88, 0xC0, 0xD0]);
    let misses = inks.deck_floor_misses();
    assert_eq!(misses.len(), 1, "{misses:?}");
    assert!(misses[0].out_of_reach, "{misses:?}");
    assert!(misses[0].what.starts_with("heat_ic steps"), "{misses:?}");
    for ground in [NORD_BAND, [0x3A, 0x3B, 0x4E]] {
        let rise = min_rise(
            &heat_ramp(ground, NON_TEXT, heat_ic_stops(true), true),
            true,
        );
        let rises: Vec<f32> = heat_ramps(ground, NON_TEXT, heat_ic_stops(true), true)
            .map(|ramp| min_rise(&ramp, true))
            .collect();
        for &other in &rises {
            assert!(other <= rise && other < HEAT_DL, "{ground:02X?}: {rises:?}");
        }
        let end = rises[rises.len() - 1];
        assert!(rise > end + 0.001, "{ground:02X?}: {rise} vs {end}");
        assert!(rise > 0.035, "{ground:02X?}: {rise}");
    }

    let grey = [0x7A, 0x7A, 0x7A];
    let inks = vitals_inks(grey, [0xFF; 3], DEFAULT_ACCENT);
    let misses = inks.deck_floor_misses();
    assert!(
        misses.iter().any(|m| m.what.starts_with("heat_text")),
        "{misses:?}"
    );
    for miss in misses.iter().filter(|m| m.what.starts_with("heat_text")) {
        assert!(miss.out_of_reach, "{miss:?}");
    }
    for text in inks.heat_text {
        assert!(to_lab(text).l > 0.95, "{text:?}");
    }
}

#[test]
fn the_floor_check_catches_a_broken_ink() {
    let good = vitals_inks(DEFAULT_DARK_BAND, DEFAULT_FG, DEFAULT_ACCENT);
    let mut broken = good;
    broken.over = good.critical_text;
    broken.heat_text[1] = good.heat[1];
    broken.heat[4] = good.heat[3];
    broken.load[2] = good.heat[2];
    broken.heat_ic[0] = good.groove;
    let mut misses = broken.deck_floor_misses();
    let mut settled = vitals_inks(NORD_BAND, [0xEC, 0xEF, 0xF4], [0x88, 0xC0, 0xD0]);
    let ramps = heat_ramps(NORD_BAND, NON_TEXT, heat_ic_stops(true), true);
    settled.heat_ic = ramps.last().expect("the end of the range");
    misses.extend(settled.deck_floor_misses());
    for (start, n) in [
        ("over 0.000 from critical_text", 1),
        ("heat_text 1", 1),
        ("heat steps", 1),
        ("heat_ic has 1 of 7 steps on the ground's near side", 1),
        ("heat_ic steps 0.036", 1),
        ("load", 1),
    ] {
        let found: Vec<_> = misses
            .iter()
            .filter(|m| m.what.starts_with(start))
            .collect();
        assert_eq!(found.len(), n, "{start}: {misses:?}");
        assert!(!found[0].out_of_reach, "{start}: {misses:?}");
    }
}

#[test]
fn the_heat_ramp_never_crosses_a_mid_tone_ground() {
    for v in 0x60..=0xA0 {
        let ground = [v; 3];
        let dark = is_dark(ground);
        let fg = if dark { [0xFF; 3] } else { [0; 3] };
        let inks = vitals_inks(ground, fg, DEFAULT_ACCENT);
        let g = to_lab(ground).l;
        let away = if dark { 1.0 } else { -1.0 };
        for ramp in [inks.heat, inks.heat_ic] {
            for c in ramp {
                let past = (to_lab(c).l - g) * away;
                assert!(past > 0.0, "{ground:02X?}: {ramp:02X?}");
            }
        }
        let misses = inks.deck_floor_misses();
        for miss in misses.iter().filter(|m| m.what.starts_with("heat")) {
            assert!(miss.out_of_reach, "{ground:02X?}: {miss:?}");
        }
    }
}

#[test]
fn polarity_and_halo_mode_follow_the_ground() {
    assert!(is_dark(DEFAULT_DARK_BAND));
    assert!(!is_dark(LIGHT_BAND));
    let dark = vitals_inks(DEFAULT_DARK_BAND, DEFAULT_FG, DEFAULT_ACCENT);
    let light = vitals_inks(LIGHT_BAND, LIGHT_FG, LIGHT_ACCENT);
    assert_eq!(dark.rise_mode, HaloMode::Add);
    assert_eq!(light.rise_mode, HaloMode::Over);
}

/// A data cell (a strip's cell, its live now box, a core square, the lens's per-core map)
/// draws the heat ramp's step for its level, whatever the theme's accent, under Increase
/// Contrast too: 979d8c3559's scheme, with no amber, orange or crimson from any level (the
/// owner, 2026-10-26: "go back to the orignal color scheme", and of the now box: "there is a
/// "now" box with green that is out of the color scheme"). The dial keeps its warm stops, at
/// the non-text floor and running orange to red ("I like the glowing orange on the dial").
#[test]
fn a_data_cell_draws_the_heat_ramp_and_only_the_dial_keeps_the_warm_stops() {
    let accents = [
        DEFAULT_ACCENT,
        LIGHT_ACCENT,
        [0x50, 0xDC, 0xFA],
        [0xFF, 0x79, 0xC6],
        [0x58, 0xA6, 0xFF],
        [0xFF, 0xB8, 0x6C],
    ];
    let grounds = [
        (DEFAULT_DARK_BAND, DEFAULT_FG),
        (LIGHT_BAND, LIGHT_FG),
        (NORD_BAND, DEFAULT_FG),
        ([0x11, 0x13, 0x18], [0xD0, 0xD0, 0xD0]),
        ([0xF6, 0xF7, 0xF9], LIGHT_FG),
    ];
    let mut accent_moved = 0;
    for (ground, fg) in grounds {
        let base = vitals_inks(ground, fg, DEFAULT_ACCENT);
        for accent in accents {
            let inks = vitals_inks(ground, fg, accent);
            let at = format!("{ground:02X?} accent {accent:02X?}");
            for ic in [false, true] {
                let (heat, thermal) = if ic {
                    (inks.heat_ic, inks.thermal_ic)
                } else {
                    (inks.heat, inks.thermal)
                };
                for i in 0..=200 {
                    let v = i as f32 / 200.0;
                    let got = inks.cell_at(v, ic);
                    assert_eq!(got, heat[usize::from(heat_step(v))], "{at} ic {ic}: {v}");
                    assert_eq!(got, base.cell_at(v, ic), "{at} ic {ic}: {v}");
                    assert!(
                        !thermal[2..].contains(&got),
                        "{at} ic {ic}: level {v} drew a dial stop {got:02X?}"
                    );
                }
                for c in &thermal[2..] {
                    assert!(contrast(*c, ground) >= NON_TEXT, "{at} ic {ic}: {c:02X?}");
                }
                let [_, _, amber, orange, red] = thermal;
                assert!(
                    red[0] > red[2] && orange[0] > orange[2] && amber[0] > amber[2],
                    "{at} ic {ic}: the dial's warm stops run amber, orange, red: \
                     {amber:02X?} {orange:02X?} {red:02X?}"
                );
            }
            assert_eq!(
                inks.cell_at(f32::NAN, false),
                inks.heat[0],
                "{at}: no reading"
            );
            // CONTROL: the accent does move the inks it owns, so the equality above is not
            // two copies of one constant.
            accent_moved += usize::from(inks.aterm != base.aterm);
        }
    }
    assert!(
        accent_moved >= 20,
        "CONTROL: accents moved aterm's ink {accent_moved} times"
    );
}
