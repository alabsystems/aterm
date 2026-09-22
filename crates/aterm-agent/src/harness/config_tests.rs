// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the harness's own `config.toml` (design §3.7, §5.8.7).
//!
//! Every test that asserts a REFUSAL carries the matching admission beside
//! it: a registry that refused everything would pass a refusal-only suite.

use super::*;

fn parsed(text: &str) -> Result<HarnessConfig, String> {
    HarnessConfig::parse(text, "config.toml")
}

#[test]
fn an_absent_file_is_the_shipped_default_and_not_an_error() {
    let dir = std::env::temp_dir().join(format!("aterm-harness-config-{}", std::process::id()));
    let missing = dir.join("nothing-here").join(CONFIG_FILE);
    assert_eq!(HarnessConfig::load(&missing), Ok(HarnessConfig::default()));
    // NEGATIVE CONTROL: a file that EXISTS and is outside the grammar is a
    // refusal, so the `Ok` above is not "load never fails".
    assert!(parsed("cap.limits.level = 3\n").is_err());
}

#[test]
fn the_canonical_emission_round_trips_through_the_reader() {
    let cfg = HarnessConfig::default();
    let text = cfg.to_toml();
    assert_eq!(parsed(&text), Ok(cfg.clone()), "{text}");
    // And a CHANGED value survives the same round trip.
    let mut changed = cfg;
    changed
        .set("cap.limits.level", "4")
        .expect("level 4 is inside the bound");
    changed
        .set(
            "cap.limits.actions.session-5h-limit",
            "[\"wait\",\"escalate\"]",
        )
        .expect("wait then escalate is inside the 5h class's allowed set");
    let text = changed.to_toml();
    assert_eq!(parsed(&text), Ok(changed), "{text}");
}

#[test]
fn every_registry_key_reads_back_and_an_unknown_one_is_refused_by_name() {
    let cfg = HarnessConfig::default();
    for key in key_names() {
        assert!(cfg.get(&key).is_some(), "{key} has no reader");
    }
    // NEGATIVE CONTROL: near-misses and a plausible invention.
    for bad in [
        "cap.limits.levl",
        "cap.limits",
        "harness.enabled",
        "cap.limits.actions.no-such-class",
        "",
    ] {
        assert_eq!(cfg.get(bad), None, "{bad} answered");
        assert!(cfg.clone().set(bad, "true").is_err(), "{bad} was written");
    }
}

#[test]
fn the_per_class_allowed_sets_are_enforced_at_set_exactly_as_5_8_7_asks() {
    let base = HarnessConfig::default();
    // The four REFUSALS the task names, each against the class the design
    // pins. The value must be unchanged after every one.
    let refused: &[(&str, &str)] = &[
        (
            "cap.limits.actions.network-offline",
            "[\"switch-model\",\"escalate\"]",
        ),
        (
            "cap.limits.actions.unknown",
            "[\"switch-account\",\"escalate\"]",
        ),
        (
            "cap.limits.actions.model-bucket-limit",
            "[\"switch-model\",\"escalate\"]",
        ),
        ("cap.limits.actions.auth", "[\"retry\",\"escalate\"]"),
        (
            "cap.limits.actions.spend-billing",
            "[\"wait\",\"escalate\"]",
        ),
        ("cap.limits.actions.unknown", "[\"retry\",\"escalate\"]"),
    ];
    for (key, value) in refused {
        let mut cfg = base.clone();
        let before = cfg.get(key).expect("a registry key");
        let err = cfg
            .set(key, value)
            .expect_err("{key} = {value} must refuse");
        assert!(err.contains(key), "{err}");
        assert_eq!(cfg, base, "a refused set changed the value: {key}");
        assert_eq!(cfg.get(key).as_deref(), Some(before.as_str()));
    }
    // POSITIVE CONTROL: the reorder the design's own sentence names, plus
    // the two members each pinned class does admit.
    let mut cfg = base.clone();
    cfg.set(
        "cap.limits.actions.session-5h-limit",
        "[\"wait\",\"escalate\"]",
    )
    .expect("the design's own example");
    cfg.set("cap.limits.actions.auth", "[\"relogin\",\"escalate\"]")
        .expect("auth admits relogin");
    cfg.set("cap.limits.actions.spend-billing", "[\"escalate\"]")
        .expect("spend-billing is pinned to escalate");
    cfg.set("cap.limits.actions.network-offline", "wait,escalate")
        .expect("the unbracketed spelling names the same row");
    assert_ne!(cfg, base);
}

