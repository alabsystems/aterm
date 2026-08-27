// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Selection rule types and smart selection engine.
//!
//! Provides regex-based rules for context-aware text selection.

pub(crate) use super::builtin_patterns::BuiltinRules;
use aterm_grapheme::char_width;
use aterm_grapheme::split_graphemes;
use aterm_regex::Regex;
use std::cmp::Ordering;

/// Priority levels for selection rules.
///
/// Higher priority rules are checked first. When multiple rules match,
/// the highest priority match wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
#[derive(Default)]
pub(crate) enum RulePriority {
    /// Lowest priority - fallback rules
    Low = 0,
    /// Normal priority - most built-in rules
    #[default]
    Normal = 1,
    /// High priority - specific patterns that should override general ones
    High = 2,
}

/// The kind of semantic unit a rule matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectionRuleKind {
    /// URL (http, https, ftp, file, etc.)
    Url,
    /// File path (absolute or relative)
    FilePath,
    /// Email address
    Email,
    /// IPv4 or IPv6 address
    IpAddress,
    /// Git commit hash (7+ hex characters)
    GitHash,
    /// Quoted string (single or double quotes)
    QuotedString,
    /// UUID
    Uuid,
    /// Semantic version (semver)
    SemVer,
    /// Custom user-defined pattern.
    Custom,
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
    /// Whether this rule is enabled
    enabled: bool,
}

impl std::fmt::Debug for SelectionRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectionRule")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("pattern", &self.pattern.as_str())
            .field("priority", &self.priority)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl SelectionRule {
    /// Create a new selection rule.
    ///
    /// # Panics
    ///
    /// Panics if the pattern is not a valid regex.
    #[allow(clippy::expect_used)] // Documented panic contract — invalid regex is a programmer error.
    pub(crate) fn new(name: &str, kind: SelectionRuleKind, pattern: &str) -> Self {
        Self {
            name: name.to_string(),
            kind,
            pattern: Regex::new(pattern).expect("Invalid regex pattern"),
            priority: RulePriority::Normal,
            enabled: true,
        }
    }

    /// Set the priority for this rule.
    #[must_use]
    pub(crate) fn with_priority(mut self, priority: RulePriority) -> Self {
        self.priority = priority;
        self
    }

    /// Enable or disable this rule.
    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Check if this rule is enabled.
    #[must_use]
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Find all matches in the given text.
    pub(crate) fn find_all<'a>(
        &'a self,
        text: &'a str,
    ) -> impl Iterator<Item = aterm_regex::Match<'a>> + 'a {
        self.pattern.find_iter(text)
    }

    /// Find the match that contains the given byte position.
    ///
    /// Returns `None` if no match contains that position.
    #[must_use]
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

/// A match result from the smart selection system.
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

impl SelectionMatch {
    /// Create a new selection match.
    ///
    /// # Panics (debug builds)
    ///
    /// Panics if `end < start` (invalid range).
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

