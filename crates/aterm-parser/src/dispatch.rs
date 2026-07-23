// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Author: Andrew Yates

//! Parser dispatch engine: advance methods, state-machine byte processing, helpers.

use aterm_alloc::ArrayVec;
use aterm_provenance::pty_wrap_ref;

use super::table::{ActionType, TRANSITIONS};
use super::{ActionSink, MAX_OSC_DATA, MAX_OSC_PARAMS, Parser, State};

// `BatchActionSink` is only referenced by the test-only batch path
// (`advance_batch`/`process_byte_batch`), so its import is `#[cfg(test)]` too.
#[cfg(test)]
use super::BatchActionSink;

#[cfg(test)]
use super::count_parser_loop_iteration;

use super::MAX_INTERMEDIATES;

/// Shared byte-processing logic for both `ActionSink` and `BatchActionSink` paths.
///
/// Eliminates ~115 lines of duplicated state-machine dispatch code while
/// preserving full monomorphization for each sink type.
macro_rules! process_byte_impl {
    ($self:expr, $byte:expr, $sink:expr) => {{
        // Block C1 control bytes (0x80-0x9F) in non-Ground, non-string states
        // when C1 controls are disabled. The static transition table contains
        // "anywhere" entries that route 0x90→DCS, 0x9B→CSI, 0x9D→OSC, etc.
        // Without this guard, a malicious byte stream can inject escape sequences
        // via C1 introducers even when the parser is mid-sequence (#7556).
        //
        // Excluded states:
        //  - Ground: handled upstream (process_byte / process_ground_special_byte)
        //  - DcsPassthrough / OscString / SosPmApcString: their custom table
        //    overrides already map 0x80-0x9F to data actions (DcsPut / OscPut),
        //    so C1 bytes are harmless payload in those states.
        if !$self.c1_controls_enabled
            && (0x80..=0x9F).contains(&$byte)
            && !matches!(
                $self.state,
                State::Ground | State::DcsPassthrough | State::OscString | State::SosPmApcString
            )
        {
            // Silently drop — consistent with Ground state behavior.
        } else if $self.c1_controls_enabled && $byte == 0x9C && $self.state == State::OscString {
            // Runtime C1 ST override: when C1 controls are enabled and byte 0x9C
            // arrives in OscString state, treat it as ST (string terminator) instead
            // of data. The static table maps 0x9C -> OscPut for UTF-8 safety (0x9C
            // is a valid continuation byte in CJK like 本=E6 9C AC), but when C1
            // controls are explicitly enabled, 0x9C must terminate the OSC sequence
            // per the DEC spec.
            $self.dispatch_osc($sink, false);
            $self.state = State::Ground;
        } else {
            let transition = TRANSITIONS[$self.state as usize][$byte as usize];
            let prev_state = $self.state;

            // Handle DCS unhook when leaving DcsPassthrough. CAN (0x18) and SUB
            // (0x1A) CANCEL the control string (VT500 "anywhere" transition to
            // Ground via Execute), so the sink must be told to DISCARD rather
            // than finalize — otherwise a half-decoded Sixel would be rendered.
            // ESC (a real `ESC \` ST, or an ESC that breaks out) and 0x9C ST are
            // legitimate terminators (canceled = false).
            if prev_state == State::DcsPassthrough && transition.next_state != State::DcsPassthrough
            {
                if $self.dcs_active {
                    let canceled = $byte == 0x18 || $byte == 0x1A;
                    $sink.dcs_unhook(canceled);
                    $self.dcs_active = false;
                }
            }

            // Handle OSC end when leaving OscString
            if prev_state == State::OscString
                && transition.next_state != State::OscString
                && transition.action != ActionType::OscEnd
            {
                $self.dispatch_osc($sink, false);
            }

            // Handle APC end when leaving SosPmApcString
            if prev_state == State::SosPmApcString
                && transition.next_state != State::SosPmApcString
                && transition.action != ActionType::ApcEnd
            {
                if $self.apc_active {
                    $sink.apc_end();
                    $self.apc_active = false;
                }
            }

            // Execute the action
            match transition.action {
                ActionType::None | ActionType::Ignore => {}
                ActionType::Print => {
                    $sink.print($byte as char);
                }
                ActionType::Execute => {
                    $sink.execute($byte);
                }
                ActionType::Clear => {
                    $self.clear();
                    $self.osc_data.clear();
                }
                ActionType::Collect => {
                    $self.collect_intermediate($byte);
                }
                ActionType::Param => {
                    $self.add_param_digit($byte);
                }
                ActionType::EscDispatch => {
                    $sink.esc_dispatch(pty_wrap_ref($self.intermediates.as_slice()), $byte);
                }
                ActionType::CsiDispatch => {
                    if $self.param_started {
                        $self.finalize_param();
                    }
                    if $self.subparam_mask != 0 {
                        $sink.csi_dispatch_with_subparams(
                            pty_wrap_ref($self.params.as_slice()),
                            pty_wrap_ref($self.intermediates.as_slice()),
                            $byte,
                            $self.subparam_mask,
                        );
                    } else {
                        $sink.csi_dispatch(
                            pty_wrap_ref($self.params.as_slice()),
                            pty_wrap_ref($self.intermediates.as_slice()),
                            $byte,
                        );
                    }
                }
                ActionType::DcsHook => {
                    if $self.param_started {
                        $self.finalize_param();
                    }
                    $sink.dcs_hook(
                        pty_wrap_ref($self.params.as_slice()),
                        pty_wrap_ref($self.intermediates.as_slice()),
                        $byte,
                    );
                    $self.dcs_active = true;
                }
                ActionType::DcsPut => {
                    $sink.dcs_put($byte);
                }
                ActionType::OscStart => {
                    $self.osc_data.clear();
                }
                ActionType::OscPut => {
                    if $self.osc_data.len() < MAX_OSC_DATA {
                        $self.osc_data.push($byte);
                    }
                }
                ActionType::OscEnd => {
                    $self.dispatch_osc($sink, true);
                }
                ActionType::ApcStart => {
                    $sink.apc_start();
                    $self.apc_active = true;
                }
                ActionType::ApcPut => {
                    if $self.apc_active {
                        $sink.apc_put($byte);
                    }
                }
                ActionType::ApcEnd => {
                    if $self.apc_active {
                        $sink.apc_end();
                        $self.apc_active = false;
                    }
                }
            }

            $self.state = transition.next_state;
        } // end else (C1 ST override)
    }};
}

