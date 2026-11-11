// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::scheme::{
    CatalogTheme, ParsedTheme, ThemeError, ThemeMeta, ThemeValueType, ThemeWarning, builtin_names,
    parse_theme_str,
};
use crate::scheme_import::{to_native_conf_package, to_native_conf_package_commented};

const BASE: &str = "foreground = #c0caf5\nbackground = #1a1b26\n";

fn with(line: &str) -> Result<ParsedTheme, ThemeError> {
    parse_theme_str(&format!("{BASE}{line}\n"))
}

fn error_key_reason(result: Result<ParsedTheme, ThemeError>) -> (Option<String>, String) {
    match result {
        Err(ThemeError::Parse { key, reason, .. }) => (key, reason),
        Ok(parsed) => panic!("expected a parse error, got {parsed:?}"),
    }
}

/// Every typography row of the table parses its example into exactly its
/// own field, and the package writer reads back equal.
#[test]
fn typography_rows_round_trip() {
    let rows: Vec<_> = THEME_KEYS
        .iter()
        .filter(|spec| spec.group == ThemeKeyGroup::Typography)
        .collect();
    assert!(!rows.is_empty());
    for spec in rows {
        let parsed = with(&format!("{} = {}", spec.name, spec.example))
            .unwrap_or_else(|e| panic!("{}: {e}", spec.name));
        assert_eq!(parsed.typography.keys(), vec![spec.name], "{}", spec.name);
        assert!(parsed.typography.is_set(spec.key));
        for written in [
            to_native_conf_package(&parsed, &["a test"]),
            to_native_conf_package_commented(&parsed, &[]),
        ] {
            let back = parse_theme_str(&written).unwrap_or_else(|e| panic!("{e}\n{written}"));
            assert_eq!(back, parsed, "{written}");
        }
        let mut cleared = parsed.typography.clone();
        cleared.clear(spec.key);
        assert!(cleared.is_empty(), "{}", spec.name);
    }
}

/// A theme carrying every key (colours, schema, bold_is_bright and all
/// the typography) round-trips through both package writers.
#[test]
fn full_package_round_trips() {
    let mut text = String::from(BASE);
    text.push_str("schema = 1\nbold_is_bright = false\n");
    for spec in THEME_KEYS
        .iter()
        .filter(|spec| spec.group == ThemeKeyGroup::Typography)
    {
        text.push_str(&format!("{} = {}\n", spec.name, spec.example));
    }
    let parsed = parse_theme_str(&text).expect("parses");
    assert_eq!(parsed.schema, Some(1));
    assert_eq!(parsed.bold_is_bright, Some(false));
    for written in [
        to_native_conf_package(&parsed, &[]),
        to_native_conf_package_commented(&parsed, &[]),
    ] {
        assert_eq!(
            parse_theme_str(&written).expect("parses"),
            parsed,
            "{written}"
        );
    }
    let colours = crate::scheme_import::to_native_conf(
        &parsed.scheme,
        parsed.appearance_declared,
        None,
        None,
        &[],
    );
    assert!(
        parse_theme_str(&colours)
            .expect("parses")
            .typography
            .is_empty()
    );
}