    /// Get the byte length of the match.
    #[must_use]
    pub(super) fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

/// Smart selection engine that applies rules to find semantic text units.
#[derive(Debug, Clone)]
pub struct SmartSelection {
    /// Selection rules, sorted by priority (highest first)
    rules: Vec<SelectionRule>,
    /// Optional host-configured word-SEPARATOR set (xterm.js `wordSeparators`
    /// semantics), consulted only by the basic word walk. `None` (the default)
    /// keeps the class-based logic (alphanumeric + `_`) byte-for-byte.
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
    pub fn new() -> Self {
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
    ///
    /// Rules are maintained in priority order (highest first).
    pub(crate) fn add_rule(&mut self, rule: SelectionRule) {
        self.rules.push(rule);
        // Highest priority first (clippy::unnecessary_sort_by — Reverse key).
        self.rules.sort_by_key(|r| std::cmp::Reverse(r.priority));
    }

    /// Get a mutable reference to a rule by name.
    pub(crate) fn get_rule_mut(&mut self, name: &str) -> Option<&mut SelectionRule> {
        self.rules.iter_mut().find(|r| r.name == name)
    }

    /// Enable or disable a rule by name.
    ///
    /// Returns `true` if the rule was found.
    pub fn set_rule_enabled(&mut self, name: &str, enabled: bool) -> bool {
        if let Some(rule) = self.get_rule_mut(name) {
            rule.set_enabled(enabled);
            true
        } else {
            false
        }
    }

    /// Find the best match at the given byte position in the text.
    ///
    /// Returns the highest-priority match that contains the position.
    #[must_use]
    pub(super) fn find_at(&self, text: &str, byte_pos: usize) -> Option<SelectionMatch> {
        // Check bounds
        if byte_pos > text.len() {
            return None;
        }

        // Try each rule in priority order
        for rule in &self.rules {
            if !rule.is_enabled() {
                continue;
            }

            if let Some(m) = rule.find_at_position(text, byte_pos) {
                return Some(SelectionMatch::new(
                    m.as_str(),
                    m.start(),
                    m.end(),
                    &rule.name,
                    rule.kind,
                ));
            }
        }

        None
    }

    /// Find the best match at the given column position in the text.
    ///
    /// This converts the column (character count) to a byte position.
    /// Useful for terminal selection where positions are in columns.
    #[must_use]
    pub fn find_at_column(&self, text: &str, column: usize) -> Option<SelectionMatch> {
        // Convert column to byte position
        let byte_pos = column_to_byte_pos(text, column);
        self.find_at(text, byte_pos)
    }

    /// Find all matches in the text.
    ///
    /// Returns matches from all enabled rules, sorted by start position.
    /// Overlapping matches from different rules are all included.
    #[must_use]
    pub fn find_all(&self, text: &str) -> Vec<SelectionMatch> {
        let mut matches = Vec::new();

        for rule in &self.rules {
            if !rule.is_enabled() {
                continue;
            }

            for m in rule.find_all(text) {
                matches.push(SelectionMatch::new(
                    m.as_str(),
                    m.start(),
                    m.end(),
                    &rule.name,
                    rule.kind,
                ));
            }
        }

        // Sort by start position, then by priority (via rule order which maintains priority)
        matches.sort_by(|a, b| {
            match a.start.cmp(&b.start) {
                Ordering::Equal => a.len().cmp(&b.len()).reverse(), // longer matches first
                other => other,
            }
        });

        matches
    }

    /// Override the characters that BREAK a word for the basic (non-rule)
    /// double-click word walk — xterm.js `wordSeparators` semantics: with a
    /// set configured, a word is a maximal run of NON-separator characters
    /// (zero-width combining marks still extend the run onto their base;
    /// zero-width format controls still break). `None` restores the default
    /// class-based logic (alphanumeric + `_`) exactly.
    ///
    /// Smart-selection RULES (url/file_path/email/…) still take precedence in
    /// [`word_boundaries_at`](Self::word_boundaries_at); the separators only
    /// shape the fallback word (disable rules via
    /// [`set_rule_enabled`](Self::set_rule_enabled) if pure separator
    /// behaviour is wanted).
    pub fn set_word_separators(&mut self, separators: Option<&str>) {
        self.word_separators = separators.map(str::to_owned);
    }

    /// The configured word-separator set, or `None` for the default
    /// class-based word logic.
    #[must_use]
    pub fn word_separators(&self) -> Option<&str> {
        self.word_separators.as_deref()
    }

    /// Get the word boundaries for smart selection at a position.
    ///
    /// If a rule matches at the position, returns the match boundaries.
    /// Otherwise, returns word boundaries based on whitespace/punctuation
    /// (or on the configured [`set_word_separators`](Self::set_word_separators)).
    /// Both `byte_pos` and the returned offsets are in bytes.
    #[must_use]
    pub fn word_boundaries_at(&self, text: &str, byte_pos: usize) -> Option<(usize, usize)> {
        // First try smart selection rules
        if let Some(m) = self.find_at(text, byte_pos) {
            return Some((m.start, m.end));
        }

        // Fall back to basic word boundaries
        Self::basic_word_boundaries_with(self.word_separators.as_deref(), text, byte_pos)
    }

    /// Get word boundaries at a column (display-width) position.
    ///
    /// Converts column→byte on input, calls `word_boundaries_at`, then
    /// converts the byte-offset result back to column offsets. This is
    /// the correct entry point for terminal UI code that works in column
    /// coordinates (#5685).
    #[must_use]
    pub fn word_boundaries_at_column(&self, text: &str, column: usize) -> Option<(usize, usize)> {
        let byte_pos = column_to_byte_pos(text, column);
        let (start_byte, end_byte) = self.word_boundaries_at(text, byte_pos)?;
        Some((
            byte_pos_to_column(text, start_byte),
            byte_pos_to_column(text, end_byte),
        ))
    }

    /// Get basic word boundaries (fallback when no rule matches).
    ///
    /// Treats sequences of alphanumeric characters and underscores as words.
    /// Unicode combining marks are treated as word continuations.
    /// Handles Unicode text: CJK, accented Latin, Cyrillic, etc.
    ///
    /// Test-only pin of the DEFAULT (no-separator) classifier; production goes
    /// through [`word_boundaries_at`](Self::word_boundaries_at), which passes
    /// the configured separators.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn basic_word_boundaries(text: &str, byte_pos: usize) -> Option<(usize, usize)> {
        Self::basic_word_boundaries_with(None, text, byte_pos)
    }

