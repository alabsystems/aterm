// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The parser's partial state as DATA, for the seamless-update carry.
//!
//! A seamless update hands every session's PTY to a successor process. The
//! PTY output queued behind the handoff reaches the successor's parser, so a
//! sequence the outgoing parser was in the middle of — a CSI split across two
//! reads, a UTF-8 glyph split the same way, an OSC title still arriving —
//! must continue in the successor exactly where it stopped. Otherwise its
//! tail is read from Ground and prints as text (`6mX` where a colour was
//! meant, replacement characters where a glyph was).
//!
//! [`Parser::carry`] projects the partial state into a [`ParserCarry`];
//! [`Parser::restore_carry`] validates one and installs it. The carry is
//! written by one build and read by another, so the reader treats it as
//! untrusted: it accepts only the states listed on [`Parser::restore_carry`],
//! with every field inside the bounds the live parser itself keeps, and
//! refuses anything else with a [`CarryRefusal`] — it never panics. The worst
//! an accepted carry can do is put the parser in a validated non-Ground state,
//! which CAN, SUB and ESC leave as they always do.

use crate::state::State;
use crate::{MAX_INTERMEDIATES, MAX_OSC_DATA, MAX_PARAMS, Parser};

/// The most OSC payload bytes a carry holds. An OSC whose collected head is
/// longer (a clipboard write, an inline image) is carried as
/// [`ParserCarry::osc_overflowed`] — the successor swallows its tail and
/// dispatches nothing — so the carry stays small whatever the payload.
pub const OSC_CARRY_CAP: usize = 4096;

/// The most bytes of a split UTF-8 character a carry holds: a four-byte
/// sequence missing its last byte.
pub const UTF8_TAIL_MAX: usize = 3;

/// A parser's partial state, as data (see the module docs).
///
/// `state` is where the parser stands. The CSI fields (`params`,
/// `subparam_mask`, `current_param`, `param_started`, `last_was_colon`) are
/// meaningful in `CsiParam` and `CsiIntermediate`; `intermediates` in
/// `EscapeIntermediate` and the CSI states; `osc`/`osc_overflowed` in
/// `OscString`; `utf8_tail`/`utf8_expected` in `Ground`. Every other field is
/// empty — [`Parser::carry`] writes it so and [`Parser::restore_carry`]
/// demands it.
///
/// DCS strings (before their hook) are carried as `DcsIgnore`, and SOS, PM
/// and APC strings as `SosPmApcString` with no APC consumer: the successor
/// swallows the rest of the string up to its terminator instead of printing
/// it (the tail of a sixel or kitty-graphics payload is base64 as text).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParserCarry {
    /// The parser state.
    pub state: State,
    /// Finalized CSI parameters (at most [`MAX_PARAMS`]).
    pub params: Vec<u16>,
    /// Bit `i` set: `params[i]` followed a colon (a subparameter).
    pub subparam_mask: u32,
    /// The CSI parameter being accumulated.
    pub current_param: u32,
    /// Whether a digit of `current_param` has been read.
    pub param_started: bool,
    /// Whether the last CSI separator was a colon.
    pub last_was_colon: bool,
    /// Intermediate (and CSI private-marker) bytes collected so far.
    pub intermediates: Vec<u8>,
    /// The OSC payload collected so far (at most [`OSC_CARRY_CAP`] bytes).
    pub osc: Vec<u8>,
    /// The OSC payload was over [`OSC_CARRY_CAP`]: the successor discards the
    /// OSC at its terminator. `osc` is then empty.
    pub osc_overflowed: bool,
    /// The leading bytes of a split UTF-8 character (at most
    /// [`UTF8_TAIL_MAX`]).
    pub utf8_tail: Vec<u8>,
    /// The length of the character `utf8_tail` begins (0 without a tail).
    pub utf8_expected: u8,
}

impl ParserCarry {
    /// Whether this carry is a Ground parser holding nothing — what a fresh
    /// parser already is, so a checkpoint records no carry for it.
    #[must_use]
    pub fn is_ground(&self) -> bool {
        *self == Self::default()
    }
}

/// Why [`Parser::restore_carry`] refused a carry. The parser is untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarryRefusal {
    /// The state is not one a carry may name (`DcsEntry`, `DcsParam`,
    /// `DcsIntermediate`, `DcsPassthrough`).
    State(State),
    /// More than [`MAX_PARAMS`] parameters.
    Params,
    /// A subparameter bit at or past `params.len()`.
    SubparamMask,
    /// `current_param` non-zero while no digit was read.
    CurrentParam,
    /// More than [`MAX_INTERMEDIATES`] intermediates, or a byte outside the
    /// ranges the state collects.
    Intermediates,
    /// An OSC payload over [`OSC_CARRY_CAP`], or one beside `osc_overflowed`.
    Osc,
    /// A UTF-8 tail that is not the prefix the parser collects: a lead byte
    /// (`0xC0..=0xF7`) with `utf8_expected` its length, then continuation
    /// bytes, shorter than that length.
    Utf8Tail,
    /// A field set that the state does not use.
    FieldOutsideState,
}

