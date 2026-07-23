// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for the smart selection system.

use super::*;

#[test]
fn test_smart_selection_with_builtin_rules() {
    let smart = SmartSelection::with_builtin_rules();
    // BuiltinRules::all() defines 11 rules (url, file_path, email, ipv4, ipv6,
    // git_hash, double_quoted_string, single_quoted_string, backtick_quoted_string,
    // uuid, semver)
    assert_eq!(
        smart.rules().len(),
        11,
        "builtin rules should include all 11 known rule types"
    );
    // Verify key rules are present by name
    let rule_names: Vec<&str> = smart.rules().iter().map(|r| r.name()).collect();
    assert!(rule_names.contains(&"url"), "should contain url rule");
    assert!(rule_names.contains(&"email"), "should contain email rule");
    assert!(
        rule_names.contains(&"file_path"),
        "should contain file_path rule"
    );
}

#[test]
fn test_find_url_at_position() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "Check out https://example.com for more info";

    // Position in the middle of the URL
    let m = smart.find_at(text, 15).unwrap();
    assert_eq!(m.matched_text(), "https://example.com");
    assert_eq!(m.rule_name(), "url");
    assert_eq!(m.kind(), SelectionRuleKind::Url);
}

#[test]
fn test_find_email_at_position() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "Contact support@example.com for help";

    let m = smart.find_at(text, 12).unwrap();
    assert_eq!(m.matched_text(), "support@example.com");
    assert_eq!(m.rule_name(), "email");
}

#[test]
fn test_find_file_path_at_position() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "Edit ./config.yaml to fix";

    let m = smart.find_at(text, 10).unwrap();
    assert_eq!(m.matched_text(), "./config.yaml");
    assert_eq!(m.rule_name(), "file_path");
}

#[test]
fn test_find_git_hash_at_position() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "Reverted commit abc1234def that broke things";

    let m = smart.find_at(text, 20).unwrap();
    assert_eq!(m.matched_text(), "abc1234def");
    assert_eq!(m.rule_name(), "git_hash");
}

#[test]
fn test_find_ipv4_at_position() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "Server 192.168.1.1 is responding";

    let m = smart.find_at(text, 10).unwrap();
    assert_eq!(m.matched_text(), "192.168.1.1");
    assert_eq!(m.rule_name(), "ipv4");
}

#[test]
fn test_find_quoted_string_at_position() {
    let smart = SmartSelection::with_builtin_rules();
    let text = r#"Run command "git status" now"#;

    let m = smart.find_at(text, 15).unwrap();
    assert_eq!(m.matched_text(), r#""git status""#);
    assert_eq!(m.kind(), SelectionRuleKind::QuotedString);
}

#[test]
fn test_find_uuid_at_position() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "Resource 550e8400-e29b-41d4-a716-446655440000 not found";

    let m = smart.find_at(text, 20).unwrap();
    assert_eq!(m.matched_text(), "550e8400-e29b-41d4-a716-446655440000");
    assert_eq!(m.rule_name(), "uuid");
}

#[test]
fn test_no_match_at_position() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "just some regular words";

    // Position on whitespace
    let m = smart.find_at(text, 4);
    assert!(m.is_none(), "whitespace position should not match any rule");
}

#[test]
fn test_find_all_matches() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "Check https://example.com and user@test.com";

    let matches = smart.find_all(text);

    // Check that we have at least the URL and email
    let url_matches: Vec<_> = matches.iter().filter(|m| m.rule_name() == "url").collect();
    let email_matches: Vec<_> = matches
        .iter()
        .filter(|m| m.rule_name() == "email")
        .collect();

    assert_eq!(url_matches.len(), 1);
    assert_eq!(email_matches.len(), 1);

    // URL should come before email in sorted order
    let url_pos = matches.iter().position(|m| m.rule_name() == "url").unwrap();
    let email_pos = matches
        .iter()
        .position(|m| m.rule_name() == "email")
        .unwrap();
    assert!(url_pos < email_pos);
}

