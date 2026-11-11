// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Parser partial state on the seamless-handoff wire ([`CheckpointMeta::parser`](super::CheckpointMeta::parser)).

use aterm_parser::{ParserCarry, State, TmuxPassthrough};

/// [`ParserCarry`] on the handoff wire.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ParserRepr {
    /// [`State::name`] of the parser state.
    pub state: String,
    /// Finalized CSI parameters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<u16>,
    /// The subparameter bitmask.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub subparam_mask: u32,
    /// The CSI parameter being accumulated.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub current_param: u32,
    /// Whether a digit of `current_param` has been read.
    #[serde(default, skip_serializing_if = "is_false")]
    pub param_started: bool,
    /// Whether the last CSI separator was a colon.
    #[serde(default, skip_serializing_if = "is_false")]
    pub last_was_colon: bool,
    /// The intermediate bytes, hex.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub intermediates: String,
    /// The OSC payload collected so far, hex.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub osc: String,
    /// The OSC payload was over the carry cap: discard it at its terminator.
    #[serde(default, skip_serializing_if = "is_false")]
    pub osc_overflowed: bool,
    /// The head of a split UTF-8 character, hex.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub utf8_tail: String,
    /// The length of the character `utf8_tail` begins.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub utf8_expected: u8,
    /// The head of a split UTF-8 character a DCS or APC string's payload ends in, hex.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub string_utf8_tail: String,
    /// The tmux passthrough wrapper name ([`aterm_parser::TmuxPassthrough::name`]), or empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tmux: String,
    /// Exact carry when outer fields hold the coarser form for older builds ([`ParserCarry::for_older_builds`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<Box<ParserRepr>>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
#[inline]
const fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
#[inline]
const fn is_zero_u8(n: &u8) -> bool {
    *n == 0
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
#[inline]
const fn is_false(b: &bool) -> bool {
    !*b
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn from_hex(hex: &str) -> Option<Vec<u8>> {
    let (pairs, rest) = hex.as_bytes().as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    pairs
        .iter()
        .map(|pair| {
            let hi = char::from(pair[0]).to_digit(16)?;
            let lo = char::from(pair[1]).to_digit(16)?;
            u8::try_from(hi * 16 + lo).ok()
        })
        .collect()
}

impl ParserRepr {
    /// Convert `carry` into its wire representation, nesting under [`Self::exact`] when needed for older builds.
    #[must_use]
    pub fn from_carry(carry: &ParserCarry) -> Self {
        match carry.for_older_builds() {
            Some(older) => Self {
                exact: Some(Box::new(Self::fields(carry))),
                ..Self::fields(&older)
            },
            None => Self::fields(carry),
        }
    }

    fn fields(carry: &ParserCarry) -> Self {
        Self {
            state: carry.state.name().to_owned(),
            params: carry.params.clone(),
            subparam_mask: carry.subparam_mask,
            current_param: carry.current_param,
            param_started: carry.param_started,
            last_was_colon: carry.last_was_colon,
            intermediates: to_hex(&carry.intermediates),
            osc: to_hex(&carry.osc),
            osc_overflowed: carry.osc_overflowed,
            utf8_tail: to_hex(&carry.utf8_tail),
            utf8_expected: carry.utf8_expected,
            string_utf8_tail: to_hex(&carry.string_utf8_tail),
            tmux: carry.tmux.name().to_owned(),
            exact: None,
        }
    }

    /// Decode the carry ([`Self::exact`] when present), or `None` for an unknown state or tmux name
    /// or invalid hex.
    #[inline]
    #[must_use]
    pub fn to_carry(&self) -> Option<ParserCarry> {
        self.exact.as_deref().unwrap_or(self).fields_to_carry()
    }

    fn fields_to_carry(&self) -> Option<ParserCarry> {
        Some(ParserCarry {
            state: State::from_name(&self.state)?,
            params: self.params.clone(),
            subparam_mask: self.subparam_mask,
            current_param: self.current_param,
            param_started: self.param_started,
            last_was_colon: self.last_was_colon,
            intermediates: from_hex(&self.intermediates)?,
            osc: from_hex(&self.osc)?,
            osc_overflowed: self.osc_overflowed,
            utf8_tail: from_hex(&self.utf8_tail)?,
            utf8_expected: self.utf8_expected,
            string_utf8_tail: from_hex(&self.string_utf8_tail)?,
            tmux: TmuxPassthrough::from_name(&self.tmux)?,
        })
    }
}

/// Read a carried [`ParserRepr`], or `None` when the value is not one:
/// `CheckpointMeta` is parsed whole (as with `lenient_title`), and a bad parser
/// carry must not cost the screen. `None` restores a Ground parser.
pub(super) fn lenient_parser<'de, D>(deserializer: D) -> Result<Option<ParserRepr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Parser(ParserRepr),
        Unreadable(serde::de::IgnoredAny),
    }
    Ok(
        match <Wire as serde::Deserialize>::deserialize(deserializer)? {
            Wire::Parser(parser) => Some(parser),
            Wire::Unreadable(_) => None,
        },
    )
}

#[cfg(test)]
#[path = "checkpoint_parser_tests.rs"]
mod tests;
