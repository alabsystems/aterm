// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn parsed(source: &str) -> crate::app_config::Config {
    aterm_toml::from_str(source).expect("test config")
}

const ABS_TMP: &str = if cfg!(windows) { "C:/tmp" } else { "/tmp" };

#[test]
fn semantic_warnings_match_dependent_runtime_clamps() {
    let config = parsed(
        "[sparkle_words.ink]\nloop = true\nsweep_ms = 400\n\
         [matrix_rain]\nalpha = 120\nhead_alpha = 20\n",
    );
    let warnings = config_semantic_warnings(&config);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings.iter().any(|warning| {
        warning.key == "sparkle_words.ink.sweep_ms"
            && warning.message.contains("effectively 600 ms")
    }));
    assert!(warnings.iter().any(|warning| {
        warning.key == "matrix_rain.head_alpha" && warning.message.contains("effectively 120")
    }));

    let clean = parsed(
        "[sparkle_words.ink]\nloop = true\nsweep_ms = 600\n\
         [matrix_rain]\nalpha = 120\nhead_alpha = 120\n",
    );
    assert!(config_semantic_warnings(&clean).is_empty());
}

#[test]
fn capability_warning_matrix_distinguishes_macos_windows_and_unsupported() {
    let translucent = parsed(
        "background_opacity = 0.7\nbackground_material = \"hud\"\n\
         window_colorspace = \"display-p3\"\n",
    );
    let keys = |gpu, platform| {
        config_backend_capability_warnings(&translucent, gpu, platform)
            .into_iter()
            .map(|warning| warning.key)
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert!(keys(true, ConfigCapabilityPlatform::MacOs).is_empty());
    assert_eq!(
        keys(false, ConfigCapabilityPlatform::MacOs),
        std::collections::BTreeSet::from([
            "background_material",
            "background_opacity",
            "window_colorspace",
        ])
    );
    assert_eq!(
        keys(true, ConfigCapabilityPlatform::Windows),
        std::collections::BTreeSet::from(["background_opacity", "window_colorspace"]),
        "Windows GPU consumes DWM material, but not per-pixel grid opacity"
    );
    assert_eq!(
        keys(false, ConfigCapabilityPlatform::Windows),
        std::collections::BTreeSet::from([
            "background_material",
            "background_opacity",
            "window_colorspace",
        ])
    );
    for platform in [
        ConfigCapabilityPlatform::Linux,
        ConfigCapabilityPlatform::Unsupported,
    ] {
        assert_eq!(
            keys(true, platform),
            std::collections::BTreeSet::from([
                "background_material",
                "background_opacity",
                "window_colorspace",
            ]),
            "{platform:?}"
        );
    }

    let opaque = parsed("background_material = \"sidebar\"\n");
    assert_eq!(
        config_backend_capability_warnings(&opaque, true, ConfigCapabilityPlatform::MacOs).len(),
        1
    );
    assert!(
        config_backend_capability_warnings(&opaque, true, ConfigCapabilityPlatform::Windows)
            .is_empty()
    );

    let platform_only = parsed(
        "font_thicken = true\nsecure_keyboard_entry = true\n[update]\nenabled = false\nowner = \"safe-owner\"\nrepo = \"aterm\"\nauto_apply = true\n",
    );
    let platform_keys = |platform| {
        config_backend_capability_warnings(&platform_only, true, platform)
            .into_iter()
            .map(|warning| warning.key)
            .collect::<std::collections::BTreeSet<_>>()
    };
    let non_updater = config_backend_capability_warnings(
        &platform_only,
        true,
        ConfigCapabilityPlatform::Unsupported,
    );
    assert_eq!(
        non_updater
            .iter()
            .map(|warning| warning.key)
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from([
            "font_thicken",
            "secure_keyboard_entry",
            "update.auto_apply",
            "update.enabled",
            "update.owner",
            "update.repo",
        ])
    );
    assert!(
        non_updater
            .iter()
            .filter(|warning| warning.key.starts_with("update."))
            .all(|warning| warning.message.contains("only on macOS and Linux")),
        "{non_updater:?}"
    );
    assert_eq!(
        platform_keys(ConfigCapabilityPlatform::Linux),
        std::collections::BTreeSet::from(["font_thicken", "secure_keyboard_entry"])
    );
    assert!(
        config_backend_capability_warnings(&platform_only, true, ConfigCapabilityPlatform::MacOs,)
            .is_empty()
    );
}

#[test]
fn platform_warning_matrix_discloses_audio_dialog_and_notification_stubs() {
    let config = parsed(
        "trail_sounds = true\ntrail_sound_volume = 0.4\nconfirm_multiline_paste = true\nallow_notifications = true\n",
    );
    let keys = |platform| {
        config_backend_capability_warnings(&config, true, platform)
            .into_iter()
            .map(|warning| warning.key)
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert!(keys(ConfigCapabilityPlatform::MacOs).is_empty());
    assert_eq!(
        keys(ConfigCapabilityPlatform::Windows),
        std::collections::BTreeSet::from(["trail_sound_volume", "trail_sounds"]),
        "Windows has dialogs/notifications but no trail-audio consumer"
    );
    assert_eq!(
        keys(ConfigCapabilityPlatform::Linux),
        std::collections::BTreeSet::from(["trail_sound_volume", "trail_sounds"]),
        "Linux delivers notifications but has no trail-audio consumer"
    );
    assert_eq!(
        keys(ConfigCapabilityPlatform::Unsupported),
        std::collections::BTreeSet::from([
            "allow_notifications",
            "trail_sound_volume",
            "trail_sounds",
        ]),
    );

    let bell = parsed("bell_sound = false\n");
    for (platform, inert) in [
        (ConfigCapabilityPlatform::MacOs, false),
        (ConfigCapabilityPlatform::Windows, false),
        (ConfigCapabilityPlatform::Linux, false),
        (ConfigCapabilityPlatform::Unsupported, true),
    ] {
        assert_eq!(
            config_backend_capability_warnings(&bell, true, platform)
                .iter()
                .any(|warning| warning.key == crate::prefs::EDIT_BELL_SOUND),
            inert,
            "{platform:?}"
        );
    }
}

#[test]
fn repoint_keys_say_a_shipped_build_does_not_read_them() {
    let config = parsed("[update]\nowner = \"me\"\nrepo = \"\"\n[packages]\naccount = \"me/x\"\n");
    let keys = |update_dev, packages_dev| {
        repoint_warnings(&config, update_dev, packages_dev)
            .into_iter()
            .map(|warning| warning.key)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        keys(false, false),
        ["update.owner", "packages.account"],
        "a valid name is still ignored by a shipped build; a blank one is harmless"
    );
    assert_eq!(keys(true, true), ["packages.account"], "not a GitHub name");
}

#[test]
fn pure_semantics_report_runtime_fallbacks_instead_of_false_green() {
    let source = r##"
font_px = 201
font_variation = ["wght=450", "not-an-axis"]
font_features = ["+ss01 toolong", "cv01=2"]

[update]
owner = "bad owner"
repo = "bad/repo"

[packages]
account = "bad/account"

[matrix_rain]
hue = "blue"

[sparkle_words.profanity]
palette = ["#112233", "not-a-color"]
"##;
    let warnings = config_semantic_warnings(&parsed(source));
    let joined = warnings
        .iter()
        .map(|warning| warning.message.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for expected in [
        "font_px",
        "is not accepted (expected",
        "font_variation[1]",
        "font_features[0]",
        "update.owner",
        "update.repo",
        "packages.account",
        "matrix_rain.hue",
        "sparkle_words.profanity.palette[1]",
    ] {
        assert!(joined.contains(expected), "missing {expected:?}: {joined}");
    }

    let clean = parsed(
        r##"font_px = 200
font_variation = ["wght=450"]
font_features = ["+ss01 -calt cv01=2"]
[update]
owner = "safe-owner"
repo = "safe.repo"
[packages]
account = "safe_owner"
[matrix_rain]
hue = "#12ABef"
[sparkle_words.profanity]
palette = ["#112233"]
"##,
    );
    assert!(
        config_semantic_warnings(&clean).is_empty(),
        "valid boundary/domain values stay clean"
    );
}

#[test]
fn security_capability_warnings_name_the_gui_hosts_that_are_actually_missing() {
    let enabled = config_semantic_warnings(&parsed(
        "allow_osc52_query = true\nallow_window_ops = true\n",
    ));
    let keys: Vec<&str> = enabled.iter().map(|warning| warning.key).collect();
    assert_eq!(keys.len(), 2, "{enabled:?}");
    assert!(keys.contains(&"allow_osc52_query") && keys.contains(&"allow_window_ops"));
    assert!(
        config_semantic_warnings(&parsed(
            "allow_osc52_query = false\nallow_window_ops = false\n"
        ))
        .is_empty(),
        "disabled capabilities are the honest fail-closed default"
    );
}

#[test]
fn custom_records_disclose_every_inert_arm() {
    let source = r#"
[[sparkle_words.custom]]
words = [" "]

[[sparkle_words.custom]]
words = ["quiet"]
burst = { kind = "glow", chance = 0 }

[[sparkle_words.custom]]
words = ["live"]
ink = "rainbow"
"#;
    let warnings = config_semantic_warnings(&parsed(source));
    let joined = warnings
        .iter()
        .map(|warning| warning.message.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for expected in [
        "record 1 has no nonblank word",
        "record 1 is inert",
        "record 2 is inert",
    ] {
        assert!(joined.contains(expected), "missing {expected:?}: {joined}");
    }
    assert!(
        warnings
            .iter()
            .all(|warning| !warning.message.contains("record 3 is inert")),
        "live custom record must not be called inert"
    );
}

#[test]
fn theme_derived_matrix_body_discloses_conditional_head_floor() {
    let warnings = config_semantic_warnings(&parsed("[matrix_rain]\nhead_alpha = 80\n"));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].message.contains("theme-derived body alpha"));

    let max = config_semantic_warnings(&parsed("[matrix_rain]\nhead_alpha = 135\n"));
    assert!(max.is_empty(), "the body cannot exceed the cap: {max:?}");
}

#[test]
fn package_admission_warnings_are_pure_and_specific() {
    let packages = parsed("[packages]\nenabled = true\n");
    let rootless = package_capability_warnings(&packages, false);
    assert_eq!(rootless.len(), 1);
    assert!(rootless[0].message.contains("verification root"));
    assert!(
        !rootless[0].message.contains("ATPKG_"),
        "no environment switch is named"
    );
    assert!(package_capability_warnings(&packages, true).is_empty());
}

#[test]
fn privacy_auto_accept_is_refused_and_names_the_grant_that_works() {
    let warnings = config_semantic_warnings(&parsed("[privacy]\nauto_accept = true\n"));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].key, "privacy.auto_accept");
    assert!(config_semantic_warnings(&parsed("[privacy]\nauto_accept = false\n")).is_empty());
}

