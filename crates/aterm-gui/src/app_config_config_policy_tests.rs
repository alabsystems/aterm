// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Config;

fn cfg(source: &str) -> Config {
    aterm_toml::from_str(source).expect("valid config")
}

#[test]
fn a_config_that_still_sets_smart_title_keys_loads_with_every_other_setting() {
    const RETIRED: &[(&str, &str)] = &[
        ("descriptive_titles", "true"),
        ("title_summary_provider", "\"ollama\""),
        ("title_summary_model", "\"qwen3.5:4b-q4_K_M\""),
        (
            "title_summary_endpoint",
            "\"http://127.0.0.1:11434/api/chat\"",
        ),
        ("title_summary_token_file", "\"/tmp/aterm-summary-token\""),
        ("title_summary_timeout_seconds", "42"),
        ("title_summary_proxy_mode", "\"direct\""),
        ("title_summary_ca_file", "\"/tmp/private-model-ca.pem\""),
        ("title_summary_interval_seconds", "15"),
        ("title_summary_context_lines", "24"),
        ("title_summary_include_output", "false"),
        ("title_summary_allow_remote", "true"),
        ("tab_title_format", "\"description\""),
        ("window_title_format", "\"description-title\""),
    ];
    let others = concat!(
        "theme = \"Nord\"\n",
        "font_px = 17.0\n",
        "scrollback_lines = 4321\n",
        "tab_status_badge = false\n",
        "[harness]\n",
        "limit_reset = true\n",
    );
    let retired: String = RETIRED
        .iter()
        .map(|(key, value)| format!("{key} = {value}\n"))
        .collect();
    let source = format!("{retired}{others}");

    let loaded = Config::parse(&source).expect("retired keys never fail the file");
    let baseline = Config::parse(others).expect("the other settings parse alone");
    assert!(
        loaded == baseline,
        "every other setting loads exactly as it would without the retired keys"
    );
    assert!(
        loaded != Config::stand_in(&source),
        "the file is not replaced by the stand-in defaults"
    );
    assert_eq!(loaded.theme.as_deref(), Some("Nord"));
    assert_eq!(loaded.font_px, Some(17.0));
    assert_eq!(loaded.scrollback_lines, Some(4321));
    assert_eq!(loaded.tab_status_badge, Some(false));

    let warnings = crate::native_config_language::key_warnings(&source);
    assert!(warnings.ignored.is_empty(), "{:?}", warnings.ignored);
    assert_eq!(
        warnings.retired.len(),
        RETIRED.len(),
        "{:?}",
        warnings.retired
    );
    for ((line, (key, _)), number) in warnings.retired.iter().zip(RETIRED).zip(1..) {
        assert_eq!(
            line,
            &format!("line {number}: {key} does nothing: Smart Titles was removed; safe to delete")
        );
    }
    assert!(
        super::unaccepted_value_notices(&source, &[]).is_empty(),
        "a retired key's value is never reported as a value its reader rejects"
    );
}

#[test]
fn a_config_that_still_sets_the_deck_style_loads_and_is_told_once() {
    let others = "[sys]\nstats_bars = \"deck\"\nstats_deck_size = \"full\"\n";
    let source =
        "[sys]\nstats_bars = \"deck\"\nstats_deck_style = \"merged\"\nstats_deck_size = \"full\"\n";
    let loaded = Config::parse(source).expect("a retired key never fails the file");
    let baseline = Config::parse(others).expect("the other settings parse alone");
    assert!(
        loaded == baseline,
        "every other setting loads as without it"
    );
    assert_eq!(
        loaded.stats_deck_size_or_default(),
        crate::stats_plan::DeckSizePref::Full,
        "the size setting beside it stands"
    );
    let warnings = crate::native_config_language::key_warnings(source);
    assert!(warnings.ignored.is_empty(), "{:?}", warnings.ignored);
    assert_eq!(
        warnings.retired,
        ["line 3: sys.stats_deck_style does nothing: Merged top deck was removed; safe to delete"],
    );
    assert!(
        super::unaccepted_value_notices(source, &[]).is_empty(),
        "a retired key's value is never a value its reader rejects"
    );
}

/// A `desktop_alerts` line left in a person's file, whatever its value (a word no `bool`
/// reads included), loads with every other setting, is told once that the key does nothing,
/// and `--validate-config` passes with that one warning; `settings set` and `unset` answer
/// the same sentence.
#[test]
fn a_config_that_still_sets_desktop_alerts_loads_and_is_told_once() {
    use crate::native_config_language::compatibility_only_message;
    let others = "font_px = 17.0\n";
    let told = format!("line 1: {}", compatibility_only_message("desktop_alerts"));
    for value in ["true", "false", "\"on\""] {
        let source = format!("desktop_alerts = {value}\n{others}");
        let loaded = Config::parse(&source).expect("a retired key never fails the file");
        let baseline = Config::parse(others).expect("the other settings parse alone");
        assert!(
            loaded == baseline,
            "{source:?}: every other setting loads as without it"
        );
        let warnings = crate::native_config_language::key_warnings(&source);
        assert!(
            warnings.ignored.is_empty(),
            "{source:?}: {:?}",
            warnings.ignored
        );
        assert_eq!(warnings.retired, std::slice::from_ref(&told), "{source:?}");
        assert_eq!(
            crate::diagnostics::validate_config_text(&source),
            Ok(vec![told.clone()]),
            "{source:?}: the file validates, with the one warning"
        );
        assert!(
            super::unaccepted_value_notices(&source, &[]).is_empty(),
            "{source:?}: a retired key's value is never a value its reader rejects"
        );
    }
    assert_eq!(
        crate::native_config_language::control_settings_refusal("desktop_alerts"),
        compatibility_only_message("desktop_alerts"),
    );
    assert!(told.contains("was removed"), "{told}");
}

#[test]
fn tab_status_policy_clamps_both_ends() {
    assert_eq!(
        cfg("tab_status_quiet_after_ms = 0").tab_status_quiet_after_ms_or_default(),
        500
    );
    assert_eq!(
        cfg("tab_status_quiet_after_ms = 999999").tab_status_quiet_after_ms_or_default(),
        120_000
    );
    assert_eq!(
        cfg("tab_status_dwell_ms = 0").tab_status_dwell_ms_or_default(),
        0
    );
    assert_eq!(
        cfg("tab_status_dwell_ms = 999999").tab_status_dwell_ms_or_default(),
        10_000
    );
}

#[test]
fn the_observation_interval_tracks_dwell_between_a_floor_and_the_budget() {
    let ms = |toml: &str| cfg(toml).tab_status_observe_interval().as_millis();
    assert_eq!(
        Config::default().tab_status_observe_interval().as_millis(),
        250
    );
    assert_eq!(ms("tab_status_dwell_ms = 0"), 50, "floored, never zero");
    assert_eq!(ms("tab_status_dwell_ms = 100"), 100, "tracks a short dwell");
    assert_eq!(
        ms("tab_status_dwell_ms = 9000"),
        250,
        "capped at the budget"
    );
    for dwell in [0_u64, 1, 100, 750, 5_000, 10_000, 999_999] {
        let config = cfg(&format!("tab_status_dwell_ms = {dwell}"));
        let interval = config.tab_status_observe_interval().as_millis();
        let resolved = u128::from(config.tab_status_dwell_ms_or_default());
        assert!(
            interval <= resolved.max(50),
            "interval {interval} outruns dwell {resolved}"
        );
    }
}
