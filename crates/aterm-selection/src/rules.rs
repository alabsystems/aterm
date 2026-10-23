// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Selection rule types and smart selection engine.

pub(crate) use super::builtin_patterns::BuiltinRules;
use aterm_regex::Regex;
use aterm_types::text_shaping::AmbiguousWidth;

/// Priority levels for selection rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[derive(Default)]
pub(crate) enum RulePriority {
    /// Lowest priority - fallback rules.
    Low = 0,
    /// Normal priority - most built-in rules.
    #[default]
    Normal = 1,
    /// High priority - specific patterns that should override general ones.
    High = 2,
}

/// The kind of semantic unit a rule matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectionRuleKind {
    /// URL (http, https, ftp, file, etc.)
    Url,
    /// File path (absolute or relative)
    FilePath,
    /// Email address.
    Email,
    /// IPv4 or IPv6 address.
    IpAddress,
    /// Git commit hash (7+ hex characters)
    GitHash,
    /// Quoted string (single or double quotes)
    QuotedString,
    /// UUID.
    Uuid,
    /// Semantic version (semver)
    SemVer,
}

/// A selection rule that matches semantic text units.
#[derive(Clone)]
pub(crate) struct SelectionRule {
    /// Human-readable name for this rule
    name: String,
    /// The kind of pattern this rule matches
    kind: SelectionRuleKind,
    /// Compiled regex pattern
    pattern: Regex,
    /// Priority for rule ordering
    priority: RulePriority,
}

impl std::fmt::Debug for SelectionRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectionRule")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("pattern", &self.pattern.as_str())
            .field("priority", &self.priority)
            .finish()
    }
}

impl SelectionRule {
    /// Create a new selection rule.
    ///
    /// # Panics
    /// Panics if the pattern is not a valid regex.
    #[allow(clippy::expect_used)]
    pub(crate) fn new(name: &str, kind: SelectionRuleKind, pattern: &str) -> Self {
        Self {
            name: name.to_string(),
            kind,
            pattern: Regex::new(pattern).expect("Invalid regex pattern"),
            priority: RulePriority::Normal,
        }
    }

    /// Set the priority for this rule.
    #[must_use]
    #[inline]
    pub(crate) const fn with_priority(mut self, priority: RulePriority) -> Self {
        self.priority = priority;
        self
    }

    /// Find all matches in the given text.
    #[cfg(test)]
    pub(crate) fn find_all<'a>(
        &'a self,
        text: &'a str,
    ) -> impl Iterator<Item = aterm_regex::Match<'a>> + 'a {
        self.pattern.find_iter(text)
    }

    /// Find the match that contains the given byte position.
    #[must_use]
    #[inline]
    pub(crate) fn find_at_position<'a>(
        &self,
        text: &'a str,
        byte_pos: usize,
    ) -> Option<aterm_regex::Match<'a>> {
        self.pattern
            .find_iter(text)
            .find(move |m| m.start() <= byte_pos && byte_pos < m.end())
    }
}

/// A smart-selection rule match result (see [`SmartSelection::word_boundaries_at`]).
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct SelectionMatch {
    /// The matched text
    text: String,
    /// Start byte offset in the original text
    start: usize,
    /// End byte offset in the original text (exclusive)
    end: usize,
    /// The rule that matched
    rule_name: String,
    /// The kind of match
    kind: SelectionRuleKind,
}

#[cfg(test)]
impl SelectionMatch {
    /// Create a new selection match.
    ///
    /// # Panics
    /// Panics if `end < start`.
    pub(crate) fn new(
        text: impl Into<String>,
        start: usize,
        end: usize,
        rule_name: impl Into<String>,
        kind: SelectionRuleKind,
    ) -> Self {
        assert!(
            end >= start,
            "SelectionMatch::new: end ({end}) must be >= start ({start})"
        );
        Self {
            text: text.into(),
            start,
            end,
            rule_name: rule_name.into(),
            kind,
        }
    }

    /// Get the matched text.
    #[must_use]
    pub(crate) fn matched_text(&self) -> &str {
        &self.text
    }

