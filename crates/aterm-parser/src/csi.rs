// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! CSI (Control Sequence Introducer) fast-path parsing.

use aterm_provenance::pty_wrap_ref;

use crate::action::ActionSink;
use crate::state::State;
use crate::{MAX_INTERMEDIATES, MAX_PARAMS, Parser};

impl Parser {
    /// Finalize the current parameter and push it to the params list.
    ///
    /// Called on semicolons, colons, and at end-of-params in CSI fast-path parsing.
    /// Sets `subparam_mask` bit if `is_subparam` is true.
    #[inline]
    pub(crate) fn push_current_param(&mut self, is_subparam: bool) {
        let param_index = self.params.len();
        if param_index < MAX_PARAMS {
            let clamped = self.current_param.min(u32::from(u16::MAX));
            let value = u16::try_from(clamped).unwrap_or(u16::MAX);
            self.params.push(value);
            if is_subparam {
                // param_index < MAX_PARAMS (24) here, so the u32 shift is in range.
                self.subparam_mask |= 1 << param_index;
            }
        }
        self.current_param = 0;
        self.param_started = false;
    }

    /// Try to parse a CSI sequence using the fast path.
    ///
    /// Returns the number of bytes consumed if successful, None if we should
    /// fall back to normal byte-by-byte parsing.
    ///
    /// The fast path handles simple CSI sequences of the form:
    /// - CSI \[private\] params \[intermediate\] final
    /// - Params are digits and semicolons
    /// - Final byte is 0x40-0x7E
    #[inline]
    pub(crate) fn try_parse_csi_fast<S: ActionSink>(
        &mut self,
        input: &[u8],
        sink: &mut S,
    ) -> Option<usize> {
        // Ultra-fast path: zero-param CSI (e.g., ESC[A, ESC[H).
        // The first byte after ESC[ is already a final byte — skip all parsing.
        // Covers CUU/CUD/CUF/CUB/CUP(home)/ED/EL/SGR(reset) with no params.
        if let Some(&first) = input.first()
            && (0x40..=0x7E).contains(&first)
        {
            self.params.clear();
            self.intermediates.clear();
            self.subparam_mask = 0;
            sink.csi_dispatch(
                pty_wrap_ref(self.params.as_slice()),
                pty_wrap_ref(self.intermediates.as_slice()),
                first,
            );
            self.state = State::Ground;
            return Some(1);
        }

        // Fast path: 1-2 digit param CSI (e.g., ESC[5A, ESC[0m, ESC[32m).
        // Covers ANSI colors (30-37,39,40-47,49), attribute resets (21-29),
        // and common single-digit cursor ops. Skips position scan + loop.
        // Slice patterns (instead of length checks + indexing) and saturating
        // arithmetic keep this hot path free of panic obligations; the
        // saturations are exact on these branches (`b0`/`b1` are digits, and
        // the two-digit value is at most 99).
        if let &[b0, b1, ..] = input
            && b0.is_ascii_digit()
        {
            let d0 = u16::from(b0.saturating_sub(b'0'));
            if (0x40..=0x7E).contains(&b1) {
                // Single digit: ESC[Nm
                self.params.set_single(d0);
                self.intermediates.clear();
                self.subparam_mask = 0;
                sink.csi_dispatch(
                    pty_wrap_ref(self.params.as_slice()),
                    pty_wrap_ref(self.intermediates.as_slice()),
                    b1,
                );
                self.state = State::Ground;
                return Some(2);
            }
            if b1 == b';' {
                // D;... — 1-digit first param, multi-param sequence.
                // Common: ESC[1;31m (bold+red), ESC[5;20r (scroll region).
                return self.parse_csi_after_first_param(input, sink, d0, 2);
            }
            if let &[_, _, b2, ..] = input
                && b1.is_ascii_digit()
            {
                let p1 = d0
                    .saturating_mul(10)
                    .saturating_add(u16::from(b1.saturating_sub(b'0')));
                if (0x40..=0x7E).contains(&b2) {
                    // Two digits: ESC[NNx
                    self.params.set_single(p1);
                    self.intermediates.clear();
                    self.subparam_mask = 0;
                    sink.csi_dispatch(
                        pty_wrap_ref(self.params.as_slice()),
                        pty_wrap_ref(self.intermediates.as_slice()),
                        b2,
                    );
                    self.state = State::Ground;
                    return Some(3);
                }
                if b2 == b';' {
                    // DD;... — 2-digit first param, multi-param sequence.
                    // Common: ESC[38;5;Nm (256-color), ESC[12;40H (CUP).
                    return self.parse_csi_after_first_param(input, sink, p1, 3);
                }
            }
        }

        self.parse_csi_general(input, sink)
    }

