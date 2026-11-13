// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    Config, EDIT_ADJUST_BASELINE, EDIT_COLUMNS, EDIT_SCROLLBACK, EditKind, NESTED_LEAVES,
    PrefsEditError, apply_prefs_edits, edit_kind, editable_fields, integer_domain, range_of,
    section_of,
};

fn set(v: &str) -> Option<String> {
    Some(v.to_string())
}

#[test]
fn nested_leaves_have_agreeing_rows_and_serde_round_trip() {
    let fields = editable_fields(&Config::default());
    for leaf in NESTED_LEAVES {
        let row = fields
            .iter()
            .find(|f| f.key == leaf.key)
            .unwrap_or_else(|| panic!("{} missing from editable_fields", leaf.key));
        assert_eq!(row.kind, leaf.kind, "{} row kind", leaf.key);
        assert_eq!(edit_kind(leaf.key), leaf.kind, "{} writer kind", leaf.key);
        match leaf.kind {
            EditKind::Bool => assert!(
                row.seed.is_some(),
                "{} is a Bool row and must seed its resolved state",
                leaf.key
            ),
            _ => assert!(
                !row.placeholder.is_empty(),
                "{} must advertise its effective default in the placeholder",
                leaf.key
            ),
        }
        let (group, order) = super::group_of(leaf.key);
        assert_ne!(group, "General", "{} fell to the catch-all group", leaf.key);
        assert!(order < u8::MAX, "{} group order unset", leaf.key);
        let _ = section_of(leaf.key); // total (never panics) — routing pinned above via group
        let sample = match leaf.kind {
            EditKind::Bool => "true",
            EditKind::Integer => "5",
            EditKind::Float => "0.5",
            EditKind::Enum { options } => options[0],
            EditKind::Color => "#123456",
            EditKind::Text if leaf.key == super::EDIT_HARNESS_LAND_AT => "18:00",
            EditKind::Text | EditKind::Theme => "sample",
        };
        let out = apply_prefs_edits("", &[(leaf.key, set(sample))])
            .unwrap_or_else(|e| panic!("{} rejected its own sample value: {e}", leaf.key));
        let _: Config = aterm_toml::from_str(&out)
            .unwrap_or_else(|e| panic!("{} wrote TOML serde rejects: {e}\n---\n{out}", leaf.key));
    }
}

#[test]
fn the_session_log_switches_are_bool_rows_beside_restore() {
    let read = |key: &str, cfg: &Config| match key {
        super::EDIT_SESSION_LOG_JOURNAL => cfg.session_log_journal(),
        _ => cfg.session_log_clause(),
    };
    let fields = editable_fields(&Config::default());
    for key in [
        super::EDIT_SESSION_LOG_JOURNAL,
        super::EDIT_SESSION_LOG_CLAUSE,
    ] {
        let (table, leaf) = key.split_once('.').expect("a dotted key");
        let off = apply_prefs_edits("", &[(key, set("false"))])
            .unwrap_or_else(|e| panic!("{key} = false: {e}"));
        assert!(
            off.lines().any(|l| l.trim() == format!("[{table}]")),
            "a [{table}] table: {off}"
        );
        assert!(
            off.lines().any(|l| l.trim() == format!("{leaf} = false")),
            "a TOML boolean, never a string: {off}"
        );
        let saved: Config = aterm_toml::from_str(&off)
            .unwrap_or_else(|e| panic!("{key} wrote TOML serde rejects: {e}\n---\n{off}"));
        assert!(!read(key, &saved), "{key} reads back off");
        let seeded = editable_fields(&saved);
        let row = seeded.iter().find(|f| f.key == key).expect("the row");
        assert_eq!(
            row.seed.as_deref(),
            Some("false"),
            "{key} seeds the saved value"
        );
        let on = apply_prefs_edits(&off, &[(key, set("true"))])
            .unwrap_or_else(|e| panic!("{key} = true: {e}"));
        let saved: Config = aterm_toml::from_str(&on)
            .unwrap_or_else(|e| panic!("{key} wrote TOML serde rejects: {e}\n---\n{on}"));
        assert!(read(key, &saved), "{key} reads back on: {on}");
        let row = fields
            .iter()
            .find(|f| f.key == key)
            .unwrap_or_else(|| panic!("{key} is a Settings row"));
        assert_eq!(row.kind, EditKind::Bool, "{key} row kind");
        assert_eq!(edit_kind(key), EditKind::Bool, "{key} writer kind");
        assert_eq!(row.seed.as_deref(), Some("true"), "{key} seeds its default");
        assert_eq!(
            super::group_of(key),
            super::group_of(super::EDIT_RESTORE_SESSION),
            "{key} sits beside Restore session at launch"
        );
        assert_eq!(
            section_of(key),
            section_of(super::EDIT_RESTORE_SESSION),
            "{key} section"
        );
    }
}

