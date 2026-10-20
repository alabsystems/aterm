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
use crate::tmux::TmuxPassthrough;
use crate::utf8::Utf8Prefix;
use crate::{MAX_INTERMEDIATES, MAX_OSC_DATA, MAX_PARAMS, Parser};

/// The most OSC payload bytes a carry holds. An OSC whose collected head is
/// longer (a clipboard write, an inline image) is carried as
/// [`ParserCarry::osc_overflowed`] — the successor swallows its tail and
/// dispatches nothing — so the carry stays small whatever the payload.
pub const OSC_CARRY_CAP: usize = 4096;

/// The most bytes of a split UTF-8 character a carry holds: a four-byte
/// sequence missing its last byte.
pub const UTF8_TAIL_MAX: usize = 3;

/// The state a hooked DCS string is carried in ([`Parser::carry_unhooking`]):
/// the SOS/PM/APC string state, restored with no APC consumer, whose
/// transitions are `DcsPassthrough`'s for every byte
/// (`tests::carry::the_hooked_string_carry_reads_every_byte_as_the_hooked_string`
/// pins the table and drives both parsers through every one- and two-byte
/// tail, in both C1 modes).
pub const HOOKED_STRING_CARRY: State = State::SosPmApcString;

/// A parser's partial state, as data (see the module docs).
///
/// `state` is where the parser stands. The parameter fields (`params`,
/// `subparam_mask`, `current_param`, `param_started`, `last_was_colon`) are
/// meaningful in `CsiParam`, `CsiIntermediate`, `DcsParam` and
/// `DcsIntermediate`; `intermediates` in `EscapeIntermediate` and those four;
/// `osc`/`osc_overflowed` in `OscString`; `utf8_tail`/`utf8_expected` in
/// `Ground`; `string_utf8_tail` in `DcsIgnore` and `SosPmApcString`. Every
/// other field is empty — [`Parser::carry`] writes it so and
/// [`Parser::restore_carry`] demands it.
///
/// A DCS header (`DcsEntry`, `DcsParam`, `DcsIntermediate`) is carried as
/// itself, as a CSI is: it holds only its parameters and intermediates, and
/// the successor hooks the string at its final byte. An ignored DCS
/// (`DcsIgnore`) is carried as itself, and SOS, PM and APC strings as
/// `SosPmApcString` with no APC consumer: the successor swallows the rest of
/// the string up to its terminator instead of printing it (the tail of a
/// sixel or kitty-graphics payload is base64 as text). A HOOKED DCS string,
/// when the caller asks for one ([`Parser::carry_unhooking`]), is carried as
/// `SosPmApcString` with no consumer too: that state reads every byte exactly
/// as `DcsPassthrough` does.
///
/// A carry that crosses to another build takes
/// [`ParserCarry::for_older_builds`] along beside it: the form a build from
/// before DCS header carries reads in its place.
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
    /// The OSC payload was over [`OSC_CARRY_CAP`]: the successor discards the
    /// OSC at its terminator. `osc` is then empty.
    pub osc_overflowed: bool,
    /// The leading bytes of a split UTF-8 character (at most
    /// [`UTF8_TAIL_MAX`]).
    pub utf8_tail: Vec<u8>,
    /// The length of the character `utf8_tail` begins (0 without a tail).
    pub utf8_expected: u8,
    /// The head of a split UTF-8 character that a DCS or APC string's payload
    /// ends in (at most [`UTF8_TAIL_MAX`] bytes): with C1 controls off, a
    /// `0x9C` that continues it is the character's byte, not the 8-bit ST, so
    /// the successor must know it is there. A carry from a build before this
    /// field has none, and one this build writes reads, to such a build, as a
    /// string whose next `0x9C` ends it (serde ignores the unknown field).
    pub string_utf8_tail: Vec<u8>,
    /// The tmux passthrough wrapper the parser stands in
    /// ([`TmuxPassthrough::Open`] or [`TmuxPassthrough::Escape`], with the
    /// payload's own state in the fields above), or at the head of
    /// ([`TmuxPassthrough::Head`], only in `DcsIgnore`). A build before
    /// wrappers were unwrapped ignores it and reads the rest of the wrapper
    /// bare, as it read every wrapper.
    pub tmux: TmuxPassthrough,
}

impl ParserCarry {
    /// Whether this carry is a Ground parser holding nothing — what a fresh
    /// parser already is, so a checkpoint records no carry for it.
    #[must_use]
    pub fn is_ground(&self) -> bool {
        *self == Self::default()
    }

