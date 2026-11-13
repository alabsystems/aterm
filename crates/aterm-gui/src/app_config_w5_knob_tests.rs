// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Config, KnobChange, RenderKnobs, ThemeCatalog};

fn cfg(toml: &str) -> Config {
    aterm_toml::from_str(toml).expect("valid toml")
}

fn knobs_of(c: &Config) -> RenderKnobs {
    RenderKnobs::from_config(
        c,
        &c.resolved_colors(
            aterm_types::Appearance::Dark,
            &ThemeCatalog::default(),
            None,
        ),
    )
}

#[test]
fn new_keys_parse_and_clamp() {
    let read = |key: &str, c: &Config| -> f64 {
        match key {
            "stem_gamma" => f64::from(c.stem_gamma_or_default()),
            "trail_sound_volume" => f64::from(c.trail_sound_volume()),
            "line_height" => f64::from(c.line_height_or_default()),
            "minimum_contrast" => f64::from(c.minimum_contrast_or_default()),
            "adjust_baseline" => f64::from(c.adjust_baseline_or_default()),
            "adjust_underline_position" => f64::from(c.adjust_underline_position_or_default()),
            "adjust_underline_thickness" => f64::from(c.adjust_underline_thickness_or_default()),
            "faint_opacity" => f64::from(c.faint_opacity_or_default()),
            "background_opacity" => f64::from(c.background_opacity_or_default()),
            other => panic!("no reader for {other}"),
        }
    };
    for (key, value, want) in [
        ("stem_gamma", "99.0", 3.0_f32),
        ("trail_sound_volume", "2.0", 1.0),
        ("trail_sound_volume", "nan", 0.0),
        ("line_height", "9.0", 2.0),
        ("line_height", "0.1", 0.8),
        ("line_height", "nan", super::DEFAULT_LINE_HEIGHT),
        ("minimum_contrast", "99.0", 21.0),
        ("adjust_baseline", "400", 32.0),
        ("adjust_underline_position", "400", 32.0),
        ("adjust_underline_thickness", "-400", -32.0),
        ("faint_opacity", "7.0", 1.0),
        ("background_opacity", "-3.0", 0.0),
    ] {
        assert_eq!(
            read(key, &cfg(&format!("{key} = {value}"))),
            f64::from(want),
            "{key} = {value}"
        );
    }
    assert_eq!(
        cfg("background_material = \"HUD\"").background_material_or_default(),
        super::BackgroundMaterial::Hud,
        "case-insensitive"
    );
    assert_eq!(
        cfg("background_material = \"under_window\"").background_material_or_default(),
        super::BackgroundMaterial::UnderWindow,
        "alias"
    );
    assert_eq!(
        cfg("background_material = \"bogus\"").background_material_or_default(),
        super::BackgroundMaterial::None,
        "unknown → none"
    );
    use aterm_render::TextBlending;
    for (source, expected) in [
        ("text_blending = \"linear\"", TextBlending::Linear),
        (
            "text_blending = \"Linear_Corrected\"",
            TextBlending::LinearCorrected,
        ),
        ("text_blending = \"gamma\"", TextBlending::LinearCorrected),
    ] {
        assert_eq!(cfg(source).text_blending_or_default(), expected, "{source}");
    }
    assert!(
        !Config::default().trail_sound_bed_or_default(),
        "ambient bed defaults off so typing notes carry no continuous drone"
    );
    assert!(cfg("trail_sound_bed = true").trail_sound_bed_or_default());
    assert!(!cfg("trail_sound_bed = false").trail_sound_bed_or_default());
}

#[test]
fn ligature_keys_map_to_shaping_modes() {
    use aterm_types::text_shaping::LigatureMode;
    assert_eq!(
        cfg("cursor_break_ligatures = true")
            .text_shaping()
            .ligature_mode,
        LigatureMode::CursorDisabled
    );
    assert_eq!(
        cfg("cursor_break_ligatures = false")
            .text_shaping()
            .ligature_mode,
        LigatureMode::Enabled
    );
    assert_eq!(
        cfg("ligatures = false\ncursor_break_ligatures = true")
            .text_shaping()
            .ligature_mode,
        LigatureMode::Disabled,
        "ligatures=false wins"
    );
    let merged = cfg("merged_ligatures = true\nligatures = false").text_shaping();
    assert!(merged.admit_collapsed);
    assert_eq!(merged.ligature_mode, LigatureMode::Disabled);
}