    /// Dispatch a CSI sequence, choosing subparam vs. normal dispatch.
    #[inline]
    fn csi_dispatch_final<S: ActionSink>(&self, sink: &mut S, final_byte: u8) {
        if self.subparam_mask != 0 {
            sink.csi_dispatch_with_subparams(
                pty_wrap_ref(self.params.as_slice()),
                pty_wrap_ref(self.intermediates.as_slice()),
                final_byte,
                self.subparam_mask,
            );
        } else {
            sink.csi_dispatch(
                pty_wrap_ref(self.params.as_slice()),
                pty_wrap_ref(self.intermediates.as_slice()),
                final_byte,
            );
        }
    }

    /// Multi-param CSI fast path: first param already parsed, continue from `pos`.
    ///
    /// Called when `try_parse_csi_fast` detects `D;` or `DD;` at the start of a
    /// CSI sequence. Avoids `parse_csi_general` overhead (clear, private-marker
    /// check, re-parsing first param digits) for common multi-param patterns:
    /// `38;5;Nm` (256-color), `R;CH` (CUP), `1;31m` (bold+red SGR).
    ///
    /// Separated from the fast path (`#[inline(never)]`) to keep the L1i-hot
    /// zero/single/two-digit dispatch compact.
    #[inline(never)]
    fn parse_csi_after_first_param<S: ActionSink>(
        &mut self,
        input: &[u8],
        sink: &mut S,
        first_param: u16,
        mut pos: usize,
    ) -> Option<usize> {
        self.params.clear();
        self.params.push(first_param);
        self.intermediates.clear();
        self.subparam_mask = 0;
        self.current_param = 0;
        self.param_started = false;

        // Clamp the scan window up front (proof-friendly spelling of
        // `input.len().min(65)`); `get`-based reads and saturating position
        // increments keep the loop free of panic obligations. All the
        // saturations are exact at runtime: `pos < scan.len() <= 65` whenever
        // they execute.
        let limit = if input.len() < 65 { input.len() } else { 65 };
        let scan = input.get(..limit).unwrap_or(input);

        // Parse remaining params (no private marker — first byte was a digit)
        while let Some(&b) = scan.get(pos) {
            if b.is_ascii_digit() {
                // `saturating_sub` is exact here: `b >= b'0'` on this branch.
                self.current_param = self
                    .current_param
                    .saturating_mul(10)
                    .saturating_add(u32::from(b.saturating_sub(b'0')));
                self.param_started = true;
                pos = pos.saturating_add(1);
            } else if b == b';' {
                self.push_current_param(false);
                pos = pos.saturating_add(1);
            } else if b == b':' {
                self.push_current_param(false);
                pos = self.parse_csi_colon_group(scan, pos);
                // Colon-subparam sequences are rare; fall through to general
                // for any remaining complexity.
                // If the next byte is `;`, consume it here so parse_csi_general_from
                // doesn't push a phantom zero param. The subparam value was already
                // pushed above, so the `;` is just a group separator (#7648).
                if scan.get(pos) == Some(&b';') {
                    pos = pos.saturating_add(1);
                }
                return self.parse_csi_general_from(input, sink, pos);
            } else if (0x40..=0x7E).contains(&b) {
                if self.param_started {
                    self.push_current_param(false);
                }
                self.csi_dispatch_final(sink, b);
                self.state = State::Ground;
                return Some(pos.saturating_add(1));
            } else if (0x20..=0x2F).contains(&b) {
                if self.param_started {
                    self.push_current_param(false);
                }
                return self.parse_csi_intermediates(input, sink, pos, limit);
            } else {
                return None;
            }
        }

        None
    }