#[test]
fn test_disable_rule() {
    let mut smart = SmartSelection::with_builtin_rules();
    let text = "Check https://example.com";

    // URL should match initially
    let m = smart
        .find_at(text, 10)
        .expect("URL should match at position 10");
    assert_eq!(m.rule_name(), "url");

    // Disable URL rule
    smart.set_rule_enabled("url", false);
    assert!(
        smart.find_at(text, 10).is_none(),
        "disabled rule should not match"
    );

    // Re-enable
    smart.set_rule_enabled("url", true);
    let m = smart
        .find_at(text, 10)
        .expect("URL should match after re-enable");
    assert_eq!(m.rule_name(), "url");
}

#[test]
fn test_custom_rule() {
    let mut smart = SmartSelection::new();

    // Add a custom rule for GitHub issue references
    let issue_rule = SelectionRule::new("github_issue", SelectionRuleKind::Custom, r"#\d+");
    smart.add_rule(issue_rule);

    let text = "Fixed in #123 and #456";

    let matches = smart.find_all(text);
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].matched_text(), "#123");
    assert_eq!(matches[1].matched_text(), "#456");
}

#[test]
fn test_priority_ordering() {
    let mut smart = SmartSelection::new();

    // Add low priority rule first
    let low = SelectionRule::new("low", SelectionRuleKind::Custom, r"\d+")
        .with_priority(RulePriority::Low);
    smart.add_rule(low);

    // Add high priority rule
    let high = SelectionRule::new("high", SelectionRuleKind::Custom, r"\d{3}")
        .with_priority(RulePriority::High);
    smart.add_rule(high);

    // Rules should be sorted by priority
    assert_eq!(smart.rules()[0].name(), "high");
    assert_eq!(smart.rules()[1].name(), "low");
}

#[test]
fn test_word_boundaries_with_match() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "Check https://example.com please";

    // On URL - should return URL boundaries
    let bounds = smart.word_boundaries_at(text, 15);
    assert_eq!(bounds, Some((6, 25)));
}

#[test]
fn test_word_boundaries_fallback() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "hello world";

    // On regular word - should return word boundaries
    let bounds = smart.word_boundaries_at(text, 2);
    assert_eq!(bounds, Some((0, 5)));
}

#[test]
fn test_find_at_column() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "URL: https://example.com here";

    // Column 10 should be in the URL
    let m = smart.find_at_column(text, 10).unwrap();
    assert_eq!(m.rule_name(), "url");
}

#[test]
fn test_selection_match_properties() {
    let m = SelectionMatch::new("test", 5, 9, "custom", SelectionRuleKind::Custom);

    assert_eq!(m.matched_text(), "test");
    assert_eq!(m.start(), 5);
    assert_eq!(m.end(), 9);
    assert_eq!(m.len(), 4);
    assert_eq!(m.rule_name(), "custom");
    assert_eq!(m.kind(), SelectionRuleKind::Custom);
}

#[test]
#[should_panic(expected = "SelectionMatch::new: end (3) must be >= start (5)")]
fn test_selection_match_invalid_range_panics() {
    let _ = SelectionMatch::new("bad", 5, 3, "custom", SelectionRuleKind::Custom);
}

#[test]
fn test_has_match_at() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "URL: https://example.com here";

    assert!(smart.has_match_at(text, 10));
    assert!(!smart.has_match_at(text, 0));
}

#[test]
fn test_rule_kind_name() {
    assert_eq!(SelectionRuleKind::Url.name(), "url");
    assert_eq!(SelectionRuleKind::FilePath.name(), "file_path");
    assert_eq!(SelectionRuleKind::Email.name(), "email");
    assert_eq!(SelectionRuleKind::IpAddress.name(), "ip_address");
    assert_eq!(SelectionRuleKind::GitHash.name(), "git_hash");
    assert_eq!(SelectionRuleKind::QuotedString.name(), "quoted_string");
    assert_eq!(SelectionRuleKind::Uuid.name(), "uuid");
    assert_eq!(SelectionRuleKind::SemVer.name(), "semver");
    assert_eq!(SelectionRuleKind::Custom.name(), "custom");
}

