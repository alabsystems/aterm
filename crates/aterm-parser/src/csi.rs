// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! CSI (Control Sequence Introducer) fast-path parsing.

use aterm_provenance::pty_wrap_ref;

use crate::action::ActionSink;
use crate::state::State;
use crate::{MAX_INTERMEDIATES, MAX_PARAMS, Parser};

impl Parser {
    /// Finalize the current parameter (clamped to `u16`) and push it, setting
    /// its `subparam_mask` bit if `is_subparam`; past `MAX_PARAMS` it is dropped.
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

    /// Close the parameter list at the final byte, or at the intermediate
    /// byte that ends it.
    ///
    /// The parameter after the last `;` is a parameter even when empty
    /// (ECMA-48 §5.4.2): `CSI 1 ; m` is `1` then the default, which xterm and
    /// vte read as bold, then reset. So the open parameter is pushed when a
    /// digit of it was read or a `;` came last, an empty one as 0 (what every
    /// handler reads as its default). A list with no separator closes with no
    /// parameter (`CSI m` stays `[]`), and a trailing `:` opens no subparameter.
    ///
    /// `after_colon`: the last separator was `:` (`last_was_colon` on the byte
    /// path, `next_is_subparam` on the fast paths). A parameter is finalized
    /// only at a separator, so a non-empty list with no digit read since means
    /// a separator came last.
    #[inline]
    pub(crate) fn close_params(&mut self, after_colon: bool) {
        if self.param_started || (!self.params.is_empty() && !after_colon) {
            self.push_current_param(after_colon);
        }
    }

    /// Parse a CSI sequence on the fast path: the bytes consumed, or `None` to
    /// fall back to the byte path.
    #[inline]
    pub(crate) fn try_parse_csi_fast<S: ActionSink>(
        &mut self,
        input: &[u8],
        sink: &mut S,
    ) -> Option<usize> {
        // Zero-parameter CSI (ESC[A, ESC[H, ESC[m): the first byte is the final.
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

        // One- or two-digit first parameter (ESC[5A, ESC[0m, ESC[32m). Slice
        // patterns and saturating arithmetic keep this hot path free of panic
        // obligations; the saturations are exact (digits, at most 99).
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

    /// Multi-param CSI fast path after a `D;` or `DD;` first parameter
    /// (`38;5;Nm`, `R;CH`, `1;31m`), continuing from `pos`. Out of line to keep
    /// the zero/one/two-digit dispatch compact in L1i.
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

        // Clamp the scan window (a proof-friendly `input.len().min(65)`); `get`
        // reads and saturating increments keep the loop free of panic
        // obligations, and the saturations are exact (`pos < scan.len() <= 65`).
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
                // Colon subparameters are rare: the general loop takes the
                // rest, its next parameter a subparameter of this one.
                self.push_current_param(false);
                return self.parse_csi_general_from(input, sink, pos.saturating_add(1), true);
            } else if (0x40..=0x7E).contains(&b) {
                self.close_params(false);
                self.csi_dispatch_final(sink, b);
                self.state = State::Ground;
                return Some(pos.saturating_add(1));
            } else if (0x20..=0x2F).contains(&b) {
                self.close_params(false);
                return self.parse_csi_intermediates(input, sink, pos, limit);
            } else {
                return None;
            }
        }

        None
    }

    /// Continue general CSI parsing at `pos`, keeping the params parsed so far:
    /// `parse_csi_after_first_param` hands over the bytes after a `:` with
    /// `next_is_subparam` set, so this loop reads the rest as the byte path does.
    #[inline(never)]
    fn parse_csi_general_from<S: ActionSink>(
        &mut self,
        input: &[u8],
        sink: &mut S,
        mut pos: usize,
        mut next_is_subparam: bool,
    ) -> Option<usize> {
        // Same proof-friendly loop shape as `parse_csi_after_first_param`:
        // clamped scan window, `get`-based reads, and saturating position
        // increments (all exact at runtime, where `pos < scan.len() <= 65`).
        let limit = if input.len() < 65 { input.len() } else { 65 };
        let scan = input.get(..limit).unwrap_or(input);

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
                self.close_params(next_is_subparam);
                self.csi_dispatch_final(sink, b);
                self.state = State::Ground;
                return Some(pos.saturating_add(1));
            } else if (0x20..=0x2F).contains(&b) {
                self.close_params(next_is_subparam);
                return self.parse_csi_intermediates(input, sink, pos, limit);
            } else {
                return None;
            }
        }

        None
    }

    /// General-path CSI parser: one pass parses params and finds the final
    /// byte, with private markers, subparams and intermediates.
    ///
    /// No speculative bulk parameter pre-parse runs first: one measured slower
    /// on every shape that reaches here, since `try_parse_csi_fast` already
    /// takes the `D;…`/`DD;…` SGR shapes.
    #[inline]
    fn parse_csi_general<S: ActionSink>(&mut self, input: &[u8], sink: &mut S) -> Option<usize> {
        self.params.clear();
        self.subparam_mask = 0;
        self.current_param = 0;
        self.param_started = false;
        self.intermediates.clear();

        let mut pos = 0;
        // Clamp the scan window to 65 bytes (a proof-friendly
        // `input.len().min(65)`); a longer CSI falls back to the byte path.
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
                self.close_params(next_is_subparam);
                self.csi_dispatch_final(sink, b);
                self.state = State::Ground;
                return Some(pos.saturating_add(1));
            } else if (0x20..=0x2F).contains(&b) {
                self.close_params(next_is_subparam);
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
