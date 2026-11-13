// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! CSI (Control Sequence Introducer) fast-path parsing.

use aterm_provenance::pty_wrap_ref;

use crate::action::ActionSink;
use crate::state::State;
use crate::{MAX_INTERMEDIATES, MAX_PARAMS, Parser};

/// Whether `mask` is a live `subparam_mask` for `len` parameters: bit `i` (`i >= 1`) for
/// `params[i]`, bit `0` clear (`params[0]` cannot follow a colon), and nothing past `len` except
/// bit `MAX_PARAMS`, which a full list carries once the cap has dropped a sub-parameter. A handler
/// reads that bit to tell a colon group the cap cut from one that ended at the cap: the bit does
/// not say which later element fell, only that a colon came after the cap.
#[inline]
pub(crate) const fn subparam_mask_fits(mask: u32, len: usize) -> bool {
    let past = mask >> len;
    (mask & 1) == 0 && (past == 0 || (len == MAX_PARAMS && past == 1))
}

/// Parse 1..=3 ASCII decimal digits at the start of `bytes`, returning `(value, digits_read)`.
#[inline]
fn parse_u16_up_to_3(bytes: &[u8]) -> Option<(u16, usize)> {
    let &b0 = bytes.first()?;
    if !b0.is_ascii_digit() {
        return None;
    }
    let mut val = u16::from(b0.saturating_sub(b'0'));
    let mut pos = 1;
    if let Some(&b1) = bytes.get(1)
        && b1.is_ascii_digit()
    {
        val = val
            .saturating_mul(10)
            .saturating_add(u16::from(b1.saturating_sub(b'0')));
        pos = 2;
        if let Some(&b2) = bytes.get(2)
            && b2.is_ascii_digit()
        {
            val = val
                .saturating_mul(10)
                .saturating_add(u16::from(b2.saturating_sub(b'0')));
            pos = 3;
        }
    }
    Some((val, pos))
}

/// Parse 1..=4 ASCII decimal digits at the start of `bytes`, returning `(value, digits_read)`.
#[inline]
fn parse_u16_up_to_4(bytes: &[u8]) -> Option<(u16, usize)> {
    let (mut val, mut pos) = parse_u16_up_to_3(bytes)?;
    if pos == 3
        && let Some(&b3) = bytes.get(3)
        && b3.is_ascii_digit()
    {
        val = val
            .saturating_mul(10)
            .saturating_add(u16::from(b3.saturating_sub(b'0')));
        pos = 4;
    }
    Some((val, pos))
}

impl Parser {
    /// Finalize the current parameter (clamped to `u16`) and push it, setting
    /// its `subparam_mask` bit if `is_subparam`. Past `MAX_PARAMS` it is dropped,
    /// and a dropped sub-parameter sets bit `MAX_PARAMS` ([`subparam_mask_fits`]).
    #[inline]
    pub(crate) fn push_current_param(&mut self, is_subparam: bool) {
        let param_index = self.params.len();
        if param_index < MAX_PARAMS {
            let clamped = self.current_param.min(u32::from(u16::MAX));
            let value = u16::try_from(clamped).unwrap_or(u16::MAX);
            self.params.push(value);
            if is_subparam {
                self.subparam_mask |= 1 << param_index;
            }
        } else if is_subparam {
            self.subparam_mask |= 1 << MAX_PARAMS;
        }
        self.current_param = 0;
        self.param_started = false;
    }

    /// Close the parameter list at the final byte or at the intermediate byte that ends it.
    /// An empty last element is pushed as `0`, after `;` (ECMA-48 §5.4.2) as after `:` (T.416
    /// 13.1.8, xterm's `CASE_ESC_COLON`), so `P:` and `P:;Q` read `P`'s group alike. Only a list
    /// with no separator closes empty.
    #[inline]
    pub(crate) fn close_params(&mut self, after_colon: bool) {
        if self.param_started || !self.params.is_empty() {
            self.push_current_param(after_colon);
        }
    }

    /// Dispatch a two-parameter CSI sequence with no intermediates or subparameters.
    #[inline]
    fn dispatch_csi_pair<S: ActionSink>(
        &mut self,
        sink: &mut S,
        p0: u16,
        p1: u16,
        final_byte: u8,
        consumed: usize,
    ) -> usize {
        self.params.clear();
        self.params.push(p0);
        self.params.push(p1);
        self.intermediates.clear();
        self.current_param = 0;
        self.param_started = false;
        self.subparam_mask = 0;
        self.last_was_colon = false;
        sink.csi_dispatch(
            pty_wrap_ref(self.params.as_slice()),
            pty_wrap_ref(self.intermediates.as_slice()),
            final_byte,
        );
        self.state = State::Ground;
        consumed
    }

