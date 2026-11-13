// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_ink_between_the_floor_and_the_target_still_holds_every_tile() {
    use crate::native_appearance::contrast_ratio;
    let surface = [30, 30, 30];
    let ink = (0..=255u8)
        .rev()
        .map(|g| [g, g, g])
        .find(|&ink| {
            let ratio = contrast_ratio(ink, surface);
            (AURORA_INK_FLOOR..AURORA_INK_TARGET).contains(&ratio)
        })
        .expect("a grey between the floor and the target on this surface");
    let (cols, rows) = AURORA_GRID;
    let tiles = || (0..rows).flat_map(|j| (0..cols).map(move |i| (i, j)));
    assert!(
        tiles().any(|(i, j)| {
            contrast_ratio(ink, aurora_tile_unheld(surface, i, j)) < AURORA_INK_FLOOR
        }),
        "control: the full pour fails this ink somewhere"
    );
    for (i, j) in tiles() {
        let ratio = contrast_ratio(ink, aurora_tile(surface, ink, i, j));
        assert!(
            ratio >= AURORA_INK_FLOOR,
            "tile ({i}, {j}) holds the ink at {ratio:.2}:1"
        );
    }
}

#[test]
fn enum_runtime_aliases_normalize_in_structured_display() {
    let field = |key: &'static str, seed: &str| EditField {
        label: key,
        key,
        kind: crate::prefs::edit_kind(key),
        seed: Some(seed.to_string()),
        placeholder: String::new(),
    };
    let aliases = [
        (crate::prefs::EDIT_CURSOR_STYLE, "beam", "bar"),
        (crate::prefs::EDIT_BIDI, "off", "disabled"),
        (crate::prefs::EDIT_BIDI, "on", "implicit"),
        (crate::prefs::EDIT_AMBIGUOUS_WIDTH, "single", "narrow"),
        (crate::prefs::EDIT_AMBIGUOUS_WIDTH, "double", "wide"),
        (crate::prefs::EDIT_PREDICTIVE_ECHO, "auto", "adaptive"),
        (crate::prefs::EDIT_PREDICTIVE_ECHO, "on", "adaptive"),
        (crate::prefs::EDIT_PREDICTIVE_ECHO, "true", "adaptive"),
        (crate::prefs::EDIT_PREDICTIVE_ECHO, "force", "always"),
        (
            crate::prefs::EDIT_TEXT_BLENDING,
            "linear_corrected",
            "linear-corrected",
        ),
        (crate::prefs::EDIT_MOTION, "reduce", "reduced"),
        (crate::prefs::EDIT_MOTION, "none", "reduced"),
        (crate::prefs::EDIT_MOTION, "off", "reduced"),
        (
            crate::prefs::EDIT_WINDOW_COLORSPACE,
            "displayp3",
            "display-p3",
        ),
        (crate::prefs::EDIT_WINDOW_COLORSPACE, "p3", "display-p3"),
        (
            crate::prefs::EDIT_BACKGROUND_MATERIAL,
            "underwindow",
            "under-window",
        ),
        (
            crate::prefs::EDIT_BACKGROUND_MATERIAL,
            "under_window",
            "under-window",
        ),
        (crate::prefs::EDIT_BACKGROUND_MATERIAL, "", "none"),
    ];
    for (key, alias, canonical) in aliases {
        let f = field(key, alias);
        assert_eq!(
            SettingsState::display_value(&f),
            canonical,
            "structured display for {key}={alias:?}"
        );
        assert!(
            matches!(f.kind, EditKind::Enum { .. }),
            "alias key {key} must remain an enum"
        );
    }

    for &style in crate::prefs::CURSOR_TRAIL_STYLES {
        let f = field(
            crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
            &format!(" {} ", style.to_ascii_uppercase()),
        );
        assert_eq!(SettingsState::display_value(&f), style, "{style}");
    }

    for &(alias, voice) in aterm_effects::trail_sound::SoundVoice::ALIASES {
        let canonical = voice.name();
        let f = field(crate::prefs::EDIT_TRAIL_SOUND_STYLE, alias);
        assert_eq!(SettingsState::display_value(&f), canonical, "{alias}");
    }
}

#[test]
fn display_value_keeps_annotated_defaults_and_full_custom_values() {
    let trail = EditField {
        label: "Trail style",
        key: crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
        kind: EditKind::Enum {
            options: crate::prefs::CURSOR_TRAIL_STYLES,
        },
        seed: None,
        placeholder: "rainbow kitty (default)".to_string(),
    };
    assert_eq!(
        SettingsState::display_value(&trail),
        "rainbow kitty (default)",
        "native display keeps the explanatory default annotation"
    );

    let custom = EditField {
        seed: Some("future multi word value".to_string()),
        ..trail
    };
    assert_eq!(
        SettingsState::display_value(&custom),
        "future multi word value"
    );
}

#[test]
fn rainbow_kitty_village_preserves_smooth_motion_at_large_phase_ms() {
    let mut prims_a = Vec::new();
    let mut prims_b = Vec::new();
    let mut prims_max = Vec::new();
    paint_rainbow_kitty_village_kitties(&mut prims_a, 10.0, 320.0, 50.0, 100_000_000, false);
    paint_rainbow_kitty_village_kitties(&mut prims_b, 10.0, 320.0, 50.0, 100_000_120, false);
    paint_rainbow_kitty_village_kitties(&mut prims_max, 10.0, 320.0, 50.0, u64::MAX, true);
    assert_ne!(
        format!("{prims_a:?}"),
        format!("{prims_b:?}"),
        "kitties continue moving smoothly past 2^24 ms"
    );
    for p in &prims_max {
        if let DrawPrim::Dot { cx, cy, r, .. } = p {
            assert!(cx.is_finite() && cy.is_finite() && r.is_finite());
        }
    }
}

