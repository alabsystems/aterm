// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Author: Andrew Yates

//! Parser dispatch engine: `advance_fast`, state-machine byte processing, helpers.

use aterm_alloc::ArrayVec;
use aterm_provenance::pty_wrap_ref;

use super::table::{ActionType, TRANSITIONS};
use super::tmux::{TMUX_HEAD, TmuxPassthrough};
use super::utf8::Utf8Prefix;
use super::{ActionSink, MAX_OSC_DATA, MAX_OSC_PARAMS, Parser, State};

#[cfg(test)]
use super::count_parser_loop_iteration;

use super::MAX_INTERMEDIATES;

/// Whether the per-byte `OscPut` keeps a byte when `osc_data` holds `len`.
#[inline]
pub(crate) const fn osc_put_keeps(len: usize) -> bool {
    len < MAX_OSC_DATA
}

/// How many of `n` OSC data bytes the bulk path appends to an `osc_data` of `len` bytes.
#[inline]
pub(crate) const fn osc_bulk_copy_len(len: usize, n: usize) -> usize {
    let cap = MAX_OSC_DATA.saturating_sub(len);
    if n < cap { n } else { cap }
}

#[cfg(clean_islands)]
macro_rules! trust_island {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! trust_island {
    ($($tt:tt)*) => {};
}

trust_island! {
    clean {
        namespace OscSplitProofs

        -- Abstract representation of a 3+ parameter OSC payload split at its first
        -- two semicolons: p0, p1, and the remainder tail.
        structure ThreeParamOsc where
          p0 : Nat
          p1 : Nat
          tail : List Nat
        deriving Repr

        -- Full split from the beginning yields [p0, p1] ++ tail.
        def fullSplit (osc : ThreeParamOsc) : List Nat :=
          osc.p0 :: osc.p1 :: osc.tail

        -- Seeded split starting at the third parameter with p0 and p1 already
        -- pushed yields [p0, p1] ++ tail without rescanning p0 or p1.
        def seededFromThird (p0 p1 : Nat) (tail : List Nat) : List Nat :=
          p0 :: p1 :: tail

        -- Theorem: seeding parse_osc_from_third_param with (p0, p1) produces the
        -- exact same parameter list as scanning from byte 0.
        theorem seeded_from_third_matches_full_split (osc : ThreeParamOsc) :
            seededFromThird osc.p0 osc.p1 osc.tail = fullSplit osc := by
          rfl

        end OscSplitProofs
    }
}

/// The state machine's per-byte dispatch, expanded in `process_byte_inner`.
macro_rules! process_byte_impl {
    ($self:expr, $byte:expr, $sink:expr) => {{
        // The byte after the ESC that left a hooked DCS string decides how the
        // string ends (`dcs_st_pending`): `\` completes the ST, so the string
        // ends complete; any other byte breaks it off, and a broken-off string
        // is canceled, as CAN and SUB cancel one (xterm answers or applies a DCS
        // only at ST). The byte itself is then read in `Escape` as always.
        if $self.dcs_st_pending {
            $self.dcs_st_pending = false;
            $sink.dcs_unhook($byte != b'\\');
        }
        if !$self.c1_controls_enabled
            && (0x80..=0x9F).contains(&$byte)
            && !matches!(
                $self.state,
                State::Ground
                    | State::DcsPassthrough
                    | State::DcsIgnore
                    | State::OscString
                    | State::SosPmApcString
            )
        {
        } else if $self.c1_controls_enabled && $byte == 0x9C && $self.state == State::OscString {
            $self.dispatch_osc($sink, false);
            $self.state = State::Ground;
        } else {
            let prev_state = $self.state;
            let read_as = if $byte == 0x9C
                && !$self.c1_controls_enabled
                && $self.string_utf8.continues($byte)
            {
                0x80
            } else {
                $byte
            };
            let transition = TRANSITIONS[prev_state as usize][read_as as usize];
            if matches!(
                prev_state,
                State::DcsPassthrough | State::DcsIgnore | State::SosPmApcString
            ) {
                $self.string_utf8 = if transition.next_state == prev_state {
                    $self.string_utf8.step($byte)
                } else {
                    Utf8Prefix::NONE
                };
            }

            let canceled = $byte == 0x18 || $byte == 0x1A;

            let mut next_state = transition.next_state;
            if let TmuxPassthrough::Head(_) = $self.tmux {
                $self.tmux = $self.tmux.after_head_byte(
                    $byte,
                    prev_state == State::DcsIgnore && next_state == State::DcsIgnore,
                );
                if $self.tmux == TmuxPassthrough::Open {
                    next_state = State::Ground;
                    $self.string_utf8 = Utf8Prefix::NONE;
                }
            }

            if prev_state == State::DcsPassthrough && transition.next_state != State::DcsPassthrough
            {
                if $self.dcs_active {
                    $self.dcs_active = false;
                    if $byte == 0x1B {
                        $self.dcs_st_pending = true;
                    } else {
                        $sink.dcs_unhook(canceled);
                    }
                }
            }

            if prev_state == State::OscString
                && transition.next_state != State::OscString
                && transition.action != ActionType::OscEnd
            {
                $self.osc_discard |= canceled;
                $self.dispatch_osc($sink, false);
            }

            if prev_state == State::SosPmApcString
                && transition.next_state != State::SosPmApcString
                && transition.action != ActionType::ApcEnd
            {
                if $self.apc_active {
                    $sink.apc_end(canceled);
                    $self.apc_active = false;
                }
            }

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
                    // RIS (`ESC c`) resets the parser whole: a tmux passthrough
                    // wrapper it arrived in ends with it.
                    if $byte == b'c' && $self.intermediates.is_empty() {
                        $self.tmux = TmuxPassthrough::Off;
                    }
                    $sink.esc_dispatch(pty_wrap_ref($self.intermediates.as_slice()), $byte);
                }
                ActionType::CsiDispatch => {
                    $self.close_params($self.last_was_colon);
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
                    $self.close_params($self.last_was_colon);
                    if $byte == TMUX_HEAD[0]
                        && $self.tmux == TmuxPassthrough::Off
                        && $self.params.is_empty()
                        && $self.intermediates.is_empty()
                    {
                        $self.tmux = TmuxPassthrough::Head(1);
                        next_state = State::DcsIgnore;
                    } else {
                        $sink.dcs_hook(
                            pty_wrap_ref($self.params.as_slice()),
                            pty_wrap_ref($self.intermediates.as_slice()),
                            $byte,
                        );
                        $self.dcs_active = true;
                    }
                }
                ActionType::DcsPut => {
                    $sink.dcs_put($byte);
                }
                ActionType::OscStart => {
                    $self.osc_data.clear();
                }
                ActionType::OscPut => {
                    if osc_put_keeps($self.osc_data.len()) {
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
                        $sink.apc_end(false);
                        $self.apc_active = false;
                    }
                }
            }

            $self.state = next_state;
        }
    }};
}