    /// Parse a CSI sequence on the fast path: the bytes consumed, or `None` to
    /// fall back to the byte path.
    #[inline]
    pub(crate) fn try_parse_csi_fast<S: ActionSink>(
        &mut self,
        input: &[u8],
        sink: &mut S,
    ) -> Option<usize> {
        let &first = input.first()?;

        // Zero-parameter CSI (ESC[A, ESC[H, ESC[m): the first byte is the final.
        if (0x40..=0x7E).contains(&first) {
            self.params.clear();
            self.intermediates.clear();
            self.current_param = 0;
            self.param_started = false;
            self.subparam_mask = 0;
            self.last_was_colon = false;
            sink.csi_dispatch(
                pty_wrap_ref(self.params.as_slice()),
                pty_wrap_ref(self.intermediates.as_slice()),
                first,
            );
            self.state = State::Ground;
            return Some(1);
        }

        // One- or two-parameter numeric CSI (1..=3 digits each, e.g. ESC[38m, ESC[100m, ESC[12;34H).
        if let Some((p0, n0)) = parse_u16_up_to_3(input) {
            if let Some(&next) = input.get(n0) {
                if (0x40..=0x7E).contains(&next) {
                    self.params.set_single(p0);
                    self.intermediates.clear();
                    self.current_param = 0;
                    self.param_started = false;
                    self.subparam_mask = 0;
                    self.last_was_colon = false;
                    sink.csi_dispatch(
                        pty_wrap_ref(self.params.as_slice()),
                        pty_wrap_ref(self.intermediates.as_slice()),
                        next,
                    );
                    self.state = State::Ground;
                    return Some(n0 + 1);
                }
                if next == b';' {
                    let after_semi = n0 + 1;
                    if let Some(&b_after) = input.get(after_semi)
                        && (0x40..=0x7E).contains(&b_after)
                    {
                        return Some(self.dispatch_csi_pair(sink, p0, 0, b_after, after_semi + 1));
                    }
                    if let Some((p1, n1)) = input.get(after_semi..).and_then(parse_u16_up_to_3)
                        && let Some(&fin) = input.get(after_semi + n1)
                        && (0x40..=0x7E).contains(&fin)
                    {
                        return Some(self.dispatch_csi_pair(
                            sink,
                            p0,
                            p1,
                            fin,
                            after_semi + n1 + 1,
                        ));
                    }
                    return self.parse_csi_after_first_param(input, sink, p0, after_semi);
                }
            }
        } else if first == b';' {
            // Empty first parameter: ESC[;H or ESC[;12H.
            if let Some(&b1) = input.get(1)
                && (0x40..=0x7E).contains(&b1)
            {
                return Some(self.dispatch_csi_pair(sink, 0, 0, b1, 2));
            }
            if let Some((p1, n1)) = input.get(1..).and_then(parse_u16_up_to_3)
                && let Some(&fin) = input.get(1 + n1)
                && (0x40..=0x7E).contains(&fin)
            {
                return Some(self.dispatch_csi_pair(sink, 0, p1, fin, 2 + n1));
            }
            return self.parse_csi_after_first_param(input, sink, 0, 1);
        } else if first == b'?'
            && let Some((p0, n0)) = input.get(1..).and_then(parse_u16_up_to_4)
            && let Some(&fin) = input.get(1 + n0)
            && (0x40..=0x7E).contains(&fin)
        {
            // Single-parameter DEC private CSI (ESC[?25l, ESC[?1049h, ESC[?2026h/l).
            self.params.set_single(p0);
            self.intermediates.set_single(b'?');
            self.current_param = 0;
            self.param_started = false;
            self.subparam_mask = 0;
            self.last_was_colon = false;
            sink.csi_dispatch(
                pty_wrap_ref(self.params.as_slice()),
                pty_wrap_ref(self.intermediates.as_slice()),
                fin,
            );
            self.state = State::Ground;
            return Some(2 + n0);
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

    /// Multi-param CSI fast path after a `D;` or `DD;` first parameter, continuing from `pos`.
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
        self.last_was_colon = false;

        let limit = if input.len() < 65 { input.len() } else { 65 };
        let scan = input.get(..limit).unwrap_or(input);

        while let Some(&b) = scan.get(pos) {
            if b.is_ascii_digit() {
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
                self.last_was_colon = true;
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

    /// Continue general CSI parsing at `pos`, keeping the params parsed so far.
    #[inline(never)]
    fn parse_csi_general_from<S: ActionSink>(
        &mut self,
        input: &[u8],
        sink: &mut S,
        mut pos: usize,
        mut next_is_subparam: bool,
    ) -> Option<usize> {
        let limit = if input.len() < 65 { input.len() } else { 65 };
        let scan = input.get(..limit).unwrap_or(input);

        while let Some(&b) = scan.get(pos) {
            if b.is_ascii_digit() {
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
                self.last_was_colon = next_is_subparam;
                self.close_params(next_is_subparam);
                self.csi_dispatch_final(sink, b);
                self.state = State::Ground;
                return Some(pos.saturating_add(1));
            } else if (0x20..=0x2F).contains(&b) {
                self.last_was_colon = next_is_subparam;
                self.close_params(next_is_subparam);
                return self.parse_csi_intermediates(input, sink, pos, limit);
            } else {
                return None;
            }
        }

        None
    }

    /// General-path CSI parser handling private markers, subparams, and intermediates in one pass.
    #[inline]
    fn parse_csi_general<S: ActionSink>(&mut self, input: &[u8], sink: &mut S) -> Option<usize> {
        self.params.clear();
        self.subparam_mask = 0;
        self.current_param = 0;
        self.param_started = false;
        self.last_was_colon = false;
        self.intermediates.clear();

        let mut pos = 0;
        let limit = if input.len() < 65 { input.len() } else { 65 };
        let scan = input.get(..limit).unwrap_or(input);

        if let Some(&marker) = scan.first()
            && (0x3C..=0x3F).contains(&marker)
        {
            self.intermediates.push(marker);
            pos = 1;
        }

        let mut next_is_subparam = false;

        while let Some(&b) = scan.get(pos) {
            if b.is_ascii_digit() {
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
                self.last_was_colon = next_is_subparam;
                self.close_params(next_is_subparam);
                self.csi_dispatch_final(sink, b);
                self.state = State::Ground;
                return Some(pos.saturating_add(1));
            } else if (0x20..=0x2F).contains(&b) {
                self.last_was_colon = next_is_subparam;
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
