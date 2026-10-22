// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// Production code may not unwrap; tests opt out once, at the crate root.
#![warn(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! VT100/ANSI escape sequence parser: a compile-time transition table after the
//! [vt100.net DEC ANSI parser](https://vt100.net/emu/dec_ansi_parser), SIMD
//! scanning of printable runs and string payloads with a scalar fallback, and
//! no allocation while parsing beyond the bounded OSC buffer.
//!
//! ## Verification
//!
//! - Exhaustive tests (`src/tests/`): representative states at every bound
//!   (each state, each bound the invariant names, every UTF-8 lead and each
//!   Table 3-7 row's pending prefixes) x every two-byte tail x both C1 modes
//!   keep the TypeInvariant
//!   (`tests::invariants::every_state_every_byte_keeps_the_type_invariant`); every
//!   introducer terminates; parameter accumulation saturates and finalizing
//!   clamps; every carry `restore_carry` accepts keeps the invariant through
//!   one more byte, and a refusal installs nothing.
//! - Deterministic fuzz (`tests/fuzz_parser.rs`): 400,000 pseudo-random
//!   chunks, raw and shaped like escape sequences, through `advance_fast`
//!   with state carried across chunks; any panic fails it.
//!
//! ## Complexity
//!
//! Key techniques:
//! - Compile-time transition table
//! - SIMD escape scanning (AVX2 intrinsics, auto-vectorized NEON) with scalar fallback
//! - Zero allocation during parse
//!
//! ## Complexity Proof (O(n))
//!
//! The parser processes input in O(n) time where n is the number of input bytes.
//! This is guaranteed by:
//!
//! 1. **Main loop**: `advance_fast` walks the input once.
//!    - Best case: SIMD `take_printable` skips runs of printable ASCII
//!    - Worst case: each byte goes through `process_byte_inner`
//!
//! 2. **Per-byte operations are O(1)**: `process_byte_inner` does:
//!    - Constant-time table lookup: `TRANSITIONS[state][byte]`
//!    - Bounded buffer operations:
//!      - `params`: ArrayVec<u16, 24> (MAX_PARAMS = 24)
//!      - `intermediates`: ArrayVec<u8, 4> (MAX_INTERMEDIATES = 4)
//!      - `osc_data`: capped at MAX_OSC_DATA = 8 MiB, with O(1) push
//!
//! 3. **No nested loops**: Each byte triggers at most one state transition
//!    and one action dispatch. No action re-processes previous input.
//!
//! 4. **Bounds enforcement**: All buffer operations check capacity before
//!    insertion, preventing unbounded growth.
//!
//! Throughput floors live in the `aterm-bench` harnesses (`verify perf`).

#![warn(missing_docs, clippy::pedantic)]
#![cfg_attr(test, allow(clippy::all, clippy::pedantic))]
#![allow(
    clippy::must_use_candidate,
    reason = "not all functions need #[must_use]"
)]
#![allow(
    clippy::module_name_repetitions,
    reason = "e.g. parser::Parser is idiomatic"
)]
#![allow(
    clippy::similar_names,
    reason = "fg/bg, row/col are domain-standard pairs"
)]
#![allow(clippy::wildcard_imports, reason = "use prelude::* is idiomatic")]
#![allow(
    clippy::match_same_arms,
    reason = "explicit match arms aid readability"
)]
#![allow(
    clippy::match_bool,
    reason = "match on bool can be clearer than if/else"
)]
#![allow(clippy::single_match_else, reason = "match for clarity")]
#![allow(
    clippy::missing_panics_doc,
    reason = "documentation coverage is incremental"
)]
#![allow(
    clippy::missing_errors_doc,
    reason = "documentation coverage is incremental"
)]
#![allow(
    clippy::doc_markdown,
    reason = "technical terms like VT100 don't need backticks"
)]

mod action;
mod carry;
mod csi;
mod dispatch;
mod invariants;
mod simd;
mod state;
/// State transition table.
pub mod table;
mod tmux;
mod utf8;
mod utf8_simd;

pub use action::{Action, ActionSink, NullSink};
pub use carry::{CarryRefusal, HOOKED_STRING_CARRY, OSC_CARRY_CAP, ParserCarry, UTF8_TAIL_MAX};
pub use state::State;
pub use table::{ActionType, TRANSITIONS};
pub use tmux::TmuxPassthrough;

use aterm_alloc::ArrayVec;

// Test instrumentation: Deterministic operation counters
// These counters replace wall-clock timing for complexity assertions in tests.
// See #1572 for the rationale behind deterministic counters.
//
// NOTE: Use full path std::cell::Cell for consistency with grid module pattern.
// Parser doesn't have a Cell type conflict, but we maintain the pattern org-wide.

