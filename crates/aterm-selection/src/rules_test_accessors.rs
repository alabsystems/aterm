// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::{SelectionRule, SelectionRuleKind, SmartSelection};

impl SelectionRuleKind {
    /// Get a human-readable name for this kind.
    #[must_use]
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::FilePath => "file_path",
            Self::Email => "email",
            Self::IpAddress => "ip_address",
            Self::GitHash => "git_hash",
            Self::QuotedString => "quoted_string",
            Self::Uuid => "uuid",
            Self::SemVer => "semver",
            Self::Custom => "custom",
        }
    }
}

impl SelectionRule {
    /// Get the rule name.
    #[must_use]
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Get the rule kind.
    #[must_use]
    pub(crate) fn kind(&self) -> SelectionRuleKind {
        self.kind
    }

    /// Get the regex pattern string.
    #[must_use]
    pub(crate) fn pattern(&self) -> &str {
        self.pattern.as_str()
    }
}

impl SmartSelection {
    /// Get a rule by name.
    #[must_use]
    pub(crate) fn get_rule(&self, name: &str) -> Option<&SelectionRule> {
        self.rules.iter().find(|r| r.name == name)
    }

    /// Get all rules.
    #[must_use]
    pub(crate) fn rules(&self) -> &[SelectionRule] {
        &self.rules
    }

    /// Check if the text at the given position matches any rule.
    #[must_use]
    pub(crate) fn has_match_at(&self, text: &str, byte_pos: usize) -> bool {
        self.find_at(text, byte_pos).is_some()
    }
}
