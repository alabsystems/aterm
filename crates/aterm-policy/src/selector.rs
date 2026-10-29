// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Sequence selector parser and matcher
//! ([`SequenceSelector`], [`DispatchedSequence`]).

use core::fmt;

#[cfg(feature = "toml")]
use serde::{Deserialize, Serialize};

/// Top-level function classifier used by [`SequenceSelector`] and [`DispatchedSequence`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "toml", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "toml", serde(rename_all = "UPPERCASE"))]
#[non_exhaustive]
pub enum FunctionKind {
    /// OSC (Operating System Command).
    Osc,
    /// CSI (Control Sequence Introducer).
    Csi,
    /// DCS (Device Control String).
    Dcs,
    /// Catch-all — universal matcher (`*` / `response any`).
    Wildcard,
}

impl fmt::Display for FunctionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Osc => f.write_str("OSC"),
            Self::Csi => f.write_str("CSI"),
            Self::Dcs => f.write_str("DCS"),
            Self::Wildcard => f.write_str("*"),
        }
    }
}

/// One parameter position in a [`SequenceSelector`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SelectorParam {
    /// Match any non-empty parameter at this position.
    Wildcard,
    /// Match by string equality.
    Literal(String),
    /// Match the literal `"?"` query marker (OSC 4 / OSC 52 query).
    Query,
    /// Match the literal "not-`?`" complement used by set-path aliases.
    NotQuery,
}

impl SelectorParam {
    /// Returns `true` if this selector parameter matches `actual`.
    #[must_use]
    #[inline]
    pub fn matches(&self, actual: &str) -> bool {
        match self {
            Self::Wildcard => !actual.is_empty(),
            Self::Literal(lit) => lit == actual,
            Self::Query => actual == "?",
            Self::NotQuery => actual != "?" && !actual.is_empty(),
        }
    }
}

/// A compiled pattern matched against a [`DispatchedSequence`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SequenceSelector {
    /// Top-level function family.
    pub function: FunctionKind,
    /// Primary code (e.g. OSC 4 → `Some(4)`; OSC catch-all within a family → `None`).
    pub major: Option<u32>,
    /// Optional trailing token.
    pub suffix: Option<String>,
    /// Per-position parameter matchers.
    pub params: Vec<SelectorParam>,
}

impl SequenceSelector {
    /// The universal matcher (matches every dispatched sequence).
    #[must_use]
    #[inline]
    pub const fn catch_all() -> Self {
        Self {
            function: FunctionKind::Wildcard,
            major: None,
            suffix: None,
            params: Vec::new(),
        }
    }

    /// Parse a selector string (TOML form) into a compiled selector.
    ///
    /// # Errors
    ///
    /// Returns [`SelectorParseError`] if the input cannot be classified as
    /// a catch-all or a function-major selector. The engine keeps such a rule
    /// but never matches it.
    #[inline]
    pub fn parse(src: &str) -> Result<Self, SelectorParseError> {
        parse_compiled(expand_alias(src))
    }

    /// Returns `true` if this selector matches `seq`.
    #[must_use]
    pub fn matches(&self, seq: &DispatchedSequence) -> bool {
        if self.function == FunctionKind::Wildcard {
            return true;
        }
        if self.function != seq.function {
            return false;
        }
        if let Some(expected_major) = self.major
            && Some(expected_major) != seq.major
        {
            return false;
        }
        if let Some(ref expected_suffix) = self.suffix
            && Some(expected_suffix.as_str()) != seq.suffix.as_deref()
        {
            return false;
        }
        if self.params.len() > seq.params.len() {
            return false;
        }
        for (selector_param, actual) in self.params.iter().zip(seq.params.iter()) {
            if !selector_param.matches(actual) {
                return false;
            }
        }
        true
    }

    /// The decision-tree bucket key for this selector.
    #[must_use]
    #[inline]
    pub const fn bucket_key(&self) -> BucketKey {
        BucketKey {
            function: self.function,
            major: self.major,
        }
    }
}