impl Parser {
    /// Process input bytes, calling sink for each action.
    pub fn advance_fast<S: ActionSink>(&mut self, input: &[u8], sink: &mut S) {
        let mut remaining = input;
        while !remaining.is_empty() {
            remaining = if self.tmux.is_open() {
                self.advance_unwrapping(remaining, sink)
            } else {
                self.advance_plain(remaining, sink)
            };
        }
    }

    /// Read `input` inside an open tmux passthrough wrapper (`crate::tmux`).
    fn advance_unwrapping<'a, S: ActionSink>(&mut self, input: &'a [u8], sink: &mut S) -> &'a [u8] {
        if self.tmux == TmuxPassthrough::Escape {
            let Some((&byte, rest)) = input.split_first() else {
                return input;
            };
            match byte {
                b'\\' => {
                    self.tmux = TmuxPassthrough::Off;
                    if self.dcs_st_pending {
                        self.advance_plain(b"\\", sink);
                    } else if matches!(
                        self.state,
                        State::OscString
                            | State::DcsPassthrough
                            | State::DcsIgnore
                            | State::SosPmApcString
                    ) {
                        self.advance_plain(b"\x1b\\", sink);
                    }
                }
                0x1B => {
                    self.tmux = TmuxPassthrough::Open;
                    self.advance_plain(&[0x1B], sink);
                }
                _ => {
                    self.tmux = TmuxPassthrough::Open;
                    self.advance_plain(&[0x1B, byte], sink);
                }
            }
            return rest;
        }
        let n = super::simd::find_any_of(input, [0x1B]).unwrap_or(input.len());
        let (run, rest) = input.split_at(n);
        if !run.is_empty() {
            self.advance_plain(run, sink);
            if !self.tmux.is_open() {
                return rest;
            }
        }
        match rest.split_first() {
            Some((_, after)) => {
                self.tmux = TmuxPassthrough::Escape;
                after
            }
            None => rest,
        }
    }

    /// The state machine over `input` as it arrived.
    fn advance_plain<'a, S: ActionSink>(&mut self, input: &'a [u8], sink: &mut S) -> &'a [u8] {
        let mut remaining = input;
        let wrapped = self.tmux.is_open();

        while !remaining.is_empty() {
            #[cfg(test)]
            count_parser_loop_iteration();

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
                    sink.print_ascii_bulk(pty_wrap_ref(printable));
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
                    self.process_byte_inner(b'[', sink);
                    continue;
                }

                if byte < 0x20 {
                    self.process_byte_inner(byte, sink);
                    continue;
                }

                if super::utf8::is_utf8_lead(byte) {
                    let consumed = self.decode_multibyte_run(byte, remaining, sink);
                    remaining = remaining.get(consumed..).unwrap_or(&[]);
                    continue;
                }

                self.process_ground_special_byte(byte, sink);
                continue;
            }

            if self.state == State::Escape
                && !self.dcs_st_pending
                && remaining.first() == Some(&b'[')
            {
                let rest = remaining.get(1..).unwrap_or(&[]);
                if let Some(consumed) = self.try_parse_csi_fast(rest, sink) {
                    remaining = rest.get(consumed..).unwrap_or(&[]);
                    self.state = State::Ground;
                    continue;
                }
            }

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
            self.process_byte_inner(byte, sink);
            if !wrapped && self.tmux.is_open() {
                return remaining;
            }
        }
        remaining
    }

    /// Process a single byte through the state machine (inner implementation).
    #[inline]
    pub(crate) fn process_byte_inner<S: ActionSink>(&mut self, byte: u8, sink: &mut S) {
        process_byte_impl!(self, byte, sink);
    }

    #[inline]
    fn process_ground_special_byte<S: ActionSink>(&mut self, byte: u8, sink: &mut S) {
        if super::utf8::is_utf8_lead(byte) {
            self.start_utf8(byte);
            return;
        }

        if (0x80..=0x9F).contains(&byte) {
            if self.c1_controls_enabled {
                self.process_byte_inner(byte, sink);
            } else {
                sink.print(char::REPLACEMENT_CHARACTER);
            }
            return;
        }

        if byte >= 0xA0 {
            sink.print(char::REPLACEMENT_CHARACTER);
            return;
        }

        self.process_byte_inner(byte, sink);
    }

    /// Bulk fast paths for the three string states.
    fn advance_string_states_bulk<'a, S: ActionSink>(
        &mut self,
        mut remaining: &'a [u8],
        sink: &mut S,
    ) -> &'a [u8] {
        // EQUIVALENT to the byte-by-byte slow path (do not "fix"): `n` is
        // the position of the FIRST control byte, so every byte in [0..n)
        // is a data byte the slow path maps to OscPut, which pushes only
        // while `osc_put_keeps(osc_data.len())`. `osc_bulk_copy_len` keeps
        // that same prefix and drops the rest (checked against that guard at
        // every breakpoint of every `len`:
        // `tests::string_bulk::osc_bulk_copy_agrees_with_per_byte_puts_at_every_breakpoint`), and
        // `remaining.get(n..)` then lands on the control byte, which both
        // paths process the same way. `osc_payload_cap_parity`
        // (tests/string_bulk.rs) drives both paths across the boundary.
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
                let copy_len = osc_bulk_copy_len(self.osc_data.len(), n);
                self.osc_data
                    .extend_from_slice(remaining.get(..copy_len).unwrap_or(&[]));
                remaining = remaining.get(n..).unwrap_or(&[]);
            }
        }

        if self.state == State::DcsPassthrough && self.dcs_active {
            let n = Self::find_dcs_passthrough_boundary(remaining);
            if n > 0 {
                let run = remaining.get(..n).unwrap_or(&[]);
                sink.dcs_put_bulk(pty_wrap_ref(run));
                self.string_utf8 = self.string_utf8.after(run);
                remaining = remaining.get(n..).unwrap_or(&[]);
            }
        }

        if self.state == State::SosPmApcString {
            let n = Self::find_dcs_terminator(remaining);
            if n > 0 {
                let run = remaining.get(..n).unwrap_or(&[]);
                if self.apc_active {
                    sink.apc_put_bulk(pty_wrap_ref(run));
                }
                self.string_utf8 = self.string_utf8.after(run);
                remaining = remaining.get(n..).unwrap_or(&[]);
            }
        }

        remaining
    }

    /// Find the first DCS/APC terminator in `input`: CAN, SUB, ESC or 0x9C (ST).
    #[inline]
    fn find_dcs_terminator(input: &[u8]) -> usize {
        super::simd::find_any_of(input, [0x18, 0x1A, 0x1B, 0x9C]).unwrap_or(input.len())
    }

    /// Find the first byte that the DCS-passthrough bulk fast path must NOT
    /// bulk-copy into `dcs_put_bulk`.
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
            self.current_param = self
                .current_param
                .saturating_mul(10)
                .saturating_add(u32::from(byte.saturating_sub(b'0')));
            self.param_started = true;
        } else if byte == b';' {
            self.finalize_param();
            self.last_was_colon = false;
        } else if byte == b':' {
            self.finalize_param();
            self.last_was_colon = true;
        }
    }

    /// Finalize the current parameter.
    #[inline]
    pub(crate) fn finalize_param(&mut self) {
        self.push_current_param(self.last_was_colon);
    }

    /// Collect an intermediate byte (dropped past `MAX_INTERMEDIATES`).
    #[inline]
    fn collect_intermediate(&mut self, byte: u8) {
        if self.intermediates.len() < MAX_INTERMEDIATES {
            self.intermediates.push(byte);
        }
    }

    /// Parse and dispatch OSC data.
    fn dispatch_osc<S: ActionSink>(&mut self, sink: &mut S, bel_terminated: bool) {
        // Parsing lives in `parse_and_dispatch_osc` (`&[u8]`, not `&mut self`)
        // so the contract verifier sees it leave the `TypeInvariant` state
        // alone; the only mutation here is `osc_data.clear()`. A string marked
        // `osc_discard` (a carried tail, or one CAN/SUB canceled) ends without
        // dispatching.
        if self.osc_discard {
            self.osc_discard = false;
        } else {
            Self::parse_and_dispatch_osc(self.osc_data.as_slice(), sink, bel_terminated);
        }
        self.osc_data.clear();
        if self.osc_data.capacity() > 4096 {
            self.osc_data.shrink_to(128);
        }
    }

    /// Split an OSC payload on `;` in one forward scan and dispatch it.
    fn parse_and_dispatch_osc<S: ActionSink>(data: &[u8], sink: &mut S, bel_terminated: bool) {
        let Some(k) = data.iter().position(|&b| b == b';') else {
            let params: [&[u8]; 1] = [data];
            sink.osc_dispatch_with_terminator(pty_wrap_ref(&params[..]), bel_terminated);
            return;
        };
        let p0 = &data[..k];
        let rest = &data[k + 1..];
        let Some(m) = rest.iter().position(|&b| b == b';') else {
            let params: [&[u8]; 2] = [p0, rest];
            sink.osc_dispatch_with_terminator(pty_wrap_ref(&params[..]), bel_terminated);
            return;
        };
        Self::parse_osc_from_third_param(p0, &rest[..m], &rest[m + 1..], sink, bel_terminated);
    }

    /// General OSC dispatch for 3+ semicolon-separated parameters, seeded with
    /// the already-split first two parameters `(p0, p1)` and `rest` after the
    /// second semicolon.
    #[inline(never)]
    fn parse_osc_from_third_param<S: ActionSink>(
        p0: &[u8],
        p1: &[u8],
        mut rest: &[u8],
        sink: &mut S,
        bel_terminated: bool,
    ) {
        let mut params: ArrayVec<&[u8], MAX_OSC_PARAMS> = ArrayVec::new();
        params.push(p0);
        params.push(p1);
        loop {
            if params.len() + 1 == MAX_OSC_PARAMS {
                params.push(rest);
                break;
            }
            match rest.iter().position(|&b| b == b';') {
                Some(k) => {
                    params.push(&rest[..k]);
                    rest = &rest[k + 1..];
                }
                None => {
                    params.push(rest);
                    break;
                }
            }
        }
        sink.osc_dispatch_with_terminator(pty_wrap_ref(params.as_slice()), bel_terminated);
    }

    /// Decode and dispatch a run of multi-byte UTF-8 characters.
    #[inline(never)]
    fn decode_multibyte_run<S: ActionSink>(
        &mut self,
        first_lead: u8,
        remaining: &[u8],
        sink: &mut S,
    ) -> usize {
        let mut chars: ArrayVec<char, 256> = ArrayVec::new();
        let orig_len = remaining.len();
        let mut rem = remaining;

        match Self::decode_multibyte_char(first_lead, rem) {
            Some((c, consumed)) => {
                chars.push(c);
                rem = rem.get(consumed..).unwrap_or(&[]);
            }
            None => {
                self.start_utf8(first_lead);
                return 0;
            }
        }

        loop {
            let taken = crate::utf8_simd::bulk_decode_run(rem, &mut chars);
            rem = rem.get(taken..).unwrap_or(&[]);
            if chars.is_full() {
                break;
            }
            match rem.first() {
                Some(&lead) if super::utf8::is_utf8_lead(lead) => {
                    match Self::decode_multibyte_char(lead, rem.get(1..).unwrap_or(&[])) {
                        Some((c, consumed)) => {
                            chars.push(c);
                            rem = rem.get(1 + consumed..).unwrap_or(&[]);
                        }
                        None => {
                            rem = rem.get(1..).unwrap_or(&[]);
                            self.start_utf8(lead);
                            break;
                        }
                    }
                }
                _ => break,
            }
        }

        match chars.as_slice() {
            [] => {}
            &[c] => sink.print(c),
            run => sink.print_unicode_bulk(pty_wrap_ref(run)),
        }

        orig_len.saturating_sub(rem.len())
    }

    /// Decode one multi-byte UTF-8 character from `lead` plus the prefix of `rem`.
    #[inline]
    pub(crate) fn decode_multibyte_char(lead: u8, rem: &[u8]) -> Option<(char, usize)> {
        if !(0xC2..=0xF4).contains(&lead) {
            return None;
        }
        if lead >= 0xF0 {
            if let &[c0, c1, c2, ..] = rem
                && (c0 & 0xC0) == 0x80
                && (c1 & 0xC0) == 0x80
                && (c2 & 0xC0) == 0x80
            {
                let cp = (u32::from(lead & 0x07) << 18)
                    | (u32::from(c0 & 0x3F) << 12)
                    | (u32::from(c1 & 0x3F) << 6)
                    | u32::from(c2 & 0x3F);
                if (0x10000..=0x0010_FFFF).contains(&cp) {
                    char::from_u32(cp).map(|c| (c, 3))
                } else {
                    None
                }
            } else {
                None
            }
        } else if lead >= 0xE0 {
            if let &[c0, c1, ..] = rem
                && (c0 & 0xC0) == 0x80
                && (c1 & 0xC0) == 0x80
            {
                let cp = (u32::from(lead & 0x0F) << 12)
                    | (u32::from(c0 & 0x3F) << 6)
                    | u32::from(c1 & 0x3F);
                if cp >= 0x800 && !(0xD800..=0xDFFF).contains(&cp) {
                    char::from_u32(cp).map(|c| (c, 2))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            if let &[c0, ..] = rem
                && (c0 & 0xC0) == 0x80
            {
                let cp = (u32::from(lead & 0x1F) << 6) | u32::from(c0 & 0x3F);
                if cp >= 0x80 {
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