#[test]
fn an_integer_refusal_names_the_bound_it_broke() {
    for (key, raw, words) in [
        (EDIT_COLUMNS, "70000", "must be at most 65535"),
        (EDIT_COLUMNS, "-1", "must be 0 or more"),
        (
            EDIT_COLUMNS,
            "wide",
            "must be a whole number from 0 to 65535",
        ),
        (
            EDIT_COLUMNS,
            "99999999999999999999",
            "must be at most 65535",
        ),
        (
            EDIT_SCROLLBACK,
            "9223372036854775808",
            "must be at most 9223372036854775807",
        ),
        (
            EDIT_SCROLLBACK,
            "99999999999999999999x",
            "must be a whole number, 0 or more",
        ),
        (
            EDIT_SCROLLBACK,
            "-99999999999999999999",
            "must be 0 or more",
        ),
        (
            "sparkle_words.profanity.jitter",
            "-99999999999999999999",
            "must be at least -128 px",
        ),
        (
            "sparkle_words.profanity.jitter",
            "200",
            "must be at most 127 px",
        ),
        (
            EDIT_ADJUST_BASELINE,
            "-9223372036854775809",
            "must be at least -9223372036854775808 px",
        ),
    ] {
        let error = apply_prefs_edits("", &[(key, set(raw))])
            .expect_err("an out-of-domain integer is refused")
            .to_string();
        assert!(
            error.contains(key) && error.contains(raw) && error.contains(words),
            "{key} = {raw}: {error}"
        );
    }
}

#[test]
fn integer_keys_enforce_their_serde_domain() {
    let fields = editable_fields(&Config::default());
    let integer_keys: Vec<&str> = fields
        .iter()
        .filter(|f| matches!(f.kind, EditKind::Integer))
        .map(|f| f.key)
        .collect();
    assert!(
        integer_keys.contains(&"scrollback_lines") && integer_keys.contains(&"matrix_rain.seed"),
        "sanity: the walk sees top-level AND nested Integer keys ({integer_keys:?})"
    );
    let saves = |key: &str, value: i128| -> bool {
        match apply_prefs_edits("", &[(key, set(&value.to_string()))]) {
            Ok(out) => {
                let _: Config = aterm_toml::from_str(&out).unwrap_or_else(|e| {
                    panic!(
                        "{key} = {value} was ACCEPTED at Save yet serde rejects the \
                         file: {e}\n---\n{out}"
                    )
                });
                true
            }
            Err(PrefsEditError::BadValue { .. }) => false,
            Err(e) => panic!("{key} = {value}: unexpected error class {e}"),
        }
    };
    for key in integer_keys {
        let domain = integer_domain(key);
        let (lo, hi) = (i128::from(*domain.start()), i128::from(*domain.end()));
        assert!(saves(key, lo), "{key}: domain floor {lo} must save");
        assert!(saves(key, hi), "{key}: domain ceiling {hi} must save");
        if lo > i128::from(i64::MIN) {
            assert!(!saves(key, lo - 1), "{key}: {} must be a BadValue", lo - 1);
        }
        if hi < i128::from(i64::MAX) {
            assert!(!saves(key, hi + 1), "{key}: {} must be a BadValue", hi + 1);
        }
        assert_eq!(
            saves(key, -1),
            lo < 0,
            "{key}: -1 acceptance must match the writable domain's signedness"
        );
        assert!(
            !saves(key, i128::from(u64::MAX) + 1),
            "{key}: u64::MAX + 1 must be a BadValue"
        );
    }
}

