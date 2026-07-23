// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Sequence selector parser and matcher (§3.3 of
//! `designs/2026-04-19-osc-policy-engine.md`).
//!
//! A [`SequenceSelector`] is the parsed form of a rule's `sequence` string,
//! e.g. `"OSC 4;*;?"` → `SequenceSelector { function: Osc, major: Some(4),
//! params: [Wildcard, Literal("?")] }`. Selectors describe *patterns* that
//! match against concrete [`DispatchedSequence`] values produced at the
//! handler boundary.
//!
//! # Grammar
//!
//! ```text
//! selector   := catch_all | function_selector
//! catch_all  := "*" | "response any"
//! function_selector
//!            := function_kind major ( separator param )* suffix?
//! function_kind
//!            := "OSC" | "CSI" | "DCS"
//! major      := unsigned-int | "*"
//! separator  := ";" | " "
//! param      := unsigned-int | "?" | "*" | token
//! suffix     := " " ident           // e.g. "set", "query", or the CSI final byte
//! ```
//!
//! Named aliases like `"OSC 52 set"` and `"response any"` are expanded first
//! (see [`expand_alias`]). Aliases always resolve to a canonical selector
//! form so the matcher does not have to know about them.
//!
//! # Matching
//!
//! Selector parameters are matched position-by-position against the
//! dispatched parameters. A `Wildcard` matches any single parameter (but
//! must be present). A `Literal` matches by string equality. When the
//! selector's parameter list is shorter than the dispatched list, the
//! selector only matches the prefix — this gives `"OSC 4"` (no params) the
//! "match any OSC 4" semantics from §3.3.
//!
//! Empty selectors and `*` are the universal matcher. `response any` matches
//! any response-producing sequence; at the data-model level this is the
//! same as `*` — the distinction is carried in the alias table for
//! documentation.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::aliases;

// ---------------------------------------------------------------------------
// Function kind
// ---------------------------------------------------------------------------

/// Top-level function classifier used by [`SequenceSelector`] and
/// [`DispatchedSequence`].
///
/// These variants mirror the three escape-sequence families covered by the
/// policy engine. CSI is treated as a single bucket even though the final
/// byte is significant — the byte is carried as the `suffix` field so both
/// `CSI t` (any `t`) and `CSI 20 t` (parameter `20`, final `t`) can be
/// expressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
#[non_exhaustive]
pub enum FunctionKind {
    /// OSC (Operating System Command) — `ESC ]`.
    Osc,
    /// CSI (Control Sequence Introducer) — `ESC [`.
    Csi,
    /// DCS (Device Control String) — `ESC P`.
    Dcs,
    /// Catch-all — universal matcher (`*` / `response any`). Used only in
    /// [`SequenceSelector`]; a dispatched sequence is never tagged
    /// `Wildcard`.
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

// ---------------------------------------------------------------------------
// Param
// ---------------------------------------------------------------------------

/// One parameter position in a [`SequenceSelector`].
///
/// The variant choice is small on purpose: we want the match decision to be
/// trivially side-effect-free and total. Complex matchers (regex, range) are
/// explicitly out of scope for v1 — the §3.3 table is the full grammar.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SelectorParam {
    /// Match any non-empty parameter at this position.
    Wildcard,
    /// Match by string equality. Numeric parameters are compared as their
    /// decimal string form (`"20"`, not `20u32`) — this keeps the matcher
    /// closed under a single comparison strategy.
    Literal(String),
    /// Match the literal `"?"` query marker (OSC 4 / OSC 52 query).
    Query,
    /// Match the literal "not-`?`" complement used by set-path aliases.
    /// Present when `"OSC 4 set"` expanded to `"OSC 4;*;<not-question>"`.
    NotQuery,
}

impl SelectorParam {
    /// Returns `true` if this selector parameter matches `actual`.
    ///
    /// Totality: every input combination is decidable without panicking.
    #[must_use]
    pub fn matches(&self, actual: &str) -> bool {
        match self {
            Self::Wildcard => !actual.is_empty(),
            Self::Literal(lit) => lit == actual,
            Self::Query => actual == "?",
            Self::NotQuery => actual != "?" && !actual.is_empty(),
        }
    }
}

// ---------------------------------------------------------------------------
// SequenceSelector
// ---------------------------------------------------------------------------