    /// Get the start byte offset.
    #[must_use]
    pub(crate) fn start(&self) -> usize {
        self.start
    }

    /// Get the end byte offset (exclusive).
    #[must_use]
    pub(crate) fn end(&self) -> usize {
        self.end
    }

    /// Get the name of the rule that matched.
    #[must_use]
    pub(crate) fn rule_name(&self) -> &str {
        &self.rule_name
    }

    /// Get the kind of match.
    #[must_use]
    pub(crate) fn kind(&self) -> SelectionRuleKind {
        self.kind
    }
}

/// Smart selection engine that applies rules to find semantic text units.
#[derive(Debug, Clone)]
pub struct SmartSelection {
    /// Selection rules, sorted by priority (highest first)
    rules: Vec<SelectionRule>,
    /// Optional host-configured word-SEPARATOR set (xterm.js `wordSeparators`
    /// semantics), consulted only by the basic word walk.
    word_separators: Option<String>,
}

impl Default for SmartSelection {
    fn default() -> Self {
        Self::new()
    }
}

impl SmartSelection {
    /// Create a new empty smart selection engine.
    #[must_use]
    #[inline]
    pub const fn new() -> Self {
        Self {
            rules: Vec::new(),
            word_separators: None,
        }
    }

    /// Create a smart selection engine with all built-in rules.
    #[must_use]
    pub fn with_builtin_rules() -> Self {
        let mut selection = Self::new();
        for rule in BuiltinRules::all() {
            selection.add_rule(rule);
        }
        selection
    }

    /// Add a selection rule.
    pub(crate) fn add_rule(&mut self, rule: SelectionRule) {
        self.rules.push(rule);
        self.rules.sort_by_key(|r| std::cmp::Reverse(r.priority));
    }

    /// Find the best match at the given byte position in the text.
    #[cfg(test)]
    #[must_use]
    pub(super) fn find_at(&self, text: &str, byte_pos: usize) -> Option<SelectionMatch> {
        let (rule, m) = self.rule_match_at(text, byte_pos)?;
        Some(SelectionMatch::new(
            m.as_str(),
            m.start(),
            m.end(),
            &rule.name,
            rule.kind,
        ))
    }

