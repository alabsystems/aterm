// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;
use crate::theme_cmd::run;
use crate::theme_cmd::tests::NEVER_LISTS;
use std::path::PathBuf;

struct ScratchDir(aterm_tempfile::TempDir);

impl std::ops::Deref for ScratchDir {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.0.path()
    }
}

impl AsRef<Path> for ScratchDir {
    fn as_ref(&self) -> &Path {
        self.0.path()
    }
}

fn scratch(tag: &str) -> ScratchDir {
    ScratchDir(
        aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-cli-theme-author-{tag}-"))
            .tempdir()
            .expect("scratch dir"),
    )
}

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

fn theme(argv: &[&str], dir: Option<&Path>) -> Outcome {
    run(&args(argv), dir, &NEVER_LISTS)
}

fn json_of(out: &Outcome) -> Value {
    aterm_json::from_str(&out.stdout).expect("one JSON document")
}

fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value
        .get(key)
        .unwrap_or_else(|| panic!("no {key:?} in {value:?}"))
}

fn codes(doc: &Value) -> Vec<String> {
    field(doc, "diagnostics")
        .as_array()
        .expect("diagnostics")
        .iter()
        .map(|d| field(d, "code").as_str().unwrap_or_default().to_string())
        .collect()
}

const GOOD: &str = "name = Harbour\nappearance = dark\nforeground = #e0e0e0\n\
                    background = #101418\ncursor = #7aa2f7\nselection = #2a3a50\n";