/// A concrete escape sequence ready for policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DispatchedSequence {
    /// Function family.
    pub function: FunctionKind,
    /// Primary parameter.
    pub major: Option<u32>,
    /// Trailing token: CSI final byte, or DCS intermediate+final (e.g. `"2000p"`).
    pub suffix: Option<String>,
    /// Remaining parameters (after `major`), in dispatch order, as raw string tokens.
    pub params: Vec<String>,
}

impl DispatchedSequence {
    /// Convenience constructor for OSC sequences.
    #[must_use]
    #[inline]
    pub fn osc(major: u32, params: impl IntoIterator<Item = String>) -> Self {
        Self {
            function: FunctionKind::Osc,
            major: Some(major),
            suffix: None,
            params: params.into_iter().collect(),
        }
    }

    /// Convenience constructor for CSI sequences.
    #[must_use]
    #[inline]
    pub fn csi(
        major: Option<u32>,
        final_byte: char,
        params: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            function: FunctionKind::Csi,
            major,
            suffix: Some(final_byte.to_string()),
            params: params.into_iter().collect(),
        }
    }

    /// Convenience constructor for DCS sequences.
    #[must_use]
    #[inline]
    pub fn dcs(intermediate_final: impl Into<String>) -> Self {
        Self {
            function: FunctionKind::Dcs,
            major: None,
            suffix: Some(intermediate_final.into()),
            params: Vec::new(),
        }
    }
}

/// Decision-tree bucket key used by the precompiled engine table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BucketKey {
    /// Function family.
    pub function: FunctionKind,
    /// Primary code, or `None` for major-level wildcards.
    pub major: Option<u32>,
}

/// Selector parser error.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectorParseError {
    /// The string was empty after trimming.
    Empty,
    /// The first token was not a recognized function kind.
    UnknownFunction(String),
    /// The major code (e.g. the `4` in `"OSC 4"`) failed to parse as a non-negative integer.
    BadMajor(String),
}

impl fmt::Display for SelectorParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("selector is empty"),
            Self::UnknownFunction(s) => write!(f, "unknown function kind: {s:?}"),
            Self::BadMajor(s) => write!(f, "major code does not parse: {s:?}"),
        }
    }
}

impl std::error::Error for SelectorParseError {}

/// Expand a named alias to its canonical selector form.
pub(crate) fn expand_alias(src: &str) -> &str {
    let trimmed = src.trim();
    match trimmed {
        "OSC 52 set" => "OSC 52;*;<not-question>",
        "OSC 52 query" => "OSC 52;*;?",
        "OSC 4 query" => "OSC 4;*;?",
        "OSC 4 set" => "OSC 4;*;<not-question>",
        "OSC 21 set named" => "OSC 21;<named>",
        "OSC 21 set indexed" => "OSC 21;<indexed>",
        "response any" | "*" => "*",
        _ => src,
    }
}

fn split_ws(s: &str) -> (&str, &str) {
    match s.split_once(char::is_whitespace) {
        Some((head, tail)) => (head, tail.trim_start()),
        None => (s, ""),
    }
}

fn parse_compiled(src: &str) -> Result<SequenceSelector, SelectorParseError> {
    let trimmed = src.trim();
    if trimmed.is_empty() {
        return Err(SelectorParseError::Empty);
    }
    if trimmed == "*" {
        return Ok(SequenceSelector::catch_all());
    }

    let (fn_tok, after_fn) = split_ws(trimmed);
    let (major_tok, rest) = split_ws(after_fn);

    let function = match fn_tok {
        "OSC" => FunctionKind::Osc,
        "CSI" => FunctionKind::Csi,
        "DCS" => FunctionKind::Dcs,
        other => return Err(SelectorParseError::UnknownFunction(other.to_owned())),
    };

    parse_function_body(function, major_tok, rest)
}

fn parse_function_body(
    function: FunctionKind,
    major_tok: &str,
    rest: &str,
) -> Result<SequenceSelector, SelectorParseError> {
    match function {
        FunctionKind::Osc => parse_osc_body(major_tok, rest),
        FunctionKind::Csi => parse_csi_body(major_tok, rest),
        FunctionKind::Dcs => parse_dcs_body(major_tok, rest),
        FunctionKind::Wildcard => Ok(SequenceSelector::catch_all()),
    }
}

