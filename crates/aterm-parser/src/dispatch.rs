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

/// Whether the per-byte `OscPut` pushes a data byte onto an `osc_data` of
/// `len` bytes: the guard itself, which the `OscPut` arm calls and
/// `tests::string_bulk::osc_bulk_copy_agrees_with_per_byte_puts_at_every_breakpoint`
/// holds the bulk path's [`osc_bulk_copy_len`] to.
#[inline]
pub(crate) fn osc_put_keeps(len: usize) -> bool {
    len < MAX_OSC_DATA
}

/// How many of `n` OSC data bytes the bulk path appends to an `osc_data` of
/// `len` bytes: the prefix [`osc_put_keeps`] would keep one byte at a time.
/// `tests::string_bulk::osc_bulk_copy_agrees_with_per_byte_puts_at_every_breakpoint`
/// checks the two agree and the cap holds.
#[inline]
pub(crate) fn osc_bulk_copy_len(len: usize, n: usize) -> usize {
    n.min(MAX_OSC_DATA.saturating_sub(len))
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
        // Block C1 control bytes (0x80-0x9F) in non-Ground, non-string states
        // when C1 controls are disabled. The static transition table contains
        // "anywhere" entries that route 0x90→DCS, 0x9B→CSI, 0x9D→OSC, etc.
        // Without this guard, a malicious byte stream can inject escape sequences
        // via C1 introducers even when the parser is mid-sequence (#7556).
        //
        // Excluded states:
        //  - Ground: handled upstream (process_ground_special_byte)
        //  - the four control-string states (DcsPassthrough, DcsIgnore,
        //    OscString, SosPmApcString): their custom table overrides already
        //    map 0x80-0x9F to data actions (DcsPut / OscPut / ApcPut / Ignore),
        //    so C1 bytes are harmless payload there, and the 8-bit ST that the
        //    DCS and APC tables keep ends those strings in either C1 mode —
        //    except, with C1 off, a 0x9C inside a UTF-8 character (below).
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
            let prev_state = $self.state;
            // With C1 controls off, a 0x9C that continues the UTF-8 character
            // a DCS or APC payload ends in (`string_utf8`) is that character's
            // byte, not the 8-bit ST its own entry makes it: U+672C 本 is
            // `E6 9C AC`, and one continuation byte in 64 is 0x9C. It reads as
            // every other continuation byte does (0x80's entry: the string's
            // data action, staying in the string). `string_utf8` holds a prefix
            // only in those three states. With C1 controls on, a raw 0x9C is
            // the 8-bit ST wherever it falls.
            let read_as = if $byte == 0x9C
                && !$self.c1_controls_enabled
                && $self.string_utf8.continues($byte)
            {
                0x80
            } else {
                $byte
            };
            let transition = TRANSITIONS[prev_state as usize][read_as as usize];
            // The payload's UTF-8 head is kept over every byte a DCS or APC
            // string reads in itself, and dropped when the string ends.
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

            // CAN (0x18) and SUB (0x1A) CANCEL a control string (the VT500
            // "anywhere" transition to Ground via Execute; xterm's string
            // table sends both to the ground state without dispatching), so
            // a DCS, OSC or APC string they end is DISCARDED, never
            // delivered: a half-decoded sixel is not rendered, a truncated
            // OSC 52 clipboard write, OSC 8 link or title is not dispatched,
            // a truncated kitty command is not run. BEL (OSC) and 0x9C ST are
            // legitimate terminators, and so is ESC for an OSC or APC string. A
            // DCS string an ESC leaves ends only at the next byte (above).
            let canceled = $byte == 0x18 || $byte == 0x1A;

            // Where the byte leaves the parser: the transition's state, unless
            // it opens a tmux passthrough wrapper (`crate::tmux`), whose payload
            // starts from Ground.
            let mut next_state = transition.next_state;
            // The head of a tmux passthrough wrapper is read in `DcsIgnore`
            // byte by byte (no bulk path reads that state): `tmux;` whole opens
            // the wrapper, anything else leaves an ignored DCS.
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

            // Handle DCS unhook when leaving DcsPassthrough. An ESC begins ST
            // only when `\` follows, so it leaves the string hooked for the next
            // byte to decide.
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

            // Handle OSC end when leaving OscString: a canceled string ends
            // as one to discard (`osc_discard`, which `dispatch_osc` clears).
            if prev_state == State::OscString
                && transition.next_state != State::OscString
                && transition.action != ActionType::OscEnd
            {
                $self.osc_discard |= canceled;
                $self.dispatch_osc($sink, false);
            }

            // Handle APC end when leaving SosPmApcString
            if prev_state == State::SosPmApcString
                && transition.next_state != State::SosPmApcString
                && transition.action != ActionType::ApcEnd
            {
                if $self.apc_active {
                    $sink.apc_end(canceled);
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
                        // `ESC P t`: perhaps a tmux passthrough wrapper. Nothing
                        // is hooked; the rest of its head decides.
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
        } // end else (C1 ST override)
    }};
}