#[test]
fn new_keys_round_trip_through_prefs_writer() {
    use crate::prefs::{
        EDIT_ADJUST_BASELINE, EDIT_BOLD_IS_BRIGHT, EDIT_CURSOR_BREAK_LIGATURES, EDIT_FAINT_OPACITY,
        EDIT_LINE_HEIGHT, EDIT_MINIMUM_CONTRAST, EDIT_SELECTION_FOREGROUND,
        EDIT_SELECTION_INACTIVE, apply_prefs_edits,
    };
    let out = apply_prefs_edits(
        "",
        &[
            (EDIT_LINE_HEIGHT, Some("1.35".to_string())),
            (EDIT_MINIMUM_CONTRAST, Some("4.5".to_string())),
            (EDIT_SELECTION_FOREGROUND, Some("#aabbcc".to_string())),
            (EDIT_SELECTION_INACTIVE, Some("true".to_string())),
            (EDIT_CURSOR_BREAK_LIGATURES, Some("true".to_string())),
            (EDIT_BOLD_IS_BRIGHT, Some("false".to_string())),
            (EDIT_FAINT_OPACITY, Some("0.4".to_string())),
            (EDIT_ADJUST_BASELINE, Some("-2".to_string())),
        ],
    )
    .expect("writes typed values");
    let c: Config = aterm_toml::from_str(&out).expect("round-trips through serde");
    assert_eq!(c.line_height_or_default(), 1.35);
    assert_eq!(c.minimum_contrast_or_default(), 4.5);
    assert_eq!(knobs_of(&c).selection_fg, Some(0x00AA_BBCC));
    assert!(c.selection_inactive_or_default());
    assert_eq!(c.cursor_break_ligatures, Some(true));
    assert_eq!(c.bold_is_bright, Some(false));
    assert_eq!(c.faint_opacity_or_default(), 0.4);
    assert_eq!(c.adjust_baseline_or_default(), -2);
}

#[test]
fn each_knob_key_diffs_to_exactly_one_change() {
    let base = RenderKnobs::default();
    assert!(base.diff(&base).is_empty(), "no change → no calls");

    let cases: [(&str, KnobChange); 9] = [
        ("line_height = 1.5", KnobChange::LineHeight(1.5)),
        ("minimum_contrast = 4.5", KnobChange::MinimumContrast(4.5)),
        (
            "selection_foreground = \"#010203\"",
            KnobChange::SelectionFg(Some(0x0001_0203)),
        ),
        (
            "selection_inactive = true",
            KnobChange::SelectionInactive(true),
        ),
        ("adjust_baseline = 2", KnobChange::AdjustBaseline(2)),
        (
            "adjust_underline_position = 2",
            KnobChange::AdjustUnderline(2, 0),
        ),
        (
            "adjust_underline_thickness = 1",
            KnobChange::AdjustUnderline(0, 1),
        ),
        (
            "underline_skip_descenders = false",
            KnobChange::UnderlineSkipDescenders(false),
        ),
        (
            "background_material = \"hud\"",
            KnobChange::BackgroundMaterial(super::BackgroundMaterial::Hud),
        ),
    ];
    for (toml, expected) in cases {
        let knobs = knobs_of(&cfg(toml));
        let changes = base.diff(&knobs);
        assert_eq!(
            changes,
            vec![expected],
            "{toml:?} must diff to exactly one renderer call"
        );
    }
    let all = knobs_of(&cfg(
        "line_height = 1.5\nminimum_contrast = 4.5\nadjust_baseline = 2",
    ));
    assert_eq!(all.diff(&base).len(), 3, "one change per reverted key");
}

#[test]
fn translucency_couples_opacity_to_the_contrast_floor() {
    let base = RenderKnobs::default();
    let glass = knobs_of(&cfg("background_opacity = 0.6"));
    let changes = base.diff(&glass);
    assert!(
        changes.contains(&KnobChange::BackgroundOpacity(0.6)),
        "opacity change must fire: {changes:?}"
    );
    assert!(
        changes.contains(&KnobChange::MinimumContrast(4.5)),
        "glass must auto-engage the WCAG-AA floor: {changes:?}"
    );
    assert_eq!(changes.len(), 2, "exactly the opacity + the auto-floor");

    let glass_hi = knobs_of(&cfg("background_opacity = 0.6\nminimum_contrast = 7.0"));
    let hi_base = knobs_of(&cfg("minimum_contrast = 7.0"));
    assert_eq!(
        hi_base.diff(&glass_hi),
        vec![KnobChange::BackgroundOpacity(0.6)],
        "an already-legible floor is not disturbed by engaging glass"
    );

    assert_eq!(
        glass.diff(&base).len(),
        2,
        "revert drops opacity + auto-floor"
    );
}
