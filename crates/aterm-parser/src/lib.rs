// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

#![warn(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Table-driven VT100/ANSI escape sequence parser.

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

/// Maximum number of CSI parameters.
pub const MAX_PARAMS: usize = 24;

/// Maximum number of intermediate bytes
pub const MAX_INTERMEDIATES: usize = 4;

/// Maximum OSC data size (8 MiB).
pub(crate) const MAX_OSC_DATA: usize = 8 * 1024 * 1024;

/// Maximum number of `;`-separated OSC parameters.
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
    /// Discard the in-flight OSC string at its terminator instead of dispatching it.
    pub(crate) osc_discard: bool,
    pub(crate) current_param: u32,
    pub(crate) param_started: bool,
    pub(crate) dcs_active: bool,
    /// Whether a hooked DCS string just saw `ESC` and awaits `\` to complete ST.
    pub(crate) dcs_st_pending: bool,
    /// Tracks whether we're in an APC sequence (vs SOS/PM)
    pub(crate) apc_active: bool,
    /// Current tmux passthrough wrapper state (`crate::tmux`).
    pub(crate) tmux: TmuxPassthrough,
    /// UTF-8 decoding buffer for multi-byte sequences
    pub(crate) utf8_buffer: [u8; 4],
    /// Number of bytes accumulated in utf8_buffer
    pub(crate) utf8_len: u8,
    /// Expected total bytes for current UTF-8 sequence
    pub(crate) utf8_expected: u8,
    /// Incomplete UTF-8 prefix at the end of a DCS/APC string payload chunk.
    pub(crate) string_utf8: utf8::Utf8Prefix,
    /// Bitmask where bit `i` marks `params[i]` as a colon subparameter of `params[i - 1]`.
    pub(crate) subparam_mask: u32,
    /// Tracks if the last separator was a colon (for next param)
    pub(crate) last_was_colon: bool,
    /// Whether 8-bit C1 controls (`0x80..=0x9F`) are interpreted.
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

    /// Whether a DCS string is currently hooked to the sink.
    #[inline]
    #[must_use]
    pub const fn in_hooked_dcs(&self) -> bool {
        self.dcs_active || self.dcs_st_pending
    }

    /// Current tmux passthrough wrapper state (`ESC P tmux; … ESC \`).
    #[inline]
    #[must_use]
    pub const fn tmux_passthrough(&self) -> TmuxPassthrough {
        self.tmux
    }
}

#[cfg(test)]
mod tests;