    /// Parse a full colon-separated subparam group starting at the `:` at
    /// `scan[pos]`, returning the position just past the group.
    ///
    /// Each `:` introduces a subparam value that must be flagged in
    /// `subparam_mask`. We loop here instead of handling only one value, so
    /// `58:5:196` in the middle of a mixed sequence like `1;58:5:196m` is
    /// fully consumed without falling through to `parse_csi_general_from`
    /// mid-group. (Extracted from `parse_csi_after_first_param` so each loop
    /// verifies as its own small unit.)
    fn parse_csi_colon_group(&mut self, scan: &[u8], mut pos: usize) -> usize {
        loop {
            let param_index = self.params.len();
            self.current_param = 0;
            self.param_started = false;
            // Consume the ':'. `pos < scan.len() <= 65` at every entry, so
            // saturating is exact; it just makes the increment provably total.
            pos = pos.saturating_add(1);
            // Parse the subparam value digits
            while let Some(&d) = scan.get(pos) {
                if !d.is_ascii_digit() {
                    break;
                }
                self.current_param = self
                    .current_param
                    .saturating_mul(10)
                    .saturating_add(u32::from(d.saturating_sub(b'0')));
                self.param_started = true;
                pos = pos.saturating_add(1);
            }
            // Push with subparam flag
            if self.param_started {
                self.push_current_param(true);
            } else if param_index < MAX_PARAMS && matches!(scan.get(pos), Some(&(b':' | b';'))) {
                // Only materialize the empty subparameter when another
                // separator actually follows. The reference state machine
                // finalizes a pending colon value only when a subsequent
                // ';'/':' arrives; a trailing ':' before a final/intermediate
                // byte is dropped, so we must not push a phantom empty here.
                self.params.push(0);
                // param_index < MAX_PARAMS (24) here, so the u32 shift is in
                // range (matches push_current_param); no `< 16` clamp needed.
                self.subparam_mask |= 1 << param_index;
            }
            // If the next byte is another ':', continue the colon group.
            if scan.get(pos) == Some(&b':') {
                continue;
            }
            break;
        }
        pos
    }

    /// Continue general CSI parsing from a given position.
    ///
    /// Used when `parse_csi_after_first_param` encounters a colon subparam
    /// and needs to fall back to the general-purpose loop. Params already
    /// parsed up to `pos` are preserved.
    #[inline(never)]
    fn parse_csi_general_from<S: ActionSink>(
        &mut self,
        input: &[u8],
        sink: &mut S,
        mut pos: usize,
    ) -> Option<usize> {
        // Same proof-friendly loop shape as `parse_csi_after_first_param`:
        // clamped scan window, `get`-based reads, and saturating position
        // increments (all exact at runtime, where `pos < scan.len() <= 65`).
        let limit = if input.len() < 65 { input.len() } else { 65 };
        let scan = input.get(..limit).unwrap_or(input);
        let mut next_is_subparam = false;

        while let Some(&b) = scan.get(pos) {
            if b.is_ascii_digit() {
                // `saturating_sub` is exact here: `b >= b'0'` on this branch.
                self.current_param = self
                    .current_param
                    .saturating_mul(10)
                    .saturating_add(u32::from(b.saturating_sub(b'0')));
                self.param_started = true;
                pos = pos.saturating_add(1);
            } else if b == b';' {
                self.push_current_param(next_is_subparam);
                next_is_subparam = false;
                pos = pos.saturating_add(1);
            } else if b == b':' {
                self.push_current_param(next_is_subparam);
                next_is_subparam = true;
                pos = pos.saturating_add(1);
            } else if (0x40..=0x7E).contains(&b) {
                if self.param_started {
                    self.push_current_param(next_is_subparam);
                }
                self.csi_dispatch_final(sink, b);
                self.state = State::Ground;
                return Some(pos.saturating_add(1));
            } else if (0x20..=0x2F).contains(&b) {
                if self.param_started {
                    self.push_current_param(next_is_subparam);
                }
                return self.parse_csi_intermediates(input, sink, pos, limit);
            } else {
                return None;
            }
        }

        None
    }