#[test]
fn privacy_master_switch_off_flags_the_features_it_silences() {
    let warnings = config_semantic_warnings(&parsed(
        "[privacy]\nenabled = false\nnotice = true\nwarmup = \"on-request\"\nobserver = true\n",
    ));
    assert_eq!(warnings.len(), 3, "{warnings:?}");
    assert!(
        warnings.iter().any(|w| w.key == "privacy.notice"),
        "{warnings:?}"
    );
    assert!(
        warnings.iter().any(|w| w.key == "privacy.warmup"),
        "{warnings:?}"
    );
    assert!(
        warnings.iter().any(|w| w.key == "privacy.observer"),
        "{warnings:?}"
    );
    assert!(config_semantic_warnings(&parsed("[privacy]\nenabled = false\n")).is_empty());
    assert!(
        config_semantic_warnings(&parsed(
            "[privacy]\nenabled = true\nnotice = true\nwarmup = \"never\"\nobserver = true\n"
        ))
        .is_empty()
    );
}

#[test]
fn privacy_flags_an_unknown_warmup_spelling_and_folder_name() {
    let warnings = config_semantic_warnings(&parsed(
        "[privacy]\nwarmup = \"first-launch\"\nwarmup_folders = [\"desktop\", \"keychain\"]\n",
    ));
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    let warmup = warnings
        .iter()
        .find(|w| w.key == "privacy.warmup")
        .expect("warmup warning");
    assert!(
        warmup.message.contains("\"first-launch\"")
            && warmup.message.contains("\"never\"")
            && warmup.message.contains("\"on-request\""),
        "the accepted vocabulary is listed: {}",
        warmup.message
    );
    let folder = warnings
        .iter()
        .find(|w| w.key == "privacy.warmup_folders")
        .expect("folder warning");
    assert!(
        folder.message.contains("warmup_folders[1]") && folder.message.contains("\"keychain\""),
        "the index and the name are both named: {}",
        folder.message
    );
}

#[test]
fn privacy_flags_a_protected_root_it_cannot_resolve() {
    let warnings = config_semantic_warnings(&parsed(&format!(
        "[privacy]\nprotected_roots = [\"~/vault\", \"{ABS_TMP}/vault\", \"vault\", \"\"]\n"
    )));
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings.iter().all(|w| w.key == "privacy.protected_roots"),
        "{warnings:?}"
    );
    assert!(warnings[0].message.contains("protected_roots[2]"));
    assert!(
        warnings[1].message.contains("protected_roots[3]"),
        "a blank entry is dropped just as silently: {}",
        warnings[1].message
    );
}

#[test]
fn privacy_flags_an_apply_hold_that_could_pin_a_build() {
    let warnings = config_semantic_warnings(&parsed("[privacy]\nwarmup_hold_ms = 3600000\n"));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].key, "privacy.warmup_hold_ms");
    assert!(
        warnings[0].message.contains("effectively 600000 ms"),
        "{}",
        warnings[0].message
    );
    assert_eq!(
        parsed("[privacy]\nwarmup_hold_ms = 3600000\n").privacy_warmup_hold_ms(),
        crate::app_config::PRIVACY_WARMUP_HOLD_MS_MAX,
        "the warning and the resolver agree on the effective value"
    );
    assert!(config_semantic_warnings(&parsed("[privacy]\nwarmup_hold_ms = 600000\n")).is_empty());
}

#[test]
fn a_valid_privacy_section_emits_no_semantic_warning() {
    let clean = parsed(&format!(
        concat!(
            "[privacy]\n",
            "enabled = true\n",
            "check = true\n",
            "notice = true\n",
            "report_attribution = true\n",
            "warmup = \"on-request\"\n",
            "warmup_folders = [\"documents\", \"desktop\", \"downloads\", \"app-data\"]\n",
            "warmup_hold_ms = 120000\n",
            "probe_interval_ms = 5000\n",
            "protected_roots = [\"~/vault\", \"{}/vault\"]\n",
            "auto_accept = false\n",
        ),
        ABS_TMP
    ));
    let warnings = config_semantic_warnings(&clean);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(config_semantic_warnings(&parsed("[privacy]")).is_empty());
    assert!(
        config_semantic_warnings(&crate::app_config::Config::default())
            .iter()
            .all(|w| !w.key.starts_with("privacy"))
    );
}