/// A compiled pattern matched against a [`DispatchedSequence`].
///
/// Selectors are produced by [`SequenceSelector::parse`] at policy-load time;
/// the `String` form stored in [`crate::Rule::sequence`] is left untouched so
/// the TOML round-trip test continues to pass. Compilation is a pure
/// transformation — a rebuild from the string always produces the same
/// selector.
///
/// # Canonical forms
///
/// * `"*"` → `SequenceSelector::catch_all()`
/// * `"response any"` → `SequenceSelector::catch_all_with_tag("response any")`
///   (functionally identical to `*`)
/// * `"OSC 4"` → function=Osc, major=Some(4), params=[]
/// * `"OSC 4;*;?"` → function=Osc, major=Some(4), params=[Wildcard, Query]
/// * `"CSI 20 t"` → function=Csi, major=Some(20), suffix=Some("t"), params=[]
/// * `"CSI t"` → function=Csi, major=None, suffix=Some("t"), params=[]
/// * `"DCS 2000p"` → function=Dcs, major=None, suffix=Some("2000p"), params=[]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SequenceSelector {
    /// Top-level function family.
    pub function: FunctionKind,
    /// Primary code (e.g. OSC 4 → `Some(4)`; OSC catch-all within a family
    /// → `None`).
    pub major: Option<u32>,
    /// Optional trailing token — the CSI final byte (`"t"`) or the DCS
    /// intermediate string (`"2000p"`). `None` for OSC.
    pub suffix: Option<String>,
    /// Per-position parameter matchers.
    pub params: Vec<SelectorParam>,
}

impl SequenceSelector {
    /// The universal matcher (matches every dispatched sequence).
    #[must_use]
    pub fn catch_all() -> Self {
        Self {
            function: FunctionKind::Wildcard,
            major: None,
            suffix: None,
            params: Vec::new(),
        }
    }

    /// Parse a selector string (TOML form) into a compiled selector.
    ///
    /// Aliases in [`aliases::ALIAS_TABLE`] are expanded first. Unknown
    /// aliases are treated as raw selectors — the parser does not reject
    /// unfamiliar tokens, it just fails to find a non-trivial structure,
    /// which means the selector will never match (fail-closed at the
    /// matcher level).
    ///
    /// # Errors
    ///
    /// Returns [`SelectorParseError`] if the input cannot be classified as
    /// a catch-all or a function-major selector. The error variant is
    /// informational — at policy-load time the caller falls back to the
    /// Hardened profile (§4.4), so per-rule parse failures never surface.
    pub fn parse(src: &str) -> Result<Self, SelectorParseError> {
        let expanded = expand_alias(src);
        parse_compiled(expanded.as_ref())
    }

    /// Returns `true` if this selector matches `seq`.
    ///
    /// Matching rules:
    ///
    /// 1. `FunctionKind::Wildcard` matches any sequence regardless of family.
    /// 2. Otherwise the function must match exactly.
    /// 3. If `major` is `Some`, the sequence's major must equal it.
    ///    `None` is the major-level wildcard (e.g. `"CSI t"`).
    /// 4. If `suffix` is `Some`, the sequence's suffix must equal it.
    ///    `None` matches any suffix.
    /// 5. `params` are matched position-by-position. The selector may be
    ///    shorter than the sequence; extra sequence parameters are ignored.
    ///    The selector may not be longer than the sequence — that is an
    ///    explicit non-match.
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

