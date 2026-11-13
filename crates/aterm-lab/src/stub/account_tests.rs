// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_routes_are_the_paths_claude_code_names_and_get_only() {
    assert_eq!(Route::of("GET", "/api/oauth/usage"), Some(Route::Usage));
    assert_eq!(Route::of("POST", "/api/oauth/usage"), None);
    assert_eq!(
        Route::of("GET", "/api/claude_cli_profile").map(Route::key),
        Some("profile")
    );
    assert_eq!(Route::of("GET", "/api/oauth/usage/x"), None);
    let q = |s: &str| crate::stub::parse_query(s);
    assert_eq!(usage_read(&q("at_wall=1&skip_spend=1")), "at_wall");
    assert_eq!(usage_read(&q("cedar_ember=1&skip_spend=1")), "cedar_ember");
    assert_eq!(usage_read(&q("")), "plain");
    assert_eq!(usage_read(&q("at_wall=0")), "plain");
}

#[test]
fn instants_expand_everywhere_and_nothing_else_moves() {
    let spec = J::parse(r#"{"a":"@ISO+60@","b":["@NOW+5@",{"c":"@ISO+x@"}],"d":"@ISO+60","e":7}"#)
        .unwrap();
    let out = expand(&spec, 1_792_000_000);
    assert_eq!(
        out.py(),
        r#"{"a": "2026-10-14T17:47:40.000Z", "b": [1792000005, {"c": "@ISO+x@"}], "d": "@ISO+60", "e": 7}"#
    );
}

#[test]
fn the_composed_account_names_no_one_and_carries_every_block() {
    let c = composed();
    let text = c.py();
    for block in [
        "five_hour",
        "seven_day",
        "limits",
        "extra_usage",
        "wattle_ember",
        "cinder_cove",
        "juniper_tide",
        "cedar_ember",
        "oauth_account",
    ] {
        assert!(text.contains(&format!("\"{block}\"")), "{block}");
    }
    let emails: Vec<&str> = text
        .split('"')
        .filter(|w| w.contains('@') && !w.starts_with("@ISO+"))
        .collect();
    assert!(
        emails.iter().all(|e| e.ends_with("@example.com")),
        "{emails:?}"
    );
    let fable = c
        .get("usage")
        .and_then(|u| u.get("limits"))
        .and_then(J::as_arr)
        .and_then(|l| {
            l.iter()
                .find(|r| r.get("kind").and_then(J::as_str) == Some("weekly_scoped"))
        })
        .and_then(|r| r.get("scope"))
        .and_then(|s| s.get("model"))
        .and_then(|m| m.get("display_name"))
        .and_then(J::as_str);
    assert_eq!(fable, Some("Fable"));
}