fn assert_village_kitties_cross_a_week_smoothly(busy: bool) {
    let bodies = |phase_ms| {
        let mut prims = Vec::new();
        paint_rainbow_kitty_village_kitties(&mut prims, 10.0, 320.0, 50.0, phase_ms, busy);
        prims
            .into_iter()
            .filter_map(|prim| match prim {
                DrawPrim::Panel { x, y, .. } => Some((x, y)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let before = bodies(604_799_999);
    let after = bodies(604_800_000);
    assert_eq!(before.len(), 4, "each kitty has a body");
    assert_eq!(after.len(), before.len());
    for (kitty, ((x0, y0), (x1, y1))) in before.into_iter().zip(after).enumerate() {
        // At either speed, one millisecond moves the stroll and the hop under 0.066 px.
        assert!(
            (x1 - x0).abs() < 0.1 && (y1 - y0).abs() < 0.1,
            "kitty {kitty}: ({x0}, {y0}) jumped to ({x1}, {y1})"
        );
    }
}

#[test]
fn rainbow_kitty_village_idle_motion_crosses_a_week_smoothly() {
    assert_village_kitties_cross_a_week_smoothly(false);
}

#[test]
fn rainbow_kitty_village_busy_motion_crosses_a_week_smoothly() {
    assert_village_kitties_cross_a_week_smoothly(true);
}

#[test]
fn rainbow_kitty_village_architecture_is_invariant_across_phase_ms_when_stations_are_stable() {
    let states = ['G', 'G', 'G', 'D', 'E', '_', 'G', 'G'];
    let arch = |phase_ms| {
        let mut prims = Vec::new();
        paint_rainbow_kitty_village_architecture(
            &mut prims,
            10.0,
            10.0,
            320.0,
            50.0,
            [24, 24, 32],
            [60, 60, 72],
            phase_ms,
            false,
            5,
            8,
            &states,
        );
        format!("{prims:?}")
    };
    let baseline = arch(0);
    for phase in [1, 500, 86_399_999, 86_400_000, 604_800_000, u64::MAX] {
        assert_eq!(arch(phase), baseline, "phase_ms={phase}");
    }
}

#[test]
fn rainbow_kitty_village_builds_and_synergies_stay_stable_when_state_is_stable_while_kitties_roam()
{
    let sky = [0x18, 0x16, 0x22];
    let rim = [0x38, 0x34, 0x4C];
    let arch_for = |states: &str, phase_ms: u64| -> Vec<crate::widget::DrawPrim> {
        let spec = format!(
            "phase={phase_ms};busy=0;done={};total={};states={states}",
            states.chars().filter(|&c| c == 'G').count(),
            states.len()
        );
        let (p, b, done, total, parsed) = crate::settings::parse_rainbow_kitty_village_spec(&spec);
        let mut prims = Vec::new();
        crate::settings::paint_rainbow_kitty_village_architecture(
            &mut prims, 0.0, 0.0, 640.0, 48.0, sky, rim, p, b, done, total, &parsed,
        );
        prims
    };
    let kitties_for = |phase_ms: u64| -> Vec<crate::widget::DrawPrim> {
        let mut prims = Vec::new();
        crate::settings::paint_rainbow_kitty_village_kitties(
            &mut prims, 0.0, 640.0, 40.0, phase_ms, false,
        );
        prims
    };

    // The village fills in as stations converge D → G, with neighbour synergies.
    let pending = arch_for("DDDDDDDDDDDD", 0);
    let one_done = arch_for("GDDDDDDDDDDD", 0);
    let pair_synergy = arch_for("GGDDDDDDDDDD", 0);
    let district_synergy = arch_for("GGGDDDDDDDDD", 0);
    let all_verified = arch_for("GGGGGGGGGGGG", 0);
    assert!(
        pending.len() < one_done.len()
            && one_done.len() < pair_synergy.len()
            && pair_synergy.len() < district_synergy.len()
            && district_synergy.len() < all_verified.len(),
        "{} < {} < {} < {} < {}",
        pending.len(),
        one_done.len(),
        pair_synergy.len(),
        district_synergy.len(),
        all_verified.len(),
    );

    // With no station working, the buildings never move with the phase.
    for stable_states in [
        "GGGGGGGGGGGG",
        "GDG_GGGGGGGG",
        "DDDDDDDDDDDD",
        "GGGGGGGGGGGGGGGGGG",
    ] {
        let base = format!("{:?}", arch_for(stable_states, 0));
        for phase in [120, 480, 1440, 5200] {
            assert_eq!(
                base,
                format!("{:?}", arch_for(stable_states, phase)),
                "{stable_states} at phase={phase}"
            );
        }
    }

    // The kitties roam all the same.
    assert_ne!(
        format!("{:?}", kitties_for(0)),
        format!("{:?}", kitties_for(360))
    );
    assert_ne!(
        format!("{:?}", kitties_for(360)),
        format!("{:?}", kitties_for(1200))
    );
}