    /// The decision-tree bucket key for this selector. Selectors with the
    /// same key share a bucket in the compiled engine (see
    /// [`crate::engine`]).
    #[must_use]
    pub fn bucket_key(&self) -> BucketKey {
        BucketKey {
            function: self.function,
            major: self.major,
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatched sequence
// ---------------------------------------------------------------------------

/// A concrete escape sequence ready for policy evaluation.
///
/// Built by the handler at dispatch time. Parameters are the raw text tokens
/// (OSC argument strings split by `;`, CSI numeric parameters as their
/// decimal string form). The `suffix` field carries CSI final bytes and DCS
/// intermediate strings.
///
/// This is the unit the engine consumes. It deliberately holds `String`s
/// (and a borrowed sub-construction helper — [`DispatchedSequence::osc`])
/// rather than raw bytes so selectors can match via string equality.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DispatchedSequence {
    /// Function family.
    pub function: FunctionKind,
    /// Primary parameter. `None` is valid only when the dispatched sequence
    /// has no numeric prefix (rare — e.g. `ESC P 2000 p`, where the whole
    /// thing is the suffix).
    pub major: Option<u32>,
    /// Trailing token: CSI final byte, or DCS intermediate+final (e.g.
    /// `"2000p"`).
    pub suffix: Option<String>,
    /// Remaining parameters (after `major`), in dispatch order, as raw
    /// string tokens.
    pub params: Vec<String>,
}

impl DispatchedSequence {
    /// Convenience constructor for OSC sequences.
    #[must_use]
    pub fn osc(major: u32, params: impl IntoIterator<Item = String>) -> Self {
        Self {
            function: FunctionKind::Osc,
            major: Some(major),
            suffix: None,
            params: params.into_iter().collect(),
        }
    }

    /// Convenience constructor for CSI sequences. `final_byte` is the CSI
    /// dispatch byte (e.g. `'t'`).
    #[must_use]
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

    /// Convenience constructor for DCS sequences. The `intermediate_final`
    /// string carries the intermediate bytes plus the final byte, e.g.
    /// `"2000p"`.
    #[must_use]
    pub fn dcs(intermediate_final: impl Into<String>) -> Self {
        Self {
            function: FunctionKind::Dcs,
            major: None,
            suffix: Some(intermediate_final.into()),
            params: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Bucket key for precompiled decision tree
// ---------------------------------------------------------------------------

/// Decision-tree bucket key used by the precompiled engine table.
///
/// Selectors with the same `(function, major)` share a bucket. Rules inside
/// the bucket are walked in declared order. Wildcard selectors bucket under
/// `(Wildcard, None)` and are consulted last as the catch-all tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BucketKey {
    /// Function family.
    pub function: FunctionKind,
    /// Primary code, or `None` for major-level wildcards.
    pub major: Option<u32>,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Selector parser error. The variants are informational — a load-time
/// parse failure is always converted to the fallback Hardened profile.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectorParseError {
    /// The string was empty after trimming.
    Empty,
    /// The first token was not a recognized function kind.
    UnknownFunction(String),
    /// The major code (e.g. the `4` in `"OSC 4"`) failed to parse as a
    /// non-negative integer.
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
///
/// Aliases are documented in [`aliases::ALIAS_TABLE`]; this function
/// translates the human-readable form to the concrete selector syntax the
/// parser recognizes. Unknown aliases pass through unchanged so the caller's
/// parser sees the original string.
pub(crate) fn expand_alias(src: &str) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    let trimmed = src.trim();
    match trimmed {
        "OSC 52 set" => Cow::Borrowed("OSC 52;*;<not-question>"),
        "OSC 52 query" => Cow::Borrowed("OSC 52;*;?"),
        "OSC 4 query" => Cow::Borrowed("OSC 4;*;?"),
        "OSC 4 set" => Cow::Borrowed("OSC 4;*;<not-question>"),
        "OSC 21 set named" => Cow::Borrowed("OSC 21;<named>"),
        "OSC 21 set indexed" => Cow::Borrowed("OSC 21;<indexed>"),
        "response any" | "*" => Cow::Borrowed("*"),
        _ => {
            // If the table grows we still want to round-trip aliases not
            // known at compile time — defer to the runtime table.
            if aliases::lookup(trimmed).is_some() {
                // Known alias with no inline translation — treat as catch-all
                // for forward-compat (fail-open here is safe because the
                // engine fails closed at the default).
                Cow::Borrowed("*")
            } else {
                Cow::Borrowed(src)
            }
        }
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

    // Split on whitespace for the "function major [rest]" prefix, then
    // tokenize the rest on `;`.
    let mut head = trimmed.splitn(3, ' ');
    let fn_tok = head.next().unwrap_or("").trim();
    let major_tok = head.next().unwrap_or("").trim();
    let rest = head.next().unwrap_or("").trim();

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
    // `major_tok` may itself carry `;params` when the caller wrote
    // `"OSC 4;*;?"` (all in one word). Split if necessary.
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
    // CSI can be `"CSI t"` (major_tok == "t", rest == "") or
    // `"CSI 20 t"` (major_tok == "20", rest == "t") or
    // `"CSI ? h"` (major_tok == "?", rest == "h").
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
    // DCS in the design is always `"DCS <intermediate><final>"`, e.g.
    // `"DCS 2000p"` — a single token after DCS. Accept both
    // `"DCS 2000p"` (major_tok=2000p, rest="") and `"DCS 2000 p"` (major_tok=2000, rest=p).
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

fn split_major(tok: &str) -> (&str, Option<&str>) {
    match tok.find(';') {
        Some(idx) => (&tok[..idx], Some(&tok[idx + 1..])),
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
        // Rest may itself be `;param;param` or `param;param`.
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

fn parse_major(tok: &str) -> Result<u32, SelectorParseError> {
    tok.parse::<u32>()
        .map_err(|_| SelectorParseError::BadMajor(tok.to_owned()))
}

fn parse_major_or_wildcard(tok: &str) -> Result<Option<u32>, SelectorParseError> {
    if tok == "*" || tok.is_empty() {
        Ok(None)
    } else {
        Ok(Some(parse_major(tok)?))
    }
}

fn is_csi_final_byte(tok: &str) -> bool {
    // A CSI final byte is a single printable non-digit character.
    let mut chars = tok.chars();
    let Some(c) = chars.next() else {
        return false;
    };
    if chars.next().is_some() {
        return false;
    }
    c.is_ascii_alphabetic() || matches!(c, '@' | '`' | '~' | '^')
}

fn non_empty_string(s: &str) -> Option<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

// ---------------------------------------------------------------------------
// Tests (unit-level; see §3.3 of the design for the full table).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_catch_all() {
        let sel = SequenceSelector::parse("*").expect("parses");
        assert_eq!(sel, SequenceSelector::catch_all());
    }

    #[test]
    fn parses_response_any_as_catch_all() {
        let sel = SequenceSelector::parse("response any").expect("parses");
        assert_eq!(sel.function, FunctionKind::Wildcard);
        assert!(sel.matches(&DispatchedSequence::osc(0, ["anything".to_owned()])));
    }

    #[test]
    fn parses_plain_osc() {
        let sel = SequenceSelector::parse("OSC 4").expect("parses");
        assert_eq!(sel.function, FunctionKind::Osc);
        assert_eq!(sel.major, Some(4));
        assert!(sel.params.is_empty());
    }

    #[test]
    fn parses_osc_with_query_suffix() {
        let sel = SequenceSelector::parse("OSC 4;*;?").expect("parses");
        assert_eq!(sel.major, Some(4));
        assert_eq!(sel.params.len(), 2);
        assert!(matches!(sel.params[0], SelectorParam::Wildcard));
        assert!(matches!(sel.params[1], SelectorParam::Query));
    }

    #[test]
    fn parses_osc_with_notquery_suffix() {
        let sel = SequenceSelector::parse("OSC 4;*;<not-question>").expect("parses");
        assert_eq!(sel.params.len(), 2);
        assert!(matches!(sel.params[1], SelectorParam::NotQuery));
    }

    #[test]
    fn parses_osc_52_set_alias() {
        let sel = SequenceSelector::parse("OSC 52 set").expect("parses");
        assert_eq!(sel.function, FunctionKind::Osc);
        assert_eq!(sel.major, Some(52));
        assert_eq!(sel.params.len(), 2);
        assert!(matches!(sel.params[1], SelectorParam::NotQuery));
    }

    #[test]
    fn parses_osc_52_query_alias() {
        let sel = SequenceSelector::parse("OSC 52 query").expect("parses");
        assert_eq!(sel.major, Some(52));
        assert!(matches!(sel.params[1], SelectorParam::Query));
    }

    #[test]
    fn parses_csi_specific_param() {
        let sel = SequenceSelector::parse("CSI 20 t").expect("parses");
        assert_eq!(sel.function, FunctionKind::Csi);
        assert_eq!(sel.major, Some(20));
        assert_eq!(sel.suffix.as_deref(), Some("t"));
    }

    #[test]
    fn parses_csi_final_only() {
        let sel = SequenceSelector::parse("CSI t").expect("parses");
        assert_eq!(sel.major, None);
        assert_eq!(sel.suffix.as_deref(), Some("t"));
    }

    #[test]
    fn parses_dcs_intermediate_final() {
        let sel = SequenceSelector::parse("DCS 2000p").expect("parses");
        assert_eq!(sel.function, FunctionKind::Dcs);
        assert_eq!(sel.suffix.as_deref(), Some("2000p"));
    }

    #[test]
    fn parses_dcs_separated_intermediate_final() {
        let sel = SequenceSelector::parse("DCS 2000 p").expect("parses");
        assert_eq!(sel.suffix.as_deref(), Some("2000p"));
    }

    #[test]
    fn rejects_unknown_function() {
        let err = SequenceSelector::parse("XYZ 1").unwrap_err();
        assert!(matches!(err, SelectorParseError::UnknownFunction(_)));
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(
            SequenceSelector::parse("").unwrap_err(),
            SelectorParseError::Empty
        );
    }

    #[test]
    fn catch_all_matches_everything() {
        let sel = SequenceSelector::catch_all();
        assert!(sel.matches(&DispatchedSequence::osc(4, [])));
        assert!(sel.matches(&DispatchedSequence::csi(Some(20), 't', [])));
        assert!(sel.matches(&DispatchedSequence::dcs("2000p")));
    }

    #[test]
    fn osc_52_set_matches_clipboard_write() {
        let sel = SequenceSelector::parse("OSC 52 set").expect("parses");
        let seq = DispatchedSequence::osc(52, ["c".to_owned(), "SGVsbG8=".to_owned()]);
        assert!(sel.matches(&seq));
    }

    #[test]
    fn osc_52_set_does_not_match_query() {
        let sel = SequenceSelector::parse("OSC 52 set").expect("parses");
        let seq = DispatchedSequence::osc(52, ["c".to_owned(), "?".to_owned()]);
        assert!(!sel.matches(&seq));
    }

    #[test]
    fn osc_52_query_matches_query() {
        let sel = SequenceSelector::parse("OSC 52 query").expect("parses");
        let seq = DispatchedSequence::osc(52, ["c".to_owned(), "?".to_owned()]);
        assert!(sel.matches(&seq));
    }

    #[test]
    fn osc_4_no_params_matches_any_osc_4() {
        let sel = SequenceSelector::parse("OSC 4").expect("parses");
        let seq = DispatchedSequence::osc(4, ["3".to_owned(), "?".to_owned()]);
        assert!(sel.matches(&seq));
        let seq2 = DispatchedSequence::osc(4, ["5".to_owned(), "red".to_owned()]);
        assert!(sel.matches(&seq2));
        // But does not match OSC 5.
        let other = DispatchedSequence::osc(5, []);
        assert!(!sel.matches(&other));
    }

    #[test]
    fn csi_final_only_matches_any_csi_t() {
        let sel = SequenceSelector::parse("CSI t").expect("parses");
        assert!(sel.matches(&DispatchedSequence::csi(Some(20), 't', [])));
        assert!(sel.matches(&DispatchedSequence::csi(Some(1), 't', [])));
        assert!(!sel.matches(&DispatchedSequence::csi(Some(1), 'h', [])));
    }

    #[test]
    fn csi_20_t_only_matches_20_t() {
        let sel = SequenceSelector::parse("CSI 20 t").expect("parses");
        assert!(sel.matches(&DispatchedSequence::csi(Some(20), 't', [])));
        assert!(!sel.matches(&DispatchedSequence::csi(Some(21), 't', [])));
    }

    #[test]
    fn dcs_2000p_matches_modal_activation() {
        let sel = SequenceSelector::parse("DCS 2000p").expect("parses");
        assert!(sel.matches(&DispatchedSequence::dcs("2000p")));
        assert!(!sel.matches(&DispatchedSequence::dcs("1000p")));
    }

    #[test]
    fn selector_longer_than_sequence_is_non_match() {
        let sel = SequenceSelector::parse("OSC 4;*;?").expect("parses");
        let seq = DispatchedSequence::osc(4, ["3".to_owned()]); // only one param
        assert!(!sel.matches(&seq));
    }

    #[test]
    fn selector_shorter_than_sequence_matches_prefix() {
        let sel = SequenceSelector::parse("OSC 4;*").expect("parses");
        let seq = DispatchedSequence::osc(4, ["3".to_owned(), "anything".to_owned()]);
        assert!(sel.matches(&seq));
    }

    #[test]
    fn bucket_key_reflects_function_and_major() {
        let sel = SequenceSelector::parse("OSC 4").expect("parses");
        assert_eq!(
            sel.bucket_key(),
            BucketKey {
                function: FunctionKind::Osc,
                major: Some(4),
            }
        );
        let wild = SequenceSelector::catch_all();
        assert_eq!(
            wild.bucket_key(),
            BucketKey {
                function: FunctionKind::Wildcard,
                major: None,
            }
        );
    }

    #[test]
    fn wildcard_param_rejects_empty_string() {
        // Guards against matching a zero-length OSC param token, which
        // today's parser never produces but a future caller might.
        assert!(!SelectorParam::Wildcard.matches(""));
    }

    #[test]
    fn not_query_param_rejects_empty_and_question() {
        assert!(!SelectorParam::NotQuery.matches(""));
        assert!(!SelectorParam::NotQuery.matches("?"));
        assert!(SelectorParam::NotQuery.matches("red"));
    }
}
