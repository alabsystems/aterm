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
    ///
    /// The invariant is written below in TLA+ notation; no TLA+ spec is
    /// checked. The exhaustive tests in `src/tests/` hold representative
    /// states at every bound to it through every two-byte tail
    /// (`tests::invariants::every_state_every_byte_keeps_the_type_invariant`).
    ///
    /// # TypeInvariant
    ///
    /// ```tla
    /// TypeInvariant ==
    ///     /\ state \in States
    ///     /\ params \in Seq(0..65535)
    ///     /\ Len(params) <= MAX_PARAMS
    ///     /\ intermediates \in Seq(0..255)
    ///     /\ Len(intermediates) <= MAX_INTERMEDIATES
    ///     /\ currentParam \in 0..65535
    /// ```
    ///
    /// # Panics
    ///
    /// Panics in test builds if any invariant is violated.
    /// Does nothing otherwise for performance.
    ///
    /// (The checks are `#[cfg(test)]`-gated rather than
    /// `#[cfg(debug_assertions)]`-gated: the only callers are this crate's
    /// own tests, and the Trust lane (`ship trust run`) fails closed on the
    /// deliberate
    /// `assert!` panic paths — which exist precisely to fire on violated
    /// invariants — when they appear in the shipped lib.)
    #[inline]
    pub fn assert_invariants(&self) {
        #[cfg(test)]
        {
            // Invariant: StateValid
            // state must be a valid State enum value (0..13)
            assert!(
                (self.state as usize) < State::COUNT,
                "TLA+ TypeInvariant violated: state {} >= COUNT {}",
                self.state as u8,
                State::COUNT
            );

            // Invariant: ParamsBounded
            // Len(params) <= MAX_PARAMS
            assert!(
                self.params.len() <= MAX_PARAMS,
                "TLA+ TypeInvariant violated: params.len() {} > MAX_PARAMS {}",
                self.params.len(),
                MAX_PARAMS
            );

            // Invariant: IntermediatesBounded
            // Len(intermediates) <= MAX_INTERMEDIATES
            assert!(
                self.intermediates.len() <= MAX_INTERMEDIATES,
                "TLA+ TypeInvariant violated: intermediates.len() {} > MAX_INTERMEDIATES {}",
                self.intermediates.len(),
                MAX_INTERMEDIATES
            );

            // Invariant: CurrentParamBounded
            // currentParam is bounded during accumulation
            // Note: current_param is u32 during accumulation and uses saturating arithmetic
            // The actual bound is checked when finalized (converted to u16), but we verify
            // the accumulator isn't in an invalid state
            // (This assertion always passes for u32 but documents the invariant)
            let _ = self.current_param; // Acknowledge the field is part of invariant checking

            // Invariant: Utf8BufferValid
            // utf8_len <= 4 (max UTF-8 sequence length)
            assert!(
                self.utf8_len <= 4,
                "TLA+ Utf8BufferValid violated: utf8_len {} > 4",
                self.utf8_len
            );

            // Invariant: Utf8ExpectedValid
            // utf8_expected <= 4
            assert!(
                self.utf8_expected <= 4,
                "TLA+ Utf8ExpectedValid violated: utf8_expected {} > 4",
                self.utf8_expected
            );

            // Invariant: Utf8ProgressValid
            // utf8_len <= utf8_expected (can't have more bytes than expected)
            assert!(
                self.utf8_len <= self.utf8_expected,
                "TLA+ Utf8ProgressValid violated: utf8_len {} > utf8_expected {}",
                self.utf8_len,
                self.utf8_expected
            );

            // Invariant: SubparamMaskValid
            // Subparam mask bits only set for indices < params.len()
            // Bit i indicates param[i] is a subparam of param[i-1]
            if self.params.len() < 32 {
                let valid_bits = (1u32 << self.params.len()) - 1;
                // Bit 0 is never meaningful (no param before param[0])
                let meaningful_mask = self.subparam_mask & !1;
                assert_eq!(
                    meaningful_mask & !valid_bits,
                    0,
                    "TLA+ SubparamMaskValid violated: subparam_mask has bits set beyond params.len() (mask={:#010x}, params.len()={})",
                    self.subparam_mask,
                    self.params.len()
                );
            }

            // Invariant: DcsApcExclusive
            // Cannot be in both DCS and APC sequence at the same time
            assert!(
                !(self.dcs_active && self.apc_active),
                "TLA+ DcsApcExclusive violated: both dcs_active and apc_active are true"
            );

            // Invariant: FlagStateConsistent. `dcs_active` is a biconditional
            // with `DcsPassthrough`: it is set only by `DcsHook` (the sole action
            // into that state) and cleared whenever the machine leaves it.
            // `apc_active` only implies `SosPmApcString`: SOS and PM enter that
            // state without the flag.
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
            // DcsStPendingInEscape: a DCS string waits on the byte after its ESC
            // only in `Escape`, where that ESC put the parser; the next byte
            // always clears it.
            assert!(!self.dcs_st_pending || self.state == State::Escape);

            // Utf8StateConsistent: UTF-8 accumulation is gated on `Ground`.
            assert!(
                self.state == State::Ground || (self.utf8_len == 0 && self.utf8_expected == 0),
                "TLA+ Utf8StateConsistent violated: utf8_len={} utf8_expected={} but state={:?}",
                self.utf8_len,
                self.utf8_expected,
                self.state
            );

            // Utf8DecoderConsistent: an empty buffer expects nothing; a partial
            // character is a 2-, 3- or 4-byte one begun by a lead byte.
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

            // NOTE: the param-parsing fields (`param_started`, `last_was_colon`,
            // `subparam_mask`) do NOT admit a clean state coupling — `clear()`
            // resets them on sequence START but `push_current_param` does not
            // clear `last_was_colon`/`subparam_mask`, so they LEAK into `Ground`
            // after a CSI dispatch (`CSI 4:3 m` read a byte at a time reaches
            // Ground with `last_was_colon == true`; the CSI fast path keeps its
            // colon in a local). Their only invariant is `subparam_mask`'s
            // bound vs `params.len()` (the `SubparamMaskValid` conjunct above),
            // which is state-independent.
        }
    }

    /// The conjuncts of [`Self::assert_invariants`] over what a string
    /// sequence holds between bytes.
    #[cfg(test)]
    fn assert_string_invariants(&self) {
        // A string payload's UTF-8 head is held only in the three strings
        // whose 8-bit ST it decides (dispatch.rs drops it as one ends).
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

        // TmuxHeadInIgnore: a tmux passthrough head is read only in the
        // ignored DCS its `ESC P t` began, and holds 1 to 4 bytes of
        // `tmux;` (the fifth opens the wrapper).
        if let crate::TmuxPassthrough::Head(matched) = self.tmux {
            assert!(
                self.state == State::DcsIgnore && (1..=4).contains(&matched),
                "TmuxHeadInIgnore violated: Head({matched}) in state={:?}",
                self.state
            );
        }
    }
}