/// A theme never names a file: each path-like or malformed name is a
/// parse error about its key, for every font-name key.
#[test]
fn font_names_never_name_files() {
    let long = "x".repeat(MAX_THEME_FONT_NAME_CHARS + 1);
    let bad = [
        "/Library/Fonts/X.ttf",
        "~/f",
        "C:\\f",
        "..\\f",
        "Mono.otf",
        "Mono.WOFF2",
        ".hidden",
        "a\u{7}b",
        long.as_str(),
    ];
    for key in ["font_family", "symbol_font", "fallback_fonts"] {
        for value in bad {
            let (got, reason) = error_key_reason(with(&format!("{key} = {value}")));
            assert_eq!(got.as_deref(), Some(key), "{key} = {value:?}: {reason}");
        }
        assert!(with(&format!("{key} =")).is_err());
        for good in ["Berkeley Mono", "display:pixel+chunky"] {
            assert!(with(&format!("{key} = {good}")).is_ok(), "{key} = {good}");
        }
    }
    let (_, reason) = error_key_reason(with("font_family = /Library/Fonts/X.ttf"));
    assert!(reason.contains(FONT_NAME_IS_PATH), "{reason}");
    assert!(with("fallback_fonts = Menlo, /tmp/x.ttf").is_err());
    assert!(with("fallback_fonts = Menlo,,Monaco").is_err());
    let nine = ["Menlo"; MAX_THEME_FONT_LIST + 1].join(", ");
    assert!(with(&format!("fallback_fonts = {nine}")).is_err());
    let eight = ["Menlo"; MAX_THEME_FONT_LIST].join(", ");
    assert!(with(&format!("fallback_fonts = {eight}")).is_ok());
}

/// The decimal and integer readers keep exactly their ranges, and each
/// refusal says the range.
#[test]
fn numeric_ranges_are_exact() {
    let ok = |line: &str| with(line).unwrap_or_else(|e| panic!("{line}: {e}"));
    let refused = |line: &str| {
        let (_, reason) = error_key_reason(with(line));
        assert!(reason.contains("allowed:"), "{line}: {reason}");
    };
    refused("line_height = 0.799");
    assert_eq!(
        ok("line_height = 0.8").typography.line_height_milli,
        Some(800)
    );
    assert_eq!(
        ok("line_height = 2.0").typography.line_height_milli,
        Some(2000)
    );
    assert_eq!(
        ok("line_height = 2").typography.line_height_milli,
        Some(2000)
    );
    refused("line_height = 2.001");
    for malformed in [
        "1e0", "+1.1", "nan", "inf", "1.1234", "1.", ".5", "-1", "1,2",
    ] {
        refused(&format!("line_height = {malformed}"));
    }
    assert_eq!(
        ok("font_size_scale = 1.25")
            .typography
            .font_size_scale_milli,
        Some(1250)
    );
    refused("font_size_scale = 1.251");
    refused("font_weight = 0");
    assert_eq!(ok("font_weight = 1").typography.font_weight, Some(1));
    assert_eq!(ok("font_weight = 1000").typography.font_weight, Some(1000));
    refused("font_weight = 1001");
    refused("font_weight = 400.5");
    for milli in 0..=9999u16 {
        assert_eq!(
            parse_milli(&milli_text(milli), 0, 9999),
            Ok(milli),
            "{milli}"
        );
    }
}

/// The feature reader accepts exactly specs the renderer's reader takes
/// (`FontFeature::parse_token`), and refuses malformed lists.
#[test]
fn feature_lists_match_text_shaping() {
    for (value, count) in [("calt, -liga, ss01=2", 3), ("+zero ss02", 2)] {
        let parsed = with(&format!("font_features = {value}")).expect(value);
        let specs = parsed.typography.font_features.expect("set");
        assert_eq!(specs.len(), count, "{value}");
        for spec in &specs {
            assert!(
                crate::text_shaping::FontFeature::parse_token(spec).is_some(),
                "{spec}"
            );
        }
    }
    let seventeen = ["calt"; MAX_THEME_FEATURES + 1].join(" ");
    for bad in [
        "toolong5",
        "calt,,ss02",
        "+ss01=2",
        "ca-t",
        "x=70000",
        &seventeen,
    ] {
        assert!(with(&format!("font_features = {bad}")).is_err(), "{bad}");
    }
    assert!(!is_feature_spec("+ss01=2"));
}