impl Parser {
    /// Process input bytes, calling sink for each action. The parser's one
    /// entry point.
    ///
    /// SIMD-scans printable text in the ground state and handles UTF-8
    /// multi-byte sequences (split across calls too). Never panics for any
    /// input, never reads out of bounds, always terminates.
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

    /// Read `input` inside an open tmux passthrough wrapper (`crate::tmux`)
    /// up to and including its next ESC, or the ESC's follower when one is
    /// pending, handing the state machine the payload unwrapped. Returns the
    /// rest.
    fn advance_unwrapping<'a, S: ActionSink>(&mut self, input: &'a [u8], sink: &mut S) -> &'a [u8] {
        if self.tmux == TmuxPassthrough::Escape {
            let Some((&byte, rest)) = input.split_first() else {
                return input;
            };
            match byte {
                b'\\' => {
                    self.tmux = TmuxPassthrough::Off;
                    // A string the payload left open ends with its wrapper,
                    // as at its ST: a program that did not double its ESCs
                    // meant this `ESC \` as its own string's end, and a
                    // string never runs on into what follows the wrapper.
                    if self.dcs_st_pending {
                        // The payload already supplied its ESC: finish that
                        // pending string terminator without canceling it with
                        // another ESC.
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
                    // An ESC the program did not double: both bytes, as they
                    // read without a wrapper.
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
            // A RIS the payload ended its run with ended the wrapper: the ESC
            // after it reads bare.
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

    /// The state machine over `input` as it arrived. Returns the rest
    /// unread when a tmux passthrough wrapper opens (outside a wrapper only:
    /// one opens no other), so [`Self::advance_fast`] reads it through the
    /// wrapper; otherwise nothing.
    fn advance_plain<'a, S: ActionSink>(&mut self, input: &'a [u8], sink: &mut S) -> &'a [u8] {
        // Every byte/slice access below uses the total `split_first`/`get`
        // spellings (the loop condition and the fast-path scanners guarantee
        // the fallbacks are unreachable at runtime), so this driver carries no
        // panic obligations of its own.
        let mut remaining = input;
        let wrapped = self.tmux.is_open();

        while !remaining.is_empty() {
            // Test instrumentation: count loop iterations for O(n) verification
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

                // C0 control fast path: LF/CR/BS/ESC are the most common
                // non-printable bytes. Route them directly to the state machine
                // without the 3 redundant range checks in process_ground_special_byte.
                if byte < 0x20 {
                    self.process_byte_inner(byte, sink);
                    continue;
                }

                // UTF-8 fast path: decode multi-byte sequences when all
                // continuation bytes are available, batching consecutive
                // non-ASCII characters for amortized dispatch overhead.
                // The decode logic is in a separate function to keep the
                // hot ASCII dispatch loop compact for L1i cache.
                if super::utf8::is_utf8_lead(byte) {
                    let consumed = self.decode_multibyte_run(byte, remaining, sink);
                    remaining = remaining.get(consumed..).unwrap_or(&[]);
                    continue;
                }

                self.process_ground_special_byte(byte, sink);
                continue;
            }

            // Not while a hooked DCS waits on this byte: `process_byte_inner`
            // ends that string first.
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

            // Bulk fast paths for the three string states (OSC/DCS/APC);
            // extracted into a helper so this driver's MIR stays within the
            // verifier's VC-generation budget. On return the caller falls
            // through to `process_byte_inner` for the boundary byte, or breaks
            // when the input is exhausted.
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
            // The last byte of a wrapper's head (only this path reads the
            // `DcsIgnore` it is read in): the rest goes through the wrapper.
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

        // Every other high byte begins no well-formed sequence: a stray
        // continuation (`A0..=BF`), `C0`/`C1` and `F5..=FF`. Each is a maximal
        // subpart on its own.
        if byte >= 0xA0 {
            sink.print(char::REPLACEMENT_CHARACTER);
            return;
        }

        self.process_byte_inner(byte, sink);
    }

    /// Bulk fast paths for the three string states, extracted from
    /// `advance_fast` (see the call site). Returns the input with the
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
            // Fall through to process_byte_inner for the C0 control byte.
        }

        // DCS bulk fast path (#7864): in DcsPassthrough most bytes are DcsPut,
        // but the boundary set is NOT the same as DCS terminators. Beyond the
        // four state-exit bytes 0x18 (CAN), 0x1A (SUB), 0x1B (ESC), 0x9C (ST),
        // the transition table maps 0x7F (DEL) to ActionType::Ignore (it stays
        // in DcsPassthrough and emits NOTHING — unlike APC, where 0x7F is
        // ApcPut data). Reusing `find_dcs_terminator` here would bulk-copy 0x7F
        // into `dcs_put_bulk`, diverging from the byte-by-byte path which drops
        // it. So scan with `find_dcs_passthrough_boundary` (terminators + 0x7F),
        // bulk-copy up to the boundary, then fall through to process_byte_inner for
        // the boundary byte: a 0x7F is Ignored (no dcs_put, stays in
        // DcsPassthrough) and the loop re-enters this branch for the remainder.
        //
        // The run's last bytes bring the payload's UTF-8 head up to date
        // (`Utf8Prefix::after`), so the byte path reads a 0x9C at the boundary
        // as ST or as a character's byte exactly as it would have byte by byte.
        if self.state == State::DcsPassthrough && self.dcs_active {
            let n = Self::find_dcs_passthrough_boundary(remaining);
            if n > 0 {
                let run = remaining.get(..n).unwrap_or(&[]);
                sink.dcs_put_bulk(pty_wrap_ref(run));
                self.string_utf8 = self.string_utf8.after(run);
                remaining = remaining.get(n..).unwrap_or(&[]);
            }
            // Fall through to process_byte_inner for the boundary byte.
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
                let run = remaining.get(..n).unwrap_or(&[]);
                if self.apc_active {
                    sink.apc_put_bulk(pty_wrap_ref(run));
                }
                self.string_utf8 = self.string_utf8.after(run);
                remaining = remaining.get(n..).unwrap_or(&[]);
            }
            // Fall through to process_byte_inner for the terminator (or for a 0x9C
            // inside a UTF-8 character, which stays in the string).
        }

        remaining
    }

    /// Find the first byte in `input` that must NOT be bulk-forwarded as DCS
    /// data — the scan boundary for the DCS fast path.
    ///
    /// True DCS terminators: 0x18 (CAN), 0x1A (SUB), 0x1B (ESC), 0x9C (ST).
    /// Unlike OSC, DCS treats 0x9C as ST even when C1 controls are otherwise
    /// disabled: the transition table leaves 0x9C as a DCS terminator, so the
    /// fast path must scan for it too. Whether a 0x9C found here IS the ST
    /// (with C1 off, one inside a UTF-8 character is not) is the byte path's
    /// call, which the bulk paths hand it.
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
    /// the transition table. This is distinct from APC (`SosPmApcString`), where 0x7F is
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
        // An OSC a seamless update carried past its head (`osc_discard`, set
        // only by `restore_carry`) ends here WITHOUT dispatching: its payload is
        // a truncated tail.
        if self.osc_discard {
            self.osc_discard = false;
        } else {
            Self::parse_and_dispatch_osc(self.osc_data.as_slice(), sink, bel_terminated);
        }
        self.osc_data.clear();
        // Shrink the buffer if a large OSC payload inflated it beyond 4 KiB.
        // Without this, a single OSC 1337 image permanently holds up to
        // MAX_OSC_DATA (8 MiB) per parser instance for the session lifetime
        // (#7272). This makes the large cap a transient spike, not bloat.
        if self.osc_data.capacity() > 4096 {
            self.osc_data.shrink_to(128);
        }
    }

    /// Parse and dispatch an OSC payload in a single forward semicolon scan.
    /// PURE in the parser state: reads `data`, writes `sink`, mutates nothing on
    /// `self` (takes `&[u8]`, not `&mut self`).
    ///
    /// Fast paths for 1-parameter (`[data]`, e.g. bare OSC 104/110/30001) and
    /// 2-parameter (`[p0, rest]`, e.g. OSC 0/1/2/7/9/133/1337) sequences avoid
    /// the 256-byte `ArrayVec` initialization and dispatch directly from the
    /// stack. When a second `;` is found at `m` (3+ parameters, e.g. OSC 8
    /// `8;;url`, OSC 52 `52;c;<base64>`, OSC 4 `4;1;rgb:...`), the already-split
    /// `(p0, p1)` and `rest[m + 1..]` are passed directly into
    /// [`Self::parse_osc_from_third_param`] so `data[..k + 1 + m + 1]` is never
    /// scanned a second time.
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
    /// second semicolon. PURE in the parser state (`&[u8]`, not `&mut self`).
    #[inline(never)]
    fn parse_osc_from_third_param<S: ActionSink>(
        p0: &[u8],
        p1: &[u8],
        mut rest: &[u8],
        sink: &mut S,
        bel_terminated: bool,
    ) {
        // THE LAST SLOT CARRIES THE REMAINDER VERBATIM, semicolons and all.
        //
        // Consumers re-join: `set_title` joins `params[1..]` and `handle_osc_8`
        // joins `params[2..]` with ';', so titles and URIs with literal
        // semicolons survive. Dropping segments past the cap would truncate an
        // OSC 8 URI that still passes the link gate, and a Cmd-click would open
        // a URL the emitter never wrote. With the tail in the last slot,
        // `join(';')` reconstructs the payload at any semicolon count, with no
        // allocation; handlers that index a fixed early param are unaffected.
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
    ///
    /// Called when a UTF-8 lead byte ([`super::utf8::is_utf8_lead`]) is
    /// encountered in the ground-state fast path. Decodes consecutive
    /// multi-byte characters into a buffer and dispatches via
    /// `print`/`print_unicode_bulk`.
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
                Some(&lead) if super::utf8::is_utf8_lead(lead) => {
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
    /// `pub(crate)` so the THRU-4a bulk lane's exhaustive first-sequence test
    /// (`utf8_simd::tests::bulk_first_sequence_matches_scalar_for_every_lead_and_continuation_class`)
    /// holds the bulk path to this exact scalar oracle.
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