#[test]
fn dotted_key_writes_create_and_edit_nested_tables_preserving_comments() {
    let out = apply_prefs_edits(
        "",
        &[
            ("update.auto_apply", set("false")),
            ("sparkle_words.profanity.enabled", set("false")),
        ],
    )
    .unwrap();
    assert!(
        out.contains("[update]"),
        "an explicit table, not a dotted key: {out}"
    );
    let c: Config = aterm_toml::from_str(&out).expect("created tables re-parse");
    assert_eq!(c.update.as_ref().and_then(|u| u.auto_apply), Some(false));
    assert_eq!(
        c.sparkle_words
            .as_ref()
            .and_then(|s| s.profanity.as_ref())
            .and_then(|p| p.enabled),
        Some(false)
    );

    let existing = "\
# my config
font_px = 12.0
[update]
# checks stay on
enabled = true  # keep me
[matrix_rain]
enabled = true
";
    let out = apply_prefs_edits(
        existing,
        &[
            ("update.auto_apply", set("false")),
            ("matrix_rain.fps", set("42")),
        ],
    )
    .unwrap();
    assert!(out.contains("# my config"), "{out}");
    assert!(out.contains("# checks stay on"), "{out}");
    assert!(out.contains("# keep me"), "{out}");
    let c: Config = aterm_toml::from_str(&out).unwrap();
    assert_eq!(c.font_px, Some(12.0));
    let update = c.update.as_ref().unwrap();
    assert_eq!(update.enabled, Some(true));
    assert_eq!(update.auto_apply, Some(false));
    let mr = c.matrix_rain.as_ref().unwrap();
    assert_eq!(mr.enabled, Some(true));
    assert_eq!(mr.fps, Some(42));

    let out = apply_prefs_edits(
        "update = { enabled = true }\n",
        &[("update.auto_apply", set("false"))],
    )
    .unwrap();
    let c: Config = aterm_toml::from_str(&out).unwrap();
    let update = c.update.as_ref().unwrap();
    assert_eq!(update.enabled, Some(true));
    assert_eq!(update.auto_apply, Some(false));
}

#[test]
fn dotted_key_removal_and_non_table_refusal() {
    let existing = "[sparkle_words]\n# note\nenabled = false\nreduced_motion = true\n";
    let out = apply_prefs_edits(existing, &[("sparkle_words.reduced_motion", None)]).unwrap();
    assert!(!out.contains("reduced_motion"), "{out}");
    assert!(out.contains("# note"), "{out}");
    let c: Config = aterm_toml::from_str(&out).unwrap();
    assert_eq!(
        c.sparkle_words.as_ref().and_then(|s| s.enabled),
        Some(false)
    );
    assert_eq!(
        c.sparkle_words.as_ref().and_then(|s| s.reduced_motion),
        None
    );
    let untouched = "font_px = 12.0\n";
    assert_eq!(
        apply_prefs_edits(untouched, &[("update.auto_apply", None)]).unwrap(),
        untouched
    );
    let err =
        apply_prefs_edits("update = 5\n", &[("update.auto_apply", set("false"))]).unwrap_err();
    assert!(matches!(err, PrefsEditError::Parse(_)), "{err}");
    assert!(err.to_string().contains("not a table"), "{err}");
    assert_eq!(
        apply_prefs_edits("update = 5\n", &[("update.auto_apply", None)]).unwrap(),
        "update = 5\n"
    );
    let err = apply_prefs_edits(
        "[update.auto_apply]\nx = 1\n",
        &[("update.auto_apply", set("true"))],
    )
    .unwrap_err();
    assert!(matches!(err, PrefsEditError::Parse(_)), "{err}");
}