#[test]
fn test_url_edge_cases() {
    let smart = SmartSelection::with_builtin_rules();

    // URL at end of sentence
    let text = "Visit https://example.com.";
    let m = smart.find_at(text, 10).unwrap();
    assert_eq!(m.matched_text(), "https://example.com");

    // URL in parentheses
    let text = "(see https://example.com)";
    let m = smart.find_at(text, 10).unwrap();
    assert_eq!(m.matched_text(), "https://example.com");

    // URL with query string
    let text = "Link: https://example.com/path?q=1&b=2";
    let m = smart.find_at(text, 10).unwrap();
    assert_eq!(m.matched_text(), "https://example.com/path?q=1&b=2");
}

#[test]
fn test_file_path_edge_cases() {
    let smart = SmartSelection::with_builtin_rules();

    // Relative path with parent
    let text = "Check ../config/app.yaml file";
    let m = smart.find_at(text, 10).unwrap();
    assert_eq!(m.matched_text(), "../config/app.yaml");
}

#[test]
fn test_multiple_matches_same_position() {
    // This tests that higher priority rules win
    let mut smart = SmartSelection::new();

    // A URL is also a valid "text" by some other rule
    let text_rule = SelectionRule::new("text", SelectionRuleKind::Custom, r"https?://\S+")
        .with_priority(RulePriority::Low);

    smart.add_rule(BuiltinRules::url());
    smart.add_rule(text_rule);

    let text = "See https://example.com here";
    let m = smart.find_at(text, 10).unwrap();

    // URL rule has higher priority, should match first
    assert_eq!(m.rule_name(), "url");
}

#[test]
fn test_empty_text() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "";

    assert!(
        smart.find_at(text, 0).is_none(),
        "empty text should have no matches"
    );
    assert!(smart.find_all(text).is_empty());
}

#[test]
fn test_position_past_end() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "short";

    assert!(
        smart.find_at(text, 100).is_none(),
        "position past end should return None"
    );
}

#[test]
fn test_get_rule() {
    let smart = SmartSelection::with_builtin_rules();

    let rule = smart.get_rule("url").expect("url rule should exist");
    assert_eq!(rule.kind(), SelectionRuleKind::Url);

    assert!(
        smart.get_rule("nonexistent").is_none(),
        "nonexistent rule should return None"
    );
}

#[test]
fn test_rule_pattern_accessor() {
    let rule = BuiltinRules::url();
    assert!(rule.pattern().contains("https?"));
}

#[test]
fn test_backtick_quoted() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "Run `git status` command";

    let m = smart.find_at(text, 6).unwrap();
    assert_eq!(m.matched_text(), "`git status`");
}

#[test]
fn test_single_quoted() {
    let smart = SmartSelection::with_builtin_rules();
    let text = "Set value='hello' here";

    let m = smart.find_at(text, 12).unwrap();
    assert_eq!(m.matched_text(), "'hello'");
}

#[test]
fn test_semver_variations() {
    let rule = BuiltinRules::semver();

    // Simple version
    let text = "version 1.2.3 released";
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches[0].as_str(), "1.2.3");

    // With v prefix
    let text = "version v2.0.0 released";
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches[0].as_str(), "v2.0.0");

    // With pre-release
    let text = "version 1.0.0-alpha released";
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches[0].as_str(), "1.0.0-alpha");
}

#[test]
fn test_complex_email() {
    let rule = BuiltinRules::email();

    let text = "Contact john.doe+test@sub.example.co.uk for info";
    let matches: Vec<_> = rule.find_all(text).collect();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].as_str(), "john.doe+test@sub.example.co.uk");
}