    /// General-path CSI parser: single-pass scan that parses params and finds
    /// the final byte simultaneously. Handles private markers, subparams,
    /// and intermediate bytes.
    ///
    /// This used to speculatively run `simd_csi::simd_parse_csi_params` first
    /// and fall back to the byte loop below when it reported subparams. That
    /// pre-parse was removed (2026-07): it never won and lost on everything
    /// that actually reaches here. `try_parse_csi_fast` already routes `D;…`
    /// and `DD;…` — i.e. `38;2;255;128;0m` and every ordinary multi-param SGR,
    /// the shapes the pre-parse was written for — to
    /// `parse_csi_after_first_param`, and a private marker sets `pos = 1`,
    /// which disabled the pre-parse anyway. What was left for it was 3+-digit
    /// first params, a leading `;`, and colon-subparam shapes: on a colon
    /// sequence the entire 72-byte `CsiParamResult` was thrown away and these
    /// same bytes re-parsed from zero, and otherwise the params were copied a
    /// second time out of it — on top of a 48-byte zero-init and an sret
    /// return from a non-inlined call. Measured in-tree on `advance_fast` +
    /// `NullSink` over 8 MiB corpora (release, best-of-5), with vs. without
    /// the pre-parse: `ESC[4:3m` / `ESC[58:2::255:0:0m` + text 1,580 -> 2,764
    /// MB/s (+75%), `ESC[123;45H` + text 3,748 -> 4,886 (+30%), and a 13-param
    /// leading-`;` SGR — the best case FOR the pre-parse — 1,731 -> 2,058
    /// (+19%). It was never faster, anywhere. A 400k-case randomized
    /// differential over CSI-shaped inputs plus 20 hand-picked edge shapes
    /// hashed bit-identical events before and after removal (442,617 calls
    /// reached this function, 126,260 of them through the pre-parse).
    #[inline]
    fn parse_csi_general<S: ActionSink>(&mut self, input: &[u8], sink: &mut S) -> Option<usize> {
        self.params.clear();
        self.subparam_mask = 0;
        self.current_param = 0;
        self.param_started = false;
        self.intermediates.clear();

        let mut pos = 0;
        // Clamp the scan window up front (proof-friendly spelling of
        // `input.len().min(65)`): cap scan to 65 bytes (64 params + final).
        let limit = if input.len() < 65 { input.len() } else { 65 };
        let scan = input.get(..limit).unwrap_or(input);

        // Check for private marker (? > < = etc.)
        if let Some(&marker) = scan.first()
            && (0x3C..=0x3F).contains(&marker)
        {
            self.intermediates.push(marker);
            pos = 1;
        }

        let mut next_is_subparam = false;

        // Single pass: parse params and find final byte simultaneously
        while let Some(&b) = scan.get(pos) {
            if b.is_ascii_digit() {
                // `saturating_sub` is exact here: `b >= b'0'` on this branch.
                self.current_param = self
                    .current_param
                    .saturating_mul(10)
                    .saturating_add(u32::from(b.saturating_sub(b'0')));
                self.param_started = true;
                pos = pos.saturating_add(1);
            } else if b == b';' {
                self.push_current_param(next_is_subparam);
                next_is_subparam = false;
                pos = pos.saturating_add(1);
            } else if b == b':' {
                self.push_current_param(next_is_subparam);
                next_is_subparam = true;
                pos = pos.saturating_add(1);
            } else if (0x40..=0x7E).contains(&b) {
                if self.param_started {
                    self.push_current_param(next_is_subparam);
                }
                self.csi_dispatch_final(sink, b);
                self.state = State::Ground;
                return Some(pos.saturating_add(1));
            } else if (0x20..=0x2F).contains(&b) {
                if self.param_started {
                    self.push_current_param(next_is_subparam);
                }
                return self.parse_csi_intermediates(input, sink, pos, limit);
            } else {
                return None;
            }
        }

        None
    }

    /// Parse intermediate bytes (0x20-0x2F) and the final byte that follows.
    #[inline]
    fn parse_csi_intermediates<S: ActionSink>(
        &mut self,
        input: &[u8],
        sink: &mut S,
        mut pos: usize,
        limit: usize,
    ) -> Option<usize> {
        // Callers always pass `limit <= input.len()`, so this clamp is a
        // runtime no-op that ties the loop below to `scan.len()`.
        let end = if limit < input.len() {
            limit
        } else {
            input.len()
        };
        let scan = input.get(..end).unwrap_or(input);
        while let Some(&ib) = scan.get(pos) {
            if (0x20..=0x2F).contains(&ib) {
                if self.intermediates.len() < MAX_INTERMEDIATES {
                    self.intermediates.push(ib);
                }
                pos = pos.saturating_add(1);
            } else if (0x40..=0x7E).contains(&ib) {
                self.csi_dispatch_final(sink, ib);
                self.state = State::Ground;
                return Some(pos.saturating_add(1));
            } else {
                return None;
            }
        }
        None
    }
}
