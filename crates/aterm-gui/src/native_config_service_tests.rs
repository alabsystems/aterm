// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn unique_config_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aterm-native-config-service-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn edit(
    base_revision: u64,
    key: &str,
    expected: ExpectedValue,
    value: Option<&str>,
) -> ConfigPatchRequest {
    ConfigPatchRequest {
        base_revision,
        edits: vec![ConfigKeyEdit {
            key: key.into(),
            expected,
            value: value.map(str::to_owned),
        }],
    }
}

#[test]
fn a_base_revision_the_service_never_published_is_refused_without_a_write() {
    let mut service =
        VersionedConfigService::new("theme = \"Nord\"\n".into()).expect("valid config");
    let before = service.snapshot();
    for base in [0, before.revision + 1] {
        assert!(matches!(
            service.patch(edit(
                base,
                "theme",
                ExpectedValue::Exact(Some("Nord".into())),
                None,
            )),
            ConfigPatchResult::Rejected { message, .. } if message == "invalid base revision"
        ));
        assert_eq!(service.snapshot(), before);
    }
    assert!(matches!(
        service.patch(edit(
            before.revision,
            "theme",
            ExpectedValue::Exact(Some("Nord".into())),
            None,
        )),
        ConfigPatchResult::Applied { .. }
    ));
}

/// `ctl settings set` writes a style by its row's name however it is cased, keeps Classic's
/// one-colour face, and refuses every retired name with the settings that say it now.
#[test]
fn settings_set_takes_a_style_and_refuses_a_retired_name() {
    for (input, written) in [
        ("Rainbow", "rainbow"),
        (" LASER ", "laser"),
        ("classic mono", "classic mono"),
        ("classic lumen", "classic mono"),
        ("beam", "beam"),
    ] {
        let mut service = VersionedConfigService::new(String::new()).unwrap();
        let base = service.snapshot().revision;
        service.patch(edit(
            base,
            crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
            ExpectedValue::Any,
            Some(input),
        ));
        let drawn = if crate::app_config::default_cursor_trail_style() == written {
            None
        } else {
            Some(written)
        };
        assert_eq!(
            service.snapshot().config.cursor_trail_style.as_deref(),
            drawn,
            "{input:?}"
        );
    }
    for was in aterm_effects::cursor_glow::spellings::RETIRED_TRAIL_STYLES {
        let mut service = VersionedConfigService::new(String::new()).unwrap();
        let before = service.snapshot();
        let result = service.patch(edit(
            before.revision,
            crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
            ExpectedValue::Any,
            Some(was.old),
        ));
        let ConfigPatchResult::Rejected { message, .. } = &result else {
            panic!("{:?} must be refused: {result:?}", was.old);
        };
        assert!(
            message.contains(&format!("cursor_trail_style = {:?}", was.style)),
            "{:?}: {message}",
            was.old
        );
        assert_eq!(
            message.contains("cursor_pet"),
            was.pet.is_some(),
            "{:?}: {message}",
            was.old
        );
        assert_eq!(service.snapshot(), before);
    }
    let mut service = VersionedConfigService::new(String::new()).unwrap();
    let before = service.snapshot();
    assert!(matches!(
        service.patch(edit(
            before.revision,
            crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
            ExpectedValue::Any,
            Some("lightnin"),
        )),
        ConfigPatchResult::Rejected { .. }
    ));
    assert_eq!(service.snapshot(), before);
    // The pet is written through the same door, beside any style.
    let mut service = VersionedConfigService::new("cursor_trail_style = \"off\"\n".into()).unwrap();
    let base = service.snapshot().revision;
    service.patch(edit(
        base,
        crate::prefs::EDIT_CURSOR_PET,
        ExpectedValue::Any,
        Some("dog"),
    ));
    let held = service.snapshot();
    assert_eq!(held.config.cursor_pet_raw(), "dog");
    assert_eq!(held.config.cursor_trail_style_raw(), "off");
}

