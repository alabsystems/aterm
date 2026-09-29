// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The parser's partial state on the seamless-handoff wire
//! ([`CheckpointMeta::parser`](super::CheckpointMeta::parser)).
//!
//! [`ParserRepr`] is [`ParserCarry`] in serde-friendly shape: the state by
//! NAME (a later build may renumber the enum, never rename a state it still
//! has), the byte strings as lowercase hex, every empty field left out. It is
//! a pure data mirror: turning it back into a [`ParserCarry`] checks only that
//! it reads (a known state name, hex that decodes); whether the carry is one a
//! parser may take is [`aterm_parser::Parser::restore_carry`]'s question,
//! asked where the carry lands (`Terminal::restore_checkpoint`).

use aterm_parser::{ParserCarry, State};

/// [`ParserCarry`] on the handoff wire (see the module docs).
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
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
fn is_zero_u8(n: &u8) -> bool {
    *n == 0
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the predicate a reference"
)]
fn is_false(b: &bool) -> bool {
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

/// Decode hex; `None` for an odd length or a non-hex digit.
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
    /// The wire form of `carry`.
    #[must_use]
    pub fn from_carry(carry: &ParserCarry) -> Self {
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
        }
    }

    /// The carry this names, or `None` when it does not read: a state name
    /// this build does not know, or a byte string that is not hex. NOT a
    /// validation of the carry (see the module docs).
    #[must_use]
    pub fn to_carry(&self) -> Option<ParserCarry> {
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
        })
    }
}

/// Read a carried [`ParserRepr`], or `None` when the value is not one — for
/// the reason `lenient_title` gives: `CheckpointMeta` is parsed whole, and a
/// parser carry is not worth the screen. A value that does not read restores
/// a Ground parser, which is what every handoff before this field did.
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
mod tests {
    use super::*;

    #[test]
    fn a_carry_crosses_the_wire_form_intact() {
        let carry = ParserCarry {
            state: State::CsiParam,
            params: vec![38, 5],
            subparam_mask: 0b10,
            current_param: 19,
            param_started: true,
            last_was_colon: true,
            intermediates: vec![b'?'],
            ..ParserCarry::default()
        };
        let repr = ParserRepr::from_carry(&carry);
        assert_eq!(repr.intermediates, "3f");
        assert_eq!(repr.to_carry(), Some(carry));
        let osc = ParserCarry {
            state: State::OscString,
            osc: b"0;t\xff".to_vec(),
            ..ParserCarry::default()
        };
        assert_eq!(ParserRepr::from_carry(&osc).to_carry(), Some(osc));
    }

    #[test]
    fn a_wire_form_that_does_not_read_is_none() {
        for repr in [
            ParserRepr {
                state: "Nowhere".into(),
                ..ParserRepr::default()
            },
            ParserRepr {
                state: "Ground".into(),
                utf8_tail: "e".into(),
                ..ParserRepr::default()
            },
            ParserRepr {
                state: "OscString".into(),
                osc: "zz".into(),
                ..ParserRepr::default()
            },
        ] {
            assert_eq!(repr.to_carry(), None, "{repr:?}");
        }
        // Negative control: the same shapes, well formed, read.
        assert!(
            ParserRepr {
                state: "Ground".into(),
                utf8_tail: "e6".into(),
                utf8_expected: 3,
                ..ParserRepr::default()
            }
            .to_carry()
            .is_some()
        );
    }
}