#[test]
fn a_file_carrying_a_forbidden_row_is_refused_whole() {
    let text = "[cap.limits.actions]\nunknown = [\"retry\", \"escalate\"]\n";
    let err = parsed(text).expect_err("retry is not in unknown's allowed set");
    assert!(err.contains("unknown"), "{err}");
    // POSITIVE CONTROL: the same file with the allowed row admits.
    let ok = parsed("[cap.limits.actions]\nunknown = [\"escalate\"]\n")
        .expect("escalate alone is the pinned row");
    assert_eq!(ok, HarnessConfig::default());
}

#[test]
fn values_outside_their_type_or_bound_are_refused_and_inside_are_taken() {
    let mut cfg = HarnessConfig::default();
    assert!(
        cfg.set("cap.limits.level", "5").is_err(),
        "level 5 is above 4"
    );
    assert!(cfg.set("cap.limits.level", "yes").is_err());
    assert!(
        cfg.set("cap.limits.enabled", "1").is_err(),
        "1 is not a bool here"
    );
    assert!(cfg.set("cap.limits.budget", "4 per 6h").is_err());
    assert!(cfg.set("cap.limits.retry_text", "resume").is_err());
    assert!(cfg.set("cap.liveness.level", "shout").is_err());
    assert_eq!(
        cfg,
        HarnessConfig::default(),
        "a refused set wrote something"
    );
    // The admitting forms, quoted and bare.
    cfg.set("cap.limits.level", "4").expect("4");
    cfg.set("cap.limits.enabled", "false").expect("false");
    cfg.set("cap.limits.budget", "\"2/30m\"")
        .expect("a quoted budget");
    cfg.set("cap.limits.retry_text", "last-prompt")
        .expect("last-prompt");
    cfg.set("cap.liveness.level", "escape").expect("escape");
    assert_eq!(cfg.get("cap.limits.level").as_deref(), Some("4"));
    assert_eq!(cfg.get("cap.limits.budget").as_deref(), Some("\"2/30m\""));
    assert_eq!(cfg.get("cap.liveness.level").as_deref(), Some("\"escape\""));
}

#[test]
fn a_timer_pair_out_of_order_is_refused_and_the_value_is_untouched() {
    let mut cfg = HarnessConfig::default();
    let before = cfg.clone();
    let err = cfg
        .set("cap.limits.transient_t1_s", "86400")
        .expect_err("t1 above t2 breaks LimitsConfig::validate");
    assert!(err.contains("t1 > t2"), "{err}");
    assert_eq!(cfg, before);
    // POSITIVE CONTROL: raising t2 first admits the same t1.
    cfg.set("cap.limits.transient_t2_s", "86400")
        .expect("t2 first");
    cfg.set("cap.limits.transient_t1_s", "86400")
        .expect("then t1");
}

#[test]
fn a_key_before_any_table_and_an_array_table_are_both_refused() {
    assert!(parsed("enabled = true\n").is_err(), "a rootless key");
    assert!(
        parsed("[[cap.limits]]\nenabled = true\n").is_err(),
        "an array table"
    );
    // POSITIVE CONTROL: the same key under its table admits.
    let cfg = parsed("[cap.limits]\nenabled = false\n").expect("under its table");
    assert!(!cfg.limits.enabled);
}

#[test]
fn a_value_of_the_wrong_shape_names_what_the_key_wants() {
    let err = parsed("[cap.limits]\nlevel = \"three\"\n").expect_err("a string level");
    assert!(err.contains("whole number"), "{err}");
    let err = parsed("[cap.limits]\nbudget = 4\n").expect_err("a bare integer budget");
    assert!(err.contains("budget"), "{err}");
}

#[test]
fn rotation_consent_is_not_a_key_here_it_lives_beside_the_roster() {
    // §4.6.2's one-home rule: `[accounts] enabled` is `accounts.toml`'s, and
    // this file refuses it BY NAME rather than carrying a second copy that
    // nothing compares with the first.
    let err = parsed("[accounts]\nenabled = true\n").expect_err("a second home");
    assert!(err.contains("accounts.enabled"), "{err}");
    assert!(HarnessConfig::default().get("accounts.enabled").is_none());
    // NEGATIVE CONTROL: the keys this file DOES own still admit.
    assert!(parsed("[cap.limits]\nenabled = true\n").is_ok());
}
