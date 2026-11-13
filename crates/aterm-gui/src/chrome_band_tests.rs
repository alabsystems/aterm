// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Under every stock High Contrast scheme the band's ink clears AA against the
/// band and the find bar's caret clears it against the well, so a remapping that
/// pairs ink with the wrong surface fails.
#[test]
fn forced_band_colors_meet_wcag_aa_on_every_stock_high_contrast_scheme() {
    for (name, palette) in hc_fixtures::STOCK {
        hc_fixtures::with_forced(palette, || {
            let hostile = Theme {
                fg: 0x0011_1318,
                bg: 0x0011_1318,
                cursor: 0x0011_1318,
                selection: 0x0011_1318,
            };
            let colors = band_colors(hostile);
            assert_eq!(colors.accent, palette.highlight, "{name}");
            assert_eq!(colors.bar_bg, palette.btn_face, "{name}: band is BTNFACE");
            assert_eq!(colors.field_bg, palette.window, "{name}: well is WINDOW");
            for (role, value) in [
                ("value", colors.value),
                ("warn", colors.warn),
                ("label", colors.label),
            ] {
                assert!(
                    contrast(value, colors.bar_bg) >= 4.5,
                    "{name} {role} must meet WCAG-AA under High Contrast"
                );
            }
            assert!(
                contrast(colors.caret, colors.field_bg) >= 4.5,
                "{name} caret must meet WCAG-AA in the well"
            );
            if contrast(colors.field_bg, colors.bar_bg) < 3.0 {
                let rule = colors.well_rule.unwrap_or_else(|| {
                    panic!(
                        "{name}: well and band stand under 3:1 \
                         with nothing drawn to separate them"
                    )
                });
                assert!(
                    contrast(rule, colors.field_bg) >= 3.0,
                    "{name} well border must be visible against the well"
                );
            } else {
                assert_eq!(
                    colors.well_rule, None,
                    "{name}: a well the fill already separates needs no border"
                );
            }
        });
    }
}

/// Installing and retracting the palette is what the host uses to decide whether
/// to invalidate the strip cache, so "changed" has to mean changed.
#[test]
fn install_forced_chrome_reports_only_real_movement() {
    let previous = forced_chrome();
    let a = hc_fixtures::STOCK[0].1;
    let b = hc_fixtures::STOCK[2].1;
    assert!(install_forced_chrome(Some(a)), "off → on is a change");
    assert!(!install_forced_chrome(Some(a)), "same palette is not");
    assert!(install_forced_chrome(Some(b)), "a different HC scheme is");
    assert_eq!(forced_chrome(), Some(b));
    assert!(install_forced_chrome(None), "on → off is a change");
    assert!(!install_forced_chrome(None), "still off is not");
    assert_eq!(forced_chrome(), None);
    install_forced_chrome(previous);
}

/// A palette move consumed by another reader still reaches the settling
/// re-sample: `AppRt::native_appearance_preferences` republishes the palette at
/// window attach and startup, and must not eat the edge the settings `Wake` needs.
#[test]
fn a_consumed_palette_edge_still_reaches_the_resample() {
    let previous = forced_chrome();
    let _ = crate::native_appearance::take_chrome_inputs_moved();

    assert!(install_forced_chrome(Some(hc_fixtures::STOCK[0].1)));
    assert!(
        !install_forced_chrome(Some(hc_fixtures::STOCK[0].1)),
        "the edge is gone: a second install reports no movement"
    );
    assert!(
        crate::native_appearance::take_chrome_inputs_moved(),
        "the palette move was swallowed by the non-resample reader"
    );
    assert!(
        !crate::native_appearance::take_chrome_inputs_moved(),
        "the latch is drained by its one consumer"
    );

    install_forced_chrome(previous);
    let _ = crate::native_appearance::take_chrome_inputs_moved();
}

/// Covers the shipping bit-packing: `install_forced_chrome`/`forced_chrome` use a
/// thread-local under `cfg(test)`, so the five-slot atomic codec needs its own test.
#[test]
fn the_shipping_palette_codec_round_trips_every_channel() {
    for c in [
        [0x00, 0x00, 0x00],
        [0xFF, 0xFF, 0xFF],
        [0x12, 0x34, 0x56],
        [0xFF, 0x00, 0x00],
        [0x00, 0xFF, 0x00],
        [0x00, 0x00, 0xFF],
    ] {
        assert_eq!(unpack(pack(c)), c, "{c:?} must survive the round trip");
    }
    assert_eq!(
        pack([0x12, 0x34, 0x56]),
        0x0012_3456,
        "packed as 0x00RRGGBB"
    );
    assert!(
        (0..=0xFF).all(|v| pack([v, v, v]) != FORCED_ABSENT),
        "FORCED_ABSENT must not collide with any packed colour"
    );
}

/// The publication protocol order: the gate slot closes first, the four tail slots
/// follow, and the gate reopens last with the real `window`.
#[test]
fn the_palette_gate_slot_is_written_last() {
    let p = hc_fixtures::STOCK[1].1;
    let seq = publish_sequence(p);
    assert_eq!(
        seq[0],
        (0, FORCED_ABSENT),
        "the gate must CLOSE before any tail slot moves"
    );
    assert_eq!(
        seq[seq.len() - 1],
        (0, pack(p.window)),
        "the gate must re-open LAST, with the real window colour"
    );
    for (i, (slot, _)) in seq[1..seq.len() - 1].iter().enumerate() {
        assert_eq!(*slot, i + 1, "the tail writes slots 1..=4 in field order");
    }
}

