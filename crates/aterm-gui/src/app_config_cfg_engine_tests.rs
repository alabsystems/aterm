// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    Config, KittySpriteAsset, MAX_KITTY_SPRITE_FILE_BYTES, MAX_USER_THEME_FILE_BYTES, ThemeCatalog,
    ThemeCatalogLimits, ThemeEntryStatus, ThemeScanCaps,
};
#[cfg(unix)]
use super::{ThemeCatalogWatchError, read_bounded_theme_file};
use aterm_core::config::BiDiMode;

fn cfg(toml: &str) -> Config {
    aterm_toml::from_str(toml).expect("valid toml")
}

#[test]
fn shell_override_resolver_keeps_launch_precedence_for_new_sessions() {
    assert_eq!(
        super::resolve_shell_override_with(None, Some("/bin/zsh")),
        Some("/bin/zsh".to_string())
    );
    assert_eq!(
        super::resolve_shell_override_with(
            Some("/opt/homebrew/bin/fish".to_string()),
            Some("/bin/zsh")
        ),
        Some("/opt/homebrew/bin/fish".to_string())
    );
    assert_eq!(
        super::resolve_shell_override_with(Some(String::new()), Some("/bin/zsh")),
        Some("/bin/zsh".to_string()),
        "an empty environment value does not mask the config"
    );
}

