// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn luma_is_light(c: [u8; 3]) -> bool {
    0.299 * f32::from(c[0]) + 0.587 * f32::from(c[1]) + 0.114 * f32::from(c[2]) > 150.0
}

fn luma_anchored_floor(c: [u8; 3], bg: [u8; 3], target: f64) -> [u8; 3] {
    let anchor = if luma_is_light(bg) {
        [0, 0, 0]
    } else {
        [255, 255, 255]
    };
    (1..=10u8)
        .map(|step| mix3(c, anchor, f32::from(step) / 10.0))
        .find(|m| contrast(*m, bg) >= target)
        .unwrap_or(anchor)
}

#[test]
fn the_floor_is_met_whenever_either_pole_can_meet_it() {
    let greys = (0..=255u8).map(|g| [g, g, g]);
    let cube = (0..=255u8)
        .step_by(51)
        .flat_map(|r| (0..=255u8).step_by(51).map(move |g| (r, g)))
        .flat_map(|(r, g)| (0..=255u8).step_by(51).map(move |b| [r, g, b]));
    let surfaces: Vec<[u8; 3]> = greys.chain(cube).collect();
    let inks: Vec<[u8; 3]> = (0..=255u8)
        .step_by(17)
        .flat_map(|v| [[v, v, v], [v, 255 - v, v / 2], [255 - v, v / 3, v]])
        .collect();
    for &bg in &surfaces {
        for target in [3.0, 4.5] {
            let reachable =
                contrast([0, 0, 0], bg) >= target || contrast([255, 255, 255], bg) >= target;
            for &c in &inks {
                let got = ensure_contrast(c, bg, target);
                let (before, after) = (contrast(c, bg), contrast(got, bg));
                if before >= target {
                    assert_eq!(got, c, "{c:?} on {bg:?} already cleared {target}");
                }
                if reachable {
                    assert!(
                        after >= target,
                        "{c:?} on {bg:?}: {after:.2} < {target} though a pole clears it"
                    );
                }
                assert!(after >= before, "{c:?} on {bg:?} lost contrast");
            }
        }
    }
}

#[test]
fn warn_and_error_seed_from_the_themes_ansi_yellow_and_red() {
    let theme = ThemeInks {
        bg: [0xfd, 0xf6, 0xe3],
        fg: [0x58, 0x6e, 0x75],
        cursor: [0x58, 0x6e, 0x75],
    };
    let solarized = AnsiHues {
        red: [0xdc, 0x32, 0x2f],
        green: [0x85, 0x99, 0x00],
        yellow: [0xb5, 0x89, 0x00],
        blue: [0x26, 0x8b, 0xd2],
        cyan: [0x2a, 0xa1, 0x98],
    };
    let c = BandInks::derive(theme, Some(solarized), BarBase::Blend);
    let hue = |ink: [u8; 3]| crate::palette::oklch(ink).2;
    assert!((hue(c.warn) - hue(solarized.yellow)).abs() < 3.0);
    assert!((hue(c.error) - hue(solarized.red)).abs() < 3.0);
    assert!(contrast(c.warn, c.bar_bg) >= 4.5 && contrast(c.error, c.bar_bg) >= 4.5);
    let mono = AnsiHues {
        red: [0x80; 3],
        yellow: [0xa0; 3],
        ..solarized
    };
    let m = BandInks::derive(theme, Some(mono), BarBase::Blend);
    let none = BandInks::derive(theme, None, BarBase::Blend);
    assert_eq!((m.warn, m.error), (none.warn, none.error));
    assert_ne!(none.error, c.error, "the fallback is not the theme's red");
}

