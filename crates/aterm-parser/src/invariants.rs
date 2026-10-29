// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! TypeInvariant assertions for the parser state machine.

use crate::Parser;
#[cfg(test)]
use crate::state::State;
#[cfg(test)]
use crate::{MAX_INTERMEDIATES, MAX_PARAMS};

impl Parser {
    /// Assert that every TypeInvariant property holds.
    /// # Panics
    /// Panics in test builds if any invariant is violated.
    /// Does nothing otherwise for performance.
    ///
    /// (The checks are `#[cfg(test)]`-gated rather than
    /// `#[cfg(debug_assertions)]`-gated: the only callers are this crate's
    /// own tests, and the Trust lane (`ship trust run`) fails closed on the
    /// deliberate `assert!` panic paths — which exist precisely to fire on
    /// violated invariants — when they appear in the shipped lib.)
    #[inline]
    pub fn assert_invariants(&self) {
        #[cfg(test)]
        {
            assert!(
                (self.state as usize) < State::COUNT,
                "TLA+ TypeInvariant violated: state {} >= COUNT {}",
                self.state as u8,
                State::COUNT
            );

            assert!(
                self.params.len() <= MAX_PARAMS,
                "TLA+ TypeInvariant violated: params.len() {} > MAX_PARAMS {}",
                self.params.len(),
                MAX_PARAMS
            );

            assert!(
                self.intermediates.len() <= MAX_INTERMEDIATES,
                "TLA+ TypeInvariant violated: intermediates.len() {} > MAX_INTERMEDIATES {}",
                self.intermediates.len(),
                MAX_INTERMEDIATES
            );

            assert!(
                self.param_started || self.current_param == 0,
                "TLA+ CurrentParamConsistent violated: current_param={} when param_started=false",
                self.current_param
            );

            assert!(
                self.utf8_len <= 4,
                "TLA+ Utf8BufferValid violated: utf8_len {} > 4",
                self.utf8_len
            );

            assert!(
                self.utf8_expected <= 4,
                "TLA+ Utf8ExpectedValid violated: utf8_expected {} > 4",
                self.utf8_expected
            );

            assert!(
                self.utf8_len <= self.utf8_expected,
                "TLA+ Utf8ProgressValid violated: utf8_len {} > utf8_expected {}",
                self.utf8_len,
                self.utf8_expected
            );

            assert_eq!(
                self.subparam_mask >> self.params.len(),
                0,
                "TLA+ SubparamMaskValid violated: subparam_mask has bits set at or beyond params.len() (mask={:#010x}, params.len()={})",
                self.subparam_mask,
                self.params.len()
            );

            assert!(
                !(self.dcs_active && self.apc_active),
                "TLA+ DcsApcExclusive violated: both dcs_active and apc_active are true"
            );

            assert_eq!(
                self.dcs_active,
                self.state == State::DcsPassthrough,
                "TLA+ FlagStateConsistent violated: dcs_active={} but state={:?}",
                self.dcs_active,
                self.state
            );
            assert!(
                !self.apc_active || self.state == State::SosPmApcString,
                "TLA+ FlagStateConsistent violated: apc_active set but state={:?}",
                self.state
            );
            assert!(!self.dcs_st_pending || self.state == State::Escape);

            assert!(
                self.state == State::Ground || (self.utf8_len == 0 && self.utf8_expected == 0),
                "TLA+ Utf8StateConsistent violated: utf8_len={} utf8_expected={} but state={:?}",
                self.utf8_len,
                self.utf8_expected,
                self.state
            );

            if self.utf8_len == 0 {
                assert_eq!(
                    self.utf8_expected, 0,
                    "Utf8DecoderConsistent violated: an empty buffer expects {} bytes",
                    self.utf8_expected
                );
            } else {
                assert!(
                    (2..=4).contains(&self.utf8_expected)
                        && (0xC0..=0xF7).contains(&self.utf8_buffer[0]),
                    "Utf8DecoderConsistent violated: utf8_buffer={:02x?} utf8_len={} utf8_expected={}",
                    self.utf8_buffer,
                    self.utf8_len,
                    self.utf8_expected
                );
            }

            self.assert_string_invariants();
        }
    }

    /// The conjuncts of [`Self::assert_invariants`] over what a string
    /// sequence holds between bytes.
    #[cfg(test)]
    fn assert_string_invariants(&self) {
        assert!(
            self.string_utf8 == crate::utf8::Utf8Prefix::NONE
                || matches!(
                    self.state,
                    State::DcsPassthrough | State::DcsIgnore | State::SosPmApcString
                ),
            "StringUtf8StateConsistent violated: {:?} held in state={:?}",
            self.string_utf8,
            self.state
        );

        if let crate::TmuxPassthrough::Head(matched) = self.tmux {
            assert!(
                self.state == State::DcsIgnore && (1..=4).contains(&matched),
                "TmuxHeadInIgnore violated: Head({matched}) in state={:?}",
                self.state
            );
        }
    }
}