impl std::fmt::Display for CarryRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::State(state) => write!(f, "a carry cannot name the {} state", state.name()),
            Self::Params => write!(f, "more than {MAX_PARAMS} CSI parameters"),
            Self::SubparamMask => f.write_str("a subparameter bit past the parameters"),
            Self::CurrentParam => f.write_str("a parameter value with no digit read"),
            Self::Intermediates => f.write_str("intermediates out of range"),
            Self::Osc => write!(f, "an OSC payload over {OSC_CARRY_CAP} bytes"),
            Self::Utf8Tail => f.write_str("a UTF-8 tail the parser could not hold"),
            Self::FieldOutsideState => f.write_str("a field the state does not use"),
        }
    }
}

impl std::error::Error for CarryRefusal {}

/// The UTF-8 sequence length `start_utf8` assigns a lead byte, or `None` for a
/// byte that does not start a multi-byte sequence there.
fn utf8_len_for_lead(lead: u8) -> Option<u8> {
    match lead {
        0xC0..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF7 => Some(4),
        _ => None,
    }
}

impl Parser {
    /// The parser's partial state as a [`ParserCarry`] — a pure read.
    ///
    /// `None` only in `DcsPassthrough`: a hooked DCS string's state lives in
    /// the handler it was hooked to, which no carry can reach. Everything else
    /// is carried: a Ground parser (with the head of a split UTF-8 character,
    /// if one is pending), an escape or CSI sequence with its parameters, an
    /// OSC string with its payload (or, over [`OSC_CARRY_CAP`], as one to
    /// discard), and an unhooked DCS or an SOS/PM/APC string as one to ignore
    /// to its terminator.
    #[must_use]
    pub fn carry(&self) -> Option<ParserCarry> {
        let mut carry = ParserCarry {
            state: self.state,
            ..ParserCarry::default()
        };
        match self.state {
            State::Ground => {
                let len = usize::from(self.utf8_len).min(UTF8_TAIL_MAX);
                if len > 0 {
                    carry.utf8_tail = self.utf8_buffer.get(..len).unwrap_or(&[]).to_vec();
                    carry.utf8_expected = self.utf8_expected;
                }
            }
            State::Escape | State::CsiEntry | State::CsiIgnore => {}
            State::EscapeIntermediate => {
                carry.intermediates = self.intermediates.as_slice().to_vec();
            }
            State::CsiParam | State::CsiIntermediate => {
                carry.params = self.params.as_slice().to_vec();
                carry.subparam_mask = self.subparam_mask;
                carry.current_param = self.current_param;
                carry.param_started = self.param_started;
                carry.last_was_colon = self.last_was_colon;
                carry.intermediates = self.intermediates.as_slice().to_vec();
            }
            State::OscString => {
                if self.osc_discard || self.osc_data.len() > OSC_CARRY_CAP {
                    carry.osc_overflowed = true;
                } else {
                    carry.osc.clone_from(&self.osc_data);
                }
            }
            State::DcsEntry | State::DcsParam | State::DcsIntermediate | State::DcsIgnore => {
                carry.state = State::DcsIgnore;
            }
            State::SosPmApcString => {}
            State::DcsPassthrough => return None,
        }
        Some(carry)
    }

    /// Whether [`Parser::carry`] holds this parser's partial sequence only as
    /// one to SWALLOW, where the live parser would still have delivered it:
    ///
    /// * an OSC string whose collected payload is over [`OSC_CARRY_CAP`] — the
    ///   live parser keeps up to [`MAX_OSC_DATA`], so an OSC 52 clipboard
    ///   write or an OSC 1337 image dispatches here and is dropped by the
    ///   carry (one the live parser already discards past that is not lost);
    /// * a DCS whose header has not reached its final byte (`DcsEntry`,
    ///   `DcsParam`, `DcsIntermediate`), which the live parser would hook and
    ///   the carry turns into `DcsIgnore`;
    /// * an APC string with its consumer started (`apc_start` was sent, e.g. a
    ///   kitty-graphics command), whose consumer the carry cannot reach.
    ///
    /// An SOS or PM string, a `DcsIgnore` and a discarded CSI are swallowed by
    /// the live parser too, so their carry loses nothing. `false` at Ground and
    /// for `DcsPassthrough` (which no carry holds at all). A pure read.
    #[must_use]
    pub fn carry_swallows_sequence(&self) -> bool {
        match self.state {
            State::OscString => !self.osc_discard && self.osc_data.len() > OSC_CARRY_CAP,
            State::DcsEntry | State::DcsParam | State::DcsIntermediate => true,
            State::SosPmApcString => self.apc_active,
            _ => false,
        }
    }