fn parse_osc_body(major_tok: &str, rest: &str) -> Result<SequenceSelector, SelectorParseError> {
    let (major_str, leftover) = split_major(major_tok);
    let major = parse_major_or_wildcard(major_str)?;

    let params = collect_osc_params(leftover, rest);
    Ok(SequenceSelector {
        function: FunctionKind::Osc,
        major,
        suffix: None,
        params,
    })
}

fn parse_csi_body(major_tok: &str, rest: &str) -> Result<SequenceSelector, SelectorParseError> {
    let (major, suffix) = if is_csi_final_byte(major_tok) && rest.is_empty() {
        (None, Some(major_tok.to_owned()))
    } else if major_tok == "*" {
        // "CSI * t" — wildcard major, trailing final byte.
        (None, non_empty_string(rest))
    } else if major_tok == "?" {
        (Some(0), non_empty_string(rest))
    } else {
        (Some(parse_major(major_tok)?), non_empty_string(rest))
    };
    Ok(SequenceSelector {
        function: FunctionKind::Csi,
        major,
        suffix,
        params: Vec::new(),
    })
}

fn parse_dcs_body(major_tok: &str, rest: &str) -> Result<SequenceSelector, SelectorParseError> {
    let suffix = if rest.is_empty() {
        Some(major_tok.to_owned())
    } else {
        Some(format!("{major_tok}{rest}"))
    };
    Ok(SequenceSelector {
        function: FunctionKind::Dcs,
        major: None,
        suffix,
        params: Vec::new(),
    })
}

#[inline]
fn split_major(tok: &str) -> (&str, Option<&str>) {
    match tok.split_once(';') {
        Some((major, rest)) => (major, Some(rest)),
        None => (tok, None),
    }
}

fn collect_osc_params(leftover: Option<&str>, rest: &str) -> Vec<SelectorParam> {
    let mut out = Vec::new();
    if let Some(l) = leftover
        && !l.is_empty()
    {
        push_params(&mut out, l);
    }
    if !rest.is_empty() {
        let trimmed = rest.trim_start_matches(';');
        push_params(&mut out, trimmed);
    }
    out
}

fn push_params(out: &mut Vec<SelectorParam>, joined: &str) {
    for raw in joined.split(';') {
        out.push(compile_param(raw));
    }
}

fn compile_param(raw: &str) -> SelectorParam {
    let s = raw.trim();
    match s {
        "*" | "<idx>" | "<selection>" | "<name>" | "<value>" | "<named>" | "<indexed>" => {
            SelectorParam::Wildcard
        }
        "?" => SelectorParam::Query,
        "<not-question>" | "<base64_non_question>" => SelectorParam::NotQuery,
        other => SelectorParam::Literal(other.to_owned()),
    }
}

#[inline]
fn parse_major(tok: &str) -> Result<u32, SelectorParseError> {
    tok.parse::<u32>()
        .map_err(|_| SelectorParseError::BadMajor(tok.to_owned()))
}

#[inline]
fn parse_major_or_wildcard(tok: &str) -> Result<Option<u32>, SelectorParseError> {
    if tok == "*" || tok.is_empty() {
        Ok(None)
    } else {
        Ok(Some(parse_major(tok)?))
    }
}

#[inline]
const fn is_csi_final_byte(tok: &str) -> bool {
    matches!(
        tok.as_bytes(),
        [b'A'..=b'Z' | b'a'..=b'z' | b'@' | b'`' | b'~' | b'^']
    )
}