/// The shipping codec on the real atomics: each field lands in its own slot, and
/// the gate slot alone decides whether a palette is visible.
#[test]
fn published_forced_chrome_is_all_or_nothing() {
    assert_eq!(published_forced_chrome(), None, "starts absent");
    let mut last = None;
    for (name, palette) in hc_fixtures::STOCK {
        assert!(publish_forced_chrome(Some(palette)), "{name}: moved");
        assert_eq!(
            published_forced_chrome(),
            Some(palette),
            "{name}: every field round-trips through its own slot"
        );
        assert!(!publish_forced_chrome(Some(palette)), "{name}: idempotent");
        last = Some(palette);
    }
    FORCED_CHROME[0].store(FORCED_ABSENT, Ordering::Release);
    assert_eq!(
        published_forced_chrome(),
        None,
        "the gate slot alone decides whether a palette is visible"
    );
    assert!(
        publish_forced_chrome(last),
        "re-publishing re-opens the gate"
    );
    assert!(publish_forced_chrome(None), "…and retracting is a move");
    assert_eq!(published_forced_chrome(), None);
}

/// On Linux the band's base tone is the Adwaita headerbar's (`#303030` dark,
/// `#EBEBEB` light) for every theme of that appearance: the band is the header of
/// a window the compositor leaves undecorated.
#[cfg(target_os = "linux")]
#[test]
fn linux_band_base_tone_is_the_exact_csd_headerbar_gray() {
    assert_eq!(
        forced_chrome(),
        None,
        "theme-derived band tones are only defined with no OS-forced palette"
    );
    for name in aterm_types::scheme::builtin_names() {
        let scheme = aterm_types::scheme::builtin(name).expect("listed scheme exists");
        let theme = Theme::from(&scheme);
        let expected = if crate::tab_bar::bg_is_light(rgb(theme.bg)) {
            CSD_HEADERBAR_LIGHT
        } else {
            CSD_HEADERBAR_DARK
        };
        assert_eq!(
            band_colors(theme).bar_bg,
            expected,
            "{name}: the band must sit on the CSD's own gray"
        );
    }
}

/// THE TONES ARE KEPT FOR THE SAME THEME (`band_colors`): asked again
/// for the theme last asked for, nothing is derived and the tones are the
/// derivation's exactly; every other theme, the same theme with the
/// meter's hues, and the first theme again each derive anew, their tones
/// their own.
#[test]
fn band_tones_are_derived_once_per_theme() {
    let derive = |theme: Theme, ansi: Option<MeterAnsi>| {
        BandColors::derive(
            ThemeInks {
                bg: rgb(theme.bg),
                fg: rgb(theme.fg),
                cursor: rgb(theme.cursor),
            },
            ansi,
            PLATFORM_BAR_BASE,
        )
    };
    let themes: Vec<Theme> = aterm_types::scheme::builtin_names()
        .iter()
        .map(|name| Theme::from(&aterm_types::scheme::builtin(name).expect("a scheme")))
        .collect();
    assert!(themes.len() > 2, "CONTROL: themes to tell apart");
    let ansi = MeterAnsi {
        red: [200, 40, 40],
        green: [40, 200, 40],
        yellow: [200, 200, 40],
        blue: [40, 40, 200],
        cyan: [40, 200, 200],
    };
    let _ = crate::work_counts::take_band_derives();
    for &theme in themes.iter().chain(&themes[..1]) {
        let colors = band_colors(theme);
        assert_eq!(colors, derive(theme, None), "the derivation's own tones");
        assert_eq!(
            crate::work_counts::take_band_derives(),
            1,
            "CONTROL: derived"
        );
        for _ in 0..3 {
            assert_eq!(band_colors(theme), colors, "kept");
        }
        assert_eq!(
            crate::work_counts::take_band_derives(),
            0,
            "kept, not derived"
        );
        let hued = band_colors(ChromePalette {
            theme,
            ansi: Some(ansi),
            authored: AuthoredChrome::NONE,
        });
        assert_eq!(
            hued,
            derive(theme, Some(ansi)),
            "the hues are the key's too"
        );
        assert_eq!(crate::work_counts::take_band_derives(), 1);
        let _ = band_colors(ChromePalette {
            theme,
            ansi: Some(ansi),
            authored: AuthoredChrome {
                border: Some([0x80; 3]),
                ..AuthoredChrome::NONE
            },
        });
        assert_eq!(crate::work_counts::take_band_derives(), 1);
    }
    assert_ne!(
        band_colors(themes[0]),
        band_colors(themes[1]),
        "CONTROL: two themes' tones differ"
    );
}

#[test]
fn band_colors_meet_wcag_aa_on_every_builtin_scheme() {
    assert_eq!(
        forced_chrome(),
        None,
        "theme-derived band tones are only defined with no OS-forced palette"
    );
    for name in aterm_types::scheme::builtin_names() {
        let scheme = aterm_types::scheme::builtin(name).expect("listed scheme exists");
        let theme = Theme::from(&scheme);
        let colors = band_colors(theme);
        for (role, value) in [
            ("value", colors.value),
            ("warn", colors.warn),
            ("label", colors.label),
        ] {
            assert!(
                contrast(value, colors.bar_bg) >= 4.5,
                "{name} {role} must meet WCAG-AA"
            );
        }
        assert!(
            contrast(colors.caret, colors.field_bg) >= 4.5,
            "{name} caret must meet WCAG-AA in the well"
        );
        match colors.well_rule {
            Some(rule) => assert!(
                contrast(colors.field_bg, colors.bar_bg) < 3.0
                    && contrast(rule, colors.field_bg) >= 3.0
                    && contrast(rule, colors.bar_bg) >= 3.0,
                "{name}: a well under 3:1 off the band draws an edge, 3:1 on the well and \
                 on the band, only that one"
            ),
            None => assert!(
                contrast(colors.field_bg, colors.bar_bg) >= 3.0,
                "{name}: the well's fill alone is its edge only at 3:1"
            ),
        }
    }
}