#[test]
fn warn_and_error_stay_coloured_and_a_lightness_step_apart_on_every_builtin() {
    use crate::palette::{NEAR_GREY_CHROMA, chroma, oklab_distance};
    let bytes = |c: aterm_types::Rgb| [c.r, c.g, c.b];
    let bases = [
        BarBase::Blend,
        BarBase::Fixed {
            light: [0xeb; 3],
            dark: [0x30; 3],
        },
    ];
    let slack = 0.01;
    let names = aterm_types::scheme::builtin_names();
    assert!(names.len() > 1);
    for name in names {
        let scheme = aterm_types::scheme::builtin(name).expect("a listed built-in");
        let fg = bytes(scheme.foreground);
        let theme = ThemeInks {
            bg: bytes(scheme.background),
            fg,
            cursor: scheme.cursor.map_or(fg, bytes),
        };
        let ansi = AnsiHues {
            red: bytes(scheme.ansi[1]),
            green: bytes(scheme.ansi[2]),
            yellow: bytes(scheme.ansi[3]),
            blue: bytes(scheme.ansi[4]),
            cyan: bytes(scheme.ansi[6]),
        };
        for base in bases {
            let c = BandInks::derive(theme, Some(ansi), base);
            let (lw, le) = (oklch(c.warn).0, oklch(c.error).0);
            let gap = lw - le;
            assert!(
                chroma(c.error) >= NEAR_GREY_CHROMA,
                "{name} {base:?}: error {:?} washed out",
                c.error
            );
            assert!(
                gap >= STATUS_LIGHTNESS_GAP - slack,
                "{name} {base:?}: warn L {lw:.3} / error L {le:.3}"
            );
            assert!(
                oklab_distance(c.warn, c.error) >= STATUS_DISTANCE,
                "{name} {base:?}: warn {:?} and error {:?} too close",
                c.warn,
                c.error
            );
            assert!(contrast(c.warn, c.bar_bg) >= 4.5, "{name} {base:?}: warn");
            assert!(contrast(c.error, c.bar_bg) >= 4.5, "{name} {base:?}: error");
        }
    }
}

#[test]
fn an_srgb_mix_floor_washes_a_red_the_lightness_floor_keeps() {
    use crate::palette::{NEAR_GREY_CHROMA, chroma, oklab_distance};
    let (bg, fg) = ([0x2e, 0x34, 0x40], [0xd8, 0xde, 0xe9]);
    let (red, yellow) = ([0xbf, 0x61, 0x6a], [0xeb, 0xcb, 0x8b]);
    let bar = mix3(bg, fg, 0.16);
    let mixed_red = ensure_contrast(red, bar, 4.5);
    let mixed_yellow = ensure_contrast(yellow, bar, 4.5);
    assert!(chroma(mixed_red) < NEAR_GREY_CHROMA);
    assert!(oklab_distance(mixed_red, mixed_yellow) < STATUS_DISTANCE);
    assert!(chroma(lightness_floor(red, bar, 4.5, true)) >= NEAR_GREY_CHROMA);
}

#[test]
fn a_mid_grey_bar_floors_its_label_on_the_side_that_reaches_aa() {
    let (bar, label) = ([0x93; 3], [0x53; 3]);
    assert!(contrast(luma_anchored_floor(label, bar, 4.5), bar) < 4.5);
    let got = ensure_contrast(label, bar, 4.5);
    assert!(contrast(got, bar) >= 4.5);
    assert!(got[0] < label[0], "the label stays darker than its bar");
}

fn pole_sweep() -> Vec<[u8; 3]> {
    let levels: Vec<u8> = (0..=255u8).step_by(15).collect();
    let mut out: Vec<[u8; 3]> = (0..=255u8).map(|g| [g, g, g]).collect();
    for &r in &levels {
        for &g in &levels {
            for &b in &levels {
                out.push([r, g, b]);
            }
        }
    }
    out.extend([[0x01, 0xff, 0x00], [0xcf, 0x0d, 0xcc]]);
    out
}

fn pole_ink_reads_best(is_light: fn([u8; 3]) -> bool, c: [u8; 3]) -> bool {
    let (ink, other) = if is_light(c) {
        (BLACK, WHITE)
    } else {
        (WHITE, BLACK)
    };
    contrast(ink, c) >= 4.58 && contrast(ink, c) >= contrast(other, c)
}

#[test]
fn bg_is_light_picks_the_pole_ink_that_reads_best() {
    let sweep = pole_sweep();
    for &c in &sweep {
        assert!(pole_ink_reads_best(bg_is_light, c), "{c:02x?}");
    }
    assert!(!luma_is_light([0x93; 3]));
    assert!(contrast(WHITE, [0x93; 3]) < 3.1);
    assert!(!luma_is_light([0x01, 0xff, 0x00]));
    assert!(contrast(WHITE, [0x01, 0xff, 0x00]) < 1.4);
    let luma_failures = sweep
        .iter()
        .filter(|&&c| !pole_ink_reads_best(luma_is_light, c))
        .count();
    assert!(luma_failures > 0, "the sweep must catch the luma rule");
}

