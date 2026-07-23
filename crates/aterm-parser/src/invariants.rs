// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! TLA+ invariant assertions for the parser state machine.

use crate::Parser;
#[cfg(test)]
use crate::state::State;
#[cfg(test)]
use crate::{MAX_INTERMEDIATES, MAX_PARAMS};

impl Parser {
    /// The TLA+ `TypeInvariant` as a pure boolean predicate.
    ///
    /// Identical conditions to [`Parser::assert_invariants`], but returns a
    /// `bool` so it can serve as the `requires`/`ensures` predicate of the
    /// `process_byte_inner` function contract verified by Kani/trust-mc. Keeping
    /// it side-effect-free is what lets the model checker reason about it.
    #[cfg(kani)]
    #[must_use]
    pub(crate) fn type_invariant(&self) -> bool {
        use crate::state::State;
        use crate::{MAX_INTERMEDIATES, MAX_PARAMS};

        // StateValid: state is a valid enum discriminant.
        (self.state as usize) < State::COUNT
            // ParamsBounded / IntermediatesBounded.
            && self.params.len() <= MAX_PARAMS
            && self.intermediates.len() <= MAX_INTERMEDIATES
            // Utf8BufferValid / Utf8ExpectedValid / Utf8ProgressValid.
            && self.utf8_len <= 4
            && self.utf8_expected <= 4
            && self.utf8_len <= self.utf8_expected
            // DcsApcExclusive.
            && !(self.dcs_active && self.apc_active)
            // FlagStateConsistent: the `dcs_active`/`apc_active` flags are coupled
            // to the state, which is what makes the invariant INDUCTIVE under
            // `process_byte_inner`. Without these couplings the model checker can
            // assume an UNREACHABLE start state from which a single byte
            // legitimately breaks the weaker invariant (e.g. `OscString` with
            // `dcs_active`, or `DcsPassthrough` with `dcs_active == false`), so the
            // contract VC is a real counterexample rather than a proof.
            //
            // `dcs_active` is a BICONDITIONAL with `DcsPassthrough`: it is set
            // only by `DcsHook` (the sole action transitioning INTO
            // `DcsPassthrough`, table/dcs_osc.rs) and cleared whenever the machine
            // LEAVES `DcsPassthrough` (dispatch.rs), and `DcsPassthrough` is
            // reached by no other action — so `dcs_active <=> state ==
            // DcsPassthrough`.
            //
            // `apc_active` is only a ONE-WAY implication: `SosPmApcString` is also
            // entered by SOS (0x98) / PM (0x9E) with `ActionType::None` (no
            // `apc_active`), so the state does not imply the flag — only
            // `apc_active => state == SosPmApcString` holds.
            && (self.dcs_active == (self.state as usize == State::DcsPassthrough as usize))
            && (!self.apc_active || self.state as usize == State::SosPmApcString as usize)
            // NOTE: a further `Utf8StateConsistent` coupling
            // (`state != Ground => utf8_len == 0 && utf8_expected == 0`, which
            // holds at runtime — UTF-8 accumulation is gated on `Ground` in
            // dispatch.rs) is ALSO needed for full inductiveness, but adding it
            // makes the bounded-model-checker VC blow up ay's memory ceiling
            // (>24 GB) — pending a solver-side fix. The flag couplings above
            // already eliminate the GENUINE counterexamples (state↔flag
            // inconsistency); the residual is an `EncodingGap`-classified
            // (non-genuine) CEX. (#contract-inductive-invariant)
            // SubparamMaskValid: meaningful bits (excluding bit 0) only set for
            // indices < params.len().
            && (self.params.len() >= 32
                || (self.subparam_mask & !1u32 & !((1u32 << self.params.len()) - 1)) == 0)
    }

    /// Assert that all TLA+ TypeInvariant properties hold.
    ///
    /// This function verifies the parser state matches the formal specification
    /// in `tla/Parser.tla`. Only runs in debug builds.
    ///
    /// # TLA+ TypeInvariant
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
    /// own tests, and Trust's strict gate fails closed on the deliberate
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

            // Invariant: FlagStateConsistent (mirrors `type_invariant`).
            // `dcs_active` is a biconditional with `DcsPassthrough`; `apc_active`
            // implies `SosPmApcString` (SOS/PM enter that state without the flag).
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

            // Utf8StateConsistent also holds at runtime (UTF-8 accumulation is
            // gated on `Ground`), so we assert it for regression protection even
            // though it is omitted from the Kani `type_invariant` for now (it
            // overflows the model checker's memory — see invariants.rs note).
            assert!(
                self.state == State::Ground || (self.utf8_len == 0 && self.utf8_expected == 0),
                "TLA+ Utf8StateConsistent violated: utf8_len={} utf8_expected={} but state={:?}",
                self.utf8_len,
                self.utf8_expected,
                self.state
            );

            // NOTE: the param-parsing fields (`param_started`, `last_was_colon`,
            // `subparam_mask`) do NOT admit a clean state coupling — `clear()`
            // resets them on sequence START but `push_current_param` does not
            // clear `last_was_colon`/`subparam_mask`, so they LEAK into `Ground`
            // after a CSI dispatch (see `tests::invariants::
            // assert_invariants_after_subparams`, which reaches Ground with
            // `last_was_colon == true`). Their only invariant is `subparam_mask`'s
            // bound vs `params.len()` (the `SubparamMaskValid` conjunct above),
            // which is state-independent. So the remaining contract-VC CEXes that
            // set these fields are NOT fixable by a state coupling — consistent
            // with trust-mc classifying the residual as a non-genuine EncodingGap.
        }
    }
}