    /// The carry a build that predates DCS header carries reads in this one's
    /// place, or `None` when such a build reads this one as it is.
    ///
    /// Until 2026-10-10 a DCS header (`DcsEntry`, `DcsParam`,
    /// `DcsIntermediate`) was carried as `DcsIgnore` holding nothing, and
    /// `restore_carry` refused a header: a successor of that vintage handed one
    /// resumes at Ground and prints the string. The header is now carried as
    /// itself (round fifteen, R15-PARSER-P1), so a carry that crosses to
    /// another build (`aterm-core`'s wire form) writes this coarse form beside
    /// it (ruling 2026-10-10): an older successor reads the `DcsIgnore` it
    /// wrote itself and swallows the string to its terminator, as it always
    /// did, and this build and every later one read the exact carry. A newer
    /// successor handed an older producer's `DcsIgnore` reads it as that
    /// producer meant it.
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

/// Why [`Parser::restore_carry`] refused a carry. The parser is untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarryRefusal {
    /// The state is not one a carry may name (`DcsPassthrough`, whose string
    /// lives in the handler it hooked).
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
    /// A UTF-8 tail (`utf8_tail`, or `string_utf8_tail`) that is not the
    /// prefix the parser collects: a byte that begins a well-formed sequence
    /// (`0xC2..=0xF4`) — with `utf8_expected` its length — then the
    /// continuation bytes a well-formed prefix of it allows (Unicode Table
    /// 3-7), shorter than that length.
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
    /// The parser's partial state as a [`ParserCarry`] — a pure read.
    ///
    /// `None` only for a hooked DCS string ([`Parser::in_hooked_dcs`]: in
    /// `DcsPassthrough`, or at the ESC that may begin its ST): its state lives
    /// in the handler it was hooked to, which no carry can reach. Everything else
    /// is carried: a Ground parser (with the head of a split UTF-8 character,
    /// if one is pending), an escape, CSI or DCS header with its parameters
    /// and intermediates, an OSC string with its payload (or, over
    /// [`OSC_CARRY_CAP`], as one to discard), and an ignored DCS or an
    /// SOS/PM/APC string as one to ignore to its terminator.
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

    /// [`Parser::carry`], and in `DcsPassthrough` too: a hooked DCS string is
    /// carried as one to IGNORE to its terminator, so its tail is swallowed
    /// rather than printed. Whether that loses anything is the HOOKED
    /// HANDLER's question — this parser cannot see what its hook collected —
    /// so the caller decides when to take it: `aterm-core`'s checkpoint takes
    /// it when the handler keeps nothing (a string it only consumes, such as
    /// tmux control mode) or holds a picture a stream's next frame
    /// replaces (a sixel), and never for a query whose reply would be lost
    /// (round eight of the update audit, finding 57). A pure read.
    ///
    /// The carried state is `SosPmApcString` with no APC consumer, because its
    /// transitions are `DcsPassthrough`'s byte for byte
    /// ([`HOOKED_STRING_CARRY`]): every byte stays in the string — DEL, the
    /// C1 range and every UTF-8 byte included — except CAN and SUB (to Ground,
    /// executed), ESC (to Escape) and the 8-bit ST `0x9C` (to Ground), in
    /// either C1 mode, since both states are exempt from the C1 guard; with C1
    /// off, a `0x9C` inside a UTF-8 character stays in either string, and the
    /// head of one the payload ends in rides as `string_utf8_tail`. With no
    /// consumer it delivers nothing, as the handler that keeps nothing does.
    /// `DcsIgnore`, the first choice, was not exact then (the review of round
    /// eight's fix): it had no entry at or above `0x80`, so a UTF-8 lead byte
    /// in a tmux control-mode line or a kitty command left the string and the
    /// rest printed as text, and with C1 off an 8-bit ST was dropped and the
    /// string swallowed the real output after it, up to the next ESC. The
    /// table has since given `DcsIgnore` those entries
    /// (`tests::c1::an_ignored_dcs_reads_every_byte_as_a_hooked_one`), which
    /// the live parser needed too. Every consumer that restores a carry at all
    /// accepts `SosPmApcString`.
    ///
    /// At the ESC that may begin the hooked string's ST, the ESC is already
    /// read, so the carry is `Escape` holding nothing: the successor reads the
    /// next byte as that ESC's, which is what follows the string whichever way
    /// it ends, and the string it never hooked ends with nothing, as above.
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
    /// one to SWALLOW, where the live parser would still have delivered it:
    ///
    /// * an OSC string whose collected payload is over [`OSC_CARRY_CAP`] — the
    ///   live parser keeps up to [`MAX_OSC_DATA`], so an OSC 52 clipboard
    ///   write or an OSC 1337 image dispatches here and is dropped by the
    ///   carry (one the live parser already discards past that is not lost);
    /// * an APC string with its consumer started (`apc_start` was sent, e.g. a
    ///   kitty-graphics command), whose consumer the carry cannot reach.
    ///
    /// An SOS or PM string, a `DcsIgnore` and a discarded CSI are swallowed by
    /// the live parser too, so their carry loses nothing, and a DCS header is
    /// carried as itself (since 2026-10-10; it was carried as `DcsIgnore`
    /// before) and hooked by the successor. `false` at Ground and for
    /// `DcsPassthrough` (which no carry holds at all). A pure read.
    #[must_use]
    pub fn carry_swallows_sequence(&self) -> bool {
        match self.state {
            State::OscString => !self.osc_discard && self.osc_data.len() > OSC_CARRY_CAP,
            State::SosPmApcString => self.apc_active,
            _ => false,
        }
    }