#[test]
fn the_greys_turn_light_once_at_the_pole() {
    let ramp: Vec<bool> = (0..=255u8).map(|g| bg_is_light([g; 3])).collect();
    let flips = ramp.windows(2).filter(|w| w[0] != w[1]).count();
    assert_eq!(flips, 1);
    assert!(!ramp[0] && ramp[255], "dark to light, never back");
    assert!(!bg_is_light([0x75; 3]));
    assert!(bg_is_light([0x76; 3]));
    assert!(contrast(BLACK, [0x76; 3]) > contrast(WHITE, [0x76; 3]));
    assert!(!bg_is_light([0x11, 0x13, 0x18]));
    assert!(bg_is_light([0xff; 3]));
    assert!(!luma_is_light([0x76; 3]), "the former rule called it dark");
}

#[test]
fn the_forced_palette_maps_each_system_colour_to_its_role() {
    let hc = ForcedPalette {
        window: [0, 0, 0],
        window_text: [0xff, 0xff, 0xff],
        highlight: [0x1a, 0xeb, 0xff],
        highlight_text: [0, 0, 0],
        btn_face: [0, 0, 0],
    };
    let c = BandInks::forced(hc);
    assert_eq!(c.bar_bg, hc.btn_face);
    assert_eq!(c.field_bg, hc.window);
    for ink in [c.label, c.value, c.warn, c.error, c.caret] {
        assert_eq!(ink, hc.window_text);
    }
    for surface in [
        c.accent,
        c.meter,
        c.ring,
        c.capsule_hover,
        c.capsule_primary_hover,
    ] {
        assert_eq!(surface, hc.highlight);
    }
    assert_eq!((c.meter_track, c.chip_ground), (hc.window, hc.window));
    for ink in [
        c.capsule_hover_ink,
        c.capsule_primary_hover_ink,
        c.on_accent,
    ] {
        assert_eq!(ink, hc.highlight_text);
    }
    assert_eq!(c.well_rule, Some(hc.window_text));
    assert_eq!(c.meter_edge, None);
    let hostile = BandInks::forced(ForcedPalette {
        window_text: [0x10, 0x10, 0x10],
        ..hc
    });
    assert!(contrast(hostile.value, hostile.bar_bg) >= FORCED_INK_FLOOR);
    let faint = BandInks::forced(ForcedPalette {
        window: [0x20, 0x20, 0x20],
        ..hc
    });
    assert_eq!(
        faint.well_rule,
        Some(hc.window_text),
        "a well 1.3:1 off the band shows no edge of its own"
    );
    let inset = BandInks::forced(ForcedPalette {
        window: [0x80, 0x80, 0x80],
        ..hc
    });
    assert_eq!(
        inset.well_rule, None,
        "a well 5:1 off the band is its own edge"
    );
}

/// A WELL ITS FILL CANNOT SHOW DRAWS ITS EDGE: a theme's find-bar well stands only a step
/// off the band (Default 1.4:1, GitHub Light's white on its grey band 1.2:1), so it carries
/// [`BandInks::well_rule`], the rim every resting control wears: 3:1 on the well and on the
/// band it closes against, and lighter than the words typed in the well (the theme's ink drew
/// it at their weight, 12:1 on Default's well, a rule heavier than the band's own seam); a
/// well 3:1 off the band is its own edge.
#[test]
fn a_well_under_three_to_one_off_the_band_draws_its_edge() {
    for (bg, fg) in [
        ([0x11, 0x13, 0x18], [0xd6, 0xd7, 0xda]),
        ([0xff, 0xff, 0xff], [0x1f, 0x23, 0x28]),
    ] {
        let c = BandInks::derive(ThemeInks { bg, fg, cursor: fg }, None, BarBase::Blend);
        assert!(contrast(c.field_bg, c.bar_bg) < WELL_EDGE_FLOOR, "{bg:?}");
        let rule = c.well_rule.expect("a faint well draws its edge");
        for (ground, on) in [(c.field_bg, "the well"), (c.bar_bg, "the band")] {
            assert!(
                contrast(rule, ground) >= WELL_EDGE_FLOOR,
                "{bg:?}: the edge on {on} is {:.2}:1",
                contrast(rule, ground)
            );
        }
        assert!(
            contrast(rule, c.field_bg) < contrast(fg, c.field_bg),
            "{bg:?}: the edge is a rim, lighter than the words in the well"
        );
    }
    let c = BandInks::derive(
        ThemeInks {
            bg: [0xff, 0xff, 0xff],
            fg: [0, 0, 0],
            cursor: [0, 0, 0],
        },
        None,
        BarBase::Fixed {
            light: [0x40, 0x40, 0x40],
            dark: [0x40, 0x40, 0x40],
        },
    );
    assert_eq!(
        c.well_rule, None,
        "a white well in a dark band is its own edge"
    );
}