#[test]
fn byte_equal_external_observation_refreshes_assets_and_analysis_generation() {
    let dir = unique_config_dir("equal-text-assets");
    let sprite = dir.join("nyan.png");
    let png = |rgba: &[u8]| crate::app_introspect::encode_rgba8_png(rgba, 2, 1).unwrap();
    std::fs::write(
        &sprite,
        png(&[0x22, 0x66, 0xee, 0xff, 0xee, 0x66, 0x22, 0xff]),
    )
    .unwrap();
    let text = format!(
        "trail_sound_volume = nan\ncursor_nyan_sprite = {:?}\n",
        sprite.to_string_lossy()
    );
    let mut service = VersionedConfigService::new(text.clone()).unwrap();
    let initial = service.snapshot();
    let initial_fp = initial.assets.kitty_sprite.fingerprint();

    std::fs::write(
        &sprite,
        png(&[0xee, 0x22, 0x66, 0xff, 0x22, 0xee, 0x66, 0xff]),
    )
    .unwrap();
    let refreshed = service.replace_external(text.clone()).unwrap();
    assert_eq!(refreshed.text, initial.text, "config bytes stay identical");
    assert!(refreshed.revision > initial.revision);
    assert!(refreshed.analysis_generation > initial.analysis_generation);
    assert_ne!(refreshed.assets.kitty_sprite.fingerprint(), initial_fp);

    let unchanged = service.replace_external(text.clone()).unwrap();
    assert_eq!(unchanged.revision, refreshed.revision);
    assert!(unchanged.analysis_generation > refreshed.analysis_generation);
    assert_eq!(unchanged.assets, refreshed.assets);

    let config_path = dir.join("aterm.toml");
    let fleet_path = dir.join(aterm_agent::ai::config::FLEET_FILE);
    crate::test_scratch::write_config(&config_path, &text);
    let observe = |service: &mut VersionedConfigService| {
        let observation = VersionedConfigService::observe_path(&config_path, false).unwrap();
        let mut prepared = VersionedConfigService::prepare_observation(
            observation,
            Arc::clone(&service.snapshot().assets.themes),
        )
        .unwrap();
        prepared.config.ai = crate::app_config::AiPolicy::read_beside(&text, Some(&dir));
        service.synchronize_prepared_observation(prepared).unwrap()
    };
    let absent = observe(&mut service);
    assert_eq!(absent.config.ai.settings().preferred.0.id, "claude");

    crate::test_scratch::write_config(&fleet_path, "schema = 1\npreferred = \"codex\"\n");
    let resolved = observe(&mut service);
    assert_eq!(resolved.text, absent.text);
    assert_eq!(resolved.semantic_values, absent.semantic_values);
    assert_eq!(resolved.assets, absent.assets);
    assert_eq!(
        resolved.config.ai.settings().preferred.0.id,
        "codex",
        "same-text worker admission publishes the newly resolved policy"
    );
    assert_eq!(resolved.config.ai.fleet_paths.first(), Some(&fleet_path));
    assert_eq!(resolved.revision, absent.revision + 1);
    assert!(resolved.analysis_generation > absent.analysis_generation);

    let repeated = observe(&mut service);
    assert_eq!(repeated.revision, resolved.revision);
    assert!(repeated.analysis_generation > resolved.analysis_generation);
    assert!(repeated.config.trail_sound_volume.unwrap().is_nan());
    assert_eq!(repeated.config.ai, resolved.config.ai);
    assert_eq!(
        service.restore_durable_snapshot().unwrap().config.ai,
        resolved.config.ai,
        "rollback keeps the admitted resolved policy"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn startup_service_publishes_sparkle_consumers_only_after_exact_preparation() {
    let pack = aterm_tempfile::manifest_dir()
        .join("../aterm-effects/toy-packs/community/tiny-triumphs/pack.toml");
    let text = format!(
        "[sparkle_words]\ntoy_packs = [{:?}]\n",
        pack.to_string_lossy()
    );
    let mut service = VersionedConfigService::new(text).unwrap();
    let preliminary = service.snapshot();
    assert!(preliminary.assets.sparkle_spec_consumers.is_none());

    let prepared = preliminary.config.prepare_path_feed_generation();
    let exact = prepared.sparkle.consumer_capabilities();
    assert!(
        exact.sparkle_or_starburst_burst && exact.rainbow_ink && exact.twotone_ink,
        "negative control: the checked-in pack has shared-setting consumers"
    );
    service.complete_startup_path_generation(exact);
    let published = service.snapshot();
    assert_eq!(published.revision, preliminary.revision);
    assert_eq!(
        published.assets.sparkle_spec_consumers.as_deref(),
        Some(&exact)
    );

    let restored = service.restore_durable_snapshot().unwrap();
    assert_eq!(
        restored.assets.sparkle_spec_consumers.as_deref(),
        Some(&exact),
        "the completed startup generation is also the durable rollback generation"
    );
}

#[test]
fn sparkle_patch_never_carries_an_authoritative_consumer_verdict_across_revisions() {
    let mut service = VersionedConfigService::new(
        "serious_mode = false\n[sparkle_words]\nenabled = false\n".to_string(),
    )
    .unwrap();
    let initial = service.snapshot();
    let prepared = initial.config.prepare_path_feed_generation();
    let exact = prepared.sparkle.consumer_capabilities();
    service.complete_startup_path_generation(exact);
    let exact_snapshot = service.snapshot();
    let exact_assets = Arc::clone(&exact_snapshot.assets);
    assert_eq!(
        exact_snapshot.assets.sparkle_spec_consumers.as_deref(),
        Some(&exact)
    );

    let unrelated = service.patch(edit(
        exact_snapshot.revision,
        "serious_mode",
        ExpectedValue::Exact(Some("false".to_string())),
        Some("true"),
    ));
    let ConfigPatchResult::Applied {
        snapshot: unrelated,
        ..
    } = unrelated
    else {
        panic!("unrelated patch must apply")
    };
    assert!(
        Arc::ptr_eq(&unrelated.assets, &exact_assets),
        "a non-sparkle patch preserves the exact admitted asset generation"
    );

    let sparkle = service.patch(edit(
        unrelated.revision,
        "sparkle_words.enabled",
        ExpectedValue::Exact(Some("false".to_string())),
        None,
    ));
    let ConfigPatchResult::Applied {
        snapshot: sparkle, ..
    } = sparkle
    else {
        panic!("sparkle patch must apply")
    };
    assert!(sparkle.assets.sparkle_spec_consumers.is_none());
    assert!(!Arc::ptr_eq(&sparkle.assets, &exact_assets));
    assert!(Arc::ptr_eq(
        &sparkle.assets.trail_style,
        &exact_assets.trail_style
    ));
    assert!(Arc::ptr_eq(&sparkle.assets.themes, &exact_assets.themes));
}

#[test]
fn unrelated_external_edit_reuses_asset_arc_and_source_change_is_negative_control() {
    let dir = unique_config_dir("external-asset-reuse");
    let first = dir.join("first.png");
    let second = dir.join("second.png");
    let first_rgba = [0x22, 0x66, 0xee, 0xff, 0xee, 0x66, 0x22, 0xff];
    let first_png = crate::app_introspect::encode_rgba8_png(&first_rgba, 2, 1).unwrap();
    std::fs::write(&first, first_png).unwrap();
    let first_source = first.to_string_lossy().into_owned();
    let mut service = VersionedConfigService::new(format!(
        "theme = \"Nord\"\ncursor_nyan_sprite = {first_source:?}\n"
    ))
    .unwrap();
    let initial = service.snapshot();
    let initial_fp = initial.assets.kitty_sprite.fingerprint();

    std::fs::write(&first, b"not a PNG").unwrap();
    let unrelated = service
        .replace_external(format!(
            "theme = \"Dracula\"\ncursor_nyan_sprite = {first_source:?}\n"
        ))
        .unwrap();
    assert!(Arc::ptr_eq(&unrelated.assets, &initial.assets));
    assert_eq!(unrelated.assets.kitty_sprite.fingerprint(), initial_fp);

    let second_rgba = [0xee, 0x22, 0x66, 0xff, 0x22, 0xee, 0x66, 0xff];
    let second_png = crate::app_introspect::encode_rgba8_png(&second_rgba, 2, 1).unwrap();
    std::fs::write(&second, second_png).unwrap();
    let second_source = second.to_string_lossy().into_owned();
    let changed = service
        .replace_external(format!(
            "theme = \"Dracula\"\ncursor_nyan_sprite = {second_source:?}\n"
        ))
        .unwrap();
    assert!(!Arc::ptr_eq(&changed.assets, &unrelated.assets));
    assert_eq!(
        changed.assets.kitty_sprite.source_id(),
        Some(second_source.as_str())
    );
    assert_ne!(changed.assets.kitty_sprite.fingerprint(), initial_fp);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn background_theme_catalog_refresh_publishes_one_atomic_snapshot_revision() {
    let mut service = VersionedConfigService::new("theme = \"Work\"\n".into()).unwrap();
    let initial = service.snapshot();
    assert!(initial.assets.themes.resolve("Work").is_err());

    let scheme = aterm_types::scheme::builtin("Dracula").unwrap();
    let themes =
        crate::app_config::ThemeCatalog::from_schemes([("Work".to_string(), scheme.clone())]);
    let refreshed = service.replace_theme_catalog(themes);
    assert_eq!(refreshed.text, initial.text);
    assert_eq!(refreshed.revision, initial.revision + 1);
    assert!(!Arc::ptr_eq(&refreshed.assets, &initial.assets));
    assert_eq!(refreshed.assets.themes.resolve("Work"), Ok(scheme));

    let duplicate = service.replace_theme_catalog(Arc::new((*refreshed.assets.themes).clone()));
    assert_eq!(duplicate.revision, refreshed.revision);
    assert!(Arc::ptr_eq(&duplicate.assets, &refreshed.assets));
    assert!(duplicate.analysis_generation > refreshed.analysis_generation);
}

#[test]
fn observation_rejects_non_utf8_oversize_and_non_file_inputs() {
    let dir = unique_config_dir("observation-errors");
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, [0xff]);
    let utf8_error = VersionedConfigService::observe_path(&path, false).unwrap_err();
    assert!(utf8_error.contains("not valid UTF-8"), "{utf8_error}");

    let file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    file.set_len(MAX_CONFIG_FILE_BYTES as u64 + 1).unwrap();
    let oversize = VersionedConfigService::observe_path(&path, false).unwrap_err();
    assert!(oversize.contains("exceeds"), "{oversize}");
    let startup = match VersionedConfigService::load_path(&path) {
        Ok(_) => panic!("startup must reject a config beyond the shared cap"),
        Err(error) => error,
    };
    assert!(startup.contains("exceeds"), "{startup}");

    let not_file = VersionedConfigService::observe_path(&dir, false).unwrap_err();
    assert!(not_file.contains("not a regular file"), "{not_file}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn reconciliation_gate_stays_closed_until_a_stable_valid_disk_generation_is_admitted() {
    let dir = unique_config_dir("reconciliation-gate");
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, "serious_mode = false\n");
    let mut service = VersionedConfigService::load_path(&path).unwrap();
    service.mark_reconciliation_required();
    assert!(service.reconciliation_required());

    crate::test_scratch::write_config(&path, "serious_mode = [\n");
    assert!(service.synchronize_from_disk().is_err());
    assert!(
        service.reconciliation_required(),
        "a malformed observation cannot reopen structured persistence"
    );

    crate::test_scratch::write_config(&path, "serious_mode = true\n");
    let admitted = service.synchronize_from_disk().unwrap();
    assert!(!service.reconciliation_required());
    assert_eq!(
        admitted
            .values()
            .unwrap()
            .get(crate::prefs::EDIT_SERIOUS_MODE)
            .map(String::as_str),
        Some("true")
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn unparsed_startup_binding_cannot_authorize_a_structured_overwrite() {
    let dir = unique_config_dir("unparsed-binding");
    let path = dir.join("aterm.toml");
    let malformed = "theme = [\n";
    crate::test_scratch::write_config(&path, malformed);
    let observation = VersionedConfigService::observe_path(&path, false).unwrap();
    let mut service = VersionedConfigService::new(String::new()).unwrap();
    service
        .bind_unparsed_disk_baseline(observation.baseline)
        .unwrap();
    let base = service.snapshot().revision;
    let ConfigPatchResult::Applied { snapshot, .. } = service.patch(edit(
        base,
        "theme",
        ExpectedValue::Exact(None),
        Some("Nord"),
    )) else {
        panic!("valid candidate should reduce before durable preflight");
    };
    let plan = service.persistence_plan(snapshot);
    assert!(plan.baseline.is_none());
    assert_eq!(plan.logical_path.as_deref(), Some(path.as_path()));

    assert!(matches!(
        crate::prefs::save_prefs_snapshot(&plan),
        crate::prefs::SaveOutcome::Conflict { message, .. }
            if message.contains("changed on disk")
    ));
    service.restore_durable_snapshot().unwrap();
    assert_eq!(service.snapshot().text.as_ref(), "");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), malformed);
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn structured_snapshot_refuses_a_retargeted_config_symlink() {
    use std::os::unix::fs::symlink;

    let dir = unique_config_dir("structured-retarget");
    let first = dir.join("first.toml");
    let second = dir.join("second.toml");
    let logical = dir.join("aterm.toml");
    crate::test_scratch::write_config(&logical, "theme = \"Default\"\n");
    crate::test_scratch::write_config(&second, "theme = \"Dracula\"\n");
    let mut service = VersionedConfigService::load_path(&logical).unwrap();
    let base = service.snapshot().revision;
    let result = service.patch(edit(
        base,
        "theme",
        ExpectedValue::Exact(Some("Default".to_string())),
        Some("Nord"),
    ));
    let ConfigPatchResult::Applied { snapshot, .. } = result else {
        panic!("valid patch must apply before persistence");
    };
    let plan = service.persistence_plan(snapshot);
    std::fs::rename(&logical, &first).unwrap();
    symlink(&second, &logical).unwrap();

    assert!(matches!(
        crate::prefs::save_prefs_snapshot(&plan),
        crate::prefs::SaveOutcome::Conflict { message, .. }
            if message.contains("changed while saving")
    ));
    assert_eq!(
        std::fs::read_to_string(&first).unwrap(),
        "theme = \"Default\"\n"
    );
    assert_eq!(
        std::fs::read_to_string(&second).unwrap(),
        "theme = \"Dracula\"\n"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_default_over_a_themes_type_value_stays_written() {
    let catalog = |extra: &str| {
        let text = format!("foreground = #e0e0e0\nbackground = #101418\n{extra}");
        let parsed = aterm_types::scheme::parse_theme_str(&text).expect("a theme");
        crate::app_config::ThemeCatalog::from_parsed([("Studio".to_string(), parsed)])
    };
    let default = crate::prefs::editable_fields(&crate::app_config::Config::default())
        .into_iter()
        .find(|field| field.key == crate::prefs::EDIT_LINE_HEIGHT)
        .and_then(|field| crate::prefs::unset_literal(&field))
        .expect("line height has a default");
    for (source, with_theme, extra, kept) in [
        ("theme = \"Studio\"\n", false, "line_height = 1.15\n", true),
        ("", true, "line_height = 1.15\n", true),
        ("", false, "line_height = 1.15\n", false),
        ("theme = \"Studio\"\n", false, "", false),
    ] {
        let mut service =
            VersionedConfigService::new_with_themes(source.into(), catalog(extra)).unwrap();
        let mut edits = vec![ConfigKeyEdit {
            key: crate::prefs::EDIT_LINE_HEIGHT.into(),
            expected: ExpectedValue::Any,
            value: Some(default.clone()),
        }];
        if with_theme {
            edits.push(ConfigKeyEdit {
                key: crate::prefs::EDIT_THEME.into(),
                expected: ExpectedValue::Any,
                value: Some("Studio".into()),
            });
        }
        service.patch(ConfigPatchRequest {
            base_revision: service.snapshot().revision,
            edits,
        });
        assert_eq!(
            service
                .value(crate::prefs::EDIT_LINE_HEIGHT)
                .unwrap()
                .is_some(),
            kept,
            "line_height = {default} over {source:?} (theme in the patch: {with_theme}, theme sets {extra:?})"
        );
    }
}

#[test]
fn an_expectation_names_a_value_not_its_spelling() {
    let patch = |source: &str, expected: Option<&str>| {
        let mut service = VersionedConfigService::new(source.into()).unwrap();
        let base = service.snapshot().revision;
        service.patch(edit(
            base,
            crate::prefs::EDIT_LINE_HEIGHT,
            ExpectedValue::Exact(expected.map(str::to_owned)),
            Some("1.3"),
        ))
    };
    for expected in ["1.0", "1"] {
        assert!(
            matches!(
                patch("line_height = 1\n", Some(expected)),
                ConfigPatchResult::Applied { .. }
            ),
            "expecting {expected:?}"
        );
    }
    for (source, expected) in [("line_height = 1\n", Some("1.2")), ("", Some("1.0"))] {
        assert!(
            matches!(patch(source, expected), ConfigPatchResult::Conflict { .. }),
            "{source:?} expecting {expected:?}"
        );
    }
}

#[test]
fn a_list_key_takes_a_list_and_an_empty_one_is_no_value() {
    // `fallback_fonts []` wrote the string "[]", which the font list read as one fallback font
    // named "[]".
    for key in [
        crate::prefs::EDIT_FALLBACK_FONTS,
        crate::prefs::EDIT_FONT_VARIATION,
    ] {
        let mut service = VersionedConfigService::new(format!("{key} = [\"wght=450\"]\n")).unwrap();
        let base = service.snapshot().revision;
        service.patch(edit(base, key, ExpectedValue::Any, Some("[]")));
        assert_eq!(service.value(key).unwrap(), None, "{key} []");
        assert_eq!(&*service.snapshot().text, "", "{key} []");

        let base = service.snapshot().revision;
        service.patch(edit(
            base,
            key,
            ExpectedValue::Any,
            Some("[\"Alpha Mono\", \"Beta Sans\"]"),
        ));
        let config = crate::app_config::Config::parse(&service.snapshot().text).unwrap();
        let list = if key == crate::prefs::EDIT_FALLBACK_FONTS {
            config.fallback_fonts
        } else {
            config.font_variation
        };
        assert_eq!(
            list.map(|list| list.0),
            Some(vec!["Alpha Mono".to_string(), "Beta Sans".to_string()]),
            "{key}: {}",
            service.snapshot().text
        );
    }
    for key in crate::prefs::LIST_KEYS {
        let mut service = VersionedConfigService::new(String::new()).unwrap();
        let base = service.snapshot().revision;
        assert!(
            matches!(
                service.patch(edit(base, key, ExpectedValue::Any, Some("[]"))),
                ConfigPatchResult::Unchanged { .. }
            ),
            "{key} []"
        );
    }
    // Over a theme's own fallback list, an empty one is a value: it takes the theme's away.
    let studio = {
        let text = "foreground = #e0e0e0\nbackground = #101418\nfallback_fonts = Menlo\n";
        let parsed = aterm_types::scheme::parse_theme_str(text).expect("a theme");
        crate::app_config::ThemeCatalog::from_parsed([("Studio".to_string(), parsed)])
    };
    let mut service =
        VersionedConfigService::new_with_themes("theme = \"Studio\"\n".into(), studio).unwrap();
    let base = service.snapshot().revision;
    service.patch(edit(
        base,
        crate::prefs::EDIT_FALLBACK_FONTS,
        ExpectedValue::Any,
        Some("[]"),
    ));
    assert_eq!(
        service
            .value(crate::prefs::EDIT_FALLBACK_FONTS)
            .unwrap()
            .as_deref(),
        Some("[]")
    );
}

#[test]
fn a_rows_words_for_no_value_are_never_written_as_one() {
    // `ctl settings unset shell` answered "now Platform (default)", and `ctl settings set
    // shell Platform` wrote shell = "Platform", a shell that does not exist; "none" wrote
    // shell_args = ["none"] (an argument), "No image" a wallpaper file of that name, and a
    // colour's "Theme" was refused as no colour.
    use crate::prefs::EditKind;
    let fields = crate::prefs::editable_fields(&crate::app_config::Config::default());
    let mut checked = 0;
    for field in &fields {
        if !matches!(field.kind, EditKind::Text | EditKind::Color)
            || field.key.starts_with("ai.")
            || crate::prefs::config_asset_source_key(field.key)
        {
            continue;
        }
        let shown = field.placeholder.trim();
        let bare = shown.split(" \u{2014} ").next().unwrap_or(shown);
        let bare = bare.split(" (").next().unwrap_or(bare).trim();
        for words in [shown, bare] {
            let mut service = VersionedConfigService::new(String::new()).unwrap();
            let base = service.snapshot().revision;
            let result = service.patch(edit(base, field.key, ExpectedValue::Any, Some(words)));
            let written = service.value(field.key).unwrap();
            assert!(
                !matches!(result, ConfigPatchResult::Rejected { .. })
                    && written
                        .as_deref()
                        .is_none_or(|written| !written.eq_ignore_ascii_case(words)),
                "{} = {words:?}: {result:?}, wrote {written:?}",
                field.key
            );
        }
        checked += 1;
    }
    assert!(checked > 20, "only {checked} text and colour keys");
}

#[test]
fn an_unset_rows_words_typed_back_on_ctl_write_nothing() {
    // `ctl settings unset window_theme` answered "now Auto (default)", and setting those words
    // back was refused (an exact option kept its mark) while bidi's "Auto (default)" was taken;
    // a switch's "On (default)" and a number's "12 px (default)" were refused too.
    use crate::prefs::EditKind;
    let service = VersionedConfigService::new(String::new()).unwrap();
    let snapshot = service.snapshot();
    let fields = crate::prefs::editable_fields(&snapshot.config);
    let mut checked = std::collections::BTreeMap::<&str, usize>::new();
    let mut written = Vec::new();
    for field in &fields {
        // The theme's words are a_default_value_is_written_as_a_removal's; an asset's key is
        // written only through Manual.
        if field.key.starts_with("ai.")
            || field.kind == EditKind::Theme
            || crate::prefs::config_asset_source_key(field.key)
        {
            continue;
        }
        let Some(now) =
            crate::native_settings::effective_value_words(&snapshot, field.key, true, None)
        else {
            continue;
        };
        // What a row is held to ("…; Off while Serious Mode …") is no part of its value.
        let words = now
            .split_once("; ")
            .map_or(now.as_str(), |(words, _)| words);
        // A choice with nothing chosen (the style before Off) has no word that writes it;
        // a refusal names the choices it takes.
        if words == "none (default)" && matches!(field.kind, EditKind::Enum { .. }) {
            continue;
        }
        let typed = crate::native_settings::control_input_value(field, words);
        let mut service = VersionedConfigService::new(String::new()).unwrap();
        let base = service.snapshot().revision;
        let result = typed
            .clone()
            .map(|typed| service.patch(edit(base, field.key, ExpectedValue::Any, Some(&typed))));
        if !matches!(result, Ok(ConfigPatchResult::Unchanged { .. })) {
            let outcome = match result {
                Ok(ConfigPatchResult::Rejected { message, .. }) => {
                    format!("refused: {message}")
                }
                Ok(_) => format!("wrote {:?}", service.snapshot().text),
                Err(refusal) => refusal,
            };
            written.push(format!("{} = {words:?}: {outcome}", field.key));
        }
        *checked
            .entry(match field.kind {
                EditKind::Enum { .. } => "choice",
                EditKind::Bool => "switch",
                EditKind::Float | EditKind::Integer => "number",
                _ => "text",
            })
            .or_default() += 1;
    }
    assert!(written.is_empty(), "{written:#?}");
    assert!(
        ["choice", "switch", "number", "text"]
            .iter()
            .all(|kind| checked.get(kind).is_some_and(|count| *count > 5)),
        "{checked:?}"
    );
}

#[test]
fn a_default_value_is_written_as_a_removal() {
    let fields = crate::prefs::editable_fields(&crate::app_config::Config::default());
    let mut checked = 0;
    for field in &fields {
        if crate::prefs::config_asset_source_key(field.key) {
            continue;
        }
        let Some(literal) = crate::prefs::unset_literal(field) else {
            continue;
        };
        let mut service = VersionedConfigService::new(String::new()).unwrap();
        let base = service.snapshot().revision;
        let result = service.patch(edit(base, field.key, ExpectedValue::Any, Some(&literal)));
        assert!(
            matches!(result, ConfigPatchResult::Unchanged { .. }),
            "{} = {literal:?} on an empty file: {result:?}",
            field.key
        );
        assert_eq!(&*service.snapshot().text, "", "{}", field.key);
        checked += 1;
    }
    assert!(checked > 50, "only {checked} keys have an unset value");

    let mut service =
        VersionedConfigService::new("window_theme = \"dark\"\nbidi = \"disabled\"\n".into())
            .unwrap();
    let base = service.snapshot().revision;
    assert!(matches!(
        service.patch(edit(base, "window_theme", ExpectedValue::Any, Some("Auto"))),
        ConfigPatchResult::Applied { .. }
    ));
    assert_eq!(&*service.snapshot().text, "bidi = \"disabled\"\n");

    // The built-in Default, in any spelling, draws what no theme does on every appearance (a
    // theme file cannot shadow it), as Settings' picker removes the key for it; a pair naming
    // it on both sides turns Follow system on, and stays.
    for (value, kept) in [
        ("Default", None),
        ("default", None),
        (
            "dark:Default,light:Default",
            Some("dark:Default,light:Default"),
        ),
        ("Nord", Some("Nord")),
    ] {
        let mut service = VersionedConfigService::new("theme = \"Nord\"\n".into()).unwrap();
        let base = service.snapshot().revision;
        service.patch(edit(base, "theme", ExpectedValue::Any, Some(value)));
        assert_eq!(service.value("theme").unwrap().as_deref(), kept, "{value}");
    }
    // A font row's no-value words (Automatic, or the theme's font as its picker and row say
    // it) name no font: written, they named one that does not resolve.
    for (key, value, kept) in [
        ("font_family", "Automatic (SF Mono)", None),
        ("font_family", "automatic", None),
        ("emoji_font", "Automatic (Apple Color Emoji)", None),
        ("fallback_fonts", "Automatic", None),
        ("font_family", "Use theme's (Menlo)", None),
        ("symbol_font", "Symbols Nerd Font (from theme Studio)", None),
        ("font_family", "Automatic Mono", Some("Automatic Mono")),
        ("shell", "Automatic", Some("Automatic")),
    ] {
        let mut service = VersionedConfigService::new(format!("{key} = \"Menlo\"\n")).unwrap();
        let base = service.snapshot().revision;
        service.patch(edit(base, key, ExpectedValue::Any, Some(value)));
        assert_eq!(
            service.value(key).unwrap().as_deref(),
            kept,
            "{key} = {value}"
        );
    }
    let mut service = VersionedConfigService::new(String::new()).unwrap();
    let base = service.snapshot().revision;
    service.patch(edit(base, "window_theme", ExpectedValue::Any, Some("dark")));
    assert_eq!(
        service.value("window_theme").unwrap().as_deref(),
        Some("dark")
    );

    let arcade = {
        let text = "foreground = #e0e0e0\nbackground = #101418\nfont_family = display:pixel\n";
        let parsed = aterm_types::scheme::parse_theme_str(text).expect("a theme");
        crate::app_config::ThemeCatalog::from_parsed([("Arcade".to_string(), parsed)])
    };
    for (source, kept) in [
        ("theme = \"Arcade\"\n", true),
        ("font_family = \"display:pixel\"\n", true),
        ("theme = \"Arcade\"\nfont_family = \"Menlo\"\n", false),
        ("", false),
    ] {
        let mut service =
            VersionedConfigService::new_with_themes(source.into(), Arc::clone(&arcade)).unwrap();
        let base = service.snapshot().revision;
        service.patch(edit(
            base,
            crate::prefs::EDIT_DISPLAY_FONT,
            ExpectedValue::Any,
            Some("off"),
        ));
        assert_eq!(
            service
                .value(crate::prefs::EDIT_DISPLAY_FONT)
                .unwrap()
                .is_some(),
            kept,
            "display_font = \"off\" over {source:?}"
        );
    }

    let laser = crate::app_config::Config::bloom_radius_style_default(
        crate::app_config::resolve_trail_style("laser"),
    );
    let kitty = crate::app_config::Config::bloom_radius_style_default(
        crate::app_config::resolve_trail_style(crate::prefs::DEFAULT_CURSOR_TRAIL_STYLE),
    );
    assert_ne!(laser, kitty, "the styles' own bloom sizes differ");
    let mut service =
        VersionedConfigService::new("cursor_trail_style = \"laser\"\n".into()).unwrap();
    let base = service.snapshot().revision;
    service.patch(edit(
        base,
        crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS,
        ExpectedValue::Any,
        Some(&kitty.to_string()),
    ));
    assert!(
        service
            .value(crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS)
            .unwrap()
            .is_some(),
        "the kitty's bloom size is not Laser's default"
    );
    let base = service.snapshot().revision;
    service.patch(edit(
        base,
        crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS,
        ExpectedValue::Any,
        Some(&laser.to_string()),
    ));
    assert_eq!(
        service
            .value(crate::prefs::EDIT_CURSOR_TRAIL_BLOOM_RADIUS)
            .unwrap(),
        None,
        "Laser's own bloom size is its default"
    );
}

/// THE AUTOMATIC FONT SIZE IS THE FONT SIZE'S DEFAULT: an explicit
/// `font_px` equal to the platform's size (12 px on macOS) resolves,
/// scales with each window and draws exactly as no `font_px` does, so a
/// write of it — `aterm ctl settings set font_px 12`, or a slider step
/// onto 12 — removes the key, and neither Modified nor the row's Changed
/// mark counts it. Before, the row's "12 (automatic)" placeholder was not
/// read as its unset value, so the set answered "saved: font_px = 12"
/// and left a Changed row whose Reset changed nothing. Negative control:
/// any other size stays written.
#[test]
fn the_automatic_font_size_is_written_as_a_removal() {
    let automatic = crate::FONT_PX.to_string();
    let mut service = VersionedConfigService::new(String::new()).unwrap();
    let base = service.snapshot().revision;
    let result = service.patch(edit(
        base,
        crate::prefs::EDIT_FONT_PX,
        ExpectedValue::Any,
        Some(&automatic),
    ));
    assert!(
        matches!(result, ConfigPatchResult::Unchanged { .. }),
        "font_px = {automatic} on an empty file: {result:?}"
    );
    assert_eq!(&*service.snapshot().text, "");

    let mut service = VersionedConfigService::new("font_px = 14\n".into()).unwrap();
    let base = service.snapshot().revision;
    service.patch(edit(
        base,
        crate::prefs::EDIT_FONT_PX,
        ExpectedValue::Any,
        Some(&automatic),
    ));
    assert_eq!(service.value(crate::prefs::EDIT_FONT_PX).unwrap(), None);
    assert_eq!(&*service.snapshot().text, "");

    let other = (crate::FONT_PX + 1.0).to_string();
    let base = service.snapshot().revision;
    service.patch(edit(
        base,
        crate::prefs::EDIT_FONT_PX,
        ExpectedValue::Any,
        Some(&other),
    ));
    assert_eq!(
        service
            .value(crate::prefs::EDIT_FONT_PX)
            .unwrap()
            .as_deref(),
        Some(other.as_str())
    );
}

#[test]
fn a_respelling_of_the_default_style_is_written_as_a_removal() {
    // The default by its row's name in any case and spacing; turning Comet off, where Off is
    // the default (Windows), keeps it to come back to.
    let (respelled, left) = if crate::app_config::DEFAULT_DECORATIVE_EFFECTS {
        (["Rainbow", " RAINBOW "], "font_px = 14\n")
    } else {
        (
            ["Off", " off "],
            "font_px = 14\ncursor_trail_style_before_off = \"comet\"\n",
        )
    };
    for spelling in respelled {
        let mut service =
            VersionedConfigService::new("cursor_trail_style = \"comet\"\nfont_px = 14\n".into())
                .unwrap();
        let base = service.snapshot().revision;
        service.patch(edit(
            base,
            crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
            ExpectedValue::Any,
            Some(spelling),
        ));
        assert_eq!(
            &*service.snapshot().text,
            left,
            "{spelling:?} is the default style"
        );
        let mut service = VersionedConfigService::new(String::new()).unwrap();
        let base = service.snapshot().revision;
        let result = service.patch(edit(
            base,
            crate::prefs::EDIT_CURSOR_TRAIL_STYLE,
            ExpectedValue::Any,
            Some(spelling),
        ));
        assert!(
            matches!(result, ConfigPatchResult::Unchanged { .. }),
            "{spelling:?} over no style: {result:?}"
        );
    }
}

/// No Settings write moves the owner's keys (root `[harness]`'s `occupancy`,
/// `occupancy_source` and `occupancy_rule`): a patch that would set, change or remove one is
/// refused and the text stays the file's, whichever key it names; the owner edits them in
/// aterm.toml at the window ([`crate::owner_keys_save`]). NEGATIVE CONTROL: another key of
/// the same table writes, and keeps the owner's keys as they were.
#[test]
fn a_settings_write_never_moves_the_owners_keys() {
    let source = "[harness]\noccupancy = \"prompt\"\n";
    let mut service = VersionedConfigService::new(source.into()).unwrap();
    for (key, value) in [
        ("harness.occupancy", Some("cruise")),
        ("harness.occupancy", None),
        ("harness.occupancy_source", Some("settings")),
        ("harness.occupancy_rule", Some("always")),
        ("harness", None),
    ] {
        let base = service.snapshot().revision;
        let result = service.patch(edit(base, key, ExpectedValue::Any, value));
        assert!(
            matches!(
                &result,
                ConfigPatchResult::Rejected { message, .. } if message == OWNER_KEYS_REFUSED
            ),
            "{key} = {value:?}: {result:?}"
        );
        assert_eq!(&*service.snapshot().text, source, "{key} = {value:?}");
    }

    let base = service.snapshot().revision;
    let result = service.patch(edit(
        base,
        "harness.dismiss_surveys",
        ExpectedValue::Any,
        Some("false"),
    ));
    let ConfigPatchResult::Applied { snapshot, .. } = result else {
        panic!("another [harness] key writes: {result:?}");
    };
    assert_eq!(OwnerKeys::of(&snapshot.text), OwnerKeys::of(source));
    assert_ne!(&*snapshot.text, source);
}

#[test]
fn a_value_the_key_already_holds_is_no_write() {
    let source = "font_px = 14  # projector\nbidi = \"disabled\"\n";
    let mut service = VersionedConfigService::new(source.into()).unwrap();
    let base = service.snapshot().revision;
    let result = service.patch(edit(base, "font_px", ExpectedValue::Any, Some("14")));
    assert!(
        matches!(result, ConfigPatchResult::Unchanged { .. }),
        "{result:?}"
    );
    assert_eq!(&*service.snapshot().text, source);
    assert_eq!(service.snapshot().revision, base);

    let result = service.patch(ConfigPatchRequest {
        base_revision: base,
        edits: vec![
            ConfigKeyEdit {
                key: "font_px".into(),
                expected: ExpectedValue::Any,
                value: Some("14.0".into()),
            },
            ConfigKeyEdit {
                key: "bidi".into(),
                expected: ExpectedValue::Any,
                value: Some("implicit".into()),
            },
        ],
    });
    let ConfigPatchResult::Applied { snapshot, .. } = result else {
        panic!("the bidi change applies: {result:?}");
    };
    assert!(
        snapshot.text.contains("font_px = 14  # projector\n"),
        "{:?}",
        snapshot.text
    );
    assert!(!snapshot.text.contains("bidi"), "{:?}", snapshot.text);

    // 16 px: no platform's automatic size (12 on macOS, 15 on Linux, 17 on Windows), which a
    // write removes instead (`the_automatic_font_size_is_written_as_a_removal`).
    assert_ne!(
        crate::FONT_PX,
        16.0,
        "premise: 16 px is written, not removed"
    );
    let base = service.snapshot().revision;
    let result = service.patch(edit(base, "font_px", ExpectedValue::Any, Some("16")));
    assert!(
        matches!(result, ConfigPatchResult::Applied { .. }),
        "{result:?}"
    );
    assert_eq!(service.value("font_px").unwrap().as_deref(), Some("16"));
}

const AUTHORED: &str = "\
# note
font_px = 14  # projector
cursor_blink = false
scrollback_lines = 5000

[update]
enabled = false

[packages]
enabled = false

[machine]
universal_control = \"leave\"

[sparkle_words.feline]
# keep kitties quiet at work
enabled = false

[harness]
# supervision off on this laptop
enabled = false
";

#[test]
fn undo_of_a_reset_restores_the_file_byte_for_byte() {
    let mut service = VersionedConfigService::new(AUTHORED.into()).unwrap();
    let base = service.snapshot().revision;
    let ConfigPatchResult::Applied { undo, .. } = service.reset_all(
        base,
        [
            "font_px",
            "cursor_blink",
            "update.enabled",
            "packages.enabled",
            "sparkle_words.feline.enabled",
            "harness.enabled",
        ]
        .map(str::to_string),
    ) else {
        panic!("the reset applies");
    };
    let reset = service.snapshot().text.to_string();
    assert!(
        !reset.contains("font_px") && !reset.contains("[update]"),
        "{reset}"
    );
    let ConfigPatchResult::Applied { undo: redo, .. } = service.undo(undo) else {
        panic!("the undo applies");
    };
    assert_eq!(&*service.snapshot().text, AUTHORED);
    let ConfigPatchResult::Applied { undo: again, .. } = service.undo(redo) else {
        panic!("the redo applies");
    };
    assert_eq!(&*service.snapshot().text, reset);
    assert!(matches!(
        service.undo(again),
        ConfigPatchResult::Applied { .. }
    ));
    assert_eq!(&*service.snapshot().text, AUTHORED);
}

#[test]
fn undo_after_an_unrelated_edit_reverts_only_its_own_lines() {
    let mut service = VersionedConfigService::new(AUTHORED.into()).unwrap();
    let base = service.snapshot().revision;
    let ConfigPatchResult::Applied { undo, .. } =
        service.patch(edit(base, "font_px", ExpectedValue::Any, None))
    else {
        panic!("the reset applies");
    };
    let reset = service.snapshot().text.to_string();
    let hand = reset.replace(
        "cursor_blink = false\n",
        "cursor_blink = true   # blinking for the demo\n",
    ) + "# my own note\n";
    service.replace_external(hand.clone()).unwrap();
    assert!(matches!(
        service.undo(undo),
        ConfigPatchResult::Applied { .. }
    ));
    let expected = AUTHORED.replace(
        "cursor_blink = false\n",
        "cursor_blink = true   # blinking for the demo\n",
    ) + "# my own note\n";
    assert_eq!(&*service.snapshot().text, expected);

    let base = service.snapshot().revision;
    let ConfigPatchResult::Applied { undo, .. } =
        service.patch(edit(base, "font_px", ExpectedValue::Any, None))
    else {
        panic!("the reset applies");
    };
    let edited = format!("font_px = 20\n{}", service.snapshot().text);
    service.replace_external(edited).unwrap();
    assert!(matches!(
        service.undo(undo),
        ConfigPatchResult::Conflict { keys, .. } if keys == ["font_px"]
    ));
}

#[test]
fn the_reverting_merge_keeps_both_changes_or_refuses() {
    let before = "a\nb\nc\nd\n";
    let after = "a\nB\nc\nd\n";
    assert_eq!(
        merge_reverting(after, before, "a\nB\nc\nD\n").as_deref(),
        Some("a\nb\nc\nD\n")
    );
    assert_eq!(
        merge_reverting(after, before, "x\na\nB\nc\nd\ny\n").as_deref(),
        Some("x\na\nb\nc\nd\ny\n")
    );
    assert_eq!(merge_reverting(after, before, "a\nBB\nc\nd\n"), None);
    assert_eq!(
        merge_reverting(after, before, after).as_deref(),
        Some(before)
    );
    assert_eq!(
        line_hunks(&["a\n", "b\n"], &["a\n", "b\n"]),
        Some(Vec::new())
    );
}

#[test]
fn invalid_multi_key_patch_changes_nothing() {
    let mut service = VersionedConfigService::new("font_px = 14.0\n".into()).unwrap();
    let before = service.snapshot();
    let result = service.patch(ConfigPatchRequest {
        base_revision: before.revision,
        edits: vec![
            ConfigKeyEdit {
                key: "font_px".into(),
                expected: ExpectedValue::Any,
                value: Some("16".into()),
            },
            ConfigKeyEdit {
                key: "cursor_blink".into(),
                expected: ExpectedValue::Any,
                value: Some("definitely".into()),
            },
        ],
    });
    assert!(matches!(result, ConfigPatchResult::Rejected { .. }));
    assert_eq!(service.snapshot(), before);
}

#[test]
fn unrelated_patch_reuses_asset_arc_and_source_changes_require_worker_admission() {
    use std::sync::Arc;

    let path = std::env::temp_dir().join(format!(
        "aterm-config-service-nyan-{}.png",
        std::process::id()
    ));
    let second_path = path.with_extension("second.png");
    let rgba = [0x22, 0x66, 0xee, 0xff, 0xee, 0x66, 0x22, 0xff];
    let png = crate::app_introspect::encode_rgba8_png(&rgba, 2, 1).unwrap();
    std::fs::write(&path, png).unwrap();
    let source = path.to_string_lossy().into_owned();
    let mut service = VersionedConfigService::new(format!(
        "theme = \"Nord\"\ncursor_nyan_sprite = {source:?}\n"
    ))
    .unwrap();
    let initial = service.snapshot();
    assert!(matches!(
        initial.assets.kitty_sprite,
        crate::app_config::KittySpriteAsset::Ready { .. }
    ));
    let initial_fp = initial.assets.kitty_sprite.fingerprint();

    std::fs::write(&path, b"not a PNG").unwrap();

    let ConfigPatchResult::Applied {
        snapshot: patched,
        undo,
    } = service.patch(edit(
        initial.revision,
        "theme",
        ExpectedValue::Exact(Some("Nord".to_string())),
        Some("Dracula"),
    ))
    else {
        panic!("patch must apply");
    };
    assert!(Arc::ptr_eq(&initial.assets, &patched.assets));
    assert!(matches!(
        patched.assets.kitty_sprite,
        crate::app_config::KittySpriteAsset::Ready { .. }
    ));

    let ConfigPatchResult::Applied {
        snapshot: undone, ..
    } = service.undo(undo)
    else {
        panic!("undo must apply");
    };
    assert!(Arc::ptr_eq(&patched.assets, &undone.assets));
    assert_eq!(service.value("theme").unwrap().as_deref(), Some("Nord"));

    let second_rgba = [0xee, 0x22, 0x66, 0xff, 0x22, 0xee, 0x66, 0xff];
    let second_png = crate::app_introspect::encode_rgba8_png(&second_rgba, 2, 1).unwrap();
    std::fs::write(&second_path, second_png).unwrap();
    let second_source = second_path.to_string_lossy().into_owned();
    let rejected = service.patch(edit(
        undone.revision,
        crate::prefs::EDIT_CURSOR_NYAN_SPRITE,
        ExpectedValue::Exact(Some(source.clone())),
        Some(&second_source),
    ));
    assert!(matches!(rejected, ConfigPatchResult::Rejected { .. }));
    assert_eq!(service.snapshot(), undone);

    let config_path = path.with_extension("toml");
    crate::test_scratch::write_config(
        &config_path,
        format!("theme = \"Nord\"\ncursor_nyan_sprite = {second_source:?}\n"),
    );
    let observation = VersionedConfigService::observe_path(&config_path, false).unwrap();
    let prepared =
        VersionedConfigService::prepare_observation(observation, Arc::clone(&undone.assets.themes))
            .unwrap();
    let source_changed = service.synchronize_prepared_observation(prepared).unwrap();
    assert!(!Arc::ptr_eq(&undone.assets, &source_changed.assets));
    assert_eq!(
        source_changed.assets.kitty_sprite.source_id(),
        Some(second_source.as_str())
    );
    assert_ne!(source_changed.assets.kitty_sprite.fingerprint(), initial_fp);

    let rejected_reset = service.reset_all(
        source_changed.revision,
        [crate::prefs::EDIT_CURSOR_NYAN_SPRITE.to_string()],
    );
    assert!(matches!(rejected_reset, ConfigPatchResult::Rejected { .. }));
    assert_eq!(service.snapshot(), source_changed);

    crate::test_scratch::write_config(&config_path, "theme = \"Nord\"\n");
    let observation = VersionedConfigService::observe_path(&config_path, false).unwrap();
    let prepared = VersionedConfigService::prepare_observation(
        observation,
        Arc::clone(&source_changed.assets.themes),
    )
    .unwrap();
    let reset = service.synchronize_prepared_observation(prepared).unwrap();
    assert!(!Arc::ptr_eq(&source_changed.assets, &reset.assets));
    assert!(matches!(
        reset.assets.kitty_sprite,
        crate::app_config::KittySpriteAsset::BuiltIn
    ));

    let missing = path.with_extension("missing.png");
    let external = service
        .replace_external(format!(
            "theme = \"External\"\ncursor_nyan_sprite = {:?}\n",
            missing.to_string_lossy()
        ))
        .expect("a bad optional sprite does not reject unrelated config");
    assert!(matches!(
        external.assets.kitty_sprite,
        crate::app_config::KittySpriteAsset::Invalid { .. }
    ));
    assert_eq!(service.value("theme").unwrap().as_deref(), Some("External"));
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(second_path);
    let _ = std::fs::remove_file(config_path);
}

#[test]
fn undo_restores_authored_empty_collections() {
    for (key, authored) in [
        ("palette", "palette = []\n"),
        ("palette_overrides", "palette_overrides = {}\n"),
        ("palette_overrides", "[palette_overrides]\n"),
        (
            "palette_overrides",
            "[palette_overrides]\n# An authored empty section.\n",
        ),
        ("shell_args", "shell_args = []\n"),
        ("font_features", "font_features = []\n"),
    ] {
        let text = format!("font_px = 13.0\n{authored}");
        let expected: aterm_toml::Value = aterm_toml::from_str(&text).unwrap();
        let mut service = VersionedConfigService::new(text).unwrap();
        assert!(service.value(key).unwrap().is_some());
        let ConfigPatchResult::Applied { snapshot, undo } =
            service.reset_all(service.snapshot().revision, [key.to_string()])
        else {
            panic!("{authored}: clear must apply");
        };
        assert!(service.value(key).unwrap().is_none(), "{authored}");
        assert!(snapshot.text.contains("font_px = 13.0"));
        let ConfigPatchResult::Applied { snapshot, .. } = service.undo(undo) else {
            panic!("{authored}: Undo must restore the authored empty collection");
        };
        assert!(service.value(key).unwrap().is_some(), "{authored}");
        let restored: aterm_toml::Value = aterm_toml::from_str(&snapshot.text).unwrap();
        assert_eq!(
            restored, expected,
            "{authored}: typed value and unrelated setting survive"
        );
        let _: crate::app_config::Config = aterm_toml::from_str(&snapshot.text).unwrap();
    }
    for master in [false, true] {
        let original = format!("font_px = 13.0\ncursor_trail = {master}\n");
        let mut service = VersionedConfigService::new(original.clone()).unwrap();
        let ConfigPatchResult::Applied { snapshot, undo } = service.patch(edit(
            service.snapshot().revision,
            "cursor_trail_style",
            ExpectedValue::Exact(None),
            Some("comet"),
        )) else {
            panic!("style choice");
        };
        let chosen = snapshot.text.to_string();
        assert!(!snapshot.semantic_values.contains_key("cursor_trail"));
        let ConfigPatchResult::Applied { .. } = service.patch(edit(
            service.snapshot().revision,
            "font_px",
            ExpectedValue::Exact(Some("13".into())),
            Some("20"),
        )) else {
            panic!("unrelated later write");
        };
        let ConfigPatchResult::Applied {
            snapshot,
            undo: redo,
        } = service.undo(undo)
        else {
            panic!("the full inverse must survive an unrelated edit");
        };
        let expected = original.replace("font_px = 13.0", "font_px = 20");
        assert_eq!(
            parse_values(&snapshot.text).unwrap(),
            parse_values(&expected).unwrap()
        );
        let ConfigPatchResult::Applied { snapshot, .. } = service.undo(redo) else {
            panic!("Redo");
        };
        let expected = chosen.replace("font_px = 13.0", "font_px = 20");
        assert_eq!(
            parse_values(&snapshot.text).unwrap(),
            parse_values(&expected).unwrap()
        );
    }

    let source = "cursor_trail = false\ncursor_momentum_glow = true\n";
    let mut service = VersionedConfigService::new(source.into()).unwrap();
    let ConfigPatchResult::Applied { snapshot, undo } = service.patch(edit(
        service.snapshot().revision,
        "cursor_trail_style",
        ExpectedValue::Exact(None),
        Some("comet"),
    )) else {
        panic!("style choice");
    };
    let newer = service
        .replace_external(snapshot.text.replace(
            "cursor_momentum_glow = false",
            "cursor_momentum_glow = true",
        ))
        .unwrap();
    assert!(
        matches!(service.undo(undo), ConfigPatchResult::Conflict { keys, .. }
        if keys.contains(&"cursor_momentum_glow".to_string()))
    );
    assert_eq!(service.snapshot(), newer);

    let source = "cursor_trail = false\ncursor_trail_style = \"laser\"\n";
    let mut service = VersionedConfigService::new(source.into()).unwrap();
    let base = service.snapshot().revision;
    let newer = service
        .replace_external(format!("{source}cursor_momentum_glow = true\n"))
        .unwrap();
    assert!(matches!(service.patch(edit(base, "cursor_trail_style",
        ExpectedValue::Exact(Some("laser".into())), Some("comet"))),
        ConfigPatchResult::Conflict { keys, .. } if keys.contains(&"cursor_momentum_glow".to_string())));
    assert_eq!(service.snapshot(), newer);

    let mut service =
        VersionedConfigService::new("[matrix_rain]\nenabled = false\nfps = 37\n".into()).unwrap();
    let base = service.snapshot().revision;
    let ConfigPatchResult::Applied { undo, .. } = service.patch(edit(
        base,
        "matrix_rain.enabled",
        ExpectedValue::Exact(Some("false".into())),
        Some("true"),
    )) else {
        panic!("nested patch");
    };
    let ConfigPatchResult::Applied { .. } = service.patch(edit(
        base,
        "matrix_rain.fps",
        ExpectedValue::Exact(Some("37".into())),
        Some("41"),
    )) else {
        panic!("stale unrelated child rebases");
    };
    let ConfigPatchResult::Applied { snapshot, .. } = service.undo(undo) else {
        panic!("conditional Undo");
    };
    assert_eq!(
        snapshot
            .semantic_values
            .get("matrix_rain.enabled")
            .map(String::as_str),
        Some("false")
    );
    assert_eq!(
        snapshot
            .semantic_values
            .get("matrix_rain.fps")
            .map(String::as_str),
        Some("41")
    );
}

#[test]
fn a_reported_palette_value_writes_back_as_toml() {
    for text in [
        "palette_overrides = { 12 = \"#88c0d0\", 3 = \"#112233\" }\n",
        "[palette_overrides]\n12 = \"#88c0d0\"\n3 = \"#112233\"\n",
    ] {
        let raw = parse_values(text).unwrap()["palette_overrides"].clone();
        let out = crate::prefs::apply_prefs_edits("", &[("palette_overrides", Some(raw))])
            .unwrap_or_else(|error| panic!("{text}: {error}"));
        let config: crate::app_config::Config = aterm_toml::from_str(&out).expect(&out);
        let slots = config.palette_overrides.expect(&out);
        assert_eq!(
            slots.get("12").map(String::as_str),
            Some("#88c0d0"),
            "{out}"
        );
        assert_eq!(slots.get("3").map(String::as_str), Some("#112233"), "{out}");
    }
    let raw = parse_values("palette = [\"#1d1f21\", \"#cc6666\"]\n").unwrap()["palette"].clone();
    let out = crate::prefs::apply_prefs_edits("", &[("palette", Some(raw))]).unwrap();
    let config: crate::app_config::Config = aterm_toml::from_str(&out).expect(&out);
    assert_eq!(
        config.palette,
        Some(vec!["#1d1f21".to_string(), "#cc6666".to_string()])
    );
    for bad in [
        "{ 300 = \"#88c0d0\" }",
        "{ 12 = \"nope\" }",
        "[]",
        "#88c0d0",
    ] {
        assert!(
            crate::prefs::apply_prefs_edits("", &[("palette_overrides", Some(bad.to_string()))])
                .is_err(),
            "{bad}"
        );
    }
}

#[test]
fn snapshot_key_identities_keep_literal_dots_distinct_from_nested_paths() {
    let source = r#""packages.include" = ["literal"]

[packages]
include = ["nested"]
"#;
    let values = parse_values(source).unwrap();
    assert_eq!(
        values.get(r#""packages.include""#).map(String::as_str),
        Some(r#"["literal"]"#),
        "one literal segment keeps its canonical quoted identity"
    );
    assert_eq!(
        values.get("packages.include").map(String::as_str),
        Some(r#"["nested"]"#),
        "the registered two-segment path remains independently addressable"
    );
    assert_eq!(
        join_config_key_path("", "packages.include"),
        r#""packages.include""#
    );
    assert_eq!(
        join_config_key_path("packages", "include"),
        "packages.include"
    );

    let mut service = VersionedConfigService::new(source.to_string()).unwrap();
    let revision = service.snapshot().revision;
    let result = service.patch(edit(
        revision,
        "packages.include",
        ExpectedValue::Exact(Some(r#"["nested"]"#.into())),
        None,
    ));
    let ConfigPatchResult::Applied { snapshot, .. } = result else {
        panic!("nested reset must apply without touching the literal key: {result:?}");
    };
    let after = snapshot.values().unwrap();
    assert!(!after.contains_key("packages.include"));
    assert_eq!(
        after.get(r#""packages.include""#).map(String::as_str),
        Some(r#"["literal"]"#),
        "a nested reset is lossless for the lookalike literal key"
    );
    assert!(
        snapshot
            .text
            .contains(r#""packages.include" = ["literal"]"#)
    );

    let mut service = VersionedConfigService::new(
        r#""packages.include" = ["literal-one"]
"#
        .to_string(),
    )
    .unwrap();
    let base = service.snapshot().revision;
    service
        .replace_external(
            r#""packages.include" = ["literal-two"]
"#
            .to_string(),
        )
        .unwrap();
    let result = service.patch(edit(
        base,
        "packages.include",
        ExpectedValue::Exact(None),
        None,
    ));
    let ConfigPatchResult::Unchanged { snapshot } = result else {
        panic!(
            "a literal-key change must not OCC-conflict with an absent nested reset: {result:?}"
        );
    };
    let values = snapshot.values().unwrap();
    assert_eq!(
        values.get(r#""packages.include""#).map(String::as_str),
        Some(r#"["literal-two"]"#)
    );
    assert!(!values.contains_key("packages.include"));
}

#[test]
fn depth_two_dotted_key_projection_occ_and_blank_revert() {
    let values = parse_values(
        "[sparkle_words]\nenabled = true\n\n[sparkle_words.profanity]\nenabled = false\n",
    )
    .unwrap();
    assert_eq!(
        values
            .get("sparkle_words.profanity.enabled")
            .map(String::as_str),
        Some("false"),
        "a hand-set depth-2 leaf projects as an exact per-key entry"
    );
    assert_eq!(
        values.get("sparkle_words.enabled").map(String::as_str),
        Some("true")
    );
    assert!(
        values.contains_key("sparkle_words") && values.contains_key("sparkle_words.profanity"),
        "every intermediate table keeps its opaque whole-table entry"
    );
    let none = parse_values("font_px = 13.0\n").unwrap();
    assert!(
        !none.keys().any(|k| k.starts_with("sparkle_words")),
        "an absent tree projects nothing (not explicit)"
    );

    let mut service = VersionedConfigService::new(
        "[sparkle_words]\n\n[sparkle_words.profanity]\nenabled = false\n".into(),
    )
    .unwrap();
    let base = service.snapshot().revision;

    service
        .replace_external("[sparkle_words]\n\n[sparkle_words.profanity]\nenabled = true\n".into())
        .unwrap();
    let stale = service.patch(edit(
        base,
        "sparkle_words.profanity.enabled",
        ExpectedValue::Exact(Some("false".into())),
        Some("false"),
    ));
    let ConfigPatchResult::Conflict { keys, .. } = stale else {
        panic!("stale depth-2 expectation must conflict, got {stale:?}");
    };
    assert_eq!(keys, vec!["sparkle_words.profanity.enabled".to_string()]);

    let rev = service.snapshot().revision;
    let applied = service.patch(edit(
        rev,
        "sparkle_words.profanity.enabled",
        ExpectedValue::Exact(Some("true".into())),
        Some("false"),
    ));
    let ConfigPatchResult::Applied { snapshot, .. } = applied else {
        panic!("current-revision depth-2 write applies, got {applied:?}");
    };
    assert!(snapshot.text.contains("enabled = false"));
    assert_eq!(
        service
            .value("sparkle_words.profanity.enabled")
            .unwrap()
            .as_deref(),
        Some("false")
    );

    let rev = service.snapshot().revision;
    let reverted = service.patch(edit(
        rev,
        "sparkle_words.profanity.enabled",
        ExpectedValue::Exact(Some("false".into())),
        None,
    ));
    let ConfigPatchResult::Applied { snapshot, .. } = reverted else {
        panic!("blank revert applies, got {reverted:?}");
    };
    assert!(
        !snapshot.text.contains("enabled"),
        "the leaf is removed: {:?}",
        snapshot.text
    );
    assert!(
        !snapshot.text.contains("[sparkle_words"),
        "bare tables go at both depths: {:?}",
        snapshot.text
    );
    assert_eq!(
        service.value("sparkle_words.profanity.enabled").unwrap(),
        None
    );
}