#[test]
fn list_keys_write_toml_arrays_and_round_trip() {
    let out = apply_prefs_edits(
        "",
        &[
            (super::EDIT_PALETTE, set("#1d1f21, #cc6666")),
            (super::EDIT_SHELL_ARGS, set("-l, -i")),
            (super::EDIT_FONT_FEATURES, set("ss01, zero, -calt")),
        ],
    )
    .unwrap();
    let c: Config = aterm_toml::from_str(&out).expect("arrays re-parse as Vec<String>");
    assert_eq!(
        c.palette,
        Some(vec!["#1d1f21".to_string(), "#cc6666".to_string()])
    );
    assert_eq!(c.shell_args, Some(vec!["-l".to_string(), "-i".to_string()]));
    assert_eq!(
        c.font_features,
        Some(vec![
            "ss01".to_string(),
            "zero".to_string(),
            "-calt".to_string()
        ])
    );
    let fields = editable_fields(&c);
    let seed = |k: &str| {
        fields
            .iter()
            .find(|f| f.key == k)
            .and_then(|f| f.seed.clone())
    };
    assert_eq!(
        seed(super::EDIT_PALETTE).as_deref(),
        Some("#1d1f21, #cc6666")
    );
    assert_eq!(seed(super::EDIT_SHELL_ARGS).as_deref(), Some("-l, -i"));
    let err = apply_prefs_edits("", &[(super::EDIT_PALETTE, set("#123456, nope"))]).unwrap_err();
    assert!(matches!(err, PrefsEditError::BadValue { .. }));
    let err = apply_prefs_edits("", &[(super::EDIT_SHELL_ARGS, set(", ,"))]).unwrap_err();
    assert!(matches!(err, PrefsEditError::BadValue { .. }));
}

#[test]
fn full_coverage_scalar_keys_classify_and_round_trip() {
    use crate::app_config::{BackgroundMaterial, WindowColorspace};
    for k in [
        super::EDIT_GPU,
        super::EDIT_CURSOR_TRAIL_RING,
        super::EDIT_CURSOR_TRAIL_BLOOM,
        super::EDIT_CURSOR_FIRE_SHIMMER,
        super::EDIT_HDR_GLOW,
        super::EDIT_RESTORE_SESSION,
        super::EDIT_TEMPORAL_RECORDING,
        super::EDIT_TRAIL_SOUND_BED,
    ] {
        assert!(
            matches!(edit_kind(k), EditKind::Bool),
            "{k} is Bool (else Save writes a string for a serde bool)"
        );
    }
    for o in super::WINDOW_COLORSPACES {
        assert!(WindowColorspace::parse(o).is_some(), "colorspace {o}");
    }
    for o in super::BACKGROUND_MATERIALS {
        assert!(BackgroundMaterial::parse(o).is_some(), "material {o}");
    }
    let out = apply_prefs_edits(
        "",
        &[
            (super::EDIT_GPU, set("false")),
            (super::EDIT_TRAIL_SOUND_BED, set("true")),
            (super::EDIT_TRAIL_SOUND_VOLUME, set("0.2")),
            (super::EDIT_CURSOR_TRAIL_LENGTH, set("48")),
            (super::EDIT_WINDOW_COLORSPACE, set("Display-P3")),
            (super::EDIT_BACKGROUND_MATERIAL, set("under-window")),
            (super::EDIT_CURSOR_TRAIL_COLOR, set("#50FA7B")),
            (super::EDIT_SHELL, set("bash")),
            (super::EDIT_WINDOW_PADDING, set("20")),
            (super::EDIT_WINDOW_PADDING_TOP, set("6")),
        ],
    )
    .unwrap();
    let c: Config = aterm_toml::from_str(&out).expect("typed values round-trip");
    assert_eq!(c.gpu, Some(false));
    assert_eq!(c.trail_sound_bed, Some(true));
    assert!(c.trail_sound_bed_or_default());
    assert_eq!(c.trail_sound_volume, Some(0.2));
    assert_eq!(c.cursor_trail_length, Some(48));
    assert_eq!(c.window_colorspace.as_deref(), Some("display-p3"));
    assert_eq!(c.background_material.as_deref(), Some("under-window"));
    assert_eq!(c.cursor_trail_color.as_deref(), Some("#50FA7B"));
    assert_eq!(c.shell.as_deref(), Some("bash"));
    assert_eq!(c.window_padding, Some(20.0));
    assert_eq!(c.window_padding_top, Some(6.0));
    assert_eq!(c.window_padding_or_default(), 20.0);
    assert_eq!(c.window_padding_top_or_default(), 6.0);
    let err =
        apply_prefs_edits("", &[(super::EDIT_WINDOW_COLORSPACE, set("rec2020"))]).unwrap_err();
    assert!(matches!(err, PrefsEditError::BadValue { .. }));
    let err = apply_prefs_edits("", &[(super::EDIT_CURSOR_TRAIL_ACCENT, set("teal"))]).unwrap_err();
    assert!(matches!(err, PrefsEditError::BadValue { .. }));
    assert!(range_of(super::EDIT_TRAIL_SOUND_VOLUME).is_some());
    assert!(range_of(super::EDIT_WINDOW_PADDING).is_some());
    assert!(range_of(super::EDIT_WINDOW_PADDING_TOP).is_some());
}