/// A newer schema skips the keys this aterm does not know and lists
/// them; every known key stays strict, and without a newer schema an
/// unknown key is still refused.
#[test]
fn schema_gate() {
    let parsed = with("schema = 2\nfuture_key = x").expect("a newer file loads");
    assert_eq!(parsed.schema, Some(2));
    assert_eq!(parsed.skipped_keys, vec!["future_key".to_string()]);
    assert_eq!(parsed.skipped_count, 1);
    let mut admitted = parsed.clone();
    let meta = ThemeMeta::admit(&mut admitted, "t", crate::Appearance::Dark);
    assert!(meta.warnings.contains(&ThemeWarning::SkippedNewerKeys {
        schema: 2,
        count: 1
    }));
    assert_eq!(meta.skipped_keys, vec!["future_key".to_string()]);
    for body in ["schema = 1\nfuture_key = x", "future_key = x"] {
        let (key, reason) = error_key_reason(with(body));
        assert_eq!(key.as_deref(), Some("future_key"));
        assert!(reason.contains("unknown key"), "{reason}");
    }
    assert!(with("schema = 2\nline_height = big").is_err());
    for bad in ["schema = 0", "schema = x", "schema = 1\nschema = 1"] {
        let (key, _) = error_key_reason(with(bad));
        assert_eq!(key.as_deref(), Some("schema"), "{bad}");
    }
    let mut body = String::from("schema = 9\n");
    for n in 0..40 {
        body.push_str(&format!("future_{n} = x\n"));
    }
    let many = with(&body).expect("loads");
    assert_eq!(many.skipped_keys.len(), MAX_SKIPPED_THEME_KEYS);
    assert_eq!(many.skipped_count, 40);
    let long = with(&format!("schema = 2\n{} = x", "k".repeat(200))).expect("loads");
    assert_eq!(long.skipped_keys[0].len(), MAX_SKIPPED_THEME_KEY_BYTES);
    let evil = with("schema = 999\n\x1b]0;pwned\x07\u{9b}2J = 1").expect("loads");
    assert_eq!(
        evil.skipped_keys,
        vec!["\\u{1b}]0;pwned\\u{7}\\u{9b}2j".to_string()]
    );
    assert!(!evil.skipped_keys[0].chars().any(char::is_control));
}

/// A typography key written twice is a duplicate naming the first line.
#[test]
fn duplicate_typography_keys() {
    let (key, reason) = error_key_reason(with("fallback_fonts = Menlo\nfallback_fonts = Monaco"));
    assert_eq!(key.as_deref(), Some("fallback_fonts"));
    assert!(reason.contains("line 3"), "{reason}");
}

/// DTP §7: no built-in carries typography or `bold_is_bright`, so a
/// built-in theme changes no type setting.
#[test]
fn builtins_have_no_typography() {
    for name in builtin_names() {
        let theme = CatalogTheme::builtin(name).expect("a built-in");
        assert!(theme.theme.typography.is_empty(), "{name}");
        assert_eq!(theme.theme.bold_is_bright, None, "{name}");
        let package = crate::scheme::ThemePackage::of_builtin(
            crate::scheme::builtin_theme(name).expect("identity"),
        )
        .expect("package");
        assert!(package.typography.is_empty());
        assert_eq!(package.colors, theme.theme.scheme);
    }
}

/// The summary names what is set and is `None` for nothing; the spoken
/// form carries no glyphs.
#[test]
fn summary_follows_the_fields() {
    assert_eq!(ThemeTypography::NONE.summary(), None);
    let parsed =
        with("font_family = Berkeley Mono\nline_height = 1.15\nemoji_font = Noto").expect("ok");
    let summary = parsed.typography.summary().expect("set");
    assert!(
        summary.contains("Berkeley Mono") && summary.contains("1.15"),
        "{summary}"
    );
    let spoken = parsed.typography.spoken().expect("set");
    assert!(!spoken.contains('×') && !spoken.contains('·'), "{spoken}");
    for spec in &THEME_KEYS {
        assert!(!spec.value.label().is_empty());
        if let ThemeValueType::Milli { min, max } = spec.value {
            assert!(min < max);
        }
    }
}