#[test]
fn check_passes_a_good_theme() {
    let dir = scratch("good");
    let file = dir.join("harbour.conf");
    std::fs::write(&file, GOOD).unwrap();
    let path = file.to_str().unwrap();
    for argv in [
        vec!["check", path],
        vec!["check", path, "--strict"],
        vec!["check", "--format=json", path],
    ] {
        let out = theme(&argv, None);
        assert_eq!(out.code, EXIT_LOADS, "{argv:?}: {out:?}");
    }
    let out = theme(&["check", path, "--format", "json", "--strict"], None);
    let doc = json_of(&out);
    assert_eq!(field(&doc, "status").as_str(), Some("ok"));
    assert_eq!(
        field(&doc, "exit_code").as_u64(),
        Some(u64::from(EXIT_LOADS))
    );
    assert_eq!(field(&doc, "id").as_str(), Some("harbour"));
    assert!(codes(&doc).is_empty(), "{doc:?}");
    let theme_doc = field(&doc, "theme");
    assert_eq!(field(theme_doc, "display_name").as_str(), Some("Harbour"));
    assert_eq!(
        field(theme_doc, "appearance_source").as_str(),
        Some("declared")
    );
    let contrast = field(&doc, "contrast").as_array().unwrap();
    assert_eq!(contrast.len(), 20);
    assert!(
        contrast
            .iter()
            .all(|row| field(row, "below_minimum").as_bool() == Some(false))
    );
    let ansi = contrast
        .iter()
        .filter(|row| {
            field(row, "drawn")
                .as_str()
                .is_some_and(|k| k.starts_with("color"))
        })
        .count();
    assert_eq!(ansi, 16);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_refuses_an_invalid_theme() {
    let dir = scratch("invalid");
    let bad = dir.join("bad.conf");
    std::fs::write(
        &bad,
        "foreground = #ffffff\nbackground = #000000\nwidth = 80\n",
    )
    .unwrap();
    let out = theme(&["check", bad.to_str().unwrap(), "--format", "json"], None);
    assert_eq!(out.code, EXIT_INVALID, "{out:?}");
    let doc = json_of(&out);
    assert_eq!(field(&doc, "status").as_str(), Some("invalid"));
    assert_eq!(field(&doc, "theme"), &Value::Null);
    let diagnostic = &field(&doc, "diagnostics").as_array().unwrap()[0];
    assert_eq!(field(diagnostic, "severity").as_str(), Some("error"));
    assert_eq!(field(diagnostic, "code").as_str(), Some("parse"));
    assert_eq!(field(diagnostic, "line").as_u64(), Some(3));
    assert_eq!(field(diagnostic, "key").as_str(), Some("width"));
    let text = theme(&["check", bad.to_str().unwrap()], None);
    assert_eq!(text.code, EXIT_INVALID);

    let kitty = dir.join("night.conf");
    std::fs::write(&kitty, "foreground #c0caf5\nbackground #1a1b26\n").unwrap();
    let doc = json_of(&theme(
        &["check", kitty.to_str().unwrap(), "--format=json"],
        None,
    ));
    let diagnostic = &field(&doc, "diagnostics").as_array().unwrap()[0];
    assert_eq!(field(diagnostic, "foreign_format").as_str(), Some("kitty"));

    let missing = theme(&["check", dir.join("absent.conf").to_str().unwrap()], None);
    assert_eq!(missing.code, EXIT_INVALID);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_reports_warnings_and_strict_fails_on_them() {
    let dir = scratch("warn");
    let file = dir.join("murky.conf");
    std::fs::write(
        &file,
        "background = #202020\nforeground = #303030\ncolor4 = #202020\nappearance = light\n",
    )
    .unwrap();
    let path = file.to_str().unwrap();
    assert_eq!(theme(&["check", path], None).code, EXIT_LOADS);
    let strict = theme(&["check", path, "--strict", "--format", "json"], None);
    assert_eq!(strict.code, EXIT_WARNINGS, "{strict:?}");
    let doc = json_of(&strict);
    assert_eq!(field(&doc, "status").as_str(), Some("warnings"));
    let found = codes(&doc);
    for code in [
        "low_text_contrast",
        "ansi_matches_background",
        "appearance_mismatch",
    ] {
        assert!(found.iter().any(|c| c == code), "{code}: {found:?}");
    }
    let at = |code: &str| {
        field(&doc, "diagnostics")
            .as_array()
            .unwrap()
            .iter()
            .find(|d| field(d, "code").as_str() == Some(code))
            .map(|d| {
                (
                    field(d, "line").as_u64(),
                    field(d, "key").as_str().map(str::to_string),
                )
            })
            .unwrap()
    };
    assert_eq!(
        at("low_text_contrast"),
        (Some(2), Some("foreground".into()))
    );
    assert_eq!(
        at("ansi_matches_background"),
        (Some(3), Some("color4".into()))
    );
    assert_eq!(
        at("appearance_mismatch"),
        (Some(4), Some("appearance".into()))
    );
    let low = field(&doc, "contrast")
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            field(row, "drawn").as_str() == Some("foreground")
                && field(row, "on").as_str() == Some("background")
        })
        .unwrap();
    assert_eq!(field(low, "below_minimum").as_bool(), Some(true));
    assert_eq!(
        theme(&["check", path, "--strict"], None).code,
        EXIT_WARNINGS
    );

    let chrome = dir.join("chrome.conf");
    let with_accent = |accent: &str| {
        format!("background = #1a1b26\nforeground = #c0caf5\naccent_color = {accent}\n")
    };
    std::fs::write(&chrome, with_accent("#1a1b26")).unwrap();
    let path = chrome.to_str().unwrap();
    let strict = theme(&["check", path, "--strict", "--format", "json"], None);
    assert_eq!(strict.code, EXIT_WARNINGS, "{strict:?}");
    let doc = json_of(&strict);
    let diagnostic = field(&doc, "diagnostics")
        .as_array()
        .unwrap()
        .iter()
        .find(|d| field(d, "code").as_str() == Some("chrome_below_floor"))
        .expect("the chrome warning");
    assert_eq!(field(diagnostic, "line").as_u64(), Some(3));
    assert_eq!(field(diagnostic, "key").as_str(), Some("accent_color"));
    std::fs::write(&chrome, with_accent("#7aa2f7")).unwrap();
    assert_eq!(theme(&["check", path, "--strict"], None).code, EXIT_LOADS);

    let shadowed = dir.join("Nord.conf");
    std::fs::write(&shadowed, GOOD).unwrap();
    let doc = json_of(&theme(
        &["check", shadowed.to_str().unwrap(), "--format=json"],
        None,
    ));
    assert_eq!(codes(&doc), ["shadowed_by_builtin"]);
    #[cfg(unix)]
    {
        let real = dir.join("real.conf");
        std::fs::write(&real, GOOD).unwrap();
        let linked = dir.join("linked.conf");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        let doc = json_of(&theme(
            &["check", linked.to_str().unwrap(), "--format=json"],
            None,
        ));
        assert!(codes(&doc).contains(&"symlink_not_followed".to_string()));
        let doc = json_of(&theme(
            &["check", real.to_str().unwrap(), "--format=json"],
            None,
        ));
        assert!(!codes(&doc).contains(&"symlink_not_followed".to_string()));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn authoring_usage_errors_exit_2() {
    for argv in [
        vec!["check"],
        vec!["check", "a", "b"],
        vec!["check", "a", "--format", "yaml"],
        vec!["check", "a", "--format", "text", "--format", "json"],
        vec!["check", "a", "--strict=yes"],
        vec!["check", "--help=extra"],
        vec!["new"],
        vec!["new", "x", "--appearance", "dim"],
        vec!["new", "Dracula"],
        vec!["new", "dark:x"],
        vec!["export"],
        vec!["export", "Nord", "--bogus"],
        vec!["schema", "extra"],
        vec!["schema", "--format", "yaml"],
    ] {
        let out = theme(&argv, None);
        assert_eq!(out.code, 2, "{argv:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{argv:?}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        let bad = OsString::from_vec(vec![0xff]);
        let out = run(
            &[
                OsString::from("check"),
                OsString::from("a"),
                OsString::from("--format"),
                bad,
            ],
            None,
            &NEVER_LISTS,
        );
        assert_eq!(out.code, 2);
        assert!(out.stderr.contains("is not UTF-8"), "{}", out.stderr);
    }
}

#[test]
fn new_writes_a_theme_that_round_trips() {
    let hold = scratch("new");
    let dir = hold.join("themes");
    let read = |out: &Outcome| {
        assert_eq!(out.code, 0, "{out:?}");
        let path = PathBuf::from(out.stdout.trim_end());
        assert_eq!(path.parent(), Some(dir.as_path()));
        scheme::parse_theme_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let seeded = |name: &str| CatalogTheme::builtin(name).unwrap().theme.scheme;

    let plain = read(&theme(&["new", "Harbour"], Some(&dir)));
    let mut want = seeded("Default");
    want.name = "Harbour".into();
    assert_eq!(plain.scheme, want);
    assert!(plain.appearance_declared);

    let light = read(&theme(
        &["new", "Paper", "--appearance", "light"],
        Some(&dir),
    ));
    assert_eq!(light.scheme.background, seeded("Default Light").background);
    assert_eq!(light.scheme.appearance, Appearance::Light);

    let paired = read(&theme(
        &[
            "new",
            "Camp",
            "--from",
            "gruvbox dark",
            "--appearance=light",
        ],
        Some(&dir),
    ));
    assert_eq!(paired.scheme.ansi, seeded("Gruvbox Light").ansi);

    let again = theme(&["new", "Harbour", "--from", "Nord"], Some(&dir));
    assert_eq!(again.code, 1, "{again:?}");
    let kept = std::fs::read_to_string(dir.join("Harbour.conf")).unwrap();
    assert_eq!(scheme::parse_scheme_str(&kept).unwrap(), want);
    let forced = read(&theme(
        &["new", "Harbour", "--from", "Nord", "--force"],
        Some(&dir),
    ));
    assert_eq!(forced.scheme.ansi, seeded("Nord").ansi);

    let path = dir.join("Harbour.conf");
    assert_ne!(
        theme(&["check", path.to_str().unwrap()], None).code,
        EXIT_INVALID
    );
    let missing = theme(&["new", "Other", "--from", "No Such Theme"], Some(&dir));
    assert_eq!(missing.code, 1);
    assert!(!dir.join("Other.conf").exists());
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

#[test]
fn export_reparses_equal_and_out_respects_force() {
    let out = theme(&["export", "Nord"], None);
    assert_eq!(out.code, 0, "{out:?}");
    let parsed = scheme::parse_theme_str(&out.stdout).expect("Nord");
    assert_eq!(parsed, CatalogTheme::builtin("Nord").unwrap().theme);
    let dir = scratch("export");
    let dest = dir.join("nord.conf");
    let dest_arg = dest.to_str().unwrap();
    let out = theme(&["export", "Nord", "--out", dest_arg], None);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(
        std::fs::read_to_string(&dest).unwrap(),
        theme(&["export", "Nord"], None).stdout
    );
    assert_eq!(
        theme(&["export", "Dracula", "--out", dest_arg], None).code,
        1
    );
    let forced = theme(&["export", "Dracula", "--out", dest_arg, "--force"], None);
    assert_eq!(forced.code, 0);
    assert_eq!(
        scheme::parse_scheme_str(&std::fs::read_to_string(&dest).unwrap()).unwrap(),
        scheme::builtin("Dracula").unwrap()
    );
    assert_eq!(theme(&["export", "No Such Theme"], None).code, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_forced_export_replaces_the_link_and_preserves_its_target() {
    let dir = scratch("export-link");
    let foreign = dir.join("foreign.conf");
    let original = b"a separate file that this export does not own\n";
    std::fs::write(&foreign, original).unwrap();
    let dest = dir.join("nord.conf");
    std::os::unix::fs::symlink(&foreign, &dest).unwrap();
    let path = dest.to_str().unwrap();

    let refused = theme(&["export", "Nord", "--out", path], None);
    assert_eq!(refused.code, 1, "{refused:?}");
    assert!(std::fs::symlink_metadata(&dest).unwrap().is_symlink());
    assert_eq!(std::fs::read(&foreign).unwrap(), original);

    let forced = theme(&["export", "Nord", "--out", path, "--force"], None);
    assert_eq!(forced.code, 0, "{forced:?}");
    assert_eq!(std::fs::read(&foreign).unwrap(), original);
    assert!(std::fs::symlink_metadata(&dest).unwrap().is_file());
    let exported = std::fs::read_to_string(&dest).unwrap();
    assert_eq!(
        scheme::parse_theme_str(&exported).unwrap(),
        CatalogTheme::builtin("Nord").unwrap().theme
    );
    let names: BTreeSet<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_signature_measures_the_signature_bar() {
    let dir = scratch("signature");
    let check = |name: &str, extra: &[&str]| {
        let path = dir.join(format!("{}.conf", name.replace(' ', "-")));
        std::fs::write(&path, theme(&["export", name], None).stdout).unwrap();
        let mut argv = vec!["check", path.to_str().unwrap(), "--format=json"];
        argv.extend_from_slice(extra);
        let out = theme(&argv, None);
        let doc = json_of(&out);
        (out.code, field(&doc, "signature").clone())
    };
    let (code, night) = check("Aterm Night", &["--signature", "--strict"]);
    assert_eq!(
        (code, night.as_array().map(Vec::len)),
        (EXIT_LOADS, Some(0))
    );
    let (code, nord) = check("Nord", &["--signature"]);
    assert_eq!(code, EXIT_LOADS);
    let missed = nord.as_array().expect("an array");
    assert!(!missed.is_empty());
    assert!(
        missed
            .iter()
            .all(|row| field(row, "code").as_str().is_some())
    );
    assert_eq!(check("Nord", &["--signature", "--strict"]).0, EXIT_WARNINGS);
    assert_eq!(check("Nord", &[]).1, Value::Null);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_built_from_the_schema_examples_parses() {
    let out = theme(&["schema"], None);
    assert_eq!(out.code, 0, "{out:?}");
    let doc = json_of(&out);
    assert_eq!(field(&doc, "version").as_u64(), Some(DOCUMENT_VERSION));
    let keys = field(&doc, "keys").as_array().unwrap();
    let build_as = |skip: Option<&str>, spell: Option<(&str, &str)>| {
        let mut text = String::new();
        for key in keys {
            let canonical = field(key, "name").as_str().unwrap();
            if Some(canonical) == skip {
                continue;
            }
            let name = match spell {
                Some((key, alias)) if key == canonical => alias,
                _ => canonical,
            };
            let example = field(key, "example").as_str().unwrap();
            match field(key, "index") {
                Value::Null => text.push_str(&format!("{name} = {example}\n")),
                index => {
                    let min = field(index, "min").as_u64().unwrap();
                    let max = field(index, "max").as_u64().unwrap();
                    for n in min..=max {
                        let key = name.replace("<N>", &n.to_string());
                        text.push_str(&format!("{key} = {example}\n"));
                    }
                }
            }
        }
        text
    };
    let build = |skip: Option<&str>| build_as(skip, None);
    let full = scheme::parse_theme_str(&build(None)).expect("the schema's examples parse");
    assert!(full.description.is_some() && full.author.is_some());
    for key in keys {
        let name = field(key, "name").as_str().unwrap();
        let required = field(key, "required").as_bool().unwrap();
        let without = scheme::parse_theme_str(&build(Some(name)));
        assert_eq!(without.is_err(), required, "{name}: {without:?}");
        for alias in field(key, "aliases").as_array().unwrap() {
            let alias = alias.as_str().unwrap();
            let aliased = scheme::parse_theme_str(&build_as(None, Some((name, alias))));
            assert_eq!(aliased.as_ref(), Ok(&full), "{alias}");
        }
    }
    for (key, spec) in keys.iter().zip(THEME_KEYS.iter()) {
        assert_eq!(field(key, "group").as_str(), Some(spec.group.label()));
        assert_eq!(
            spec.group == ThemeKeyGroup::Chrome,
            matches!(
                spec.key,
                ThemeKey::AccentColor
                    | ThemeKey::ActiveTabColor
                    | ThemeKey::BorderColor
                    | ThemeKey::SearchMatchColor
            ),
            "{}",
            spec.name
        );
    }
    let text = theme(&["schema", "--format", "text"], None);
    assert_eq!(text.code, 0);
    assert!(!text.stdout.is_empty());
}

struct TypeHost(CatalogTheme);

impl ThemeHost for TypeHost {
    fn list(&self, _: &crate::theme_cmd::ThemeListRequest) -> String {
        String::new()
    }
    fn lookup(&self, id: &str) -> Result<CatalogTheme, String> {
        if id.eq_ignore_ascii_case(&self.0.id) {
            return Ok(self.0.clone());
        }
        CatalogTheme::builtin(id).ok_or_else(|| "no theme by that name".to_string())
    }
    fn measure(&self, _: Rgb) -> Appearance {
        Appearance::Dark
    }
    fn font_available(&self, family: &str) -> Option<bool> {
        Some(!family.contains("Zz"))
    }
}

const STUDIO: &str = "name = Studio\nappearance = dark\nforeground = #e0e0e0\n\
                      background = #101418\nbold_is_bright = false\n\
                      font_family = Zz No Such Font\nline_height = 1.15\n\
                      fallback_fonts = Menlo, Zz Other\nfont_features = calt, -liga\n";

fn type_host() -> TypeHost {
    let mut parsed = scheme::parse_theme_str(STUDIO).expect("studio parses");
    let meta = ThemeMeta::admit(&mut parsed, "Studio", Appearance::Dark);
    TypeHost(CatalogTheme {
        id: "Studio".to_string(),
        theme: parsed,
        meta,
    })
}

#[test]
fn schema_names_each_keys_group_and_config_key() {
    let doc = json_of(&theme(&["schema"], None));
    let keys = field(&doc, "keys").as_array().unwrap();
    assert_eq!(keys.len(), THEME_KEYS.len());
    for (key, spec) in keys.iter().zip(THEME_KEYS.iter()) {
        assert_eq!(field(key, "group").as_str(), Some(spec.group.label()));
        assert_eq!(field(key, "config_key").as_str(), spec.config_key);
        assert_ne!(field(key, "constraints"), &Value::Null, "{}", spec.name);
    }
    let gate = field(field(&doc, "syntax"), "schema_gate");
    assert_eq!(
        field(gate, "current").as_u64(),
        Some(u64::from(scheme::THEME_SCHEMA_VERSION))
    );
    let text = theme(&["schema", "--format", "text"], None).stdout;
    for spec in THEME_KEYS
        .iter()
        .filter(|s| s.group == ThemeKeyGroup::Typography)
    {
        assert!(text.contains(spec.name), "{}", spec.name);
    }
}

#[test]
fn check_notes_a_font_that_is_not_installed() {
    let host = type_host();
    let dir = scratch("type-check");
    let file = dir.join("Studio.conf");
    std::fs::write(&file, STUDIO).unwrap();
    let path = file.to_str().unwrap();
    let out = run(
        &args(&["check", path, "--strict", "--format", "json"]),
        None,
        &host,
    );
    assert_eq!(out.code, EXIT_LOADS, "{out:?}");
    let doc = json_of(&out);
    let notes: Vec<&Value> = field(&doc, "diagnostics")
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| field(d, "severity").as_str() == Some("note"))
        .collect();
    assert_eq!(notes.len(), 2, "{doc:?}");
    assert!(
        notes
            .iter()
            .all(|d| field(d, "code").as_str() == Some("font_not_installed"))
    );
    let family = notes
        .iter()
        .find(|d| field(d, "key").as_str() == Some("font_family"))
        .expect("the family's note");
    assert_eq!(field(family, "line").as_u64(), Some(6));
    let typography = field(field(&doc, "theme"), "typography");
    assert_eq!(field(typography, "line_height").as_str(), Some("1.15"));
    let text = run(&args(&["check", path]), None, &host).stdout;
    assert!(text.contains("note[font_not_installed]"), "{text}");
    std::fs::write(&file, format!("{STUDIO}line_height = big\n")).unwrap();
    let bad = run(&args(&["check", path, "--strict"]), None, &host);
    assert_eq!(bad.code, EXIT_INVALID);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn theme_check_warns_on_close_palette_anchors() {
    let dir = scratch("palette-anchors");
    let file = dir.join("Murk.conf");
    let path = file.to_str().unwrap().to_string();
    let check = |text: &str| {
        std::fs::write(&file, text).unwrap();
        let out = theme(&["check", &path, "--format", "json"], None);
        codes(&json_of(&out))
    };
    let murk = "foreground = #9a9a9a\nbackground = #6a6a6a\ncolor0 = #707070\n\
                color7 = #8a8a8a\ncolor15 = #909090\npalette_generate = true\n";
    let found = check(murk);
    assert!(
        found.iter().any(|c| c == "palette_anchors_close"),
        "{found:?}"
    );
    assert!(found.iter().any(|c| c == "palette_generate"), "{found:?}");
    let wide = "foreground = #f0f0f0\nbackground = #101010\ncolor0 = #202020\n\
                color15 = #f8f8f8\npalette_generate = true\n";
    let found = check(wide);
    assert!(
        !found.iter().any(|c| c == "palette_anchors_close"),
        "{found:?}"
    );
    assert!(found.iter().any(|c| c == "palette_generate"), "{found:?}");
    let off = murk.replace("palette_generate = true\n", "");
    let found = check(&off);
    assert!(
        !found
            .iter()
            .any(|c| c == "palette_generate" || c == "palette_anchors_close"),
        "{found:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn export_and_new_carry_typography() {
    let host = type_host();
    let out = run(&args(&["export", "Studio"]), None, &host);
    assert_eq!(out.code, 0, "{out:?}");
    let parsed = scheme::parse_theme_str(&out.stdout).expect("exported");
    assert_eq!(parsed.typography, host.0.theme.typography);
    assert_eq!(parsed.bold_is_bright, Some(false));
    let hold = scratch("type-new");
    let dir = hold.join("themes");
    let made = run(
        &args(&["new", "Desk", "--from", "Studio"]),
        Some(&dir),
        &host,
    );
    assert_eq!(made.code, 0, "{made:?}");
    let written = std::fs::read_to_string(dir.join("Desk.conf")).unwrap();
    let back = scheme::parse_theme_str(&written).expect("new parses");
    assert_eq!(back.typography, host.0.theme.typography);
    assert_eq!(back.bold_is_bright, Some(false));
    let plain = run(
        &args(&["new", "Plain", "--from", "Nord"]),
        Some(&dir),
        &host,
    );
    assert_eq!(plain.code, 0);
    let plain = std::fs::read_to_string(dir.join("Plain.conf")).unwrap();
    assert!(
        scheme::parse_theme_str(&plain)
            .unwrap()
            .typography
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

fn contrib_themes() -> PathBuf {
    aterm_tempfile::manifest_dir().join("../../contrib/themes/")
}

fn strict_check(path: &Path) -> (Outcome, Value) {
    let path = path.to_str().expect("a UTF-8 path");
    let out = theme(&["check", path, "--strict", "--format", "json"], None);
    let doc = json_of(&out);
    (out, doc)
}

#[test]
fn contrib_themes_pass_the_strict_check() {
    let dir = contrib_themes();
    let mut checked = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("contrib/themes exists") {
        let path = entry.expect("a readable entry").path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name == "README.md" || name == "GUIDE.md" {
            continue;
        }
        if name == "pack" {
            assert!(path.is_dir(), "contrib/themes/pack is the pack's folder");
            continue;
        }
        assert_eq!(
            path.extension().and_then(|e| e.to_str()),
            Some("conf"),
            "{name}: contrib/themes holds one .conf per theme"
        );
        let (out, doc) = strict_check(&path);
        assert_eq!(out.code, EXIT_LOADS, "{name}: {}", out.stdout);
        assert!(codes(&doc).is_empty(), "{name}: {doc:?}");
        let identity = field(&doc, "theme");
        for key in ["description", "author"] {
            assert!(field(identity, key).as_str().is_some(), "{name}: no {key}");
        }
        assert_eq!(
            field(identity, "appearance_source").as_str(),
            Some("declared"),
            "{name}: declare appearance"
        );
        checked.push(path);
    }
    assert!(
        checked.iter().any(|p| p.ends_with("TEMPLATE.conf")),
        "{checked:?}"
    );

    let scratch = scratch("contrib-control");
    let murky = scratch.join("murky.conf");
    let text = std::fs::read_to_string(&checked[0]).expect("a contributed theme");
    let background = scheme::parse_theme_str(&text)
        .expect("it loads")
        .scheme
        .background;
    let sunk: String = text
        .lines()
        .map(|line| {
            let key = line.split('=').next().unwrap_or_default().trim();
            if key.eq_ignore_ascii_case("foreground") || key.eq_ignore_ascii_case("fg") {
                format!("{key} = {}", hex(background))
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&murky, sunk).unwrap();
    let (out, doc) = strict_check(&murky);
    assert_eq!(out.code, EXIT_WARNINGS, "{}", out.stdout);
    assert!(
        codes(&doc).contains(&"low_text_contrast".to_string()),
        "{doc:?}"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn pack_themes_pass_the_strict_check() {
    let dir = scratch("pack-strict");
    for theme in aterm_theme_pack::themes() {
        let path = dir.join(format!("{}.conf", theme.id));
        std::fs::write(&path, theme.text).unwrap();
        let (out, doc) = strict_check(&path);
        let id = theme.id;
        assert_eq!(out.code, EXIT_LOADS, "{id}: {}", out.stdout);
        assert!(codes(&doc).is_empty(), "{id}: {doc:?}");
        let identity = field(&doc, "theme");
        for key in ["description", "author"] {
            assert!(field(identity, key).as_str().is_some(), "{id}: no {key}");
        }
        assert_eq!(
            field(identity, "appearance_source").as_str(),
            Some("declared"),
            "{id}"
        );
        let want = match theme.appearance {
            Appearance::Dark => "dark",
            Appearance::Light => "light",
        };
        assert_eq!(field(identity, "appearance").as_str(), Some(want), "{id}");
    }
    let theme = &aterm_theme_pack::themes()[0];
    let background = scheme::parse_theme_str(theme.text)
        .expect("it loads")
        .scheme
        .background;
    let sunk: String = theme
        .text
        .lines()
        .map(|line| {
            if line.starts_with("foreground =") {
                format!("foreground = {}", hex(background))
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_ne!(
        sunk.trim_end(),
        theme.text.trim_end(),
        "the control changed the text"
    );
    let murky = dir.join("murky.conf");
    std::fs::write(&murky, sunk).unwrap();
    let (out, doc) = strict_check(&murky);
    assert_eq!(out.code, EXIT_WARNINGS, "{}", out.stdout);
    assert!(
        codes(&doc).contains(&"low_text_contrast".to_string()),
        "{doc:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
