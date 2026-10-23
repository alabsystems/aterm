// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The parser's partial state as data, for the seamless-update carry.

use crate::state::State;
use crate::tmux::TmuxPassthrough;
use crate::utf8::Utf8Prefix;
use crate::{MAX_INTERMEDIATES, MAX_OSC_DATA, MAX_PARAMS, Parser};

/// The most OSC payload bytes a carry holds.
pub const OSC_CARRY_CAP: usize = 4096;

/// The most bytes of a split UTF-8 character a carry holds: a four-byte sequence missing its last byte.
pub const UTF8_TAIL_MAX: usize = 3;

/// The state a hooked DCS string is carried in ([`Parser::carry_unhooking`]): the SOS/PM/APC string state with no APC consumer, whose transitions are `DcsPassthrough`'s for every byte (`tests::carry::the_hooked_string_carry_reads_every_byte_as_the_hooked_string`).
pub const HOOKED_STRING_CARRY: State = State::SosPmApcString;

/// A parser's partial state, as data.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParserCarry {
    /// The parser state.
    pub state: State,
    /// Finalized CSI or DCS parameters (at most [`MAX_PARAMS`]).
    pub params: Vec<u16>,
    /// Bit `i` set: `params[i]` followed a colon (a subparameter).
    pub subparam_mask: u32,
    /// The parameter being accumulated.
    pub current_param: u32,
    /// Whether a digit of `current_param` has been read.
    pub param_started: bool,
    /// Whether the last parameter separator was a colon.
    pub last_was_colon: bool,
    /// Intermediate (and private-marker) bytes collected so far.
    pub intermediates: Vec<u8>,
    /// The OSC payload collected so far (at most [`OSC_CARRY_CAP`] bytes).
    pub osc: Vec<u8>,
    /// The OSC payload was over [`OSC_CARRY_CAP`]: the successor discards the OSC at its terminator.
    pub osc_overflowed: bool,
    /// The leading bytes of a split UTF-8 character (at most [`UTF8_TAIL_MAX`]).
    pub utf8_tail: Vec<u8>,
    /// The length of the character `utf8_tail` begins (0 without a tail).
    pub utf8_expected: u8,
    /// The head of a split UTF-8 character that a DCS or APC string's payload ends in (at most [`UTF8_TAIL_MAX`] bytes): with C1 controls off, a `0x9C` that continues it is the character's byte, not ST.
    pub string_utf8_tail: Vec<u8>,
    /// The tmux passthrough wrapper the parser stands in ([`TmuxPassthrough::Open`] or [`TmuxPassthrough::Escape`], with the payload's own state in the fields above), or at the head of ([`TmuxPassthrough::Head`], only in `DcsIgnore`).
    pub tmux: TmuxPassthrough,
}

impl ParserCarry {
    /// Whether this carry is a Ground parser holding nothing — what a fresh parser already is, so a checkpoint records no carry for it.
    #[inline]
    #[must_use]
    pub fn is_ground(&self) -> bool {
        *self == Self::default()
    }

    /// The carry a build that predates DCS header carries reads in this one's place, or `None` when such a build reads this one as it is.
    #[must_use]
    pub fn for_older_builds(&self) -> Option<ParserCarry> {
        matches!(
            self.state,
            State::DcsEntry | State::DcsParam | State::DcsIntermediate
        )
        .then(|| ParserCarry {
            state: State::DcsIgnore,
            ..ParserCarry::default()
        })
    }
}

/// Why [`Parser::restore_carry`] refused a carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarryRefusal {
    /// The state is not one a carry may name (`DcsPassthrough`, whose string lives in the handler it hooked).
    State(State),
    /// More than [`MAX_PARAMS`] parameters.
    Params,
    /// A subparameter bit at or past `params.len()`.
    SubparamMask,
    /// `current_param` non-zero while no digit was read.
    CurrentParam,
    /// More than [`MAX_INTERMEDIATES`] intermediates, or a byte outside the ranges the state collects.
    Intermediates,
    /// An OSC payload over [`OSC_CARRY_CAP`], or one beside `osc_overflowed`.
    Osc,
    /// A UTF-8 tail (`utf8_tail`, or `string_utf8_tail`) that is not the prefix the parser collects: a byte that begins a well-formed sequence (`0xC2..=0xF4`) — with `utf8_expected` its length — then the continuation bytes a well-formed prefix of it allows (Unicode Table 3-7), shorter than that length.
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