fn kitty_fixture(name: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    std::env::temp_dir().join(format!(
        "aterm-nyan-asset-{}-{}-{name}.png",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn config_with_kitty(path: &std::path::Path) -> Config {
    cfg(&format!(
        "theme = \"Nord\"\ncursor_nyan_sprite = {:?}\n",
        path.to_string_lossy()
    ))
}

fn theme_fixture_dir(name: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let dir = std::env::temp_dir().join(format!(
        "aterm-theme-catalog-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_theme(path: &std::path::Path, foreground: &str, background: &str) {
    std::fs::write(
        path,
        format!("foreground = {foreground}\nbackground = {background}\n"),
    )
    .unwrap();
}

const SMALL_FILE_CAP: ThemeScanCaps = ThemeScanCaps {
    files: 4,
    ..ThemeScanCaps::PRODUCTION
};

const _: () = assert!(
    ThemeScanCaps::PRODUCTION.files < ThemeScanCaps::PRODUCTION.candidates
        && ThemeScanCaps::PRODUCTION.candidates <= ThemeScanCaps::PRODUCTION.entries
);

fn discover_capped(dir: &std::path::Path, caps: ThemeScanCaps) -> ThemeCatalog {
    ThemeCatalog::discover_in_capped(dir, false, caps).unwrap_or_default()
}

#[test]
fn theme_catalog_is_sorted_bounded_safe_and_parse_validated() {
    let dir = theme_fixture_dir("bounded");
    let cap = SMALL_FILE_CAP.files;
    for index in (0..(cap + 2)).rev() {
        write_theme(
            &dir.join(format!("Theme-{index:03}.conf")),
            "#ddeeff",
            "#102030",
        );
    }
    let catalog = discover_capped(&dir, SMALL_FILE_CAP);
    let names = catalog.ready_names().collect::<Vec<_>>();
    assert_eq!(names.len(), cap);
    assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(names[0], "Theme-000");
    assert_eq!(names[cap - 1], format!("Theme-{:03}", cap - 1));
    assert!(catalog.truncated());
    assert_ne!(catalog.fingerprint(), 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn theme_catalog_meta_shows_name_and_keys_on_the_stem() {
    use aterm_types::Appearance;
    use aterm_types::scheme::{AppearanceSource, ThemeWarning};
    let dir = theme_fixture_dir("meta");
    std::fs::write(
        dir.join("studio.conf"),
        "name = Studio Night\ndescription = deep navy\nauthor = Jo\n\
         foreground = #c0caf5\nbackground = #1a1b26\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("paper.conf"),
        "foreground = #202020\nbackground = #fafafa\ncolor7 = #fafafa\n",
    )
    .unwrap();
    let catalog = ThemeCatalog::discover_in(&dir);
    assert_eq!(
        catalog.ready_names().collect::<Vec<_>>(),
        ["paper", "studio"]
    );

    let studio = catalog.meta("STUDIO").expect("the stem is the id");
    assert_eq!(studio.display_name, "Studio Night");
    assert_eq!(studio.description.as_deref(), Some("deep navy"));
    assert_eq!(studio.author.as_deref(), Some("Jo"));
    assert_eq!(studio.appearance, Appearance::Dark);
    assert_eq!(studio.appearance_source, AppearanceSource::Measured);
    assert!(studio.warnings.is_empty(), "{:?}", studio.warnings);
    assert!(catalog.resolve("studio").is_ok());
    assert!(catalog.resolve("Studio Night").is_err());
    assert_eq!(catalog.meta("Studio Night"), None);

    let paper = catalog.meta("paper").expect("paper loaded");
    assert_eq!(paper.display_name, "paper");
    assert_eq!(paper.appearance, Appearance::Light);
    assert_eq!(
        catalog.resolve("paper").unwrap().appearance,
        Appearance::Light
    );
    assert_eq!(
        paper.warnings,
        vec![ThemeWarning::AnsiMatchesBackground { slot: 7 }]
    );
    assert_eq!(
        catalog
            .ready_themes()
            .map(|(id, meta)| (id, meta.display_name.as_str()))
            .collect::<Vec<_>>(),
        [("paper", "paper"), ("studio", "Studio Night")]
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn theme_catalog_builtin_meta_matches_the_classifier() {
    use super::measured_appearance;
    let catalog = ThemeCatalog::empty();
    for theme in aterm_types::scheme::builtin_theme_list() {
        let scheme = aterm_types::scheme::builtin(theme.name).expect("listed");
        assert_eq!(
            measured_appearance(scheme.background),
            theme.appearance,
            "{}",
            theme.name
        );
        let meta = catalog.meta(theme.name).expect("a built-in has meta");
        assert_eq!(meta.display_name, theme.name);
        assert_eq!(meta.pair.as_deref(), theme.pair);
        assert_eq!(meta.description.as_deref(), Some(theme.description));
    }
    assert_eq!(catalog.meta("no such theme"), None);
}

#[test]
fn theme_catalog_lists_a_file_shadowing_a_builtin() {
    for (file, row, builtin) in [
        ("Nord", "Nord", "Nord"),
        ("nord", "nord", "Nord"),
        ("default", "default", "Default"),
    ] {
        let dir = theme_fixture_dir("shadow");
        write_theme(&dir.join(format!("{file}.conf")), "#010203", "#fefdfc");
        write_theme(&dir.join("Work.conf"), "#ddeeff", "#102030");
        let catalog = ThemeCatalog::discover_in(&dir);
        let listing = catalog.listing().collect::<Vec<_>>();
        assert!(
            listing.contains(&(row, ThemeEntryStatus::Shadowed(builtin))),
            "{file}: {listing:?}"
        );
        assert!(listing.contains(&("Work", ThemeEntryStatus::Ready)));
        assert_eq!(catalog.ready_names().collect::<Vec<_>>(), ["Work"]);
        assert_eq!(
            catalog.resolve(file).unwrap(),
            aterm_types::scheme::builtin(builtin).unwrap(),
            "the built-in still wins for {file}"
        );
        assert!(
            ThemeEntryStatus::Shadowed(builtin)
                .reason()
                .is_some_and(|reason| reason.contains(builtin))
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn theme_catalog_keeps_a_row_per_case_variant_shadowing_a_builtin() {
    let dir = theme_fixture_dir("shadow-twins");
    write_theme(&dir.join("Nord.conf"), "#010203", "#fefdfc");
    write_theme(&dir.join("nord.conf"), "#030201", "#fcfdfe");
    let files = std::fs::read_dir(&dir).unwrap().count();
    let catalog = ThemeCatalog::discover_in(&dir);
    let listing = catalog.listing().collect::<Vec<_>>();
    assert_eq!(listing.len(), files, "{listing:?}");
    assert!(
        listing
            .iter()
            .all(|(_, status)| *status == ThemeEntryStatus::Shadowed("Nord")),
        "{listing:?}"
    );
    assert_eq!(
        catalog.resolve("Nord").unwrap(),
        aterm_types::scheme::builtin("Nord").unwrap()
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn theme_catalog_caps_files_after_the_name_filters() {
    let dir = theme_fixture_dir("cap-after-filter");
    let cap = SMALL_FILE_CAP.files;
    write_theme(&dir.join("Nord.conf"), "#010203", "#fefdfc");
    write_theme(&dir.join("Dracula.conf"), "#010203", "#fefdfc");
    for index in 0..cap {
        write_theme(
            &dir.join(format!("Theme-{index:03}.conf")),
            "#ddeeff",
            "#102030",
        );
    }
    let catalog = discover_capped(&dir, SMALL_FILE_CAP);
    assert_eq!(catalog.ready_names().count(), cap);
    assert!(!catalog.truncated());
    assert_eq!(
        catalog.limits(),
        ThemeCatalogLimits {
            seen: cap + 2,
            kept: cap,
        }
    );
    assert_eq!(
        catalog
            .listing()
            .filter(|(_, status)| matches!(status, ThemeEntryStatus::Shadowed(_)))
            .count(),
        2
    );

    write_theme(&dir.join("Theme-999.conf"), "#ddeeff", "#102030");
    let over = discover_capped(&dir, SMALL_FILE_CAP);
    assert_eq!(over.ready_names().count(), cap);
    assert!(over.truncated());
    assert!(over.resolve("Theme-999").is_err());
    assert_eq!(over.limits().kept, cap);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn theme_catalog_past_the_entry_cap_keeps_a_prefix() {
    let caps = ThemeScanCaps {
        entries: 8,
        ..ThemeScanCaps::PRODUCTION
    };
    let dir = theme_fixture_dir("entry-cap");
    for index in 0..=caps.entries {
        std::fs::write(
            dir.join(format!("Theme-{index:04}.conf")),
            "foreground = #ddeeff\nbackground = #102030\n",
        )
        .unwrap();
    }
    let catalog = discover_capped(&dir, caps);
    assert!(catalog.truncated());
    let ready = catalog.ready_names().collect::<Vec<_>>();
    assert_eq!(ready.len(), caps.entries);
    assert!(catalog.resolve(ready[0]).is_ok());
    assert_eq!(catalog.limits().seen, caps.entries);
    let live = ThemeCatalog::discover_in_capped(&dir, true, caps)
        .expect("a truncated scan is not a failure");
    assert_eq!(live.ready_names().count(), caps.entries);
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn theme_catalog_lists_a_non_utf8_file_name() {
    use std::os::unix::ffi::OsStrExt;
    let dir = theme_fixture_dir("non-utf8");
    let path = dir.join(std::ffi::OsStr::from_bytes(b"Caf\xff.conf"));
    write_theme(&path, "#ddeeff", "#102030");
    let catalog = ThemeCatalog::discover_in(&dir);
    let listing = catalog.listing().collect::<Vec<_>>();
    assert_eq!(listing, [("Caf\u{FFFD}", ThemeEntryStatus::Unreadable)]);
    assert!(catalog.ready_names().next().is_none());
    assert!(catalog.resolve("Caf\u{FFFD}").is_err());
    assert_eq!(catalog.limits().kept, 0);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn theme_catalog_hides_unsafe_malformed_oversized_and_symlink_entries() {
    let dir = theme_fixture_dir("invalid");
    write_theme(&dir.join("Work.conf"), "#123456", "#abcdef");
    write_theme(&dir.join("dark:Trap.conf"), "#ffffff", "#000000");
    write_theme(&dir.join(" Bad.conf"), "#ffffff", "#000000");
    std::fs::write(dir.join("Broken.conf"), "foreground = #ffffff\n").unwrap();
    let oversized = std::fs::File::create(dir.join("Huge.conf")).unwrap();
    oversized
        .set_len((MAX_USER_THEME_FILE_BYTES + 1) as u64)
        .unwrap();
    drop(oversized);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.join("Work.conf"), dir.join("Alias.conf")).unwrap();
        let fifo = dir.join("Pipe.conf");
        let fifo_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        // SAFETY: `fifo_path` is a valid NUL-terminated path and the return value is checked before the no-follow/nonblocking open is tested.
        assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
    }

    let catalog = ThemeCatalog::discover_in(&dir);
    assert_eq!(catalog.ready_names().collect::<Vec<_>>(), ["Work"]);
    for invalid in ["dark:Trap", " Bad", "Broken", "Huge"] {
        assert!(
            catalog.resolve(invalid).is_err(),
            "{invalid} must fail closed"
        );
    }
    #[cfg(unix)]
    {
        assert!(catalog.resolve("Alias").is_err());
        let alias = catalog.listing().find(|(name, _)| *name == "Alias");
        assert!(
            matches!(alias, Some((_, ThemeEntryStatus::Invalid(why))) if why.contains("symlink")),
            "{alias:?}"
        );
        assert!(
            ThemeCatalog::try_discover_in(&dir).is_ok(),
            "a linked theme never blocks a live reload"
        );
        assert!(
            read_bounded_theme_file(&dir.join("Alias.conf")).is_err(),
            "the handle-level open must reject a symlink even if it is swapped in after scanning"
        );
        assert!(
            read_bounded_theme_file(&dir.join("Pipe.conf")).is_err(),
            "the nonblocking handle-level open must reject a FIFO without waiting for a writer"
        );
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[test]
fn live_theme_file_swap_retains_valid_catalog_until_recovery() {
    use std::os::unix::fs::symlink;

    let dir = theme_fixture_dir("live-file-swap");
    let work = dir.join("Work.conf");
    write_theme(&work, "#123456", "#abcdef");
    let active = ThemeCatalog::try_discover_in(&dir).expect("initial valid catalog");
    assert!(active.resolve("Work").is_ok());

    let rejected = ThemeCatalog::try_discover_in_after_scan(&dir, || {
        std::fs::remove_file(&work).unwrap();
        symlink(dir.join("missing.conf"), &work).unwrap();
    });
    assert_eq!(rejected, Err(ThemeCatalogWatchError::FileUnavailable));

    std::fs::remove_file(&work).unwrap();
    write_theme(&work, "#fedcba", "#101820");
    let recovered = ThemeCatalog::try_discover_in(&dir).expect("recovered catalog");
    assert!(recovered.resolve("Work").is_ok());
    assert_ne!(recovered, active);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn theme_catalog_has_one_case_insensitive_identity_and_rejects_collisions() {
    let dir = theme_fixture_dir("case-identity");
    write_theme(&dir.join("Work.conf"), "#123456", "#abcdef");
    let catalog = ThemeCatalog::discover_in(&dir);
    assert_eq!(catalog.ready_names().collect::<Vec<_>>(), ["Work"]);
    assert_eq!(catalog.resolve("work"), catalog.resolve("WORK"));

    let collided = ThemeCatalog::from_schemes([
        (
            "Work".to_string(),
            aterm_types::scheme::builtin("Dracula").unwrap(),
        ),
        (
            "work".to_string(),
            aterm_types::scheme::builtin("GitHub Light").unwrap(),
        ),
    ]);
    assert!(collided.ready_names().next().is_none());
    let upper = collided.resolve("Work").unwrap_err();
    let lower = collided.resolve("work").unwrap_err();
    assert_eq!(upper, lower);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn one_theme_catalog_drives_identical_preview_renderer_and_engine_colors() {
    let dir = theme_fixture_dir("projection");
    write_theme(&dir.join("Work.conf"), "#123456", "#abcdef");
    let catalog = ThemeCatalog::discover_in(&dir);
    let config = cfg("theme = \"work\"");
    let scheme = catalog.resolve("Work").unwrap();
    assert_eq!(catalog.resolve("work").unwrap(), scheme);
    let renderer = config.theme_for_with_assets(aterm_types::Appearance::Dark, &catalog);
    let engine =
        config.applied_terminal_config_for_with_assets(aterm_types::Appearance::Dark, &catalog);
    assert_eq!(renderer.fg, scheme.to_theme_parts().fg);
    assert_eq!(renderer.bg, scheme.to_theme_parts().bg);
    assert_eq!(engine.default_foreground, scheme.foreground);
    assert_eq!(engine.default_background, scheme.background);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn kitty_asset_resolves_once_to_shared_rgba_with_stable_identity() {
    let path = kitty_fixture("ready");
    let rgba = [
        0xff, 0x10, 0x20, 0xff, 0x20, 0xff, 0x30, 0x80, 0x10, 0x20, 0xff, 0x40, 0xff, 0xff, 0xff,
        0xff,
    ];
    let png = crate::app_introspect::encode_rgba8_png(&rgba, 2, 2).unwrap();
    std::fs::write(&path, png).unwrap();
    let config = config_with_kitty(&path);
    let first = config.resolve_asset_catalog();
    let second = config.resolve_asset_catalog();
    let (
        KittySpriteAsset::Ready {
            source_id,
            w,
            h,
            rgba: resolved,
            fp,
        },
        KittySpriteAsset::Ready { fp: second_fp, .. },
    ) = (&first.kitty_sprite, &second.kitty_sprite)
    else {
        panic!("valid PNG must resolve Ready");
    };
    assert_eq!(source_id.as_ref(), path.to_string_lossy());
    assert_eq!((*w, *h), (2, 2));
    assert_eq!(resolved.as_ref(), rgba);
    assert_ne!(*fp, 0);
    assert_eq!(fp, second_fp, "same source bytes have stable identity");
    let _ = std::fs::remove_file(path);
}

#[test]
fn kitty_asset_hostile_sources_fail_closed() {
    let invalid = |path: &std::path::Path, case: &str| {
        assert!(
            matches!(
                config_with_kitty(path).resolve_asset_catalog().kitty_sprite,
                KittySpriteAsset::Invalid { .. }
            ),
            "{case}"
        );
    };
    invalid(&kitty_fixture("missing"), "missing");

    let oversized = kitty_fixture("encoded-limit");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len((MAX_KITTY_SPRITE_FILE_BYTES as u64) + 1)
        .unwrap();
    invalid(&oversized, "encoded bytes past the cap");

    let wide = kitty_fixture("dimension-limit");
    let width = super::MAX_KITTY_SPRITE_DIMENSION + 1;
    let png =
        crate::app_introspect::encode_rgba8_png(&vec![0x7f; width * 4], width as u32, 1).unwrap();
    std::fs::write(&wide, png).unwrap();
    invalid(&wide, "dimension past the cap");
    let fixtures = vec![oversized, wide];

    #[cfg(unix)]
    let fixtures = {
        use std::os::unix::ffi::OsStrExt as _;
        let fifo = kitty_fixture("writerless-fifo");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path");
        // SAFETY: `fifo_c` is a live NUL-terminated pathname and mkfifo retains no pointer. `kitty_fixture` gives this test a unique final component.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        invalid(&fifo, "writerless FIFO");
        let target = kitty_fixture("symlink-target");
        let linked = kitty_fixture("symlink-final");
        std::fs::write(&target, b"not consulted").expect("write symlink target");
        std::os::unix::fs::symlink(&target, &linked).expect("create final symlink");
        invalid(&linked, "final-component symlink");
        let mut fixtures = fixtures;
        fixtures.extend([fifo, linked, target]);
        fixtures
    };
    for path in fixtures {
        let _ = std::fs::remove_file(path);
    }
}

fn write_toy_pack(label: &str, words: &[&str], burst: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "aterm-toy-pack-test-{}-{serial}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create toy-pack fixture directory");
    let path = dir.join("pack.toml");
    let words = words
        .iter()
        .map(|word| super::toml_basic_string(word))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        "schema = 1\n\
         [pack]\n\
         id = \"community.test.{label}\"\n\
         name = \"{label}\"\n\
         version = 1\n\
         authors = [\"aterm tests\"]\n\
         license = \"Apache-2.0\"\n\
         [[toy]]\n\
         id = \"recipe\"\n\
         lang = \"en\"\n\
         words = [{words}]\n\
         burst = {{ kind = \"{burst}\" }}\n"
    );
    std::fs::write(&path, source).expect("write toy-pack fixture");
    path
}

fn config_with_toy_packs(paths: &[std::path::PathBuf], tail: &str) -> Config {
    let paths = paths
        .iter()
        .map(|path| super::toml_basic_string(&path.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(", ");
    cfg(&format!("[sparkle_words]\ntoy_packs = [{paths}]\n{tail}"))
}

#[test]
fn w6_font_config_warns_and_skips_unresolvable() {
    let fixture = aterm_tempfile::manifest_dir().join("../aterm-render/assets/DejaVuSansMono.ttf");
    let fixture = fixture.to_str().expect("a UTF-8 font path");
    let toml = format!(
        "font_family_bold = \"definitely-not-a-real-font-xyzzy\"\n\
         font_synthetic_style = false\n\
         fallback_fonts = [{}, \"also-not-a-real-font-xyzzy\"]\n",
        super::toml_basic_string(fixture)
    );
    let (fc, warns) = super::FontConfig::from_config(&cfg(&toml));
    assert_eq!(fc.styled_paths[0], None, "bogus bold family skipped");
    assert!(!fc.synthetic_style);
    assert_eq!(fc.fallback_fonts, vec![fixture.to_string()]);
    assert_eq!(
        warns.len(),
        2,
        "one warning per unresolvable entry: {warns:?}"
    );
    assert!(warns[0].contains("font_family_bold"));
    assert!(warns[1].contains("fallback_fonts"));
}

#[test]
fn w6_rejected_oversized_face_preserves_last_working_font() {
    let root = std::env::temp_dir().join(format!(
        "aterm-font-config-oversized-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("oversized.ttf");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(aterm_render::font_file::MAX_FONT_FILE_BYTES as u64 + 1)
        .unwrap();
    let config = cfg(&format!(
        "font_family_bold = {}\n",
        super::toml_basic_string(&path.to_string_lossy())
    ));
    let (mut next, warnings) = super::FontConfig::from_config(&config);
    let mut working = super::FontConfig::default();
    working.styled_paths[0] = Some(aterm_render::FaceRef::new("/admitted/working-bold.ttf", 0));
    next.preserve_rejected_from(&config, &working);

    assert_eq!(next.styled_paths[0], working.styled_paths[0]);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("font_family_bold"), "{warnings:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn w6_writerless_fifo_is_diagnosed_without_entering_live_font_config() {
    use std::os::unix::ffi::OsStrExt as _;

    let root = std::env::temp_dir().join(format!("aterm-font-config-fifo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("face.fifo");
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the CString is live for the call and the private fixture path does not alias another test's FIFO.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    let config = cfg(&format!(
        "font_family_italic = {}\n",
        super::toml_basic_string(&path.to_string_lossy())
    ));
    let (resolved, warnings) = super::FontConfig::from_config(&config);
    assert!(resolved.styled_paths[1].is_none());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("font_family_italic"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn an_ignored_font_px_is_a_load_notice() {
    let with = |font_px: Option<f32>| Config {
        font_px,
        ..Config::default()
    };
    let validate_flags = |config: &Config| {
        crate::diagnostics::config_semantic_warnings(config)
            .iter()
            .any(|w| w.key == "font_px")
    };
    for fine in [
        None,
        Some(14.0),
        Some(crate::FONT_PX_MIN),
        Some(crate::FONT_PX_MAX),
    ] {
        assert_eq!(super::font_px_load_notice(&with(fine)), None, "{fine:?}");
        assert!(!validate_flags(&with(fine)), "{fine:?}");
    }
    for bad in [0.0, 500.0, -5.0, f32::NAN, f32::INFINITY] {
        assert!(
            super::font_px_load_notice(&with(Some(bad))).is_some(),
            "{bad}"
        );
        assert!(validate_flags(&with(Some(bad))), "{bad}");
        assert_eq!(
            super::resolve_font_px_with(None, Some(bad)),
            crate::FONT_PX,
            "{bad}: the banner's claim is the resolver's behaviour"
        );
    }
}

#[test]
fn restart_notices_flags_columns_lines_and_gpu_only() {
    use super::restart_notices;
    let a = cfg("columns = 100\nlines = 40\ngpu = true");
    assert!(restart_notices(&a, &a).is_empty());
    let hot = cfg("columns = 100\nlines = 40\ngpu = true\nfont_px = 18");
    assert!(restart_notices(&a, &hot).is_empty());
    let cols = cfg("columns = 120\nlines = 40\ngpu = true");
    assert_eq!(restart_notices(&a, &cols).len(), 1);
    let rows = cfg("columns = 100\nlines = 50\ngpu = true");
    assert_eq!(restart_notices(&a, &rows).len(), 1);
    let both = cfg("columns = 120\nlines = 50\ngpu = true");
    assert_eq!(restart_notices(&a, &both).len(), 1);
    let cleared = cfg("lines = 40\ngpu = true");
    assert_eq!(restart_notices(&a, &cleared).len(), 1);
    let gpu_flipped = cfg("columns = 100\nlines = 40\ngpu = false");
    assert_eq!(restart_notices(&a, &gpu_flipped).len(), 1);
    let gpu_cleared = cfg("columns = 100\nlines = 40");
    assert_eq!(restart_notices(&a, &gpu_cleared).len(), 1);
    let dims_and_gpu = cfg("columns = 120\nlines = 40\ngpu = false");
    assert_eq!(restart_notices(&a, &dims_and_gpu).len(), 2);
}

#[test]
fn engine_keys_resolve_onto_terminal_config() {
    use aterm_core::config::TerminalConfig;
    use aterm_core::terminal::CursorStyle;
    type Check = fn(&TerminalConfig) -> bool;
    let cases: [(&str, Check); 10] = [
        ("cursor_style = \" BAR \"", |t| {
            t.cursor_style == CursorStyle::BlinkingBar
        }),
        ("cursor_style = \"Beam\"\ncursor_blink = false", |t| {
            t.cursor_style == CursorStyle::SteadyBar
        }),
        ("bidi = \"disabled\"", |t| t.bidi.mode == BiDiMode::Disabled),
        ("bidi = \" Explicit \"", |t| {
            t.bidi.mode == BiDiMode::Explicit
        }),
        ("bidi = \" ON \"", |t| t.bidi.mode == BiDiMode::Implicit),
        ("ambiguous_width = \" wide \"", |t| t.ambiguous_width_double),
        ("ambiguous_width = \" DOUBLE \"", |t| {
            t.ambiguous_width_double
        }),
        ("ambiguous_width = \" NARROW \"", |t| {
            !t.ambiguous_width_double
        }),
        ("bold_is_bright = false", |t| !t.bold_is_bright),
        ("faint_opacity = 0.25", |t| t.faint_opacity == 0.25),
    ];
    for (source, check) in cases {
        let tc = cfg(source).applied_terminal_config();
        assert!(check(&tc), "{source}");
    }
    assert_eq!(
        cfg("font_px = 14.0\nline_height = 1.2").applied_terminal_config(),
        Config::default().applied_terminal_config()
    );
}

#[test]
fn font_features_parse_onto_primary_face() {
    let s = cfg("font_features = [\"ss01\", \"-calt\", \"+zero\"]").text_shaping();
    assert_eq!(s.font_features.len(), 1, "one primary-face feature set");
    let set = &s.font_features[0];
    assert_eq!(set.font_id, 0);
    let tags: Vec<(&[u8], u32)> = set.features.iter().map(|f| (&f.tag[..], f.value)).collect();
    assert_eq!(
        tags,
        vec![(&b"ss01"[..], 1), (&b"calt"[..], 0), (&b"zero"[..], 1)]
    );
    let grouped = cfg("font_features = [\"+ss01 +zero\"]").text_shaping();
    assert_eq!(grouped.font_features[0].features.len(), 2);
    let empty = cfg("font_features = [\"\", \"+\", \"-\"]").text_shaping();
    assert!(empty.font_features.is_empty(), "{:?}", empty.font_features);
}

#[test]
fn security_flags_are_opt_in_and_fail_closed() {
    let flags = |source: &str| {
        let tc = cfg(source).applied_terminal_config();
        [
            tc.allow_osc52_query,
            tc.allow_window_ops,
            tc.allow_notifications,
            tc.allow_palette_reconfigure,
        ]
    };
    assert_eq!(
        flags("scrollback_lines = 5000"),
        [false, false, false, true],
        "palette sets are allowed by default"
    );
    assert_eq!(
        flags(
            "allow_osc52_query = true\nallow_window_ops = true\n\
             allow_notifications = true\nallow_palette_reconfigure = true"
        ),
        [true; 4]
    );
}

#[test]
fn palette_sets_refused_only_when_the_owner_says_false() {
    assert!(
        !cfg("allow_palette_reconfigure = false")
            .applied_terminal_config()
            .allow_palette_reconfigure
    );
    assert!(
        cfg("allow_palette_reconfigure = true")
            .applied_terminal_config()
            .allow_palette_reconfigure
    );
    assert!(!aterm_core::config::TerminalConfig::default().allow_palette_reconfigure);
}

#[test]
fn sparkle_absent_table_decorates_the_alt_screen_and_master_off_disables() {
    let deco = Config::default()
        .sparkle_deco_config()
        .expect("an absent table resolves the defaults");
    assert!(!deco.suppress_in_alt_screen);
    assert!(
        cfg("[sparkle_words]\nsuppress_in_alt_screen = true")
            .sparkle_deco_config()
            .unwrap()
            .suppress_in_alt_screen
    );
    assert!(
        cfg("[sparkle_words]\nenabled = false")
            .sparkle_deco_config()
            .is_none()
    );
}

#[test]
fn sparkle_style_tokens_resolve_trimmed_any_case_with_fallback() {
    use crate::word_decorations::{FelineStyle, ProfanityStyle};
    for (style, expected) in [
        (" Nova ", ProfanityStyle::Nova),
        (" SPARKLE ", ProfanityStyle::Sparkle),
        ("rainbow", ProfanityStyle::Rainbow),
        ("comet", ProfanityStyle::Rainbow),
    ] {
        let deco = cfg(&format!("[sparkle_words.profanity]\nstyle = \"{style}\""))
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(deco.profanity_style, expected, "{style:?}");
    }
    for (style, expected) in [(" Paw ", FelineStyle::Paw), ("lion", FelineStyle::Cat)] {
        let deco = cfg(&format!("[sparkle_words.feline]\nstyle = \"{style}\""))
            .sparkle_deco_config()
            .unwrap();
        assert_eq!(deco.feline_style, expected, "{style:?}");
    }
    let deco = cfg("[sparkle_words.profanity]\nsupernova_chance = 250")
        .sparkle_deco_config()
        .unwrap();
    assert_eq!(deco.supernova_chance, 100);
}

#[test]
fn sparkle_custom_specs_resolve_and_survive_ink_off() {
    let toml = concat!(
        "[sparkle_words.ink]\nenabled = false\n",
        "[[sparkle_words.custom]]\nwords = [\"Ultrathink\", \"猫神\"]\n",
        "ink = { colorway = \"rainbow\" }\n",
    );
    let c = cfg(toml);
    let deco = c.sparkle_deco_config().expect("resolves");
    assert!(deco.spec_table.has_custom(), "custom specs registered");
    assert!(
        deco.spec_table
            .override_for(aterm_lexicon::form_hash("ultrathink"))
            .is_some(),
        "keyed by the FOLDED surface hash"
    );
    assert!(
        deco.spec_table
            .override_for(aterm_lexicon::form_hash("猫神"))
            .is_some(),
        "CJK surfaces keyed RAW"
    );
    assert!(
        !deco.ink_enabled && deco.emphasis,
        "emphasis scans with ink off when custom specs exist (v3 §6 gate)"
    );
    let deco = cfg("[sparkle_words.ink]\nenabled = false")
        .sparkle_deco_config()
        .unwrap();
    assert!(!deco.emphasis, "no customs: the ink AND-gate stands");
    let over = c.sparkle_override_toml().expect("custom words synthesize");
    assert!(over.contains("forms = [\"Ultrathink\"]"), "{over}");
    assert!(over.contains("cjk = true"), "{over}");
    assert!(over.contains("forms = [\"猫神\"]"), "{over}");
}

#[test]
fn sparkle_words_product_toggle_suppresses_all_non_feline_effects() {
    let c = cfg(concat!(
        "[sparkle_words.profanity]\nenabled = false\n",
        "[sparkle_words.emphasis]\nenabled = true\nextra_words = [\"wow\"]\n",
        "[[sparkle_words.custom]]\nwords = [\"ultrathink\"]\n",
        "burst = { kind = \"starburst\", chance = 100 }\n",
    ));
    let deco = c
        .sparkle_deco_config()
        .expect("keyword kitties remain independently enabled");
    assert!(!deco.profanity, "built-in sparkle words are off");
    assert!(!deco.emphasis, "emphasis cannot bypass the product switch");
    assert!(
        !deco.canine,
        "every non-feline class shares the product switch"
    );
    assert!(deco.feline, "keyword kitties remain independently on");
    assert!(
        !deco.spec_table.has_custom(),
        "custom and Toy Pack effects cannot bypass the product switch"
    );

    assert!(
        cfg("[sparkle_words.profanity]\nenabled = false\n\
             [sparkle_words.feline]\nenabled = false")
        .sparkle_deco_config()
        .is_none(),
        "turning both top-level toys off produces no decoration engine"
    );
}

#[test]
fn toy_packs_load_skip_invalid_and_apply_documented_precedence() {
    use aterm_effects::spec::BurstKind;

    let first = write_toy_pack("first", &["packshared", "firsttoy"], "sparkle");
    let invalid_dir =
        std::env::temp_dir().join(format!("aterm-toy-pack-invalid-{}", std::process::id()));
    std::fs::create_dir_all(&invalid_dir).expect("create invalid fixture directory");
    let invalid = invalid_dir.join("pack.toml");
    std::fs::write(&invalid, "schema = 99\n").expect("write invalid fixture");
    let later = write_toy_pack("later", &["packshared", "latertoy"], "glow");
    let paths = [first.clone(), invalid.clone(), later.clone()];

    let imported = config_with_toy_packs(&paths, "");
    let (deco, override_toml) = imported.sparkle_runtime_parts().expect("sparkle resolves");
    assert!(
        deco.profanity && deco.feline,
        "an invalid pack disables nothing"
    );
    let shared = deco
        .spec_table
        .override_for(aterm_lexicon::form_hash("packshared"))
        .and_then(|spec| spec.burst)
        .expect("shared imported recipe");
    assert_eq!(shared.kind, BurstKind::Glow, "later valid pack wins");
    let lexicon =
        aterm_lexicon::Lexicon::with_languages_and_override(&["en"], override_toml.as_deref())
            .expect("compiled pack lexicon parses");
    assert_eq!(
        lexicon
            .scan(
                "firsttoy packshared latertoy",
                &aterm_lexicon::ScanOptions::default()
            )
            .len(),
        3,
        "both valid packs reach the production scanner"
    );

    let inline = config_with_toy_packs(
        &paths,
        "[[sparkle_words.custom]]\n\
         words = [\"packshared\"]\n\
         burst = { kind = \"nova\", chance = 100 }\n",
    );
    let (deco, _) = inline.sparkle_runtime_parts().expect("inline resolves");
    let shared = deco
        .spec_table
        .override_for(aterm_lexicon::form_hash("packshared"))
        .and_then(|spec| spec.burst)
        .expect("inline shared recipe");
    assert_eq!(
        shared.kind,
        BurstKind::Nova,
        "inline custom is the final, most-local override"
    );

    for path in [first, invalid, later] {
        let _ = std::fs::remove_dir_all(path.parent().expect("fixture parent"));
    }
}

#[test]
fn render_recompute_uses_only_the_prepared_sparkle_generation() {
    let pack = write_toy_pack("render-no-host-io", &["preparedword"], "glow");
    let config = config_with_toy_packs(std::slice::from_ref(&pack), "");
    let prepared = config.prepare_sparkle_runtime();

    std::fs::remove_file(&pack).expect("remove admitted Toy Pack source");
    super::reset_sparkle_host_prepare_count();
    let mut app = crate::App::headless_for_test();
    app.config = config;
    app.prepared_sparkle = prepared;
    app.sparkle_dirty = true;
    super::reset_sparkle_host_prepare_count();

    app.recompute_sparkle();
    assert_eq!(
        super::sparkle_host_prepare_count(),
        0,
        "render-time recompute must not open/compile any configured feed"
    );
    assert!(
        app.sparkle.as_ref().is_some_and(|resolved| resolved
            .cfg
            .spec_table
            .override_for(aterm_lexicon::form_hash("preparedword"))
            .is_some()),
        "the immutable prepared Toy Pack remains active after its source disappears"
    );
    let _ = std::fs::remove_dir_all(pack.parent().expect("fixture parent"));
}

#[test]
fn prepared_config_apply_does_not_refingerprint_path_feeds() {
    let dir =
        std::env::temp_dir().join(format!("aterm-prepared-apply-no-io-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, "# comment-only generation\n");
    let observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, false).unwrap();
    let config = Config::default();
    let assets = config.resolve_asset_catalog();
    let feeds = config.path_feed_fingerprints();
    let sparkle = config.prepare_sparkle_runtime();
    let mut app = crate::App::headless_for_test();
    super::reset_path_feed_fingerprint_count();

    app.apply_prepared_config_generation(crate::native_font_catalog::PreparedConfigGeneration {
        observation,
        config,
        values: std::collections::BTreeMap::new(),
        disk_faults: std::sync::Arc::default(),
        assets,
        path_feed_fps: feeds,
        sparkle,
        fonts: None,
        warnings: Vec::new(),
        font_refusals: Vec::new(),
        reason: crate::app_colorscheme::ThemeChange::ConfigReload,
    });
    assert_eq!(
        super::path_feed_fingerprint_count(),
        0,
        "the event-loop reducer must consume the worker's immutable fingerprint"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// A GOOD READ EQUAL TO A FAULT'S STAND-IN IS STILL A GOOD READ: it took the early return for a
/// generation that changes nothing, so the fault's hold stood, and a later refused read lifted a
/// person's `false`. CONTROL: the same text with nothing to end changes nothing.
#[test]
fn a_good_read_equal_to_a_fault_s_stand_in_still_sets_the_switches() {
    use super::UpdateSwitchOrigin::{Admitted, FaultHold};
    const OFF: &str = "[update]\nauto_apply = false\n";
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let mut app = crate::App::headless_for_test();
    apply_config_text(&mut app, dir.path(), OFF);
    let off = app.config.update.clone();
    assert_eq!(off.as_ref().and_then(|u| u.auto_apply), Some(false));
    assert_eq!(app.update_switch_origin, Admitted);
    app.update_switch_origin = FaultHold;
    apply_config_text(&mut app, dir.path(), OFF);
    assert_eq!(app.config.update, off, "CONTROL: it changes nothing");
    assert_eq!(app.update_switch_origin, Admitted);
    app.take_refused_update_text("font_px = 12.0\n");
    assert_eq!(
        app.config.update, off,
        "a refused read lifts no false a good read gave"
    );
}

/// WHAT A REFUSED READ SAID STANDS OVER A GENERATION OLDER THAN IT: a theme refresh re-installs the
/// last file that loaded, and a generation can still be in preparation when a newer save is refused.
/// Either erased an off the watcher would not send again, so the window installed over a person's
/// `false`. The admitted `[update]` stays beside the off, so a later refused text can still end it.
/// NEGATIVE CONTROL: a read heard after the refused text speaks for the file.
#[test]
fn what_a_refused_read_said_stands_over_a_generation_older_than_it() {
    const AUTO_OFF: &str = "[update]\nauto_apply = false\n";
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let mut app = crate::App::headless_for_test();
    let off = |app: &crate::App| app.config.update.as_ref().and_then(|u| u.auto_apply);
    apply_config_text(&mut app, dir.path(), "font_px = 13.0\n");
    assert_eq!(off(&app), None);
    app.hear_refused_update_text(AUTO_OFF.to_owned());
    assert_eq!(off(&app), Some(false));
    apply_config_text(&mut app, dir.path(), "font_px = 14.0\n");
    assert_eq!(off(&app), Some(false), "that generation is older");
    app.hear_refused_update_text("font_px = 14.0\n".to_owned());
    assert_eq!(off(&app), None, "the line went: it can still end its off");
    app.hear_refused_update_text(AUTO_OFF.to_owned());
    assert_eq!(off(&app), Some(false));
    app.take_update_switches_of_text("font_px = \"x\"\n");
    assert_eq!(off(&app), Some(false), "so is a reload that did not load");

    app.refused_update_text = None;
    apply_config_text(&mut app, dir.path(), "font_px = 15.0\n");
    assert_eq!(off(&app), None, "NEGATIVE CONTROL: a read heard since");

    app.hear_refused_update_text(AUTO_OFF.to_owned());
    app.native_config_service.mark_reconciliation_required();
    let reconciled = prepared_generation(dir.path(), "font_px = 16.0\n");
    app.apply_reconciled_prepared_config_generation(reconciled);
    assert_eq!(
        app.native_config_service.snapshot().text.as_ref(),
        "font_px = 16.0\n",
        "PRECONDITION: that generation installed"
    );
    assert_eq!(
        off(&app),
        Some(false),
        "so is one a reconciliation admitted"
    );
}

/// A generation's own off is the admitted text's, so what a refused read kept from before it
/// cannot end that off. The copy of the older `[update]` goes as the generation installs.
#[test]
fn a_refused_read_cannot_end_an_off_a_generation_since_admitted() {
    const AUTO_OFF: &str = "[update]\nauto_apply = false\n";
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let mut app = crate::App::headless_for_test();
    let off = |app: &crate::App| app.config.update.as_ref().and_then(|u| u.auto_apply);
    apply_config_text(&mut app, dir.path(), "font_px = 13.0\n");
    app.hear_refused_update_text(AUTO_OFF.to_owned());
    assert_eq!(off(&app), Some(false));
    app.refused_update_text = None;
    apply_config_text(&mut app, dir.path(), AUTO_OFF);
    assert_eq!(off(&app), Some(false), "the file now says it");
    app.hear_refused_update_text("font_px = 14.0\n".to_owned());
    assert_eq!(off(&app), Some(false));
}

fn apply_config_text(app: &mut crate::App, dir: &std::path::Path, text: &str) {
    let generation = prepared_generation(dir, text);
    app.apply_prepared_config_generation(generation);
}

fn prepared_generation(
    dir: &std::path::Path,
    text: &str,
) -> crate::native_font_catalog::PreparedConfigGeneration {
    let path = dir.join("aterm.toml");
    crate::test_scratch::write_config(&path, text);
    let observation =
        crate::native_config_service::VersionedConfigService::observe_path(&path, false)
            .expect("observe");
    let config: Config = aterm_toml::from_str(text).expect("valid toml");
    let assets = config.resolve_asset_catalog();
    let path_feed_fps = config.path_feed_fingerprints();
    let sparkle = config.prepare_sparkle_runtime();
    crate::native_font_catalog::PreparedConfigGeneration {
        observation,
        config,
        values: std::collections::BTreeMap::new(),
        disk_faults: std::sync::Arc::default(),
        assets,
        path_feed_fps,
        sparkle,
        fonts: None,
        warnings: Vec::new(),
        font_refusals: Vec::new(),
        reason: crate::app_colorscheme::ThemeChange::ConfigReload,
    }
}

#[test]
fn a_misspelled_key_added_by_a_live_edit_still_warns_through_the_dedupe() {
    let dir = std::env::temp_dir().join(format!("aterm-typo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let mut app = crate::App::headless_for_test();

    apply_config_text(&mut app, &dir, "windw_padding = 20\n");
    let ignored = app
        .messages
        .live_by_key(crate::message_reporters::ConfigFamily::IgnoredKeys.key())
        .expect("a misspelled key must warn even when the parse dedupes");
    assert!(
        ignored
            .msg
            .detail
            .iter()
            .any(|l| l.contains("windw_padding")),
        "the row names the key that was ignored: {:?}",
        ignored.msg
    );

    let mut chord = crate::message_reporters::ConfigWarnings::default();
    chord.push(
        crate::message_reporters::ConfigFamily::Keybindings,
        "config keybindings: skipping \"ctrl+x\": unknown action \"foo\"".into(),
    );
    for msg in chord.into_messages() {
        app.post_message(msg);
    }

    apply_config_text(&mut app, &dir, "# fixed\n");
    assert!(
        app.messages
            .live_by_key(crate::message_reporters::ConfigFamily::IgnoredKeys.key())
            .is_none(),
        "removing the bad key clears its row"
    );
    assert!(
        app.messages
            .live_by_key(crate::message_reporters::ConfigFamily::Keybindings.key())
            .is_some(),
        "a comment-only save leaves the chord's still-true row standing"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_theme_that_does_not_load_is_a_live_row_until_its_file_arrives() {
    const NAME: &str = "Aterm Test Theme 7F1C";
    let dir = std::env::temp_dir().join(format!("aterm-theme-miss-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let mut app = crate::App::headless_for_test();
    apply_config_text(&mut app, &dir, &format!("theme = {NAME:?}\n"));
    let key = crate::message_reporters::ConfigFamily::UnacceptedValues.key();
    let row = app
        .messages
        .live_by_key(key)
        .expect("a theme that does not load is a live row");
    assert!(
        row.msg.detail.iter().any(|line| line.contains(NAME)),
        "{:?}",
        row.msg
    );

    app.reload_theme_catalog(ThemeCatalog::from_schemes([(
        NAME.to_string(),
        aterm_types::ColorScheme::default(),
    )]));
    assert!(
        app.messages.live_by_key(key).is_none(),
        "the theme file arriving takes the row down"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn malformed_external_lexicon_does_not_drop_valid_toy_pack_layer() {
    let pack = write_toy_pack("lexicon-isolation", &["packstillworks"], "glow");
    let external = pack
        .parent()
        .expect("fixture parent")
        .join("broken-lexicon.toml");
    std::fs::write(&external, "[[entry]\nclass = \"emphasis\"")
        .expect("write malformed external lexicon");
    let tail = format!(
        "lexicon = {}\n",
        super::toml_basic_string(&external.to_string_lossy())
    );
    let config = config_with_toy_packs(std::slice::from_ref(&pack), &tail);
    let (deco, override_toml) = config.sparkle_runtime_parts().expect("sparkle resolves");

    assert!(
        deco.spec_table
            .override_for(aterm_lexicon::form_hash("packstillworks"))
            .is_some(),
        "the compiled Toy Pack spec survives the rejected user layer"
    );
    let lexicon =
        aterm_lexicon::Lexicon::with_languages_and_override(&["en"], override_toml.as_deref())
            .expect("generated layers remain independently valid");
    assert_eq!(
        lexicon
            .scan("packstillworks", &aterm_lexicon::ScanOptions::default())
            .len(),
        1,
        "the Toy Pack trigger still reaches the production scanner"
    );

    std::fs::remove_dir_all(pack.parent().expect("fixture parent"))
        .expect("remove isolation fixtures");
}

#[test]
fn external_lexicon_cap_is_exact_and_enforced_by_the_runtime_loader() {
    let path = kitty_fixture("lexicon-cap");
    let prefix =
        "[[entry]]\nclass=\"emphasis\"\nlang=\"en\"\nmode=\"forms\"\nforms=[\"boundedword\"]\n#";
    let mut exact = prefix.to_string();
    exact.push_str(&"x".repeat(aterm_effects::file_feed::MAX_SPARKLE_LEXICON_BYTES - prefix.len()));
    std::fs::write(&path, &exact).expect("write exact-limit lexicon");
    let config = cfg(&format!(
        "[sparkle_words]\nlexicon = {}\n",
        super::toml_basic_string(&path.to_string_lossy())
    ));
    assert!(
        config
            .sparkle_override_toml()
            .is_some_and(|source| source.contains("boundedword")),
        "a regular lexicon exactly at the cap is admitted"
    );

    exact.push('x');
    std::fs::write(&path, exact).expect("write over-limit lexicon");
    assert!(
        config.sparkle_override_toml().is_none(),
        "the same lexicon one byte over the cap is skipped"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn toy_pack_path_count_is_bounded() {
    let active = write_toy_pack("active", &["activepackword"], "glow");
    let overflow = write_toy_pack("overflow", &["overflowpackword"], "nova");
    let mut paths = vec![active.clone(); super::MAX_ACTIVE_TOY_PACKS];
    paths.push(overflow.clone());
    let config = config_with_toy_packs(&paths, "");
    let (deco, override_toml) = config.sparkle_runtime_parts().expect("sparkle resolves");
    assert!(
        deco.spec_table
            .override_for(aterm_lexicon::form_hash("activepackword"))
            .is_some(),
        "accepted paths load"
    );
    assert!(
        deco.spec_table
            .override_for(aterm_lexicon::form_hash("overflowpackword"))
            .is_none(),
        "paths beyond the hard cap are inactive"
    );
    assert!(
        !override_toml
            .as_deref()
            .unwrap_or_default()
            .contains("overflowpackword"),
        "capped paths do not leak into the scanner document"
    );
    for path in [active, overflow] {
        let _ = std::fs::remove_dir_all(path.parent().expect("fixture parent"));
    }
}

#[test]
fn sparkle_ink_round_trip_and_clamps() {
    let deco =
        cfg("[sparkle_words.ink]\nenabled = true\nstrength = 0.4\nsweep_ms = 1000\nloop = true")
            .sparkle_deco_config()
            .expect("ink table keeps the feature on");
    assert!(deco.ink_enabled);
    assert_eq!(deco.ink_strength, 0.4);
    assert_eq!(deco.ink_sweep_ms, 1000);
    assert!(deco.ink_loop);
    let deco = cfg("[sparkle_words.ink]\nstrength = 3.0\nsweep_ms = 50")
        .sparkle_deco_config()
        .unwrap();
    assert_eq!(deco.ink_strength, 1.0, "strength clamps to 0.0..=1.0");
    assert_eq!(deco.ink_sweep_ms, 350, "sweep_ms clamps to 350..=6000");
    let deco = cfg("[sparkle_words.ink]\nsweep_ms = 99999")
        .sparkle_deco_config()
        .unwrap();
    assert_eq!(deco.ink_sweep_ms, 6000);
    let deco = cfg("[sparkle_words.ink]\nloop = true\nsweep_ms = 400")
        .sparkle_deco_config()
        .unwrap();
    assert_eq!(
        deco.ink_sweep_ms, 600,
        "looping shimmer floors sweep at 600"
    );
    let deco = cfg("[sparkle_words.emphasis]\nenabled = false")
        .sparkle_deco_config()
        .unwrap();
    assert!(!deco.emphasis && deco.ink_enabled);
}

#[test]
fn prepared_emphasis_capability_matches_scannable_unoverridden_runtime_words() {
    let capability = |source: &str| {
        cfg(source)
            .prepare_sparkle_runtime()
            .consumer_capabilities()
            .emphasis_class_default
    };
    assert!(capability(
        "[sparkle_words.emphasis]\nextra_words = [\"megathink\"]\n"
    ));
    for source in [
        "[sparkle_words.emphasis]\nextra_words = []\n",
        "[sparkle_words.emphasis]\nextra_words = [\"   \"]\n",
        "[sparkle_words.emphasis]\nextra_words = [\"two words\"]\n",
        "[sparkle_words.emphasis]\nextra_words = [\"kitty\"]\n",
        "[sparkle_words.emphasis]\nextra_words = [\"megathink\"]\nignore_words = [\"MEGATHINK\"]\n",
        concat!(
            "[sparkle_words.emphasis]\nextra_words = [\"megathink\"]\n",
            "[[sparkle_words.custom]]\nwords = [\"megathink\"]\n",
            "graphic = { collection = \"cats\" }\n",
        ),
    ] {
        assert!(
            !capability(source),
            "no class-default emphasis match should survive: {source}"
        );
    }

    let dir =
        std::env::temp_dir().join(format!("aterm-emphasis-capability-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lexicon = dir.join("lexicon.toml");
    std::fs::write(
        &lexicon,
        concat!(
            "[[entry]]\nclass = \"emphasis\"\nlang = \"en\"\n",
            "mode = \"forms\"\nforms = [\"fromfile\"]\n",
            "[[entry]]\nclass = \"emphasis\"\nlang = \"zh\"\n",
            "mode = \"forms\"\ncjk = true\nforms = [\"超\"]\n",
        ),
    )
    .unwrap();
    let path = super::toml_basic_string(&lexicon.to_string_lossy());
    assert!(capability(&format!("[sparkle_words]\nlexicon = {path}\n")));
    assert!(
        !capability(&format!(
            "[sparkle_words]\nlexicon = {path}\n\
             [[sparkle_words.custom]]\nwords = [\"fromfile\"]\n\
             graphic = {{ collection = \"cats\" }}\n\
             [sparkle_words.emphasis]\nignore_words = [\"超\"]\n"
        )),
        "custom ownership plus ignore removes every external default consumer"
    );

    std::fs::write(
        &lexicon,
        concat!(
            "[[entry]]\nclass = \"emphasis\"\nlang = \"zh\"\n",
            "mode = \"forms\"\ncjk = true\nforms = [\"超\"]\n",
        ),
    )
    .unwrap();
    assert!(!capability(&format!("[sparkle_words]\nlexicon = {path}\n")));
    assert!(capability(&format!(
        "[sparkle_words]\nlexicon = {path}\n\
         [sparkle_words.feline]\ncjk_single_char = true\n"
    )));
    let _ = std::fs::remove_dir_all(dir);
}