impl Parser {
    /// Process input bytes, calling sink for each action.
    ///
    /// # Safety
    ///
    /// This function:
    /// - Never panics for any input
    /// - Never accesses out-of-bounds memory
    /// - Always terminates
    pub fn advance<S: ActionSink>(&mut self, input: &[u8], sink: &mut S) {
        for &byte in input {
            // Test instrumentation: count iterations for O(n) verification
            #[cfg(test)]
            count_parser_loop_iteration();

            self.process_byte(byte, sink);
        }
    }

    /// Process input with fast path for ground state.
    ///
    /// This is an optimization that uses SIMD scanning for printable text.
    /// On typical terminal output (mostly printable text), this is 5-10x
    /// faster than the basic `advance` method.
    ///
    /// Handles UTF-8 multi-byte sequences properly for non-ASCII characters.
    pub fn advance_fast<S: ActionSink>(&mut self, input: &[u8], sink: &mut S) {
        self.advance_simd_loop(
            input,
            sink,
            |sink, data| sink.print_ascii_bulk(pty_wrap_ref(data)),
            Self::process_byte_inner,
            /* count_loops */ true,
            /* replay_escape_bracket_on_fail */ false,
            /* set_ground_after_escape_fast_path */ true,
        );
    }

    /// Process a single byte through the state machine (inner implementation).
    ///
    /// Under Kani/trust-mc this carries a function contract: starting from a
    /// state satisfying the TLA+ `TypeInvariant` (see [`Parser::type_invariant`]),
    /// processing any byte preserves it. The real dispatch lives in
    /// [`Parser::process_byte_dispatch`] because the Kani contract proc-macro
    /// no-ops on a bare `macro!()` body — it needs a real call expression to wrap.
    /// In non-Kani builds the `#[inline]` wrapper compiles away to a direct call.
    #[cfg_attr(kani, kani::requires(self.type_invariant()))]
    #[cfg_attr(
        kani,
        kani::modifies(
            &self.state,
            &self.params,
            &self.intermediates,
            &self.osc_data,
            &self.current_param,
            &self.param_started,
            &self.dcs_active,
            &self.apc_active,
            &self.utf8_buffer,
            &self.utf8_len,
            &self.utf8_expected,
            &self.subparam_mask,
            &self.last_was_colon
        )
    )]
    #[cfg_attr(kani, kani::ensures(|_| self.type_invariant()))]
    #[inline]
    pub(crate) fn process_byte_inner<S: ActionSink>(&mut self, byte: u8, sink: &mut S) {
        self.process_byte_dispatch(byte, sink);
    }

    /// The state-machine dispatch body for a single byte, separated from
    /// [`Parser::process_byte_inner`] so the latter can carry a Kani contract.
    #[inline]
    pub(crate) fn process_byte_dispatch<S: ActionSink>(&mut self, byte: u8, sink: &mut S) {
        process_byte_impl!(self, byte, sink);
    }

    /// Process input with batch printing optimization.
    ///
    /// Like `advance_fast`, but passes entire printable slices to a
    /// specialized `print_str` method for even better performance.
    ///
    /// Test-only: production uses `advance_fast` (which monomorphizes the
    /// `process_byte_impl!` dispatch via `process_byte_inner`). This batch
    /// variant has no production callers, so gating it behind `#[cfg(test)]`
    /// removes a redundant second monomorphization of that ~150-line dispatch
    /// macro from the release binary. The batch parity tests (`src/tests/`)
    /// keep verifying it stays identical to the production path.
    #[cfg(test)]
    pub fn advance_batch<S: BatchActionSink>(&mut self, input: &[u8], sink: &mut S) {
        self.advance_simd_loop(
            input,
            sink,
            |sink, printable| {
                // `take_printable` (via `find_non_printable`) returns only bytes
                // in 0x20-0x7E — all valid single-byte UTF-8 codepoints.
                // Kani proof `printable_slice_is_valid_utf8` verifies this.
                // SAFETY: take_printable returns only bytes 0x20-0x7E, all valid
                // single-byte UTF-8. Kani proof `printable_slice_is_valid_utf8`
                // verifies this invariant (#7866).
                let s = unsafe { std::str::from_utf8_unchecked(printable) };
                sink.print_str(pty_wrap_ref(s));
            },
            Self::process_byte_batch,
            /* count_loops */ false,
            /* replay_escape_bracket_on_fail */ true,
            /* set_ground_after_escape_fast_path */ false,
        );
    }

    #[inline]
    fn process_ground_special_byte<S, ProcessByte>(
        &mut self,
        byte: u8,
        sink: &mut S,
        process_byte: &mut ProcessByte,
    ) where
        S: ActionSink,
        ProcessByte: FnMut(&mut Self, u8, &mut S),
    {
        if (0xC0..=0xF7).contains(&byte) {
            self.start_utf8(byte);
            return;
        }

        if (0x80..=0x9F).contains(&byte) {
            if self.c1_controls_enabled {
                process_byte(self, byte, sink);
            } else {
                sink.print(char::REPLACEMENT_CHARACTER);
            }
            return;
        }

        if (0xA0..=0xBF).contains(&byte) || byte >= 0xF8 {
            sink.print(char::REPLACEMENT_CHARACTER);
            return;
        }

        process_byte(self, byte, sink);
    }

    /// The three mode flags are plain `bool` arguments rather than const
    /// generics: Trust's VC generation does not support MIR switches on
    /// const-generic bool discriminants, and each call site's unique closure
    /// types already give this function a dedicated monomorphization in which
    /// LLVM constant-folds the literal flag arguments — so codegen is
    /// unchanged.
    #[allow(clippy::too_many_lines)]
    #[allow(
        clippy::too_many_arguments,
        reason = "the extra bool flag is a parameter added for the Trust gate (mode flags stay plain bool args, not const generics, which Trust's VC generation cannot switch on); it cannot be dropped without regressing lowering"
    )]
    fn advance_simd_loop<S, EmitPrintable, ProcessByte>(
        &mut self,
        input: &[u8],
        sink: &mut S,
        mut emit_printable: EmitPrintable,
        mut process_byte: ProcessByte,
        count_loops: bool,
        replay_escape_bracket_on_fail: bool,
        set_ground_after_escape_fast_path: bool,
    ) where
        S: ActionSink,
        EmitPrintable: FnMut(&mut S, &[u8]),
        ProcessByte: FnMut(&mut Self, u8, &mut S),
    {
        // Every byte/slice access below uses the total `split_first`/`get`
        // spellings (the loop condition and the fast-path scanners guarantee
        // the fallbacks are unreachable at runtime), so this driver carries no
        // panic obligations of its own.
        let mut remaining = input;

        while !remaining.is_empty() {
            if count_loops {
                // Test instrumentation: count loop iterations for O(n) verification
                #[cfg(test)]
                count_parser_loop_iteration();
            }

            if self.utf8_len > 0
                && self.state == State::Ground
                && let Some((&byte, tail)) = remaining.split_first()
            {
                remaining = tail;
                self.process_utf8_byte(byte, sink);
                continue;
            }

            if self.state == State::Ground {
                let (printable, rest) = super::simd::take_printable(remaining);
                if !printable.is_empty() {
                    emit_printable(sink, printable);
                }

                remaining = rest;
                let Some((&byte, tail)) = remaining.split_first() else {
                    break;
                };
                remaining = tail;

                if byte == 0x1B && remaining.first() == Some(&b'[') {
                    remaining = remaining.get(1..).unwrap_or(&[]);
                    if let Some(consumed) = self.try_parse_csi_fast(remaining, sink) {
                        remaining = remaining.get(consumed..).unwrap_or(&[]);
                        continue;
                    }
                    self.state = State::Escape;
                    self.clear();
                    process_byte(self, b'[', sink);
                    continue;
                }

                // C0 control fast path: LF/CR/BS/ESC are the most common
                // non-printable bytes. Route them directly to the state machine
                // without the 3 redundant range checks in process_ground_special_byte.
                if byte < 0x20 {
                    process_byte(self, byte, sink);
                    continue;
                }

                // UTF-8 fast path: decode multi-byte sequences when all
                // continuation bytes are available, batching consecutive
                // non-ASCII characters for amortized dispatch overhead.
                // The decode logic is in a separate function to keep the
                // hot ASCII dispatch loop compact for L1i cache.
                if (0xC0..=0xF7).contains(&byte) {
                    let consumed = self.decode_multibyte_run(byte, remaining, sink);
                    remaining = remaining.get(consumed..).unwrap_or(&[]);
                    continue;
                }

                self.process_ground_special_byte(byte, sink, &mut process_byte);
                continue;
            }

            if self.state == State::Escape && remaining.first() == Some(&b'[') {
                let rest = remaining.get(1..).unwrap_or(&[]);
                if let Some(consumed) = self.try_parse_csi_fast(rest, sink) {
                    remaining = rest.get(consumed..).unwrap_or(&[]);
                    if set_ground_after_escape_fast_path {
                        self.state = State::Ground;
                    }
                    continue;
                }

                if replay_escape_bracket_on_fail {
                    remaining = rest;
                    self.state = State::Escape;
                    self.clear();
                    process_byte(self, b'[', sink);
                    continue;
                }
            }

            // Bulk fast paths for the three string states (OSC/DCS/APC);
            // extracted into a helper so this driver's MIR stays within the
            // verifier's VC-generation budget. On return the caller falls
            // through to `process_byte` for the boundary byte, or breaks when
            // the input is exhausted (matching the previous inline blocks).
            if matches!(
                self.state,
                State::OscString | State::DcsPassthrough | State::SosPmApcString
            ) {
                remaining = self.advance_string_states_bulk(remaining, sink);
                if remaining.is_empty() {
                    break;
                }
            }

            let Some((&byte, tail)) = remaining.split_first() else {
                break;
            };
            remaining = tail;
            process_byte(self, byte, sink);
        }
    }

    /// Bulk fast paths for the three string states, extracted from
    /// `advance_simd_loop` (see the call site). Returns the input with the
    /// bulk-consumed prefix removed; the boundary byte (if any) is left for
    /// the caller's byte-by-byte dispatch.
    fn advance_string_states_bulk<'a, S: ActionSink>(
        &mut self,
        mut remaining: &'a [u8],
        sink: &mut S,
    ) -> &'a [u8] {
        // OSC bulk fast path (#7864): bytes 0x20-0xFF are all OscPut
        // in the OscString state. Scan for the first C0 control byte
        // and bulk-append everything before it.
        //
        // VERIFIED EQUIVALENT to the byte-by-byte slow path (do not "fix"):
        // `n` is the position of the FIRST control byte, so every byte in
        // [0..n) is a data byte that the slow path maps to OscPut. OscPut
        // pushes only while `osc_data.len() < MAX_OSC_DATA`, so the slow
        // path keeps exactly the first `MAX_OSC_DATA - len` of them and
        // drops the rest. `copy_len = n.min(capacity_left)` copies that
        // same prefix; bytes in [copy_len..n) are dropped identically.
        // `remaining.get(n..)` then lands on the control byte,
        // which both paths process the same way. The regression test
        // `osc_over_capacity_truncation_parity` (tests/batch.rs) asserts
        // this across the MAX_OSC_DATA boundary.
        if self.state == State::OscString {
            let n = if self.c1_controls_enabled {
                remaining
                    .iter()
                    .position(|&b| b < 0x20 || b == 0x9C)
                    .unwrap_or(remaining.len())
            } else {
                super::simd::find_c0_control(remaining).unwrap_or(remaining.len())
            };
            if n > 0 {
                let capacity_left = MAX_OSC_DATA.saturating_sub(self.osc_data.len());
                let copy_len = n.min(capacity_left);
                self.osc_data
                    .extend_from_slice(remaining.get(..copy_len).unwrap_or(&[]));
                remaining = remaining.get(n..).unwrap_or(&[]);
            }
            // Fall through to process_byte for the C0 control byte.
        }

        // DCS bulk fast path (#7864): in DcsPassthrough most bytes are DcsPut,
        // but the boundary set is NOT the same as DCS terminators. Beyond the
        // four state-exit bytes 0x18 (CAN), 0x1A (SUB), 0x1B (ESC), 0x9C (ST),
        // the transition table maps 0x7F (DEL) to ActionType::Ignore (it stays
        // in DcsPassthrough and emits NOTHING — unlike APC, where 0x7F is
        // ApcPut data). Reusing `find_dcs_terminator` here would bulk-copy 0x7F
        // into `dcs_put_bulk`, diverging from the byte-by-byte path which drops
        // it. So scan with `find_dcs_passthrough_boundary` (terminators + 0x7F),
        // bulk-copy up to the boundary, then fall through to process_byte for
        // the boundary byte: a 0x7F is Ignored (no dcs_put, stays in
        // DcsPassthrough) and the loop re-enters this branch for the remainder.
        if self.state == State::DcsPassthrough && self.dcs_active {
            let n = Self::find_dcs_passthrough_boundary(remaining);
            if n > 0 {
                sink.dcs_put_bulk(pty_wrap_ref(remaining.get(..n).unwrap_or(&[])));
                remaining = remaining.get(n..).unwrap_or(&[]);
            }
            // Fall through to process_byte for the boundary byte.
        }

        // APC bulk fast path (#7864 parity sibling): SosPmApcString uses the
        // SAME terminator set as DCS — 0x18 (CAN), 0x1A (SUB), 0x1B (ESC),
        // 0x9C (ST) — and the transition table maps every other byte to
        // ApcPut, so `find_dcs_terminator` is reusable verbatim. There is no
        // runtime C1 override for APC (unlike OSC's 0x9C special-case), so
        // the static terminator set is correct in both c1 modes.
        //
        // Unlike the DCS branch (which gates the WHOLE branch on dcs_active),
        // the scan/advance here runs UNCONDITIONALLY and only the
        // `apc_put_bulk` CALL is gated on `apc_active`: SosPmApcString can be
        // entered without apc_active via SOS (0x98) / PM (0x9E), and those
        // payloads must still be CONSUMED to reach the terminator — they are
        // merely dropped, mirroring the `if self.apc_active` guard on ApcPut
        // in process_byte_impl!.
        if self.state == State::SosPmApcString {
            let n = Self::find_dcs_terminator(remaining);
            if n > 0 {
                if self.apc_active {
                    sink.apc_put_bulk(pty_wrap_ref(remaining.get(..n).unwrap_or(&[])));
                }
                remaining = remaining.get(n..).unwrap_or(&[]);
            }
            // Fall through to process_byte for the terminator.
        }

        remaining
    }

    /// Process a single byte for BatchActionSink.
    ///
    /// Test-only: the only caller is `advance_batch` (also `#[cfg(test)]`).
    /// This is the second monomorphization of `process_byte_impl!`; gating it
    /// out keeps release builds to the single `process_byte_inner` instance.
    #[cfg(test)]
    fn process_byte_batch<S: BatchActionSink>(&mut self, byte: u8, sink: &mut S) {
        process_byte_impl!(self, byte, sink);
    }

    /// Find the first byte in `input` that must NOT be bulk-forwarded as DCS
    /// data — the scan boundary for the DCS fast path.
    ///
    /// True DCS terminators: 0x18 (CAN), 0x1A (SUB), 0x1B (ESC), 0x9C (ST).
    /// Unlike OSC, DCS treats 0x9C as ST even when C1 controls are otherwise
    /// disabled: the transition table leaves 0x9C as a DCS terminator, so the
    /// fast path must scan for it too.
    ///
    /// Unlike OSC, DCS treats 0x9C as ST even when C1 controls are
    /// otherwise disabled: the transition table leaves 0x9C as a DCS
    /// terminator, so the fast path must scan for it too.
    ///
    /// The scan is SIMD-accelerated via [`super::simd::find_any_of`]
    /// (OR-reduced equality compares); its scalar fallback is the same
    /// set-membership predicate, so behavior is identical to the bytewise path.
    #[inline]
    fn find_dcs_terminator(input: &[u8]) -> usize {
        super::simd::find_any_of(input, [0x18, 0x1A, 0x1B, 0x9C]).unwrap_or(input.len())
    }

    /// Find the first byte that the DCS-passthrough bulk fast path must NOT
    /// bulk-copy into `dcs_put_bulk`.
    ///
    /// This is the DCS-terminator set (0x18 CAN, 0x1A SUB, 0x1B ESC, 0x9C ST)
    /// PLUS 0x7F (DEL). In `DcsPassthrough` the transition table maps DEL to
    /// `ActionType::Ignore` (it stays in the state and emits nothing), so the
    /// bulk copy must stop at it and let the byte-by-byte path drop it —
    /// otherwise the fast path would leak DEL into the DCS sink, diverging from
    /// `advance`. This is distinct from APC (`SosPmApcString`), where 0x7F is
    /// `ApcPut` data and must keep flowing, so APC continues to use
    /// [`Parser::find_dcs_terminator`].
    ///
    /// The scan is SIMD-accelerated via [`super::simd::find_any_of`]
    /// (OR-reduced equality compares); its scalar fallback is the same
    /// set-membership predicate, so behavior is identical to the bytewise path.
    #[inline]
    fn find_dcs_passthrough_boundary(input: &[u8]) -> usize {
        super::simd::find_any_of(input, [0x18, 0x1A, 0x1B, 0x9C, 0x7F]).unwrap_or(input.len())
    }

    /// Clear parameters and intermediates (on entry to escape sequences).
    #[inline]
    fn clear(&mut self) {
        self.params.clear();
        self.intermediates.clear();
        self.current_param = 0;
        self.param_started = false;
        self.subparam_mask = 0;
        self.last_was_colon = false;
    }

    /// Add a digit to the current parameter, or handle separator (`;` or `:`).
    #[inline]
    pub(crate) fn add_param_digit(&mut self, byte: u8) {
        if byte.is_ascii_digit() {
            // `saturating_sub` is exact here: `byte >= b'0'` on this branch.
            self.current_param = self
                .current_param
                .saturating_mul(10)
                .saturating_add(u32::from(byte.saturating_sub(b'0')));
            self.param_started = true;
        } else if byte == b';' {
            // Semicolon: finalize current param and start new one
            self.finalize_param();
            self.last_was_colon = false;
        } else if byte == b':' {
            // Colon: finalize current param, mark next as subparameter
            self.finalize_param();
            self.last_was_colon = true;
        }
    }

    /// Finalize the current parameter.
    ///
    /// Delegates to `push_current_param` using `last_was_colon` as the
    /// subparam flag (byte-by-byte path tracks colon state in a field,
    /// while the CSI fast-path passes it explicitly).
    #[inline]
    pub(crate) fn finalize_param(&mut self) {
        self.push_current_param(self.last_was_colon);
    }

    /// Collect an intermediate byte.
    ///
    /// (Named `collect_intermediate` rather than `collect` so Trust's
    /// unbounded-allocation recognizer does not misclassify this parser
    /// method as `Iterator::collect`.)
    #[inline]
    fn collect_intermediate(&mut self, byte: u8) {
        if self.intermediates.len() < MAX_INTERMEDIATES {
            self.intermediates.push(byte);
        }
    }

    /// Process a single byte through the state machine (basic method).
    ///
    /// Note: This is the simple byte-by-byte method. For better UTF-8 support,
    /// use `advance_fast` instead which properly handles multi-byte sequences.
    #[inline]
    fn process_byte<S: ActionSink>(&mut self, byte: u8, sink: &mut S) {
        if byte >= 0x80 && self.state == State::Ground {
            // C1 control codes (0x80-0x9F) security check
            // When c1_controls_enabled is false (default), treat C1 bytes as invalid
            // UTF-8 and emit replacement character instead of processing as controls.
            // This prevents escape sequence injection attacks in UTF-8 terminals.
            if (0x80..=0x9F).contains(&byte) {
                if self.c1_controls_enabled {
                    self.process_byte_inner(byte, sink);
                } else {
                    sink.print(char::REPLACEMENT_CHARACTER);
                }
                return;
            }
            // Latin-1 range (0xA0-0xFF): These bytes are valid printable Latin-1
            // characters. The transition table has no entries for them in Ground state
            // (they'd be silently dropped). Print them as their Unicode equivalents
            // (Latin-1 maps 1:1 to Unicode codepoints U+00A0-U+00FF).
            // SAFETY: 0xA0-0xFF are valid Unicode scalar values.
            sink.print(byte as char);
            return;
        }
        self.process_byte_inner(byte, sink);
    }

    /// Parse and dispatch OSC data.
    ///
    /// `bel_terminated` indicates whether the OSC was terminated by BEL (0x07)
    /// vs ST (ESC \\ or C1 0x9C). Passed through to
    /// [`ActionSink::osc_dispatch_with_terminator`] so response-generating
    /// handlers (e.g., OSC 52 clipboard query) can echo the same terminator.
    ///
    /// Performance: fast path for common 2-param OSC sequences (title set,
    /// CWD, shell integration marks) avoids the ArrayVec construction and
    /// full-buffer `;` scan. The format is `<cmd>;<payload>` with no further
    /// semicolons — covers OSC 0/1/2/7/9 which are the highest-frequency
    /// sequences in typical shell output. All other sequences fall through
    /// to the general ArrayVec split path. (#7355)
    fn dispatch_osc<S: ActionSink>(&mut self, sink: &mut S, bel_terminated: bool) {
        // The OSC payload parsing is a PURE function of the byte slice — it only
        // READS `osc_data` and writes the sink; it touches no parser state. It is
        // factored into `parse_and_dispatch_osc` (a `&[u8]` associated fn, NOT
        // `&mut self`) so the contract verifier can treat it as having no effect
        // on the `TypeInvariant` state: the only self-mutation here is the
        // invariant-preserving `osc_data.clear()` (len -> 0). (#osc-pure-parse)
        Self::parse_and_dispatch_osc(self.osc_data.as_slice(), sink, bel_terminated);
        self.osc_data.clear();
        // Shrink the buffer if a large OSC payload inflated it beyond 4 KiB.
        // Without this, a single OSC 1337 image permanently holds up to
        // MAX_OSC_DATA (8 MiB) per parser instance for the session lifetime
        // (#7272). This makes the large cap a transient spike, not bloat.
        if self.osc_data.capacity() > 4096 {
            self.osc_data.shrink_to(128);
        }
    }

    /// Parse and dispatch an OSC payload. PURE in the parser state: reads `data`,
    /// writes `sink`, mutates nothing on `self` (takes `&[u8]`, not `&mut self`).
    ///
    /// Fast path for common 2-param OSC sequences (title set, CWD, shell
    /// integration marks) avoids the `ArrayVec` construction and full-buffer `;`
    /// scan. The format is `<cmd>;<payload>` with no further semicolons — covers
    /// OSC 0/1/2/7/9, the highest-frequency sequences in typical shell output.
    /// All other sequences fall through to the general `ArrayVec` split. (#7355)
    fn parse_and_dispatch_osc<S: ActionSink>(data: &[u8], sink: &mut S, bel_terminated: bool) {
        // Fast path: a NUMERIC command + a single ';' + payload with no further
        // semicolons (the overwhelmingly common shape). Now covers MULTI-digit
        // commands too — OSC 10/11/12 (colours), OSC 104, and especially OSC 133
        // (shell-integration marks, emitted per prompt+command) — not just the
        // single-digit OSC 0/1/2/7/9 the old `data[1] == ';'` gate caught. Byte-for-byte
        // equal to the `data.split(';')` general path whenever exactly one ';' is
        // present (it yields exactly the two segments `[cmd, payload]`); everything
        // else — NO ';' (a single-param command like a bare OSC 104), a non-numeric
        // command, or TWO+ ';' (OSC 8 hyperlinks `8;;url`, OSC 52-with-mode, OSC 4
        // colour lists) — falls through so param segmentation stays identical. Avoids
        // the 256-byte ArrayVec zero-init + full-buffer split scan.
        if let Some(k) = data.iter().position(|&b| b == b';')
            && k >= 1
            && data[..k].iter().all(u8::is_ascii_digit)
            && !data[k + 1..].contains(&b';')
        {
            let params: [&[u8]; 2] = [&data[..k], &data[k + 1..]];
            sink.osc_dispatch_with_terminator(pty_wrap_ref(&params[..]), bel_terminated);
        } else {
            Self::parse_osc_general(data, sink, bel_terminated);
        }
    }

    /// General OSC dispatch using ArrayVec split. Called for OSC sequences
    /// that don't match the 2-param fast path (multi-digit commands, multiple
    /// semicolons, etc.). PURE in the parser state (`&[u8]`, not `&mut self`).
    #[inline(never)]
    fn parse_osc_general<S: ActionSink>(data: &[u8], sink: &mut S, bel_terminated: bool) {
        let mut params: ArrayVec<&[u8], MAX_OSC_PARAMS> = ArrayVec::new();
        for segment in data.split(|&b| b == b';') {
            if params.is_full() {
                break;
            }
            params.push(segment);
        }
        sink.osc_dispatch_with_terminator(pty_wrap_ref(params.as_slice()), bel_terminated);
    }

    /// Decode and dispatch a run of multi-byte UTF-8 characters.
    ///
    /// Called when a UTF-8 lead byte (0xC0..=0xF7) is encountered in the
    /// ground-state fast path. Decodes consecutive multi-byte characters
    /// into a buffer and dispatches via `print`/`print_unicode_bulk`.
    ///
    /// Separated from the main dispatch loop (`#[inline(never)]`) to keep
    /// the hot ASCII path compact for L1 instruction cache. The function
    /// call overhead (~2 cycles) is negligible compared to the per-character
    /// decode cost, and UTF-8 multi-byte workloads keep this function warm
    /// in L1i anyway.
    ///
    /// Returns the number of bytes consumed from `remaining` (the slice
    /// after the lead byte).
    #[inline(never)]
    fn decode_multibyte_run<S: ActionSink>(
        &mut self,
        first_lead: u8,
        remaining: &[u8],
        sink: &mut S,
    ) -> usize {
        // `ArrayVec` scratch: a multibyte run only touches the slots it writes,
        // so this keeps the "no ~1 KiB zero-init per CJK/emoji run" property the
        // previous `MaybeUninit` buffer had (a typical ~28-char run touches ~112
        // bytes) while staying entirely in safe, Trust-verifiable code. The
        // decode branches use slice patterns and guarded `char::from_u32` (the
        // guards make the `None` arm unreachable), so the loop carries no panic
        // obligations.
        let mut chars: ArrayVec<char, 256> = ArrayVec::new();
        let orig_len = remaining.len();
        let mut rem = remaining;

        // First character: the lead byte is passed separately (not part of
        // `remaining`), so it takes the scalar leaf. On failure this is the
        // incomplete/invalid-lead case — buffer it for the byte-by-byte straddle
        // path and stop, consuming nothing of `remaining` (the lead is retained).
        match Self::decode_multibyte_char(first_lead, rem) {
            Some((c, consumed)) => {
                // Cannot overflow: `chars` is empty here.
                chars.push(c);
                // Total spelling of `&rem[consumed..]`: the decode branch
                // guarantees `consumed <= rem.len()`.
                rem = rem.get(consumed..).unwrap_or(&[]);
            }
            None => {
                self.start_utf8(first_lead);
                // Dispatch nothing decoded, consume nothing beyond the lead.
                return 0;
            }
        }

        // From here `rem` starts on a byte boundary, so the THRU-4a bulk lane can
        // scan contiguous homogeneous runs. It appends the same chars the scalar
        // loop would and stops at every boundary (class change / invalid /
        // incomplete / full); the scalar leaf below then handles that one byte,
        // and we re-enter the bulk lane. This is the only structural change from
        // the pure per-character loop — semantics are byte-for-byte identical.
        loop {
            let taken = crate::utf8_simd::bulk_decode_run(rem, &mut chars);
            rem = rem.get(taken..).unwrap_or(&[]);
            if chars.is_full() {
                break;
            }
            match rem.first() {
                // A multibyte lead the bulk lane could not take (a different
                // class than a run it just finished, or a malformed/incomplete
                // sequence): decode exactly one via the scalar oracle.
                Some(&lead) if (0xC0..=0xF7).contains(&lead) => {
                    match Self::decode_multibyte_char(lead, rem.get(1..).unwrap_or(&[])) {
                        Some((c, consumed)) => {
                            // Not full (checked above), so this cannot overflow.
                            chars.push(c);
                            // `1 + consumed` bytes: the lead plus its continuations.
                            rem = rem.get(1 + consumed..).unwrap_or(&[]);
                        }
                        None => {
                            // Invalid/overlong/surrogate/incomplete — byte-by-byte
                            // fallback. Advance PAST the lead first: `start_utf8`
                            // buffers it into the straddle machine, so the consumed
                            // count must INCLUDE it or the caller re-feeds the same
                            // lead (a spurious U+FFFD). This matches the pre-THRU-4a
                            // scalar loop, which advanced `rem` before `start_utf8`.
                            rem = rem.get(1..).unwrap_or(&[]);
                            self.start_utf8(lead);
                            break;
                        }
                    }
                }
                _ => break,
            }
        }

        // Dispatch decoded characters.
        match chars.as_slice() {
            [] => {}
            &[c] => sink.print(c),
            run => sink.print_unicode_bulk(pty_wrap_ref(run)),
        }

        // Exact: `rem` is always a suffix of `remaining`, so
        // `rem.len() <= orig_len` and the saturation never engages.
        orig_len.saturating_sub(rem.len())
    }

    /// Decode one multi-byte UTF-8 character from `lead` plus the prefix of
    /// `rem`, returning the char and the number of bytes consumed from `rem`.
    ///
    /// Pure leaf helper (mirrors `decode_utf8_validated`): keeping the shift
    /// arithmetic and continuation-byte checks in a function with no callees
    /// beyond `char::from_u32` lets Trust lower it natively and prove every
    /// obligation, while the caller's loop stays obligation-free.
    ///
    /// `pub(crate)` so the THRU-4a bulk lane's Kani equivalence proof
    /// (`proofs.rs::bulk_decode_first_sequence_matches_scalar`) can check the
    /// vectorized path against this exact scalar oracle.
    #[inline]
    pub(crate) fn decode_multibyte_char(lead: u8, rem: &[u8]) -> Option<(char, usize)> {
        // Each branch produces Option<(char, bytes_consumed_from_rem)>.
        if lead >= 0xF0 {
            // 4-byte: SMP characters (emoji, math symbols)
            if let &[c0, c1, c2, ..] = rem
                && (c0 & 0xC0) == 0x80
                && (c1 & 0xC0) == 0x80
                && (c2 & 0xC0) == 0x80
            {
                let cp = (u32::from(lead & 0x07) << 18)
                    | (u32::from(c0 & 0x3F) << 12)
                    | (u32::from(c1 & 0x3F) << 6)
                    | u32::from(c2 & 0x3F);
                // cp must be in 0x10000..=0x10FFFF to be a valid Unicode
                // scalar value. The lower bound excludes surrogates and
                // overlongs. The upper bound rejects codepoints above the
                // Unicode maximum — e.g., 0xF4 0x90 0x80 0x80 decodes to
                // U+110000 which is not a valid char. (#7159)
                if (0x10000..=0x0010_FFFF).contains(&cp) {
                    // The range guard above makes `from_u32` always `Some`
                    // here (no surrogates, no overlongs, within Unicode
                    // range); `None` falls through to the byte-by-byte
                    // fallback, unreachable at runtime.
                    char::from_u32(cp).map(|c| (c, 3))
                } else {
                    None // Overlong encoding
                }
            } else {
                None
            }
        } else if lead >= 0xE0 {
            // 3-byte: BMP non-ASCII (CJK, Hangul, Greek, Cyrillic, etc.)
            if let &[c0, c1, ..] = rem
                && (c0 & 0xC0) == 0x80
                && (c1 & 0xC0) == 0x80
            {
                let cp = (u32::from(lead & 0x0F) << 12)
                    | (u32::from(c0 & 0x3F) << 6)
                    | u32::from(c1 & 0x3F);
                if cp >= 0x800 && !(0xD800..=0xDFFF).contains(&cp) {
                    // Guards: >= 0x800 (not overlong), not a surrogate,
                    // <= 0xFFFF (lead < 0xF0) — `from_u32` is always `Some`.
                    char::from_u32(cp).map(|c| (c, 2))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            // 2-byte: Latin extensions, IPA, etc. (U+0080-U+07FF)
            if let &[c0, ..] = rem
                && (c0 & 0xC0) == 0x80
            {
                let cp = (u32::from(lead & 0x1F) << 6) | u32::from(c0 & 0x3F);
                if cp >= 0x80 {
                    // 0x80..=0x7FF are all valid Unicode scalar values.
                    char::from_u32(cp).map(|c| (c, 1))
                } else {
                    None
                }
            } else {
                None
            }
        }
    }
}