#[test]
fn the_predictive_editing_switch_is_a_bool_row_that_round_trips() {
    let key = super::EDIT_PREDICTIVE_ECHO_EDITING;
    assert!(matches!(edit_kind(key), EditKind::Bool));
    assert_eq!(section_of(key), section_of(super::EDIT_PREDICTIVE_ECHO));
    let fields = editable_fields(&Config::default());
    let row = fields
        .iter()
        .find(|f| f.key == key)
        .expect("a settings row");
    assert_eq!(row.seed.as_deref(), Some("true"), "on by default");
    let out = apply_prefs_edits("", &[(key, set("false"))]).expect("a Bool writes");
    let c: Config = aterm_toml::from_str(&out).expect("a TOML bool round-trips");
    assert_eq!(c.predictive_echo_editing, Some(false));
    assert!(!c.predictive_echo_editing_or_default());
    assert!(Config::default().predictive_echo_editing_or_default());
}

#[test]
fn chrome_color_rows_are_wired() {
    let fields = editable_fields(&Config::default());
    let at = |key: &str| {
        fields
            .iter()
            .position(|f| f.key == key)
            .unwrap_or_else(|| panic!("{key} row"))
    };
    for key in [
        super::EDIT_ACCENT_COLOR,
        super::EDIT_BORDER_COLOR,
        super::EDIT_SEARCH_MATCH_COLOR,
    ] {
        assert_eq!(
            super::group_of(key),
            super::group_of(super::EDIT_ACTIVE_TAB_COLOR),
            "{key} sits beside Selected tab color"
        );
        assert!(matches!(fields[at(key)].kind, EditKind::Color), "{key}");
        let written = apply_prefs_edits("", &[(key, Some("#336699".to_string()))]).unwrap();
        let c: Config = aterm_toml::from_str(&written).unwrap();
        let resolved = c.resolved_colors(
            aterm_types::Appearance::Dark,
            &crate::app_config::ThemeCatalog::default(),
            None,
        );
        assert_eq!(resolved.config_keys(), vec![key.to_string()], "{key}");
        let reset = apply_prefs_edits(&written, &[(key, None)]).unwrap();
        let c: Config = aterm_toml::from_str(&reset).unwrap();
        assert!(
            c.resolved_colors(
                aterm_types::Appearance::Dark,
                &crate::app_config::ThemeCatalog::default(),
                None,
            )
            .config_keys()
            .is_empty()
        );
        assert!(
            matches!(
                apply_prefs_edits("", &[(key, Some("teal".to_string()))]).unwrap_err(),
                PrefsEditError::BadValue { .. }
            ),
            "{key}: not a colour"
        );
    }
}