// Counter for main loop iterations in advance_fast (O(n) verification).
#[cfg(test)]
thread_local! {
    static PARSER_LOOP_ITERATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Increment the parser loop iteration counter.
#[cfg(test)]
fn count_parser_loop_iteration() {
    PARSER_LOOP_ITERATIONS.with(|c| c.set(c.get() + 1));
}

/// Take (read and reset) the parser loop iteration count.
#[cfg(test)]
fn take_parser_loop_iterations() -> usize {
    PARSER_LOOP_ITERATIONS.with(|c| {
        let v = c.get();
        c.set(0);
        v
    })
}

/// Maximum number of CSI parameters. Real SGR runs are long: Kakoune emits a
/// 17-parameter SGR (`tests::csi::parse_csi_kakoune_sgr`) that a cap of 16
/// would truncate.
pub const MAX_PARAMS: usize = 24;

/// Maximum number of intermediate bytes
pub const MAX_INTERMEDIATES: usize = 4;

/// Maximum OSC data size (8 MiB): room for the base64 of a full-HD PNG in an
/// OSC 1337 `File=` image. It bounds one in-flight sequence, and `dispatch_osc`
/// shrinks the buffer after a large one, so it is a transient spike. Handlers
/// keep their own payload limits; this cap bounds only raw buffering.
pub(crate) const MAX_OSC_DATA: usize = 8 * 1024 * 1024;

/// Maximum number of OSC parameters (`;`-separated segments); the last carries
/// the remainder verbatim. Independent of `MAX_PARAMS`: OSC payloads carry no
/// long SGR runs.
pub(crate) const MAX_OSC_PARAMS: usize = 16;

/// VT parser state machine.
///
/// ## Example
///
/// ```
/// use aterm_parser::{ActionSink, Parser};
/// use aterm_provenance::{Provenance, Pty};
///
/// struct PrintSink;
/// impl ActionSink for PrintSink {
///     fn print(&mut self, c: char) { print!("{}", c); }
///     fn execute(&mut self, _byte: u8) {}
///     fn csi_dispatch(
///         &mut self,
///         _params: &Provenance<[u16], Pty>,
///         _intermediates: &Provenance<[u8], Pty>,
///         _final_byte: u8,
///     ) {}
///     fn esc_dispatch(&mut self, _intermediates: &Provenance<[u8], Pty>, _final_byte: u8) {}
///     fn osc_dispatch(&mut self, _params: &Provenance<[&[u8]], Pty>) {}
///     fn dcs_hook(
///         &mut self,
///         _params: &Provenance<[u16], Pty>,
///         _intermediates: &Provenance<[u8], Pty>,
///         _final_byte: u8,
///     ) {}
///     fn dcs_put(&mut self, _byte: u8) {}
///     fn dcs_unhook(&mut self, _canceled: bool) {}
///     fn apc_start(&mut self) {}
///     fn apc_put(&mut self, _byte: u8) {}
///     fn apc_end(&mut self, _canceled: bool) {}
/// }
///
/// let mut parser = Parser::new();
/// let mut sink = PrintSink;
/// parser.advance_fast(b"Hello, World!", &mut sink);
/// ```
#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "parser state machine has many boolean flags"
)]
pub struct Parser {
    pub(crate) state: State,
    pub(crate) params: ArrayVec<u16, MAX_PARAMS>,
    pub(crate) intermediates: ArrayVec<u8, MAX_INTERMEDIATES>,
    pub(crate) osc_data: Vec<u8>,
    /// Discard the OSC string being collected at its terminator instead of
    /// dispatching it. Set by [`Parser::restore_carry`] for an OSC whose head
    /// was over [`OSC_CARRY_CAP`] (the tail alone is a truncated payload: a
    /// clipped clipboard write, a broken image) and by the CAN or SUB that
    /// cancels the string. Cleared by every OSC exit and by [`Parser::reset`].
    pub(crate) osc_discard: bool,
    pub(crate) current_param: u32,
    pub(crate) param_started: bool,
    pub(crate) dcs_active: bool,
    /// A hooked DCS string met an ESC: the parser stands in `Escape`, and the
    /// next byte decides how the string ends. `\` completes the ST and the
    /// string ends complete (`dcs_unhook(false)`); any other byte breaks it off,
    /// and a broken-off string is canceled (`dcs_unhook(true)`), as CAN and SUB
    /// cancel one: xterm answers or applies a DCS only at ST. Until then the
    /// string is still hooked to the sink ([`Parser::in_hooked_dcs`]).
    pub(crate) dcs_st_pending: bool,
    /// Tracks whether we're in an APC sequence (vs SOS/PM)
    pub(crate) apc_active: bool,
    /// Where the parser stands with respect to a tmux passthrough wrapper
    /// (`crate::tmux`): reading its head, or inside it, unwrapping.
    pub(crate) tmux: TmuxPassthrough,
    /// UTF-8 decoding buffer for multi-byte sequences
    pub(crate) utf8_buffer: [u8; 4],
    /// Number of bytes accumulated in utf8_buffer
    pub(crate) utf8_len: u8,
    /// Expected total bytes for current UTF-8 sequence
    pub(crate) utf8_expected: u8,
    /// The head of a split UTF-8 character that a DCS or APC string's payload
    /// ends in (`DcsPassthrough`, `DcsIgnore`, `SosPmApcString`; nothing in
    /// every other state). With C1 controls off a `0x9C` that continues it is
    /// the character's byte, not the 8-bit ST.
    pub(crate) string_utf8: utf8::Utf8Prefix,
    /// Bit i set: param\[i\] is a colon subparameter of param\[i-1\] (SGR 4:x).
    /// u32, so every index below `MAX_PARAMS` (24) has a bit.
    pub(crate) subparam_mask: u32,
    /// Tracks if the last separator was a colon (for next param)
    pub(crate) last_was_colon: bool,
    /// Interpret 8-bit C1 controls (0x80-0x9F). Off by default, the secure
    /// choice for a UTF-8 terminal: the bytes are then invalid UTF-8 (U+FFFD in
    /// Ground), since C1 controls embedded in text can inject escape sequences
    /// (dgl.cx/2023/09/ansi-terminal-security). On only for legacy applications.
    pub(crate) c1_controls_enabled: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    /// Create a new parser in the ground state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            params: ArrayVec::new_const(),
            intermediates: ArrayVec::new_const(),
            osc_data: Vec::with_capacity(128),
            osc_discard: false,
            current_param: 0,
            param_started: false,
            dcs_active: false,
            dcs_st_pending: false,
            apc_active: false,
            tmux: TmuxPassthrough::Off,
            utf8_buffer: [0; 4],
            utf8_len: 0,
            utf8_expected: 0,
            string_utf8: utf8::Utf8Prefix::NONE,
            subparam_mask: 0,
            last_was_colon: false,
            c1_controls_enabled: false,
        }
    }

    /// Create a parser that interprets 8-bit C1 controls (0x80-0x9F).
    #[must_use]
    #[cfg(test)]
    pub fn with_c1_controls() -> Self {
        let mut parser = Self::new();
        parser.c1_controls_enabled = true;
        parser
    }

    /// Reset parser to ground state.
    pub fn reset(&mut self) {
        self.state = State::Ground;
        self.params.clear();
        self.intermediates.clear();
        self.osc_data.clear();
        self.osc_discard = false;
        self.current_param = 0;
        self.param_started = false;
        self.dcs_active = false;
        self.dcs_st_pending = false;
        self.apc_active = false;
        self.tmux = TmuxPassthrough::Off;
        self.utf8_len = 0;
        self.utf8_expected = 0;
        self.string_utf8 = utf8::Utf8Prefix::NONE;
        self.subparam_mask = 0;
        self.last_was_colon = false;
    }

    /// Get current parser state.
    #[inline]
    pub const fn state(&self) -> State {
        self.state
    }

    /// Whether a DCS string is hooked to the sink: inside its payload
    /// (`DcsPassthrough`), or at the ESC that may begin its ST, where the next
    /// byte decides whether it ends complete or broken off. What the hook
    /// collected lives in the sink, which no [`Parser::carry`] reaches.
    #[inline]
    #[must_use]
    pub const fn in_hooked_dcs(&self) -> bool {
        self.dcs_active || self.dcs_st_pending
    }

    /// Where the parser stands with respect to a tmux passthrough wrapper
    /// (`ESC P tmux; … ESC \`, unwrapped as tmux unwraps it): an open one
    /// leaves the parser mid-sequence whatever [`Parser::state`] its payload
    /// is in. `CAN` then `ESC \` end one from anywhere: the CAN ends the
    /// payload's sequence (and the head of a wrapper not yet open), the
    /// `ESC \` the wrapper.
    #[inline]
    #[must_use]
    pub const fn tmux_passthrough(&self) -> TmuxPassthrough {
        self.tmux
    }
}

#[cfg(test)]
mod tests;
