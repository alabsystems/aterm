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
    /// The heap bytes this selector holds past its own size: its suffix and its parameter matchers.
    #[must_use]
    pub(crate) fn heap_bytes(&self) -> usize {
        const _: () = assert!(
            !std::mem::needs_drop::<(FunctionKind, Option<u32>)>(),
            "a field counted as holding nothing has drop glue: count it"
        );
        let Self {
            function: _,
            major: _,
            suffix,
            params,
        } = self;
        params.iter().fold(
            suffix.as_ref().map_or(0, String::capacity).saturating_add(
                params
                    .capacity()
                    .saturating_mul(std::mem::size_of::<SelectorParam>()),
            ),
            |sum, param| match param {
                SelectorParam::Literal(lit) => sum.saturating_add(lit.capacity()),
                SelectorParam::Wildcard | SelectorParam::Query | SelectorParam::NotQuery => sum,
            },
        )
    }

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
    let suffix = if rest.is_empty() && (major_tok.is_empty() || major_tok == "*") {
        None
    } else if rest.is_empty() {
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
    if !tok.bytes().all(|b| b.is_ascii_digit()) {
        return Err(SelectorParseError::BadMajor(tok.to_owned()));
    }
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
#[path = "selector_tests.rs"]
mod tests;