    /// Install `carry` (see [`Parser::carry`]) after validating it; on `Err`
    /// the parser is untouched.
    ///
    /// Accepted states: `Ground`, `Escape`, `EscapeIntermediate`, `CsiEntry`,
    /// `CsiParam`, `CsiIntermediate`, `CsiIgnore`, `OscString`, and the two
    /// ignore states, `DcsIgnore` and `SosPmApcString` (restored with no APC
    /// consumer). Enforced: at most [`MAX_PARAMS`] parameters, subparameter
    /// bits only below `params.len()`, at most [`MAX_INTERMEDIATES`]
    /// intermediates in `0x20..=0x2F` (and the CSI private markers
    /// `0x3C..=0x3F` in a CSI state), an OSC payload of at most
    /// [`OSC_CARRY_CAP`] bytes, a UTF-8 tail that is a prefix the parser itself
    /// collects, and every field the state does not use empty.
    ///
    /// C1-control interpretation is this parser's own setting and is kept.
    pub fn restore_carry(&mut self, carry: &ParserCarry) -> Result<(), CarryRefusal> {
        Self::validate_carry(carry)?;
        self.reset();
        self.state = carry.state;
        for &param in &carry.params {
            self.params.push(param);
        }
        self.subparam_mask = carry.subparam_mask;
        self.current_param = carry.current_param;
        self.param_started = carry.param_started;
        self.last_was_colon = carry.last_was_colon;
        for &byte in &carry.intermediates {
            self.intermediates.push(byte);
        }
        self.osc_data.extend_from_slice(&carry.osc);
        self.osc_discard = carry.osc_overflowed;
        for (slot, &byte) in self.utf8_buffer.iter_mut().zip(&carry.utf8_tail) {
            *slot = byte;
        }
        self.utf8_len = u8::try_from(carry.utf8_tail.len()).unwrap_or(0);
        self.utf8_expected = carry.utf8_expected;
        Ok(())
    }

    /// The whole of [`Parser::restore_carry`]'s acceptance predicate.
    fn validate_carry(carry: &ParserCarry) -> Result<(), CarryRefusal> {
        let csi = matches!(
            carry.state,
            State::CsiEntry | State::CsiParam | State::CsiIntermediate | State::CsiIgnore
        );
        let accepted = csi
            || matches!(
                carry.state,
                State::Ground
                    | State::Escape
                    | State::EscapeIntermediate
                    | State::OscString
                    | State::DcsIgnore
                    | State::SosPmApcString
            );
        if !accepted {
            return Err(CarryRefusal::State(carry.state));
        }

        // The CSI parameter fields.
        let has_params = !carry.params.is_empty()
            || carry.subparam_mask != 0
            || carry.current_param != 0
            || carry.param_started
            || carry.last_was_colon;
        if has_params && !matches!(carry.state, State::CsiParam | State::CsiIntermediate) {
            return Err(CarryRefusal::FieldOutsideState);
        }
        if carry.params.len() > MAX_PARAMS {
            return Err(CarryRefusal::Params);
        }
        // `params.len() <= MAX_PARAMS (24) < 32`, so the shift is in range.
        if carry.subparam_mask >> carry.params.len() != 0 {
            return Err(CarryRefusal::SubparamMask);
        }
        if !carry.param_started && carry.current_param != 0 {
            return Err(CarryRefusal::CurrentParam);
        }

        // Intermediates: what the state's Collect actions gather.
        if !carry.intermediates.is_empty()
            && !matches!(
                carry.state,
                State::EscapeIntermediate | State::CsiParam | State::CsiIntermediate
            )
        {
            return Err(CarryRefusal::FieldOutsideState);
        }
        if carry.intermediates.len() > MAX_INTERMEDIATES
            || (carry.state == State::EscapeIntermediate && carry.intermediates.is_empty())
            || !carry.intermediates.iter().all(|&byte| {
                (0x20..=0x2F).contains(&byte) || (csi && (0x3C..=0x3F).contains(&byte))
            })
        {
            return Err(CarryRefusal::Intermediates);
        }

        // The OSC payload.
        if (!carry.osc.is_empty() || carry.osc_overflowed) && carry.state != State::OscString {
            return Err(CarryRefusal::FieldOutsideState);
        }
        if carry.osc.len() > OSC_CARRY_CAP.min(MAX_OSC_DATA)
            || (carry.osc_overflowed && !carry.osc.is_empty())
        {
            return Err(CarryRefusal::Osc);
        }

        // The UTF-8 tail: exactly a prefix `start_utf8` + `process_utf8_byte`
        // can hold, and only at Ground (UTF-8 is collected nowhere else).
        if (!carry.utf8_tail.is_empty() || carry.utf8_expected != 0) && carry.state != State::Ground
        {
            return Err(CarryRefusal::FieldOutsideState);
        }
        match carry.utf8_tail.split_first() {
            None if carry.utf8_expected == 0 => {}
            None => return Err(CarryRefusal::Utf8Tail),
            Some((&lead, continuation)) => {
                let Some(expected) = utf8_len_for_lead(lead) else {
                    return Err(CarryRefusal::Utf8Tail);
                };
                if carry.utf8_expected != expected
                    || carry.utf8_tail.len() >= usize::from(expected)
                    || !continuation.iter().all(|&b| (0x80..=0xBF).contains(&b))
                {
                    return Err(CarryRefusal::Utf8Tail);
                }
            }
        }
        Ok(())
    }
}