#[test]
fn display_font_and_tab_color_registry_rows_are_wired() {
    let off = Config {
        display_font: Some("off".to_string()),
        font_family: Some("Menlo".to_string()),
        ..Config::default()
    };
    assert_eq!(off.font_family_request().as_deref(), Some("Menlo"));
    assert!(off.display_faces_request().is_empty());
    let set = |v: &str| Some(v.to_string());
    let out = apply_prefs_edits(
        "",
        &[
            (super::EDIT_DISPLAY_FONT, set("pixel")),
            (super::EDIT_ACTIVE_TAB_COLOR, set("#FF00AA")),
        ],
    )
    .unwrap();
    let c: Config = aterm_toml::from_str(&out).unwrap();
    assert_eq!(c.display_font.as_deref(), Some("pixel"));
    assert_eq!(c.active_tab_color.as_deref(), Some("#FF00AA"));
    assert_eq!(
        c.resolved_colors(
            aterm_types::Appearance::Dark,
            &crate::app_config::ThemeCatalog::default(),
            None
        )
        .authored_chrome()
        .active_tab,
        Some([0xFF, 0x00, 0xAA])
    );
    assert_eq!(c.font_family_request(), None);
    assert_eq!(c.display_faces_request(), ["pixel"]);
    let out =
        apply_prefs_edits("", &[(super::EDIT_DISPLAY_FONT, set(" pixel + engraved "))]).unwrap();
    let mixed: Config = aterm_toml::from_str(&out).unwrap();
    assert_eq!(mixed.display_font.as_deref(), Some("pixel+engraved"));
    assert_eq!(mixed.display_faces_request(), ["pixel", "engraved"]);
    let reversed = Config {
        display_font: Some("engraved+pixel".to_string()),
        font_family: Some("Menlo".to_string()),
        ..Config::default()
    };
    assert_eq!(
        reversed.display_faces_request(),
        ["pixel", "engraved"],
        "the toggle order changes nothing"
    );
    assert_eq!(reversed.font_family_request().as_deref(), Some("Menlo"));
    for bad in [
        "doom",
        "pixel+doom",
        "engraved+engraved",
        "chunky+pixel+engraved+bubble",
    ] {
        assert!(
            matches!(
                apply_prefs_edits("", &[(super::EDIT_DISPLAY_FONT, set(bad))]).unwrap_err(),
                PrefsEditError::BadValue { .. }
            ),
            "{bad} must be rejected"
        );
    }
    assert!(matches!(
        apply_prefs_edits("", &[(super::EDIT_ACTIVE_TAB_COLOR, set("pink"))]).unwrap_err(),
        PrefsEditError::BadValue { .. }
    ));
    let mut cleared = c.clone();
    cleared.display_font = None;
    assert_eq!(cleared.font_family_request(), cleared.font_family);
    assert!(cleared.display_faces_request().is_empty());
    let mut typo = c;
    typo.display_font = Some("dooom".to_string());
    assert_eq!(typo.font_family_request(), typo.font_family);
    assert!(typo.display_faces_request().is_empty());
}

#[test]
fn the_trail_color_note_follows_the_colour_law() {
    use aterm_effects::cursor_fx::{
        glow_color_follows_live_cursor, own_trail_color, trail_color_default_name,
    };
    use aterm_effects::cursor_glow::GlowStyle;
    let old = "Blank: Lightning keeps its violet, and Beam, Comet and Sparkle keep their own \
               color until you set a cursor color; other styles follow the cursor color. \
               Cursor color (or \u{201c}cursor\u{201d}) makes every style follow it.";
    let mut checked = 0;
    for row in aterm_effects::cursor_glow::spellings::TRAIL_STYLES {
        for block in [true, false] {
            for pinned in [false, true] {
                let note = super::trail_color_note(row.name, block, pinned);
                let case = format!("{} block={block} pinned={pinned}: {note}", row.name);
                let Some(style) = row.style else {
                    assert!(note.contains("Off"), "{case}");
                    continue;
                };
                assert_ne!(note, old, "{case}");
                if !style.draws_trail_color() {
                    assert_eq!(note.contains("does not use these"), block, "{case}");
                    assert!(!note.contains("follows the cursor color"), "{case}");
                    let rod = match trail_color_default_name(style, pinned) {
                        Some("Shifting colors") => "shifts through colors".to_string(),
                        Some(own) => own.to_lowercase(),
                        None => panic!("{case}: a rod of its own"),
                    };
                    assert_eq!(note.contains(&rod), !block, "{case}");
                } else if style == GlowStyle::Classic {
                    assert_eq!(
                        trail_color_default_name(style, pinned),
                        Some("Shifting colors")
                    );
                    assert!(note.contains("shifts through colors"), "{case}");
                } else if let Some((_, own)) = own_trail_color(style, pinned) {
                    assert!(note.contains(&own.to_lowercase()), "{case}");
                    assert_eq!(
                        note.contains("until you set a cursor color"),
                        glow_color_follows_live_cursor(style, true),
                        "{case}"
                    );
                } else {
                    assert!(note.contains("follows the cursor color"), "{case}");
                }
                checked += 1;
            }
        }
    }
    assert!(checked > 30, "{checked}");
    assert!(super::trail_color_note("classic mono", true, false).contains("cursor color"));
}
