// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The parser's partial state on the seamless-handoff wire
//! ([`CheckpointMeta::parser`](super::CheckpointMeta::parser)).
//!
//! [`ParserRepr`] is [`ParserCarry`] in serde shape: the state by name (a
//! later build may renumber the enum, never rename a state it still has), byte
//! strings as lowercase hex, empty fields omitted. Converting back checks only
//! that it reads; whether a parser may take the carry is
//! [`aterm_parser::Parser::restore_carry`]'s decision, made in
//! `Terminal::restore_checkpoint`.
//!
//! The carry crosses builds in both directions, so a carry an older build
//! would refuse rides in the form that build reads, with the exact one beside
//! it under [`ParserRepr::exact`] (ruling 2026-10-10): a DCS header, carried as
//! itself since then, goes out as `{"state":"DcsIgnore","exact":{…}}`. A build
//! before the key reads the `DcsIgnore` it wrote itself for a header, and
//! ignores the key; this build reads `exact`. An older producer's `DcsIgnore`
//! reads here as it was meant.

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
    /// The head of a split UTF-8 character a DCS or APC string's payload
    /// ends in, hex. A build before it ignores it (and reads the string's
    /// next `0x9C` as its ST); one after it reads its absence as no head.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub string_utf8_tail: String,
    /// The carry itself, when the fields beside it are the coarser form a
    /// build that predates it reads in its place
    /// ([`ParserCarry::for_older_builds`]): a DCS header, which such a build
    /// refuses (it would resume at Ground and print the string), rides there
    /// as the `DcsIgnore` that build wrote itself. Such a build ignores the
    /// key; this one reads only it, one level deep (an `exact` inside it is
    /// not read).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<Box<ParserRepr>>,
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
    /// The wire form of `carry`: its fields, or the fields of the form a build
    /// that predates it reads ([`ParserCarry::for_older_builds`]) with the
    /// carry itself under [`Self::exact`].
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

    /// `carry`'s own fields, with no [`Self::exact`].
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
            exact: None,
        }
    }

    /// The carry this names — [`Self::exact`] when there is one, else these
    /// fields — or `None` for an unknown state name or non-hex bytes. Not a
    /// validation of the carry (see the module docs).
    #[must_use]
    pub fn to_carry(&self) -> Option<ParserCarry> {
        self.exact.as_deref().unwrap_or(self).fields_to_carry()
    }

    /// The carry these fields name, [`Self::exact`] aside.
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
        let string = ParserCarry {
            state: State::SosPmApcString,
            string_utf8_tail: vec![0xE6],
            ..ParserCarry::default()
        };
        let repr = ParserRepr::from_carry(&string);
        assert_eq!(repr.string_utf8_tail, "e6");
        assert_eq!(repr.to_carry(), Some(string));
    }

    /// A DCS HEADER CROSSES BUILDS BOTH WAYS (round fifteen, R15-PARSER-P1;
    /// ruling 2026-10-10). Carried as itself, it rides under `exact`, beside
    /// the fields a build before the key reads: the `DcsIgnore` holding
    /// nothing that such a build wrote for every header, which its
    /// `restore_carry` accepts (without the key, which its `ParserRepr` does
    /// not have and serde drops, it reads exactly that). This build reads the
    /// header. An older producer's `DcsIgnore` reads here as it was meant, and
    /// a carry an older build reads as it is crosses with no `exact`.
    #[test]
    fn a_dcs_header_rides_beside_the_form_an_older_build_reads() {
        let header = ParserCarry {
            state: State::DcsIntermediate,
            params: vec![1],
            current_param: 2,
            param_started: true,
            intermediates: vec![b'$'],
            ..ParserCarry::default()
        };
        let repr = ParserRepr::from_carry(&header);
        let older_reads = ParserRepr {
            exact: None,
            ..repr.clone()
        };
        assert_eq!(
            older_reads,
            ParserRepr {
                state: "DcsIgnore".into(),
                ..ParserRepr::default()
            },
            "nothing beside `exact` that an older build's restore refuses"
        );
        let older_carry = older_reads.to_carry().expect("reads");
        assert_eq!(header.for_older_builds(), Some(older_carry.clone()));
        aterm_parser::Parser::new()
            .restore_carry(&older_carry)
            .expect("the older form restores");
        assert_eq!(repr.to_carry(), Some(header.clone()), "this build reads it");
        let mut parser = aterm_parser::Parser::new();
        parser
            .restore_carry(&repr.to_carry().expect("reads"))
            .expect("the header restores");
        assert_eq!(parser.state(), State::DcsIntermediate);

        let older_producer = ParserRepr {
            state: "DcsIgnore".into(),
            ..ParserRepr::default()
        };
        assert_eq!(older_producer.to_carry(), Some(older_carry));

        for carry in [
            ParserCarry {
                state: State::CsiParam,
                params: vec![38],
                ..ParserCarry::default()
            },
            ParserCarry {
                state: State::DcsIgnore,
                ..ParserCarry::default()
            },
        ] {
            let repr = ParserRepr::from_carry(&carry);
            assert_eq!(repr.exact, None, "{carry:?}");
            assert_eq!(repr.to_carry(), Some(carry));
        }
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
