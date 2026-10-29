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
                self.subparam_mask |= 1 << param_index;
            }
        }
        self.current_param = 0;
        self.param_started = false;
    }

    /// Close the parameter list at the final byte or at the intermediate byte that ends it.
    /// An empty trailing parameter after `;` pushes `0` (ECMA-48 §5.4.2), while a trailing `:` opens no subparameter.
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

        if let &[b0, b1, ..] = input
            && b0.is_ascii_digit()
        {
            let d0 = u16::from(b0.saturating_sub(b'0'));
            if (0x40..=0x7E).contains(&b1) {
                self.params.set_single(d0);
                self.intermediates.clear();
                self.current_param = 0;
                self.param_started = false;
                self.subparam_mask = 0;
                self.last_was_colon = false;
                sink.csi_dispatch(
                    pty_wrap_ref(self.params.as_slice()),
                    pty_wrap_ref(self.intermediates.as_slice()),
                    b1,
                );
                self.state = State::Ground;
                return Some(2);
            }
            if b1 == b';' {
                return self.parse_csi_after_first_param(input, sink, d0, 2);
            }
            if let &[_, _, b2, ..] = input
                && b1.is_ascii_digit()
            {
                let p1 = d0
                    .saturating_mul(10)
                    .saturating_add(u16::from(b1.saturating_sub(b'0')));
                if (0x40..=0x7E).contains(&b2) {
                    self.params.set_single(p1);
                    self.intermediates.clear();
                    self.current_param = 0;
                    self.param_started = false;
                    self.subparam_mask = 0;
                    self.last_was_colon = false;
                    sink.csi_dispatch(
                        pty_wrap_ref(self.params.as_slice()),
                        pty_wrap_ref(self.intermediates.as_slice()),
                        b2,
                    );
                    self.state = State::Ground;
                    return Some(3);
                }
                if b2 == b';' {
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