    /// Whether the sequence [`Parser::carry_swallows_sequence`] names is a
    /// PICTURE — an APC string with its consumer started (a kitty-graphics
    /// command) or an OSC 1337 payload (an iTerm2 inline image) — whose loss
    /// a graphics stream's next frame makes good. An OSC 52 clipboard write
    /// and any other OSC are not: nothing replaces them. A pure read.
    #[must_use]
    pub fn carry_swallows_picture(&self) -> bool {
        self.carry_swallows_sequence()
            && match self.state {
                State::SosPmApcString => true,
                State::OscString => self.osc_data.starts_with(b"1337;"),
                _ => false,
            }
    }

    /// Install `carry` (see [`Parser::carry`]) after validating it; on `Err`
    /// the parser is untouched.
    ///
    /// Accepted states: every state but `DcsPassthrough` — `Ground`, `Escape`,
    /// `EscapeIntermediate`, the four CSI states, the three DCS header states
    /// (`DcsEntry`, `DcsParam`, `DcsIntermediate`), `OscString`, and the two
    /// ignore states, `DcsIgnore` and `SosPmApcString` (restored with no APC
    /// consumer). A restored header is a sequence like a restored CSI: the
    /// program's next byte decides what it dispatches, and no DCS or APC
    /// consumer is armed by a restore. Enforced: at most [`MAX_PARAMS`]
    /// parameters, subparameter bits only below `params.len()`, at most
    /// [`MAX_INTERMEDIATES`] intermediates in `0x20..=0x2F` (and the private
    /// markers `0x3C..=0x3F` in a CSI or DCS header state), an OSC payload of at
    /// most [`OSC_CARRY_CAP`] bytes, UTF-8 tails that are prefixes the parser
    /// itself collects, and every field the state does not use empty.
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
        // A CSI or DCS header: the states that collect the private markers.
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
        // The states that hold parameters.
        let parameters = matches!(
            carry.state,
            State::CsiParam | State::CsiIntermediate | State::DcsParam | State::DcsIntermediate
        );

        // The parameter fields.
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
        // `params.len() <= MAX_PARAMS (24) < 32`, so the shift is in range.
        if carry.subparam_mask >> carry.params.len() != 0 {
            return Err(CarryRefusal::SubparamMask);
        }
        if !carry.param_started && carry.current_param != 0 {
            return Err(CarryRefusal::CurrentParam);
        }

        // Intermediates: what the state's Collect actions gather.
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
        match Utf8Prefix::from_bytes(&carry.utf8_tail) {
            Some(prefix) if prefix.expected() == carry.utf8_expected => {}
            _ => return Err(CarryRefusal::Utf8Tail),
        }

        // A string payload's UTF-8 head: the same prefixes, and only in the
        // two string states a carry names.
        if !carry.string_utf8_tail.is_empty()
            && !matches!(carry.state, State::DcsIgnore | State::SosPmApcString)
        {
            return Err(CarryRefusal::FieldOutsideState);
        }
        if Utf8Prefix::from_bytes(&carry.string_utf8_tail).is_none() {
            return Err(CarryRefusal::Utf8Tail);
        }

        // A wrapper's head: 1 to 4 bytes of `tmux;`, read only in the ignored
        // DCS its `ESC P t` began. An open wrapper holds any payload state.
        if let TmuxPassthrough::Head(matched) = carry.tmux
            && (carry.state != State::DcsIgnore || !(1..=4).contains(&matched))
        {
            return Err(CarryRefusal::FieldOutsideState);
        }
        Ok(())
    }
}