impl Parser {
    /// The parser's partial state as a [`ParserCarry`]; a pure read.
    #[must_use]
    pub fn carry(&self) -> Option<ParserCarry> {
        if self.dcs_st_pending {
            return None;
        }
        let mut carry = ParserCarry {
            state: self.state,
            tmux: self.tmux,
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
            State::Escape | State::CsiEntry | State::CsiIgnore | State::DcsEntry => {}
            State::EscapeIntermediate => {
                carry.intermediates = self.intermediates.as_slice().to_vec();
            }
            State::CsiParam | State::CsiIntermediate | State::DcsParam | State::DcsIntermediate => {
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
            State::DcsIgnore | State::SosPmApcString => {
                carry.string_utf8_tail = self.string_utf8.as_slice().to_vec();
            }
            State::DcsPassthrough => return None,
        }
        Some(carry)
    }

    /// [`Parser::carry`], and in `DcsPassthrough` too: a hooked DCS string is carried as one to IGNORE to its terminator, so its tail is swallowed rather than printed.
    #[must_use]
    pub fn carry_unhooking(&self) -> ParserCarry {
        if self.dcs_st_pending {
            return ParserCarry {
                state: State::Escape,
                tmux: self.tmux,
                ..ParserCarry::default()
            };
        }
        self.carry().unwrap_or_else(|| ParserCarry {
            state: HOOKED_STRING_CARRY,
            string_utf8_tail: self.string_utf8.as_slice().to_vec(),
            tmux: self.tmux,
            ..ParserCarry::default()
        })
    }

    /// Whether [`Parser::carry`] holds this parser's partial sequence only as
    /// one to swallow, where the live parser would still deliver it:
    ///
    /// * an OSC string whose payload is over [`OSC_CARRY_CAP`] (the live parser
    ///   keeps up to [`MAX_OSC_DATA`], so an OSC 52 write or OSC 1337 image
    ///   would dispatch);
    /// * an APC string whose consumer was started (`apc_start`), which the carry
    ///   cannot reach.
    #[inline]
    #[must_use]
    pub fn carry_swallows_sequence(&self) -> bool {
        match self.state {
            State::OscString => !self.osc_discard && self.osc_data.len() > OSC_CARRY_CAP,
            State::SosPmApcString => self.apc_active,
            _ => false,
        }
    }

    /// Whether the sequence [`Parser::carry_swallows_sequence`] names is a picture a graphics stream's next frame replaces: an APC string with its consumer started (kitty graphics) or an OSC 1337 payload (an inline image).
    #[inline]
    #[must_use]
    pub fn carry_swallows_picture(&self) -> bool {
        self.carry_swallows_sequence()
            && match self.state {
                State::SosPmApcString => true,
                State::OscString => self.osc_data.starts_with(b"1337;"),
                _ => false,
            }
    }

    /// Install `carry` (see [`Parser::carry`]) after validating it; on `Err` the parser is untouched.
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
        self.string_utf8 =
            Utf8Prefix::from_bytes(&carry.string_utf8_tail).unwrap_or(Utf8Prefix::NONE);
        self.tmux = carry.tmux;
        Ok(())
    }

    /// The whole of [`Parser::restore_carry`]'s acceptance predicate.
    fn validate_carry(carry: &ParserCarry) -> Result<(), CarryRefusal> {
        // A hooked string lives in its handler.
        if carry.state == State::DcsPassthrough {
            return Err(CarryRefusal::State(carry.state));
        }
        let header = matches!(
            carry.state,
            State::CsiEntry
                | State::CsiParam
                | State::CsiIntermediate
                | State::CsiIgnore
                | State::DcsEntry
                | State::DcsParam
                | State::DcsIntermediate
        );
        let parameters = matches!(
            carry.state,
            State::CsiParam | State::CsiIntermediate | State::DcsParam | State::DcsIntermediate
        );

        let has_params = !carry.params.is_empty()
            || carry.subparam_mask != 0
            || carry.current_param != 0
            || carry.param_started
            || carry.last_was_colon;
        if has_params && !parameters {
            return Err(CarryRefusal::FieldOutsideState);
        }
        if carry.params.len() > MAX_PARAMS {
            return Err(CarryRefusal::Params);
        }
        if carry.subparam_mask >> carry.params.len() != 0 {
            return Err(CarryRefusal::SubparamMask);
        }
        if !carry.param_started && carry.current_param != 0 {
            return Err(CarryRefusal::CurrentParam);
        }

        let collects = parameters || carry.state == State::EscapeIntermediate;
        if !carry.intermediates.is_empty() && !collects {
            return Err(CarryRefusal::FieldOutsideState);
        }
        if carry.intermediates.len() > MAX_INTERMEDIATES
            || (carry.state == State::EscapeIntermediate && carry.intermediates.is_empty())
            || !carry.intermediates.iter().all(|&byte| {
                (0x20..=0x2F).contains(&byte) || (header && (0x3C..=0x3F).contains(&byte))
            })
        {
            return Err(CarryRefusal::Intermediates);
        }

        if (!carry.osc.is_empty() || carry.osc_overflowed) && carry.state != State::OscString {
            return Err(CarryRefusal::FieldOutsideState);
        }
        if carry.osc.len() > OSC_CARRY_CAP.min(MAX_OSC_DATA)
            || (carry.osc_overflowed && !carry.osc.is_empty())
        {
            return Err(CarryRefusal::Osc);
        }

        if (!carry.utf8_tail.is_empty() || carry.utf8_expected != 0) && carry.state != State::Ground
        {
            return Err(CarryRefusal::FieldOutsideState);
        }
        match Utf8Prefix::from_bytes(&carry.utf8_tail) {
            Some(prefix) if prefix.expected() == carry.utf8_expected => {}
            _ => return Err(CarryRefusal::Utf8Tail),
        }

        if !carry.string_utf8_tail.is_empty()
            && !matches!(carry.state, State::DcsIgnore | State::SosPmApcString)
        {
            return Err(CarryRefusal::FieldOutsideState);
        }
        if Utf8Prefix::from_bytes(&carry.string_utf8_tail).is_none() {
            return Err(CarryRefusal::Utf8Tail);
        }

        if let TmuxPassthrough::Head(matched) = carry.tmux
            && (carry.state != State::DcsIgnore || !(1..=4).contains(&matched))
        {
            return Err(CarryRefusal::FieldOutsideState);
        }
        Ok(())
    }
}