    /// The highest-priority rule whose match contains `byte_pos`, and
    /// that match — the one lookup both [`Self::word_boundaries_at`] and the
    /// tests' `find_at` view go through.
    fn rule_match_at<'t>(
        &self,
        text: &'t str,
        byte_pos: usize,
    ) -> Option<(&SelectionRule, aterm_regex::Match<'t>)> {
        if byte_pos > text.len() {
            return None;
        }
        self.rules
            .iter()
            .find_map(|rule| Some((rule, rule.find_at_position(text, byte_pos)?)))
    }

    /// Override the characters that BREAK a word for the basic (non-rule) double-click word walk.
    pub fn set_word_separators(&mut self, separators: Option<&str>) {
        self.word_separators = separators.map(str::to_owned);
    }

    /// Get the word boundaries for smart selection at a position.
    #[must_use]
    pub fn word_boundaries_at(&self, text: &str, byte_pos: usize) -> Option<(usize, usize)> {
        // First try smart selection rules.
        if let Some((_, m)) = self.rule_match_at(text, byte_pos) {
            return Some((m.start(), m.end()));
        }

        Self::basic_word_boundaries_with(self.word_separators.as_deref(), text, byte_pos)
    }

    /// Get word boundaries at a column (display-width) position of `text` laid out left to right in one go.
    #[must_use]
    #[inline]
    pub fn word_boundaries_at_column(&self, text: &str, column: usize) -> Option<(usize, usize)> {
        self.word_boundaries_at_column_with(text, column, AmbiguousWidth::Single)
    }

    /// [`word_boundaries_at_column`](Self::word_boundaries_at_column) with the columns measured under the terminal's `ambiguous_width` policy.
    #[must_use]
    pub fn word_boundaries_at_column_with(
        &self,
        text: &str,
        column: usize,
        ambiguous: AmbiguousWidth,
    ) -> Option<(usize, usize)> {
        let byte_pos = column_to_byte_pos_with(text, column, ambiguous);
        if byte_pos >= text.len() {
            return None;
        }
        let (start_byte, end_byte) = self.word_boundaries_at(text, byte_pos)?;
        Some(byte_span_to_columns_with(
            text, start_byte, end_byte, ambiguous,
        ))
    }

    /// Get basic word boundaries (fallback when no rule matches).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn basic_word_boundaries(text: &str, byte_pos: usize) -> Option<(usize, usize)> {
        Self::basic_word_boundaries_with(None, text, byte_pos)
    }

    /// [`basic_word_boundaries`](Self::basic_word_boundaries) against an
    /// optional word-SEPARATOR set: `Some(seps)` switches the char classifier
    /// to xterm.js `wordSeparators` semantics (see
    /// [`set_word_separators`](Self::set_word_separators)); `None` is the
    /// default class-based walk.
    #[must_use]
    fn basic_word_boundaries_with(
        seps: Option<&str>,
        text: &str,
        byte_pos: usize,
    ) -> Option<(usize, usize)> {
        if byte_pos > text.len() || text.is_empty() {
            return None;
        }

        let pos = byte_pos.min(text.len().saturating_sub(1));

        let snap = (0..=pos).rev().find(|&i| text.is_char_boundary(i))?;

        let is_word = |c: char| is_word_char(seps, c);
        let from = aterm_grapheme::word_break_before(text, snap, is_word);
        let word = aterm_grapheme::words(text.get(from..)?, is_word)
            .map(|word| from.saturating_add(word.start)..from.saturating_add(word.end))
            .find(|word| word.end > snap)?;
        (word.start <= snap).then_some((word.start, word.end))
    }
}

/// Whether `ch`, standing on its own, is a word character under the active
/// classifier: the configured separator set (a word is any run of
/// non-separators — xterm.js `wordSeparators`), or the default class
/// ([`aterm_grapheme::is_word_char`]: alphanumeric + `_`).
#[inline]
fn is_word_char(seps: Option<&str>, ch: char) -> bool {
    match seps {
        Some(s) => !s.contains(ch),
        None => aterm_grapheme::is_word_char(ch),
    }
}

/// Convert a GRID column to the byte position of the grapheme drawn there.
#[cfg(test)]
pub(crate) fn column_to_byte_pos(text: &str, column: usize) -> usize {
    column_to_byte_pos_with(text, column, AmbiguousWidth::Single)
}

/// [`column_to_byte_pos`] under the terminal's `ambiguous_width` policy.
fn column_to_byte_pos_with(text: &str, column: usize, ambiguous: AmbiguousWidth) -> usize {
    for g in aterm_grapheme::grid_graphemes_with(text, ambiguous) {
        if column < g.end_col {
            return g.byte_offset;
        }
    }
    text.len()
}

/// The grid columns `start..end` (exclusive) that bytes `start_byte..end_byte` are drawn in.
#[cfg(test)]
pub(crate) fn byte_span_to_columns(
    text: &str,
    start_byte: usize,
    end_byte: usize,
) -> (usize, usize) {
    byte_span_to_columns_with(text, start_byte, end_byte, AmbiguousWidth::Single)
}

/// [`byte_span_to_columns`] under the terminal's `ambiguous_width` policy.
fn byte_span_to_columns_with(
    text: &str,
    start_byte: usize,
    end_byte: usize,
    ambiguous: AmbiguousWidth,
) -> (usize, usize) {
    let mut start = None;
    let mut end = 0;
    for g in aterm_grapheme::grid_graphemes_with(text, ambiguous) {
        if g.byte_offset >= end_byte {
            break;
        }
        if start.is_none() && g.byte_offset >= start_byte {
            start = Some(g.start_col);
        }
        end = g.end_col;
    }
    (start.unwrap_or(end), end)
}

#[cfg(test)]
#[path = "rules_tests.rs"]
mod rule_tests;
