// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Parser state definitions.

/// Parser states based on vt100.net DEC ANSI parser.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum State {
    /// Initial state, normal text processing
    #[default]
    Ground = 0,

    /// After ESC, waiting for next byte
    Escape,

    /// ESC followed by intermediate byte (0x20-0x2F)
    EscapeIntermediate,

    /// After ESC [, start of CSI sequence
    CsiEntry,

    /// Collecting CSI parameters (digits, semicolons)
    CsiParam,

    /// CSI with intermediate bytes
    CsiIntermediate,

    /// Invalid CSI sequence, ignoring until final byte
    CsiIgnore,

    /// After ESC P, start of DCS sequence
    DcsEntry,

    /// Collecting DCS parameters
    DcsParam,

    /// DCS with intermediate bytes
    DcsIntermediate,

    /// Passing through DCS data
    DcsPassthrough,

    /// Invalid DCS, ignoring
    DcsIgnore,

    /// Collecting OSC string (after ESC ])
    OscString,

    /// SOS, PM, or APC string
    SosPmApcString,
}

// NOTE(#2368): State is returned by Parser::state() (prelude API); helpers stay pub.
impl State {
    /// Total number of states (for table sizing).
    pub const COUNT: usize = 14;

    /// Returns true if this is a ground state.
    #[inline]
    pub const fn is_ground(self) -> bool {
        matches!(self, State::Ground)
    }

    /// The state's name, as a static string (identical to its `Debug` form).
    ///
    /// Exists so a caller that must REPORT a state — the seamless-update
    /// capture naming the partial sequence it abandoned when a session's parser
    /// was left mid-sequence (the 2026-09-22/23 update audit: an unterminated
    /// `ESC ] 0 ; x` refused every in-session update) — can log it without
    /// allocating, and without an exhaustive match it could not write from
    /// outside this crate (`State` is `#[non_exhaustive]`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            State::Ground => "Ground",
            State::Escape => "Escape",
            State::EscapeIntermediate => "EscapeIntermediate",
            State::CsiEntry => "CsiEntry",
            State::CsiParam => "CsiParam",
            State::CsiIntermediate => "CsiIntermediate",
            State::CsiIgnore => "CsiIgnore",
            State::DcsEntry => "DcsEntry",
            State::DcsParam => "DcsParam",
            State::DcsIntermediate => "DcsIntermediate",
            State::DcsPassthrough => "DcsPassthrough",
            State::DcsIgnore => "DcsIgnore",
            State::OscString => "OscString",
            State::SosPmApcString => "SosPmApcString",
        }
    }

    /// Returns true if we're inside a CSI sequence.
    #[inline]
    pub const fn is_csi(self) -> bool {
        matches!(
            self,
            State::CsiEntry | State::CsiParam | State::CsiIntermediate | State::CsiIgnore
        )
    }

    /// Returns true if we're inside a DCS sequence.
    #[inline]
    pub const fn is_dcs(self) -> bool {
        matches!(
            self,
            State::DcsEntry
                | State::DcsParam
                | State::DcsIntermediate
                | State::DcsPassthrough
                | State::DcsIgnore
        )
    }
}
