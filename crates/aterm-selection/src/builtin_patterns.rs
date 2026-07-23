// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Built-in selection rule patterns.
//!
//! Regex patterns for recognizing semantic text units: URLs, file paths,
//! email addresses, IP addresses, git hashes, quoted strings, UUIDs,
//! and semantic versions.

use super::rules::{RulePriority, SelectionRule, SelectionRuleKind};

/// Built-in rule patterns.
pub(crate) struct BuiltinRules;

impl BuiltinRules {
    // URL pattern: matches http, https, ftp, file URLs
    // Captures the protocol and the rest of the URL
    // Uses non-greedy matching to avoid capturing trailing punctuation
    const URL_PATTERN: &'static str =
        r#"(?i)(?:https?|ftp|file)://[^\s<>\[\](){}'"`,;|\\^]+[^\s<>\[\](){}'"`,;|\\^.!?:]"#;

    // File path pattern: matches absolute and relative paths
    // Unix: /path/to/file, ./relative, ../parent
    // Windows: C:\path\to\file, .\relative
    const FILE_PATH_PATTERN: &'static str = r"(?:/(?:[a-zA-Z0-9._-]+/)*[a-zA-Z0-9._-]+|\.{1,2}/(?:[a-zA-Z0-9._-]+/)*[a-zA-Z0-9._-]+|[A-Za-z]:[/\\](?:[a-zA-Z0-9._-]+[/\\])*[a-zA-Z0-9._-]+)";

    // Email pattern: RFC 5321 compliant (simplified)
    const EMAIL_PATTERN: &'static str = r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]*[a-zA-Z0-9])?)*\.[a-zA-Z]{2,}";

    // IPv4 pattern with optional port
    const IPV4_PATTERN: &'static str = r"(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)\.(?:25[0-5]|2[0-4][0-9]|[01]?[0-9][0-9]?)(?::\d{1,5})?";

    // IPv6 pattern (simplified - matches most common formats)
    const IPV6_PATTERN: &'static str = r"\[?(?:(?:[0-9a-fA-F]{1,4}:){7}[0-9a-fA-F]{1,4}|(?:[0-9a-fA-F]{1,4}:){1,7}:|(?:[0-9a-fA-F]{1,4}:){1,6}:[0-9a-fA-F]{1,4}|::(?:[0-9a-fA-F]{1,4}:){0,5}[0-9a-fA-F]{1,4}|::)\]?(?::\d{1,5})?";

    // Git hash pattern: 7-40 hex characters
    // Uses word boundaries to avoid matching partial hex strings
    // Note: This may match within larger hex strings at word boundaries
    const GIT_HASH_PATTERN: &'static str = r"\b[0-9a-fA-F]{7,40}\b";

    // Double-quoted string
    const DOUBLE_QUOTED_PATTERN: &'static str = r#""(?:[^"\\]|\\.)*""#;

    // Single-quoted string
    const SINGLE_QUOTED_PATTERN: &'static str = r"'(?:[^'\\]|\\.)*'";

    // Backtick-quoted string (common in markdown and shells)
    const BACKTICK_QUOTED_PATTERN: &'static str = r"`(?:[^`\\]|\\.)*`";

    // UUID pattern (8-4-4-4-12 hex format)
    const UUID_PATTERN: &'static str =
        r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}";

    // Semantic version pattern
    const SEMVER_PATTERN: &'static str = r"v?\d+\.\d+\.\d+(?:-[a-zA-Z0-9]+(?:\.[a-zA-Z0-9]+)*)?(?:\+[a-zA-Z0-9]+(?:\.[a-zA-Z0-9]+)*)?";

    /// Create the URL selection rule.
    #[must_use]
    pub(crate) fn url() -> SelectionRule {
        SelectionRule::new("url", SelectionRuleKind::Url, Self::URL_PATTERN)
            .with_priority(RulePriority::High)
    }

    /// Create the file path selection rule.
    #[must_use]
    pub(crate) fn file_path() -> SelectionRule {
        SelectionRule::new(
            "file_path",
            SelectionRuleKind::FilePath,
            Self::FILE_PATH_PATTERN,
        )
    }

    /// Create the email selection rule.
    #[must_use]
    pub(crate) fn email() -> SelectionRule {
        SelectionRule::new("email", SelectionRuleKind::Email, Self::EMAIL_PATTERN)
            .with_priority(RulePriority::High)
    }

    /// Create the IPv4 address selection rule.
    #[must_use]
    pub(crate) fn ipv4() -> SelectionRule {
        SelectionRule::new("ipv4", SelectionRuleKind::IpAddress, Self::IPV4_PATTERN)
    }

    /// Create the IPv6 address selection rule.
    #[must_use]
    pub(crate) fn ipv6() -> SelectionRule {
        SelectionRule::new("ipv6", SelectionRuleKind::IpAddress, Self::IPV6_PATTERN)
            .with_priority(RulePriority::Low)
    }

    /// Create the git hash selection rule.
    #[must_use]
    pub(crate) fn git_hash() -> SelectionRule {
        SelectionRule::new(
            "git_hash",
            SelectionRuleKind::GitHash,
            Self::GIT_HASH_PATTERN,
        )
    }

    /// Create the double-quoted string selection rule.
    #[must_use]
    pub(crate) fn double_quoted_string() -> SelectionRule {
        SelectionRule::new(
            "double_quoted",
            SelectionRuleKind::QuotedString,
            Self::DOUBLE_QUOTED_PATTERN,
        )
    }

    /// Create the single-quoted string selection rule.
    #[must_use]
    pub(crate) fn single_quoted_string() -> SelectionRule {
        SelectionRule::new(
            "single_quoted",
            SelectionRuleKind::QuotedString,
            Self::SINGLE_QUOTED_PATTERN,
        )
    }

    /// Create the backtick-quoted string selection rule.
    #[must_use]
    pub(crate) fn backtick_quoted_string() -> SelectionRule {
        SelectionRule::new(
            "backtick_quoted",
            SelectionRuleKind::QuotedString,
            Self::BACKTICK_QUOTED_PATTERN,
        )
    }

    /// Create the UUID selection rule.
    ///
    /// High priority so the full 8-4-4-4-12 UUID outranks the generic
    /// `git_hash` rule, whose `[0-9a-fA-F]{7,40}` pattern would otherwise
    /// cannibalize a UUID's 8- and 12-hex groups at equal priority and
    /// truncate the selection to that segment.
    #[must_use]
    pub(crate) fn uuid() -> SelectionRule {
        SelectionRule::new("uuid", SelectionRuleKind::Uuid, Self::UUID_PATTERN)
            .with_priority(RulePriority::High)
    }

    /// Create the semantic version selection rule.
    #[must_use]
    pub(crate) fn semver() -> SelectionRule {
        SelectionRule::new("semver", SelectionRuleKind::SemVer, Self::SEMVER_PATTERN)
            .with_priority(RulePriority::Low)
    }

    /// Get all built-in rules.
    #[must_use]
    pub(crate) fn all() -> Vec<SelectionRule> {
        vec![
            Self::url(),
            Self::file_path(),
            Self::email(),
            Self::ipv4(),
            Self::ipv6(),
            Self::git_hash(),
            Self::double_quoted_string(),
            Self::single_quoted_string(),
            Self::backtick_quoted_string(),
            Self::uuid(),
            Self::semver(),
        ]
    }
}