    /// [`basic_word_boundaries`](Self::basic_word_boundaries) against an
    /// optional word-SEPARATOR set: `Some(seps)` switches the char classifier
    /// to xterm.js `wordSeparators` semantics (see
    /// [`set_word_separators`](Self::set_word_separators)); `None` is the
    /// default class-based walk, byte-for-byte.
    #[must_use]
    fn basic_word_boundaries_with(
        seps: Option<&str>,
        text: &str,
        byte_pos: usize,
    ) -> Option<(usize, usize)> {
        if byte_pos > text.len() || text.is_empty() {
            return None;
        }

        // Clamp to last valid position
        let pos = byte_pos.min(text.len().saturating_sub(1));

        // Snap to nearest char boundary at or before pos
        let snap = (0..=pos).rev().find(|&i| text.is_char_boundary(i))?;

        // Check if the character at the snapped position is a word character
        let ch = text[snap..].chars().next()?;
        if !continues_word(seps, ch) {
            return None;
        }

        // Walk backwards from snap to find start of word
        let start = text[..snap]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| continues_word(seps, c))
            .last()
            .map_or(snap, |(i, _)| i);

        // Walk forwards from snap to find end of word
        let end = text[snap..]
            .char_indices()
            .skip(1) // skip the char at snap
            .find(|&(_, c)| !continues_word(seps, c))
            .map_or(text.len(), |(i, _)| snap + i);

        // Treat combining marks as continuations, not standalone words.
        if !text[start..end].chars().any(|c| anchors_word(seps, c)) {
            return None;
        }

        Some((start, end))
    }
}

/// Whether `ch` EXTENDS the word run under the active classifier: the
/// configured separator set (a word is any run of non-separators — xterm.js
/// `wordSeparators`), or the default class-based logic. Combining marks always
/// extend (they belong to their base grapheme even if listed as separators);
/// zero-width format controls always break (invisible characters must not
/// weld two words together).
#[inline]
fn continues_word(seps: Option<&str>, ch: char) -> bool {
    match seps {
        Some(s) => is_combining_mark(ch) || (!s.contains(ch) && !is_zero_width_word_breaker(ch)),
        None => is_word_continuation_char(ch),
    }
}

/// Whether `ch` can ANCHOR a word (a run of pure combining marks is not a
/// word): the non-mark continuation chars under either classifier.
#[inline]
fn anchors_word(seps: Option<&str>, ch: char) -> bool {
    match seps {
        Some(_) => continues_word(seps, ch) && !is_combining_mark(ch),
        None => is_word_core_char(ch),
    }
}

#[inline]
fn is_word_core_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

#[inline]
fn is_word_continuation_char(ch: char) -> bool {
    is_word_core_char(ch) || is_combining_mark(ch)
}

#[inline]
fn is_combining_mark(ch: char) -> bool {
    char_width(ch) == 0 && !is_zero_width_word_breaker(ch)
}

#[inline]
fn is_zero_width_word_breaker(ch: char) -> bool {
    matches!(
        ch,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{FEFF}'
    )
}

/// Convert a column (display-width) position to a byte position.
pub(crate) fn column_to_byte_pos(text: &str, column: usize) -> usize {
    let mut current_col = 0;

    for grapheme in split_graphemes(text) {
        // Column points at this grapheme's first cell.
        if current_col >= column {
            return grapheme.byte_offset;
        }

        current_col += grapheme.width;

        // Column points inside a multi-cell grapheme; return its start.
        if current_col > column {
            return grapheme.byte_offset;
        }
    }

    text.len()
}

/// Convert a byte position to a column (display-width) position.
pub(crate) fn byte_pos_to_column(text: &str, byte_pos: usize) -> usize {
    let mut current_col = 0;

    for grapheme in split_graphemes(text) {
        if grapheme.byte_offset >= byte_pos {
            break;
        }
        current_col += grapheme.width;
    }

    current_col
}

#[cfg(test)]
#[path = "rules_test_accessors.rs"]
mod test_accessors;

#[cfg(test)]
#[path = "rules_tests.rs"]
mod rule_tests;