#[test]
fn host_semantics_accept_valid_lexicon_and_report_missing_or_rejected_layers() {
    let thread = std::thread::current();
    let root = std::env::temp_dir().join(format!(
        "aterm-config-lexicon-{}-{}",
        std::process::id(),
        thread.name().unwrap_or("test").replace(':', "-")
    ));
    std::fs::create_dir_all(&root).unwrap();
    let valid = root.join("valid.toml");
    let invalid = root.join("invalid.toml");
    let conflict = root.join("conflict.toml");
    let oversized = root.join("oversized.toml");
    std::fs::write(
        &valid,
        "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"ultrathink\"]\n",
    )
    .unwrap();
    std::fs::write(&invalid, "[[entry]\nclass = \"emphasis\"\n").unwrap();
    std::fs::write(
        &conflict,
        "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"abc猫\"]\n",
    )
    .unwrap();
    std::fs::write(
        &oversized,
        vec![b'x'; aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES + 1],
    )
    .unwrap();

    let config_for = |path: &std::path::Path| {
        parsed(&format!(
            "[sparkle_words]\nlexicon = {:?}\n",
            path.to_string_lossy()
        ))
    };
    assert!(
        config_host_semantic_warnings(&config_for(&valid))
            .iter()
            .all(|warning| warning.key != "sparkle_words.lexicon")
    );
    let rejected = config_host_semantic_warnings(&config_for(&invalid));
    assert!(rejected.iter().any(|warning| {
        warning.key == "sparkle_words.lexicon" && warning.message.contains("rejected")
    }));
    let missing = config_host_semantic_warnings(&config_for(&root.join("missing.toml")));
    assert!(missing.iter().any(|warning| {
        warning.key == "sparkle_words.lexicon" && warning.message.contains("unreadable")
    }));
    let oversized_warnings = config_host_semantic_warnings(&config_for(&oversized));
    assert!(oversized_warnings.iter().any(|warning| {
        warning.key == "sparkle_words.lexicon"
            && warning.message.contains("unreadable")
            && warning
                .message
                .contains(&aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES.to_string())
    }));
    let conflict = config_host_semantic_warnings(&config_for(&conflict));
    assert!(conflict.iter().any(|warning| {
        warning.key == "sparkle_words.lexicon"
            && warning.message.contains("merged-layer conflict")
            && warning.message.contains("abc猫")
    }));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn host_semantics_mirrors_runtime_merged_lexicon_conflicts_and_cjk_filter() {
    let inline = parsed(
        r#"[sparkle_words.feline]
cjk_single_char = false

[[sparkle_words.custom]]
words = ["犬", "abc猫"]
ink = "rainbow"
"#,
    );
    let warnings = config_host_semantic_warnings(&inline);
    assert!(warnings.iter().any(|warning| {
        warning.key == "sparkle_words.custom.words"
            && warning.message.contains("record 1 word 1")
            && warning.message.contains("requires cjk_single_char = true")
    }));
    assert!(warnings.iter().any(|warning| {
        warning.key == "sparkle_words.custom.words"
            && warning.message.contains("record 1 word 2")
            && warning.message.contains("dropped")
    }));

    let opted_in = parsed(
        r#"[sparkle_words.feline]
cjk_single_char = true

[[sparkle_words.custom]]
words = ["犬", "abc猫"]
ink = "rainbow"
"#,
    );
    let opted_in = config_host_semantic_warnings(&opted_in);
    assert!(
        opted_in
            .iter()
            .all(|warning| !warning.message.contains("requires cjk_single_char = true")),
        "resolved opt-in must filter the same warning as recompute_sparkle: {opted_in:?}"
    );
    assert!(opted_in.iter().any(|warning| {
        warning.key == "sparkle_words.custom.words" && warning.message.contains("record 1 word 2")
    }));

    let extra = parsed("[sparkle_words.emphasis]\nextra_words = [\"abc猫\"]\n");
    assert!(config_host_semantic_warnings(&extra).iter().any(|warning| {
        warning.key == "sparkle_words.emphasis.extra_words"
            && warning
                .message
                .starts_with("sparkle_words.emphasis.extra_words[0]")
    }));
}

#[test]
fn host_validation_reports_only_authored_launch_flag_masking() {
    if !crate::test_alone::alone() {
        return;
    }
    let _ = crate::cli::parse_cli(vec!["--columns".into(), "120".into()]);

    let authored: crate::app_config::Config = aterm_toml::from_str("columns = 80\n").unwrap();
    let warnings = config_host_semantic_warnings(&authored);
    assert!(warnings.iter().any(|warning| {
        warning.key == "columns"
            && warning.message.contains("--columns overrides")
            && warning.message.contains("effective value is 120")
    }));

    let absent = config_host_semantic_warnings(&crate::app_config::Config::default());
    assert!(
        absent.iter().all(|warning| warning.key != "columns"),
        "an absent TOML token must not receive a source diagnostic"
    );
}

#[test]
fn validate_accepts_good_config_and_rejects_bad() {
    assert!(validate_config_text("font_px = 14.0\ngpu = true\ntheme = \"Dracula\"").is_ok());
    assert!(validate_config_text("").is_ok());
    let err = validate_config_text("font_px = \"big\"").unwrap_err();
    assert!(
        !err.is_empty(),
        "a type mismatch must yield an error message"
    );
    assert!(validate_config_text("font_px = = 1").is_err());
}

fn shell_warns(toml: &str) -> Vec<String> {
    validate_config_text(toml)
        .expect("parses")
        .into_iter()
        .filter(|w| w.starts_with("shell:") && !w.contains("overrides the saved value"))
        .collect()
}

/// Unix: `shell` resolves as the spawn resolves it, a bare name on PATH.
#[cfg(not(windows))]
#[test]
fn validate_flags_a_shell_the_spawn_cannot_exec() {
    assert!(
        shell_warns("shell = \"sh\"").is_empty(),
        "a bare name found on PATH is clean"
    );
    let bare = shell_warns("shell = \"aterm-no-such-shell-xyz\"");
    assert!(
        bare.iter().any(|w| w.contains("is not on PATH")),
        "a bare name nothing on PATH answers must warn: {bare:?}"
    );
    let missing = shell_warns("shell = \"/nonexistent/definitely-not-a-shell\"");
    assert!(
        missing.iter().any(|w| w.contains("does not exist")),
        "a nonexistent path must warn: {missing:?}"
    );
    assert!(
        shell_warns("shell = \"/bin/sh\"").is_empty(),
        "an absolute existing shell is clean"
    );
}

/// Windows: the spellings a user writes validate clean.
#[cfg(windows)]
#[test]
fn validate_accepts_the_windows_shell_spellings_a_user_would_write() {
    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    let cmd_backslash = format!("{sysroot}\\System32\\cmd.exe");
    assert!(
        std::path::Path::new(&cmd_backslash).is_file(),
        "test premise: cmd.exe must exist at {cmd_backslash}"
    );

    let root = std::env::temp_dir().join(format!("aterm-shellcfg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let fixture = |dir: &str, file: &str| -> String {
        let d = root.join(dir);
        std::fs::create_dir_all(&d).expect("fixture dir");
        let f = d.join(file);
        std::fs::write(&f, b"placeholder, never executed").expect("fixture file");
        f.display().to_string()
    };
    let spaced = fixture("Program Files\\Git\\bin", "bash.exe");
    assert!(
        spaced.contains(' '),
        "premise: {spaced:?} must hold a space"
    );
    let apostrophe = fixture("O'Brien", "bash.exe");
    assert!(apostrophe.contains('\''), "premise: {apostrophe:?}");
    let percent = fixture("%tools%", "nu.exe");
    assert!(percent.contains("%tools%"), "premise: {percent:?}");

    let mut accepted = vec![
        "cmd".to_string(),
        "cmd.exe".to_string(),
        "CMD.EXE".to_string(),
        "powershell".to_string(),
        cmd_backslash.clone(),
        cmd_backslash.replace('\\', "/"),
        cmd_backslash.to_uppercase(),
        spaced.clone(),
        spaced.replace('\\', "/"),
        apostrophe.clone(),
        percent.clone(),
    ];
    for optional in ["bash", "pwsh", "nu", "wsl"] {
        if matches!(
            aterm_pty::classify_shell_name(std::ffi::OsStr::new(optional)),
            aterm_pty::ShellResolution::Resolved(_)
        ) {
            accepted.push(optional.to_string());
        }
    }
    let authored = |value: &str| -> String {
        if value.contains('\'') {
            format!("shell = \"{}\"", value.replace('\\', "\\\\"))
        } else {
            format!("shell = '{value}'")
        }
    };
    for value in &accepted {
        let warnings = shell_warns(&authored(value));
        assert!(
            warnings.is_empty(),
            "{value:?} is a legitimate Windows shell and must validate clean, got {warnings:?}"
        );
    }

    let missing_path = format!("{sysroot}\\System32\\aterm-no-such-shell-xyz.exe");
    let cmd_with_args = format!("{cmd_backslash} /K dir");
    let mut rejections = Vec::new();
    for (value, needle) in [
        ("aterm-no-such-shell-xyz", "is not on %PATH%"),
        (missing_path.as_str(), "does not exist"),
        (sysroot.as_str(), "not an executable file"),
        ("System32\\cmd.exe", "relative path"),
        ("\\Windows\\System32\\cmd.exe", "relative path"),
        ("C:cmd.exe", "relative path"),
        (r#""C:\Windows\System32\cmd.exe" /K dir"#, "quoted"),
        ("cmd /K dir", "carries arguments"),
        (cmd_with_args.as_str(), "carries arguments"),
        ("%COMSPEC%", "never environment-expanded"),
        ("%USERPROFILE%\\bin\\bash.exe", "never environment-expanded"),
    ] {
        let warnings = shell_warns(&authored(value));
        assert!(
            warnings.iter().any(|w| w.contains(needle)),
            "{value:?} must be rejected with {needle:?}, got {warnings:?}"
        );
        rejections.extend(warnings);
    }

    for (value, program) in [
        ("cmd /K dir", "cmd"),
        (cmd_with_args.as_str(), cmd_backslash.as_str()),
    ] {
        let keep = format!("Write shell = {program:?}");
        let warnings = shell_warns(&authored(value));
        assert!(
            warnings
                .iter()
                .any(|w| w.contains(&keep) && w.contains("shell_args")),
            "{value:?} must say {keep:?} and point at shell_args, got {warnings:?}"
        );
    }

    let missing_spaced = format!("{}\\Program Files\\Git\\bin\\nosuch.exe", root.display());
    let spaced_warnings = shell_warns(&authored(&missing_spaced));
    assert!(
        spaced_warnings.iter().any(|w| w.contains("does not exist"))
            && !spaced_warnings
                .iter()
                .any(|w| w.contains("carries arguments")),
        "a missing path that merely holds a space is not a command line: {spaced_warnings:?}"
    );
    rejections.extend(spaced_warnings);

    let stem = cmd_backslash.trim_end_matches(".exe").to_string();
    let ext_warnings = shell_warns(&authored(&stem));
    assert!(
        ext_warnings
            .iter()
            .any(|w| w.contains("appends no extension") && w.contains("cmd.exe")),
        "{stem:?} must be rejected by offering the .exe spelling, got {ext_warnings:?}"
    );
    rejections.extend(ext_warnings);

    for warning in &rejections {
        assert!(
            !warning.contains("/bin/") && !warning.contains("/usr/"),
            "Windows advice must never name a POSIX path: {warning:?}"
        );
        assert!(
            !warning.contains("bare name"),
            "a Windows bare name is legal and must not be called out as the defect: {warning:?}"
        );
    }

    assert_eq!(
        super::windows_shell_command_line_head("cmd /K dir"),
        Some("cmd")
    );
    for not_a_command_line in [
        "cmd",
        "cmd ",
        " ",
        spaced.as_str(),
        "aterm-no-such-shell-xyz -l",
        "%COMSPEC% /K dir",
    ] {
        assert_eq!(
            super::windows_shell_command_line_head(not_a_command_line),
            None,
            "{not_a_command_line:?} is one program's name, not a command line"
        );
    }

    assert_eq!(
        super::unexpanded_windows_env_var("%COMSPEC%"),
        Some("COMSPEC")
    );
    assert_eq!(
        super::unexpanded_windows_env_var("%ProgramFiles(x86)%\\sh.exe"),
        Some("ProgramFiles(x86)")
    );
    for literal in ["50%.exe", "100% done\\sh.exe", "%%", "C:\\a%b\\sh.exe"] {
        assert_eq!(
            super::unexpanded_windows_env_var(literal),
            None,
            "{literal:?} holds no environment-variable reference"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn cli_validation_rejects_the_same_oversized_generation_as_runtime_and_manual() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-config-validation-limit-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(crate::native_config_service::MAX_CONFIG_FILE_BYTES as u64 + 1)
        .unwrap();

    let (message, valid) = validate_config_path(&path);
    assert!(!valid, "{message}");
    assert!(
        validate_config_text(&" ".repeat(crate::native_config_service::MAX_CONFIG_FILE_BYTES + 1),)
            .is_err()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cli_validation_verdict_says_the_file_loads_and_counts_the_warnings() {
    let dir = std::env::temp_dir().join(format!(
        "aterm-config-validation-verdict-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, "qqq_unknown = 1\n");
    let (message, valid) = validate_config_path(&path);
    assert!(valid, "{message}");
    assert!(message.contains("qqq_unknown"), "{message}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn validate_surfaces_toy_packs_that_runtime_would_skip() {
    let mut paths = vec!["/aterm-test/definitely-missing-pack.toml".to_string()];
    paths.extend((1..=8).map(|i| format!("/aterm-test/missing-pack-{i}.toml")));
    let quoted = paths
        .iter()
        .map(|path| format!("{path:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let warnings = validate_config_text(&format!("[sparkle_words]\ntoy_packs = [{quoted}]\n"))
        .expect("structurally valid config");
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("only the first 8 load"))
    );
    assert!(
        warnings
            .iter()
            .any(|warning| { warning.contains("toy_packs[0]") && warning.contains("unreadable") })
    );
}

#[test]
fn validate_flags_bad_keybind_and_key_sequence_entries() {
    let cfg = r#"
[key_sequences]
"shift+entr" = "hi"
[keybindings]
"cmd+k" = "no_such_action"
"#;
    let warnings = validate_config_text(cfg).expect("structurally valid TOML");
    let joined = warnings.join("\n");
    assert!(joined.contains("shift+entr"), "bad chord flagged: {joined}");
    assert!(
        joined.contains("no_such_action"),
        "unknown action flagged: {joined}"
    );
    assert_eq!(warnings.len(), 2, "exactly the two offenders: {joined}");
}

#[test]
fn validate_flags_shadow_and_collision() {
    let cfg = r#"
[key_sequences]
"cmd+t" = "\n"
[keybindings]
"cmd+c" = "find"
"cmd+t" = "new_tab"
"#;
    let joined = validate_config_text(cfg)
        .expect("structurally valid TOML")
        .join("\n");
    let suite_live = crate::app_input::HARDCODED_SUPER_CHORDS;
    assert_eq!(
        joined.contains("conflicts with built-in Copy"),
        suite_live,
        "{joined}"
    );
    assert_eq!(
        joined.contains("conflicts with built-in New Terminal Tab"),
        suite_live,
        "{joined}"
    );
    assert!(joined.contains("also bound in [keybindings]"), "{joined}");
}

#[test]
fn validate_accepts_the_unbind_spellings() {
    let cfg = r#"
[keybindings]
"f11" = "none"
"ctrl+tab" = "unbind"
"#;
    let warnings = validate_config_text(cfg).expect("structurally valid TOML");
    assert!(
        warnings.is_empty(),
        "unbind entries are not unknown actions: {warnings:?}"
    );
}

#[test]
fn validate_flags_empty_key_sequence() {
    let joined = validate_config_text("[key_sequences]\n\"ctrl+g\" = \"\"\n")
        .expect("structurally valid TOML")
        .join("\n");
    assert!(
        joined.contains("empty value would silently disable"),
        "empty key_sequences value flagged: {joined}"
    );
}

#[test]
fn validate_flags_font_theme_cursor_and_color_typos() {
    let joined = validate_config_text(
        "font_family = \"definitely-not-a-real-font-xyzzy\"\n\
         theme = \"NoSuchTheme\"\n\
         cursor_style = \"blok\"\n\
         foreground = \"#zzz\"\n\
         selection_foreground = \"12345\"\n\
         palette = [\"#102030\", \"nope\"]\n",
    )
    .expect("structurally valid TOML")
    .join("\n");
    assert!(
        joined.contains("does not resolve to a font file"),
        "{joined}"
    );
    assert!(joined.contains("NoSuchTheme"), "{joined}");
    assert!(joined.contains("cursor_style"), "{joined}");
    assert!(joined.contains("foreground: expected #RRGGBB"), "{joined}");
    assert!(
        joined.contains("selection_foreground: expected #RRGGBB"),
        "{joined}"
    );
    assert!(joined.contains("palette[1]"), "{joined}");
    assert!(
        !joined.contains("palette[0]"),
        "valid entry not flagged: {joined}"
    );

    let clean = validate_config_text(
        "theme = \"Default\"\ncursor_style = \"bar\"\nforeground = \"#aabbcc\"\n\
         minimum_contrast = 4.5\nline_height = 1.2\n",
    )
    .expect("valid");
    assert!(clean.is_empty(), "clean config must not warn: {clean:?}");

    for (source, token) in [
        ("theme = \"dark:Nord,lite:Dracula\"\n", "\"lite\""),
        ("theme = \"dark:,light:\"\n", "dark"),
    ] {
        let warnings = validate_config_text(source).expect("valid");
        assert!(
            warnings.len() == 1 && warnings[0].contains(token),
            "{source}: {warnings:?}"
        );
    }

    let native = validate_config_text("font_hinting = \"native\"\n")
        .expect("the retired hinting spelling remains loadable");
    assert_eq!(native.len(), 1, "{native:?}");
    assert!(native[0].contains("\"light\""), "{native:?}");
}

#[test]
fn a_shift_composed_chord_is_reported_rather_than_accepted_in_silence() {
    for (chord, glyph, base) in [
        ("ctrl+shift+?", '?', '/'),
        ("cmd+shift+}", '}', ']'),
        ("ctrl+_", '_', '-'),
        ("alt+:", ':', ';'),
    ] {
        let warning = super::keybinding_row_warning(chord, "next_tab", None)
            .unwrap_or_else(|| panic!("{chord} can never fire and must say so"));
        assert!(
            warning.message.contains(chord),
            "the warning names the row: {}",
            warning.message
        );
        assert!(
            warning.message.contains(glyph),
            "...and the glyph that cannot arrive: {}",
            warning.message
        );
        assert!(
            warning.message.contains(base),
            "...and the spelling that works: {}",
            warning.message
        );
    }

    for ok in [
        "ctrl+shift+t",
        "cmd+1",
        "ctrl+shift+right",
        "alt+enter",
        "cmd+=",
    ] {
        assert!(
            super::keybinding_row_warning(ok, "next_tab", None).is_none(),
            "{ok} is reachable and must not warn"
        );
    }
}

#[test]
fn validate_refuses_to_green_light_a_config_whose_key_is_misspelled() {
    let joined = validate_config_text("cursor_trail_stlye = \"rainbow\"\n")
        .expect("structurally valid TOML")
        .join("\n");
    assert!(
        joined.contains("cursor_trail_stlye")
            && joined.contains("did you mean \"cursor_trail_style\""),
        "a misspelled key must be reported with its spelling: {joined}"
    );
    assert!(
        validate_config_text("cursor_trail_style = \"rainbow\"\n")
            .expect("valid")
            .is_empty(),
        "the correct spelling must still validate green"
    );
}

#[test]
fn validate_refuses_to_green_light_a_value_outside_the_vocabulary_it_accepts() {
    for (source, key, intended) in [
        ("window_theme = \"drak\"\n", "window_theme", "dark"),
        ("motion = \"redcued\"\n", "motion", "reduced"),
        ("right_click = \"of\"\n", "right_click", "off"),
    ] {
        let warnings = validate_config_text(source).expect("structurally valid TOML");
        let joined = warnings.join("\n");
        assert!(
            joined.contains(key) && joined.contains(&format!("did you mean {intended:?}")),
            "{key} must name {intended}: {joined}"
        );
    }
    for accepted in ["window_theme = \"dark\"\n", "right_click = \"off\"\n"] {
        let warnings = validate_config_text(accepted).expect("valid");
        assert!(warnings.is_empty(), "{accepted:?} warned: {warnings:?}");
    }
    for (source, key) in [
        ("cursor_style = \"blok\"\n", "cursor_style"),
        ("cursor_trail_style = \"phasr\"\n", "cursor_trail_style"),
    ] {
        let warnings = validate_config_text(source).expect("structurally valid TOML");
        assert_eq!(
            warnings.iter().filter(|line| line.starts_with(key)).count(),
            1,
            "{key} must be explained once, not twice: {warnings:?}"
        );
    }
}

#[test]
fn validate_flags_unknown_cursor_trail_style() {
    let joined = validate_config_text("cursor_trail_style = \"phasr\"\n")
        .expect("structurally valid TOML")
        .join("\n");
    assert!(
        joined.contains("cursor_trail_style") && joined.contains("phasr"),
        "typo'd style must be flagged: {joined}"
    );
    for ok in ["phaser", "off", "Rainbow", "fire", "classic mono"] {
        let clean =
            validate_config_text(&format!("cursor_trail_style = \"{ok}\"\n")).expect("valid");
        assert!(clean.is_empty(), "{ok:?} must validate green: {clean:?}");
    }
    // A retired name is flagged with the name it became.
    for (old, now) in [("embers", "fire"), ("ocean", "water"), ("nyan", "rainbow")] {
        let joined = validate_config_text(&format!("cursor_trail_style = \"{old}\"\n"))
            .expect("valid")
            .join("\n");
        assert!(
            joined.contains(old) && joined.contains(&format!("cursor_trail_style = \"{now}\"")),
            "{old:?}: {joined}"
        );
    }
}

#[test]
fn validate_flags_unknown_trail_sound_style() {
    let joined = validate_config_text("trail_sound_style = \"glas bel\"\n")
        .expect("structurally valid TOML")
        .join("\n");
    assert!(
        joined.contains("trail_sound_style") && joined.contains("glas bel"),
        "typo'd typing sound must be flagged: {joined}"
    );
    for ok in crate::prefs::TRAIL_SOUND_STYLES.iter().copied().chain([
        "Glass Bell",
        "water",
        "mech",
        "thock",
        "raindrop",
        " felt ",
    ]) {
        let clean =
            validate_config_text(&format!("trail_sound_style = \"{ok}\"\n")).expect("valid");
        assert!(
            clean.iter().all(|w| !w.contains("is not a typing sound")),
            "{ok:?} must validate green: {clean:?}"
        );
    }
}

fn sample() -> DiagInfo {
    DiagInfo {
        version: "0.3.0".into(),
        git_commit: "abc1234",
        build_time: "2026-06-23T00:00:00Z",
        target_os: "macos",
        target_arch: "aarch64",
        profile: "release",
        compiler:
            "rustc 1.96.0 (ac68faa2) \u{00b7} rust \u{00b7} release \u{00b7} trust_verify off"
                .into(),
        update_pin_sha256: "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925"
            .to_string(),
        renderer_default: "cpu",
        shell_integration_runtime: "active (Zsh loader prepared)".into(),
        agent_primer: "claude installed, codex not detected — auto-prime: on \
                       (agents_auto_prime)"
            .into(),
        privacy: "full_disk_access=unknown probe=none fda_scope=unknown \
                  warmup=on-request folders=documents,desktop,downloads hold_ms=120000 \
                  probe_interval_ms=5000 protected_roots=17 notice=on report_attribution=on"
            .into(),
        debugger: "mode=unknown member=unknown self_hardened=yes \
                   right=system.privilege.taskport.debug probe=none"
            .into(),
        capabilities: vec![("kitty_graphics", true), ("soft_fonts", false)],
        objc_protocols_absent: vec!["NSApplicationDelegate"],
        config_path: "/home/u/.config/aterm/aterm.toml".into(),
        config_presence: ConfigPresence::Absent,
        env: vec![("ATERM_CHILD".into(), "1".into())],
    }
}

fn rejected_invalid() -> String {
    crate::app_config::launch_config_notice(
        std::path::Path::new("/home/u/.config/aterm/aterm.toml"),
        crate::app_config::LaunchConfigProblem::Invalid(
            &"TOML parse error at line 1, column 11\n  |\n1 | font_px = \n  |           \
              ^\nexpected a value",
        ),
    )
}

#[test]
fn diagnose_says_a_rejected_config_is_not_the_one_in_force() {
    let mut info = sample();
    info.config_presence = ConfigPresence::Rejected(rejected_invalid());
    let r = info.render();
    assert!(
        r.contains("REJECTED"),
        "the rejected state has its own token: {r}"
    );
    assert!(
        !r.contains("[present]"),
        "the in-force token must never stand for a rejected file: {r}"
    );
    assert!(
        r.contains("expected a value"),
        "the launch's reason, every line of it, is carried onto stdout: {r}"
    );
    let mut loaded = sample();
    loaded.config_presence = ConfigPresence::Loaded;
    assert!(loaded.render().contains("[present]"));
}

#[test]
fn show_config_marks_a_rejected_config_and_its_listing_as_defaults() {
    let notice = crate::app_config::launch_config_notice(
        std::path::Path::new("/home/u/.config/aterm/aterm.toml"),
        crate::app_config::LaunchConfigProblem::Unreadable(&"Permission denied (os error 13)"),
    );
    let out = show_config_report(Some(&notice));
    assert!(
        out.contains("REJECTED"),
        "the header names the third state: {out}"
    );
    assert!(
        !out.contains("[present]") && !out.contains("[absent"),
        "neither honest-file token may stand in for a rejected one: {out}"
    );
    assert!(
        out.contains("os error 13"),
        "the reason reaches stdout: {out}"
    );
    assert!(out.contains("font_px:"), "the resolved values still print");
}

#[test]
fn the_privacy_line_reports_unknown_from_the_inert_arm_and_never_probes() {
    use aterm_containment::{FdaProbe, FdaState, ProbeLabel};

    let config = parsed(&format!(
        "[privacy]\nprotected_roots = [\"{ABS_TMP}/one\", \"{ABS_TMP}/two\"]\n"
    ));
    let inert = privacy_line(&config, None);
    for token in [
        "full_disk_access=unknown",
        "probe=none",
        "fda_scope=unknown",
        "warmup=automatic",
        "hold_ms=120000",
        "probe_interval_ms=5000",
        "protected_roots=2",
        "notice=on",
        "report_attribution=on",
    ] {
        assert!(inert.contains(token), "{token} missing from {inert}");
    }

    let granted = FdaProbe {
        state: FdaState::Granted,
        label: ProbeLabel::OpenOk,
    };
    let live = privacy_line(&config, Some(granted));
    assert!(
        live.contains("full_disk_access=granted probe=open_ok"),
        "{live}"
    );
    assert!(
        live.contains("fda_scope=this_process"),
        "a completed grant establishes the observing host's scope, and this line \
         renders the same rule the privacy verb does: {live}"
    );

    let unchecked = privacy_line(&parsed("[privacy]\ncheck = false\n"), Some(granted));
    assert!(
        unchecked.contains("full_disk_access=unknown probe=off"),
        "{unchecked}"
    );

    let disabled = privacy_line(&parsed("[privacy]\nenabled = false\n"), Some(granted));
    assert!(
        disabled.starts_with("off ([privacy] enabled = false)")
            && disabled.contains("reads unknown"),
        "{disabled}"
    );

    assert!(
        sample()
            .render()
            .contains("privacy:   full_disk_access=unknown probe=none"),
        "the section is rendered under its own label"
    );
    let live_collect = collect().privacy;
    assert!(
        !live_collect.contains("probe=open_"),
        "collect() must never perform the probe: {live_collect}"
    );
    assert!(
        live_collect.starts_with("off ([privacy]")
            || live_collect.contains("full_disk_access=unknown"),
        "{live_collect}"
    );
}

#[test]
fn the_debugger_line_reports_unknown_from_the_inert_arm_and_never_probes() {
    use aterm_containment::consent::{
        DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel, SelfHardening,
    };

    let config = parsed("");
    let inert = debugger_line(&config, None, SelfHardening::NotHardened);
    assert_eq!(
        inert,
        "mode=unknown member=unknown self_hardened=no \
         right=system.privilege.taskport.debug probe=none"
    );

    let observed = DebuggerProbe {
        mode: DebuggerMode::Disabled,
        member: DebuggerMember::Admin,
        label: DebuggerProbeLabel::StatusOk,
    };
    let live = debugger_line(&config, Some(observed), SelfHardening::Hardened);
    assert!(
        live.starts_with("mode=disabled member=admin self_hardened=yes ")
            && live.ends_with(" probe=status_ok"),
        "{live}"
    );
    let refused = debugger_line(
        &config,
        Some(DebuggerProbe::refused(
            DebuggerProbeLabel::RefusedOutOfBundle,
        )),
        SelfHardening::Unknown,
    );
    assert!(
        refused.starts_with("mode=unknown member=unknown self_hardened=unknown ")
            && refused.ends_with(" probe=refused_out_of_bundle"),
        "{refused}"
    );

    let unchecked = debugger_line(
        &parsed("[privacy]\ncheck = false\n"),
        Some(observed),
        SelfHardening::Hardened,
    );
    assert!(
        unchecked.starts_with("mode=unknown member=unknown ") && unchecked.ends_with(" probe=off"),
        "{unchecked}"
    );

    let disabled = debugger_line(
        &parsed("[privacy]\nenabled = false\n"),
        Some(observed),
        SelfHardening::Hardened,
    );
    assert!(
        disabled.starts_with("off ([privacy] enabled = false)")
            && disabled.contains("reads unknown"),
        "{disabled}"
    );

    assert!(
        sample()
            .render()
            .contains("\ndebugger:  mode=unknown member=unknown "),
        "the section is rendered under its own label"
    );
    let live_collect = collect().debugger;
    assert!(
        live_collect.starts_with("off ([privacy]")
            || (live_collect.starts_with("mode=unknown member=unknown ")
                && (live_collect.ends_with(" probe=none") || live_collect.ends_with(" probe=off"))),
        "collect() must never perform the probe: {live_collect}"
    );
}

#[test]
fn agent_primer_line_states_the_knob_as_configured() {
    let on = agent_primer_line(&crate::app_config::Config::default());
    assert!(on.contains("auto-prime: on"), "{on}");
    let config = crate::app_config::Config {
        agents_auto_prime: Some(false),
        ..crate::app_config::Config::default()
    };
    let off = agent_primer_line(&config);
    assert!(off.contains("auto-prime: off"), "{off}");
}

fn theme_list_scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aterm-theme-list-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn tsv_rows(out: &str) -> Vec<Vec<&str>> {
    out.lines()
        .skip(1)
        .filter(|line| !line.starts_with('#'))
        .map(|line| line.split('\t').collect())
        .collect()
}

fn tsv_field<'a>(row: &[&'a str], column: &str) -> &'a str {
    let index = THEME_LIST_TSV_COLUMNS
        .iter()
        .position(|c| *c == column)
        .expect("a listing column");
    row[index]
}

#[test]
fn catalog_theme_is_the_catalogs_view_of_a_theme() {
    let dir = theme_list_scratch("entry");
    std::fs::write(
        dir.join("Paper.conf"),
        "name = Paper Day\nauthor = Me\nforeground = #202020\nbackground = #f4f4f0\n",
    )
    .unwrap();
    std::fs::write(dir.join("Broken.conf"), "foreground = #ffffff\n").unwrap();
    let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);

    let paper = catalog_theme(&catalog, "paper").expect("a loaded file");
    assert_eq!(paper.id, "Paper");
    assert_eq!(paper.meta.display_name, "Paper Day");
    assert_eq!(paper.theme.author.as_deref(), Some("Me"));
    assert!(!paper.theme.appearance_declared);
    assert_eq!(paper.meta.appearance, aterm_types::Appearance::Light);
    assert_eq!(
        paper.theme.scheme.appearance,
        aterm_types::Appearance::Light
    );
    assert_eq!(
        theme_measured_appearance(paper.theme.scheme.background),
        aterm_types::Appearance::Light
    );

    let broken = catalog_theme(&catalog, "Broken").expect_err("did not load");
    assert!(broken.contains("Broken"), "{broken}");
    assert!(catalog_theme(&catalog, "Absent").is_err());
    let nord = theme_catalog_entry("nord").expect("a built-in");
    assert_eq!(nord.id, "Nord");
    assert!(nord.theme.appearance_declared);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_theme_list_has_one_row_per_theme_default_first() {
    let dir = theme_list_scratch("rows");
    let theme = "foreground = #e0e0e0\nbackground = #101010\n";
    std::fs::write(dir.join("Work.conf"), format!("name = Work Night\n{theme}")).unwrap();
    std::fs::write(dir.join("Broken.conf"), "foreground = #ffffff\n").unwrap();
    std::fs::write(dir.join("Nord.conf"), theme).unwrap();
    let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);
    let builtins = aterm_types::scheme::builtin_theme_list();
    let expected = builtins.len() + catalog.listing().count();
    assert_eq!(catalog.listing().count(), 3, "every file has a catalog row");

    let tsv = theme_list(&catalog, Some(&dir), None, ThemeListFormat::Tsv);
    assert_eq!(
        tsv.lines().next(),
        Some(THEME_LIST_TSV_COLUMNS.join("\t").as_str()),
        "the header names the columns"
    );
    let rows = tsv_rows(&tsv);
    assert_eq!(rows.len(), expected, "{tsv}");
    assert_eq!(tsv_field(&rows[0], "id"), "Default", "{tsv}");
    for (row, builtin) in rows.iter().zip(&builtins) {
        assert_eq!(tsv_field(row, "id"), builtin.name);
        assert_eq!(tsv_field(row, "source"), "builtin");
        assert_eq!(tsv_field(row, "pair"), builtin.pair.unwrap_or_default());
    }
    let file = |id: &str| {
        rows.iter()
            .find(|row| tsv_field(row, "id") == id && tsv_field(row, "source") == "file")
            .unwrap_or_else(|| panic!("no file row {id}: {tsv}"))
    };
    let work = file("Work");
    assert_eq!(tsv_field(work, "label"), "Work Night");
    assert_eq!(tsv_field(work, "appearance"), "dark");
    assert_eq!(tsv_field(work, "appearance_source"), "measured");
    assert_eq!(tsv_field(work, "status"), "ready");
    let broken = file("Broken");
    assert_eq!(tsv_field(broken, "status"), "invalid");
    assert!(!tsv_field(broken, "notes").is_empty(), "a refusal says why");
    assert_eq!(tsv_field(broken, "appearance"), "");
    let nord = file("Nord");
    assert_eq!(tsv_field(nord, "status"), "shadowed");
    assert!(!tsv_field(nord, "notes").is_empty());
    assert_eq!(
        tsv.lines().last(),
        Some(format!("#folder\t{}", dir.display()).as_str()),
        "the folder closes the listing"
    );

    let text = theme_list(&catalog, Some(&dir), None, ThemeListFormat::Text);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[1].starts_with("ID "), "{text}");
    let row_ids: Vec<&str> = lines[2..]
        .iter()
        .take_while(|line| !line.is_empty())
        .filter(|line| !line.starts_with(' '))
        .map(|line| line.split("  ").next().unwrap_or_default())
        .collect();
    let tsv_ids: Vec<&str> = rows.iter().map(|row| tsv_field(row, "id")).collect();
    assert_eq!(row_ids, tsv_ids, "{text}");
    let broken_at = lines
        .iter()
        .position(|line| line.starts_with("Broken "))
        .expect("broken row");
    assert!(
        lines[broken_at + 1].starts_with("    ") && lines[broken_at + 1].len() > 8,
        "the reason follows its row: {text}"
    );
    assert!(text.contains(&dir.display().to_string()), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_theme_list_names_typography_and_skipped_keys() {
    let dir = theme_list_scratch("typography");
    let base = "foreground = #e0e0e0\nbackground = #101010\n";
    std::fs::write(
        dir.join("Studio.conf"),
        format!("{base}font_family = Berkeley Mono\nline_height = 1.15\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("Future.conf"),
        format!("schema = 2\n{base}future_key = x\n"),
    )
    .unwrap();
    std::fs::write(dir.join("Plain.conf"), base).unwrap();
    let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);
    let tsv = theme_list(&catalog, Some(&dir), None, ThemeListFormat::Tsv);
    let rows = tsv_rows(&tsv);
    let row = |id: &str| {
        rows.iter()
            .find(|row| tsv_field(row, "id") == id)
            .unwrap_or_else(|| panic!("{id}: {tsv}"))
    };
    assert!(tsv_field(row("Studio"), "typography").contains("Berkeley Mono"));
    assert_eq!(tsv_field(row("Studio"), "skipped"), "0");
    assert_eq!(tsv_field(row("Future"), "skipped"), "1");
    assert_eq!(tsv_field(row("Future"), "status"), "ready");
    assert!(tsv_field(row("Future"), "notes").contains("future_key"));
    assert_eq!(tsv_field(row("Plain"), "typography"), "");
    let text = theme_list(&catalog, Some(&dir), None, ThemeListFormat::Text);
    assert!(
        text.lines().nth(1).is_some_and(|h| h.contains("TYPE")),
        "{text}"
    );
    let future_at = text
        .lines()
        .position(|line| line.starts_with("Future "))
        .expect("the Future row");
    assert!(
        text.lines()
            .nth(future_at + 1)
            .is_some_and(
                |line| line.trim_start().starts_with("warning:") && line.contains("future_key")
            ),
        "{text}"
    );
    let wire = theme_list_wire_rows(&catalog, Some(&dir), "Default");
    let studio = wire
        .iter()
        .find(|line| line.contains("id=\"Studio\""))
        .expect("Studio's row");
    assert!(studio.contains("type=\"Berkeley Mono"), "{studio}");
    assert!(studio.ends_with("skipped=0"), "{studio}");
    let plain = wire
        .iter()
        .find(|line| line.contains("id=\"Plain\""))
        .expect("Plain's row");
    assert!(plain.contains("type=- "), "{plain}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_tsv_row_has_the_header_column_count() {
    let dir = theme_list_scratch("tsv");
    let hostile = "Tab\there\u{1b}[31m";
    if cfg!(unix) {
        std::fs::write(
            dir.join(format!("{hostile}.conf")),
            "foreground = #e0e0e0\nbackground = #101010\n",
        )
        .unwrap();
    }
    std::fs::write(dir.join("Plain.conf"), "nonsense\twith\ttabs\n").unwrap();
    let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);
    let tsv = theme_list(&catalog, Some(&dir), None, ThemeListFormat::Tsv);
    let rows = tsv_rows(&tsv);
    assert!(rows.len() > aterm_types::scheme::builtin_theme_list().len());
    for row in &rows {
        assert_eq!(row.len(), THEME_LIST_TSV_COLUMNS.len(), "{row:?}");
    }
    assert!(
        !tsv.contains('\u{1b}'),
        "no raw escape reaches the terminal"
    );
    assert!(hostile.split('\t').count() > 1, "negative control");
    assert_eq!(listing_field(hostile).split('\t').count(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_appearance_filter_keeps_only_that_side() {
    let dir = theme_list_scratch("side");
    std::fs::write(
        dir.join("Paper.conf"),
        "foreground = #202020\nbackground = #fafafa\n",
    )
    .unwrap();
    std::fs::write(dir.join("Broken.conf"), "foreground = #ffffff\n").unwrap();
    let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);
    let all = tsv_rows(&theme_list(&catalog, None, None, ThemeListFormat::Tsv)).len();
    let mut kept = 0;
    for (side, word) in [
        (aterm_types::Appearance::Light, "light"),
        (aterm_types::Appearance::Dark, "dark"),
    ] {
        let tsv = theme_list(&catalog, None, Some(side), ThemeListFormat::Tsv);
        let rows = tsv_rows(&tsv);
        assert!(!rows.is_empty());
        for row in &rows {
            assert_eq!(tsv_field(row, "appearance"), word, "{tsv}");
        }
        kept += rows.len();
    }
    assert_eq!(kept + 1, all, "only the unloaded file drops out");
    let light = theme_list(
        &catalog,
        None,
        Some(aterm_types::Appearance::Light),
        ThemeListFormat::Tsv,
    );
    assert!(light.lines().any(|l| l.starts_with("Paper\t")), "{light}");
    assert!(
        !light.lines().any(|l| l.starts_with("Default\t")),
        "{light}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_truncated_folder_says_so_in_both_formats() {
    let dir = theme_list_scratch("cap");
    let theme = "foreground = #e0e0e0\nbackground = #101010\n";
    std::fs::write(dir.join("Only.conf"), theme).unwrap();
    let small = crate::app_config::ThemeCatalog::discover_in(&dir);
    let tsv = theme_list(&small, Some(&dir), None, ThemeListFormat::Tsv);
    assert!(!tsv.lines().any(|l| l.starts_with("#truncated")), "{tsv}");
    let cap = crate::app_config::MAX_USER_THEME_FILES;
    for index in 0..=cap {
        std::fs::write(dir.join(format!("Theme-{index:03}.conf")), theme).unwrap();
    }
    let catalog = crate::app_config::ThemeCatalog::discover_in(&dir);
    let tsv = theme_list(&catalog, Some(&dir), None, ThemeListFormat::Tsv);
    let trailer: Vec<&str> = tsv.lines().last().unwrap_or_default().split('\t').collect();
    assert_eq!(trailer[0], "#truncated", "{tsv}");
    assert_eq!(trailer[1], cap.to_string());
    assert_eq!(trailer[2], (cap + 2).to_string());
    let text = theme_list(&catalog, Some(&dir), None, ThemeListFormat::Text);
    let last = text.lines().last().unwrap_or_default();
    assert!(last.contains(&cap.to_string()) && last.contains(&(cap + 2).to_string()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn user_keybinding_note_labels_unbinds_and_unknowns() {
    assert_eq!(
        user_keybinding_note_for("none", Some("Copy")),
        "  (unbinds a default; still conflicts with Copy)",
        "an unbind over a live built-in cannot imply the chord is free"
    );
    assert_eq!(
        user_keybinding_note_for("unbind", Some("Next Tab")),
        "  (unbinds a default; still conflicts with Next Tab)",
        "both unbind spellings keep the caveat"
    );
    assert_eq!(
        user_keybinding_note_for("none", None),
        "  (unbinds a default)",
        "with no built-in claiming the chord there is nothing to add"
    );
    assert_eq!(
        user_keybinding_note_for("no_such_action", Some("Copy")),
        "  (UNKNOWN action)"
    );
    assert_eq!(
        user_keybinding_note_for("copy", Some("Copy")),
        "  (conflicts with Copy)"
    );
}

#[test]
fn an_unbind_over_a_live_builtin_keeps_the_conflict_caveat() {
    let unbind = keybinding_row_warning("cmd+c", "none", Some("Copy"))
        .expect("an unbind cannot swallow the built-in it is unable to mask");
    assert_eq!(unbind.key, "keybindings");
    assert!(
        unbind.message.contains("unbinds a default")
            && unbind.message.contains("conflicts with built-in Copy"),
        "{unbind:?}"
    );
    assert!(
        !unbind.message.contains("unknown action"),
        "the documented unbind spelling is still not an unknown action: {unbind:?}"
    );

    assert!(keybinding_row_warning("cmd+c", "none", None).is_none());
    assert!(keybinding_row_warning("f11", "unbind", None).is_none());

    assert!(
        keybinding_row_warning("cmd+c", "no_such_action", Some("Copy"))
            .expect("unknown action")
            .message
            .contains("unknown action \"no_such_action\"")
    );
    assert!(
        keybinding_row_warning("cmd+c", "find", Some("Copy"))
            .expect("plain shadow")
            .message
            .contains("conflicts with built-in Copy")
    );
    assert!(
        keybinding_row_warning("shift+entr", "none", Some("Copy"))
            .expect("an invalid chord outranks every later arm")
            .message
            .contains("invalid")
    );
}

#[test]
fn show_config_names_the_shell_a_new_tab_starts() {
    use aterm_pty::ShellOrigin as O;
    let out = show_config();
    let config = crate::app_config::load_config();
    let chosen = crate::app_config::resolve_shell_override(&config);
    let (spawned, _) =
        aterm_pty::select_shell_with_origin(chosen.as_deref().map(std::ffi::OsStr::new));
    let spawned = spawned.to_string_lossy().into_owned();
    let (program, origin) = show_config_shell(&config);
    assert!(!program.is_empty());
    assert_eq!(program, spawned, "the row is the program the spawn starts");
    assert!(
        out.contains(&format!("\nshell:          {program}\n")),
        "{out}"
    );
    assert!(
        out.contains(&format!("\nshell_origin:   {origin}\n")),
        "{out}"
    );
    assert_eq!(shell_origin_token(O::Override, true), "--shell");
    assert_eq!(shell_origin_token(O::Override, false), "aterm.toml");
    #[cfg(unix)]
    {
        assert_eq!(shell_origin_token(O::Env, false), "default:SHELL");
        assert_eq!(shell_origin_token(O::Passwd, false), "default:passwd");
        assert_eq!(shell_origin_token(O::Default, true), "default:/bin/sh");
    }
}

#[test]
fn show_config_names_the_console_host_on_windows_only() {
    let out = show_config();
    let row = out.lines().find(|l| l.starts_with("console_host:"));
    #[cfg(windows)]
    {
        let config = crate::app_config::load_config();
        let host = aterm_pty::preview_console_host(&aterm_pty::ConsoleHostChoice::from_config(
            config.conpty.as_deref(),
        ));
        assert!(!host.line.is_empty());
        let want = format!("console_host:   {}", host.line);
        assert_eq!(row, Some(want.as_str()), "{out}");
        assert!(
            out.find("\nshell_origin:") < out.find("\nconsole_host:"),
            "the host follows the shell it hosts: {out}"
        );
    }
    #[cfg(not(windows))]
    assert_eq!(row, None, "{out}");
}

#[test]
fn show_config_type_rows_name_their_sources() {
    let parse = |extra: &str| {
        aterm_types::scheme::parse_theme_str(&format!(
            "foreground = #e0e0e0\nbackground = #101418\n{extra}"
        ))
        .expect("theme")
    };
    let themes = crate::app_config::ThemeCatalog::from_parsed([
        (
            "Studio".to_string(),
            parse("font_family = display:pixel\nline_height = 1.25\nfont_size_scale = 1.1"),
        ),
        ("Plain".to_string(), parse("")),
    ]);
    let admit = |_: Option<&str>| Ok(None);
    let rows = |text: &str, flag: Option<&str>| {
        let config: crate::app_config::Config = aterm_toml::from_str(text).expect("toml");
        show_config_type_rows(&config, &themes, flag, admit)
    };
    let themed = rows("theme = \"Studio\"\nfont_px = 20.0\n", None);
    let default = aterm_render::default_primary_identity().map_or_else(
        || "the embedded last-resort face".to_string(),
        |face| face.path,
    );
    assert_eq!(
        themed.font_family,
        format!("(default) {default} with display faces pixel  (source: theme Studio)")
    );
    assert_eq!(themed.line_height, "1.25  (source: theme Studio)");
    assert_eq!(themed.font_size_scale, "1.1  (source: theme Studio)");
    assert_eq!(themed.font_px, [22.0, 22.0], "the scale applies to font_px");
    let mine = rows("theme = \"Studio\"\nline_height = 1.0\n", Some("Menlo"));
    assert_eq!(mine.font_family, "Menlo  (source: --font)");
    assert_eq!(mine.line_height, "1  (source: config)");
    let split = rows("theme = \"dark:Studio,light:Plain\"\n", None);
    assert_eq!(
        split.line_height,
        format!(
            "1.25 (dark, theme Studio) / {} (light, default)",
            crate::app_config::DEFAULT_LINE_HEIGHT
        )
    );
    let plain = rows("theme = \"Plain\"\n", None);
    assert!(
        plain.font_family.ends_with("(source: default)"),
        "{}",
        plain.font_family
    );
    let default = crate::app_config::DEFAULT_LINE_HEIGHT.to_string();
    assert_eq!(plain.line_height, format!("{default}  (source: default)"));
    let nan = rows("theme = \"Plain\"\nline_height = nan\n", None);
    assert_eq!(
        nan.line_height,
        format!("{default}  (source: default)"),
        "a non-finite config line height falls back to the platform default"
    );
    let themed_nan = rows("theme = \"Studio\"\nline_height = nan\n", None);
    assert_eq!(
        themed_nan.line_height, "1.25  (source: theme Studio)",
        "a non-finite config line height leaves the theme's in force"
    );
}

#[test]
fn show_config_colour_source_names_theme_and_overrides() {
    let themes = crate::app_config::ThemeCatalog::default();
    let config: crate::app_config::Config = aterm_toml::from_str(
        "theme = \"dark:Dracula,light:no-such-theme\"\nforeground = \"#102030\"\n\
         palette = [\"#010203\", \"nope\"]",
    )
    .unwrap();
    let [dark, light] = [
        aterm_types::Appearance::Dark,
        aterm_types::Appearance::Light,
    ]
    .map(|appearance| {
        super::color_source_label(&config.resolved_colors(appearance, &themes, None))
    });
    assert!(dark.starts_with("Dracula (built-in)"), "{dark}");
    assert!(light.starts_with("Default (fallback: "), "{light}");
    for side in [&dark, &light] {
        assert!(
            side.contains("config sets foreground, palette[0]"),
            "{side}"
        );
        assert!(side.contains("ignored palette[1]"), "{side}");
    }
    let row = super::appearance_labels(dark.clone(), light.clone());
    assert!(
        row.contains(" (dark) / ") && row.ends_with(" (light)"),
        "{row}"
    );
    let plain = super::color_source_label(&crate::app_config::Config::default().resolved_colors(
        aterm_types::Appearance::Dark,
        &themes,
        None,
    ));
    assert_eq!(plain, "Default (built-in)");
}

#[test]
fn renderer_label_tracks_resolved_gpu_precedence() {
    use crate::app_config::resolve_want_gpu_with;
    assert_eq!(
        renderer_label(resolve_want_gpu_with(true, true, Some(true))),
        "cpu"
    );
    assert_eq!(
        renderer_label(resolve_want_gpu_with(false, true, Some(false))),
        GPU_BACKEND_LABEL
    );
    assert_eq!(
        renderer_label(resolve_want_gpu_with(false, false, Some(false))),
        "cpu"
    );
    assert_eq!(
        renderer_label(resolve_want_gpu_with(false, false, Some(true))),
        GPU_BACKEND_LABEL
    );
    assert_eq!(
        renderer_label(resolve_want_gpu_with(false, false, None)),
        GPU_BACKEND_LABEL
    );
}

#[test]
fn show_face_rejects_an_unresolvable_family() {
    let (msg, ok) = show_face("definitely-not-a-real-font-xyzzy");
    assert!(!ok, "unresolvable family must report failure");
    assert!(
        msg.ends_with('\n'),
        "a message is always a whole line: {msg:?}"
    );
}

#[test]
fn show_face_reports_the_default_face_for_the_generic_monospace_name() {
    let default = aterm_render::default_primary_identity().expect("a default face loads");
    let (msg, ok) = show_face("monospace");
    assert!(ok, "{msg}");
    assert!(
        msg.contains(&format!("path:        {}\n", default.path)),
        "{msg}"
    );
}