#[inline]
fn non_empty_string(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every selector spelling of the grammar, aliases included, and the
    /// selector it parses to; then the spellings that are refused.
    #[test]
    fn selector_spellings_parse() {
        use FunctionKind::{Csi, Dcs, Osc};
        use SelectorParam::{NotQuery, Query, Wildcard};
        let sel =
            |function, major, suffix: Option<&str>, params: &[SelectorParam]| SequenceSelector {
                function,
                major,
                suffix: suffix.map(str::to_owned),
                params: params.to_vec(),
            };
        for (spelling, want) in [
            ("*", SequenceSelector::catch_all()),
            ("response any", SequenceSelector::catch_all()),
            ("OSC 4", sel(Osc, Some(4), None, &[])),
            ("OSC 4;*;?", sel(Osc, Some(4), None, &[Wildcard, Query])),
            (
                "OSC 4;*;<not-question>",
                sel(Osc, Some(4), None, &[Wildcard, NotQuery]),
            ),
            (
                "OSC 52 set",
                sel(Osc, Some(52), None, &[Wildcard, NotQuery]),
            ),
            ("OSC 52 query", sel(Osc, Some(52), None, &[Wildcard, Query])),
            ("OSC 4 query", sel(Osc, Some(4), None, &[Wildcard, Query])),
            ("OSC 4 set", sel(Osc, Some(4), None, &[Wildcard, NotQuery])),
            ("OSC 21 set named", sel(Osc, Some(21), None, &[Wildcard])),
            ("OSC 21 set indexed", sel(Osc, Some(21), None, &[Wildcard])),
            ("CSI 20 t", sel(Csi, Some(20), Some("t"), &[])),
            ("CSI  20  t", sel(Csi, Some(20), Some("t"), &[])),
            ("CSI t", sel(Csi, None, Some("t"), &[])),
            ("CSI ? h", sel(Csi, Some(0), Some("h"), &[])),
            ("CSI  ?  h", sel(Csi, Some(0), Some("h"), &[])),
            ("OSC  52", sel(Osc, Some(52), None, &[])),
            ("OSC\t4;*;?", sel(Osc, Some(4), None, &[Wildcard, Query])),
            ("DCS 2000p", sel(Dcs, None, Some("2000p"), &[])),
            ("DCS 2000 p", sel(Dcs, None, Some("2000p"), &[])),
            ("DCS  2000  p", sel(Dcs, None, Some("2000p"), &[])),
        ] {
            assert_eq!(SequenceSelector::parse(spelling), Ok(want), "{spelling:?}");
        }
        assert!(matches!(
            SequenceSelector::parse("XYZ 1"),
            Err(SelectorParseError::UnknownFunction(_))
        ));
        assert_eq!(SequenceSelector::parse(""), Err(SelectorParseError::Empty));
    }

    /// `(selector, sequence, matches?)` — each selector against the
    /// sequences its original one-case test drove, including the near misses.
    #[test]
    fn selector_matching() {
        let osc = |major: u32, params: &[&str]| {
            DispatchedSequence::osc(major, params.iter().map(|&p| p.to_owned()))
        };
        let csi =
            |major: u32, final_byte: char| DispatchedSequence::csi(Some(major), final_byte, []);
        for (selector, sequence, matches) in [
            ("*", osc(4, &[]), true),
            ("*", csi(20, 't'), true),
            ("*", DispatchedSequence::dcs("2000p"), true),
            ("OSC 52 set", osc(52, &["c", "SGVsbG8="]), true),
            ("OSC 52 set", osc(52, &["c", "?"]), false),
            ("OSC 52 query", osc(52, &["c", "?"]), true),
            ("OSC 4", osc(4, &["3", "?"]), true),
            ("OSC 4", osc(4, &["5", "red"]), true),
            ("OSC 4", osc(5, &[]), false),
            ("CSI t", csi(20, 't'), true),
            ("CSI t", csi(1, 't'), true),
            ("CSI t", csi(1, 'h'), false),
            ("CSI 20 t", csi(20, 't'), true),
            ("CSI 20 t", csi(21, 't'), false),
            ("CSI ? h", csi(0, 'h'), true),
            ("CSI ? h", csi(25, 'h'), false),
            ("DCS 2000p", DispatchedSequence::dcs("2000p"), true),
            ("DCS 2000p", DispatchedSequence::dcs("1000p"), false),
            ("OSC 4;*;?", osc(4, &["3"]), false),
            ("OSC 4;*", osc(4, &["3", "anything"]), true),
        ] {
            let sel = SequenceSelector::parse(selector).expect("parses");
            assert_eq!(
                sel.matches(&sequence),
                matches,
                "{selector} vs {sequence:?}"
            );
        }
    }

    #[test]
    fn not_query_param_rejects_empty_and_question() {
        assert!(!SelectorParam::NotQuery.matches(""));
        assert!(!SelectorParam::NotQuery.matches("?"));
        assert!(SelectorParam::NotQuery.matches("red"));
    }
}
