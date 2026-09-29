// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Parser action dispatch for `TerminalHandler`.
//!
//! This module implements the **parser actions layer** of the terminal handler
//! concern separation (#2157). It receives parsed escape sequences from the
//! VT parser via the `ActionSink` trait and dispatches them to typed handler
//! methods. This layer depends only on `parser` (for the trait) and
//! `charset` (for character translation).
//!
//! ## Concern layers
//!
//! - **Parser actions** (this file): `ActionSink` dispatch from parser events
//! - **State transitions** (`handler_state.rs`): grid/mode mutations from typed operations
//! - **Side-effects**: callbacks and external service activation (inline in handler files)

use crate::grid::{CellFlags, PackedColor};
use crate::parser::ActionSink;
use aterm_provenance::{Provenance, Pty};
use aterm_types::charset::{GlMapping, SingleShift};

use super::super::handler_osc_1337::PlacementCursor;
use super::super::transient_state::{KittyParent, KittyRelation, KittyVariant, KittyVirtual};
use super::{TerminalHandler, Vt52CursorState};
use crate::terminal::kitty_graphics::{self, KittyAction, KittyCommand, KittyMedium};
use aterm_grid::{
    Grid, ImageData, ImageFormat, ImageRef, ImageScaling, KittyPlacementTag, SourceRect,
};
use std::collections::HashSet;
use std::num::NonZeroU32;
use std::sync::Arc;

impl TerminalHandler<'_> {
    /// Stamp the ECHO ANCHOR: where this print action's run ENDED — one past
    /// the last glyph, so a glyph printed at the last column with the wrap
    /// deferred (the DEC caret parked ON that column, `pending_wrap` set)
    /// reports the column past it (2026-09-12) — plus a monotonic action
    /// count (see `TransientState::print_anchor`). Called at the tail of
    /// every `ActionSink` print path — two stores, a cursor read and a flag
    /// read per print ACTION (bulk runs amortize it over the whole run), so
    /// the hot blast path pays nothing measurable. Observability only: no
    /// parser or grid decision ever reads it back.
    #[inline]
    fn stamp_print_anchor(&mut self) {
        let cursor = self.grid.cursor();
        let col = cursor
            .col
            .saturating_add(u16::from(self.grid.pending_wrap()));
        self.transient.print_anchor = Some((cursor.row, col));
        self.transient.print_anchor_seq = self.transient.print_anchor_seq.wrapping_add(1);
    }
}

impl ActionSink for TerminalHandler<'_> {
    fn print(&mut self, c: char) {
        self.note_sync_open_action();
        // Handle VT52 cursor addressing state
        match self.transient.vt52_cursor_state {
            Vt52CursorState::WaitingRow => {
                // First byte after ESC Y - row (encoded as row + 32)
                let row = (c as u8).saturating_sub(32);
                self.transient.vt52_cursor_state = Vt52CursorState::WaitingCol(row);
                return;
            }
            Vt52CursorState::WaitingCol(row) => {
                // Second byte after ESC Y - column (encoded as col + 32)
                let col = (c as u8).saturating_sub(32);
                self.grid.set_cursor(u16::from(row), u16::from(col));
                self.transient.vt52_cursor_state = Vt52CursorState::None;
                return;
            }
            Vt52CursorState::None => {}
        }

        self.write_char(c);
        self.stamp_print_anchor();
    }

    /// FAST PATH: Print a run of ASCII bytes without per-character overhead.
    ///
    /// This is called by the parser for runs of printable ASCII (0x20-0x7E).
    /// Uses three tiers of optimization:
    ///
    /// 1. Ultra-fast: Default style, autowrap, no insert mode → `write_ascii_blast`
    /// 2. Fast: Styled but no RGB/hyperlinks/insert, autowrap → `write_ascii_run_styled`
    /// 3. Fallback: Per-character `write_char` for complex cases
    fn print_ascii_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        self.note_sync_open_action();
        let data = data.as_ref();
        // Blockers that require per-character processing
        if self.transient.vt52_cursor_state != Vt52CursorState::None {
            // VT52 cursor addressing consumes characters specially
            for &byte in data {
                self.print(byte as char);
            }
            return;
        }

        // Per-character fallback: only for conditions that truly require it.
        // Charset translation, insert mode, and no-autowrap need per-char processing
        // because they change behavior at each character position.
        if !self.charset.is_ascii_passthrough() || self.modes.insert_mode || !self.modes.auto_wrap {
            for &byte in data {
                self.write_char(byte as char);
            }
            self.stamp_print_anchor();
            return;
        }

        // SELECTION CUSTODY — ordinary output damages the rows it OVERWRITES; see
        // `TerminalHandler::write_char`, which brackets the per-character paths this
        // one bypasses (including both fallbacks above). Bracketing the BULK call
        // rather than each glyph is what keeps this off the per-glyph cost design §10
        // rejected: one pair of reads for a whole run, however long. The absolute
        // frame is invariant, so a run that wraps and scrolls mid-flight is still
        // named exactly by its first and last cursor row.
        let origin = self.grid.output_damage_origin();
        // Check if style needs CellExtras overflow (RGB, hyperlinks, etc.).
        // Both flags cached at mutation time — no per-bulk-call overhead.
        if self.style.has_style_extras() || self.transient.has_transient_extras {
            self.write_ascii_bulk_with_extras(data);
        } else {
            self.write_ascii_bulk_fast(data);
        }
        self.grid.damage_selection_output(origin);
        self.stamp_print_anchor();
    }

    /// FAST PATH: Print a run of decoded non-ASCII characters.
    ///
    /// Called by the parser for consecutive multi-byte UTF-8 sequences.
    /// Amortizes per-character overhead (charset, clipboard, style checks)
    /// over the entire run. Falls back to per-character for complex cases.
    fn print_unicode_bulk(&mut self, chars: &Provenance<[char], Pty>) {
        self.note_sync_open_action();
        let chars = chars.as_ref();
        // VT52 cursor addressing consumes characters specially
        if self.transient.vt52_cursor_state != Vt52CursorState::None {
            for &c in chars {
                self.print(c);
            }
            return;
        }

        // SELECTION CUSTODY — as in `print_ascii_bulk`: one bracket for the whole
        // run. `write_unicode_bulk`'s own per-character fallbacks route through
        // `write_char`, which brackets itself; the extra record is idempotent.
        let origin = self.grid.output_damage_origin();
        self.write_unicode_bulk(chars);
        self.grid.damage_selection_output(origin);
        self.stamp_print_anchor();
    }

    /// Execute C0 and C1 control characters.
    ///
    /// Handles single-byte control codes that don't require parameters:
    ///
    /// **C0 codes (0x00-0x1F):**
    /// - **0x07** (BEL): Ring bell (triggers callback)
    /// - **0x08** (BS): Backspace with reverse wraparound support
    /// - **0x09** (HT): Horizontal tab
    /// - **0x0A-0x0C** (LF/VT/FF): Line feed (with optional CR in LNM mode)
    /// - **0x0D** (CR): Carriage return
    /// - **0x0E** (SO): Shift Out - select G1 character set
    /// - **0x0F** (SI): Shift In - select G0 character set
    /// - **0x18/0x1A** (CAN/SUB): Cancel/abort current sequence
    ///
    /// **C1 codes (0x80-0x9F):**
    /// - **0x84** (IND): Index - same as ESC D
    /// - **0x85** (NEL): Next line - same as ESC E
    /// - **0x88** (HTS): Tab set - same as ESC H
    /// - **0x8D** (RI): Reverse index - same as ESC M
    /// - **0x8E/0x8F** (SS2/SS3): Single shift - same as ESC N/O
    fn execute(&mut self, byte: u8) {
        self.note_sync_open_action();
        // Per VT220 spec: a control character arriving mid-sequence cancels
        // any in-progress ESC Y cursor addressing (VT52 mode).
        if self.transient.vt52_cursor_state != Vt52CursorState::None {
            self.transient.vt52_cursor_state = Vt52CursorState::None;
        }

        // Per VT220 spec: SS2/SS3 single-shift is cleared on any control
        // character, not just on the next graphic character.
        self.charset.clear_single_shift();

        match byte {
            // C0 control codes (0x00-0x1F)
            0x07 => self.handle_bell(),
            0x08 => {
                // BS (Backspace)
                // Per VT510: when DECLRMM is active, the "left margin" for BS
                // is the DECLRMM left margin, and reverse wraparound wraps to
                // the right margin (not last column).
                let left_bound = if self.modes.left_right_margin_mode {
                    self.grid.horizontal_margins().left
                } else {
                    0
                };
                if self.grid.cursor_col() <= left_bound && self.modes.reverse_wraparound {
                    let row = self.grid.cursor_row();
                    let top = self.grid.scroll_region().top;
                    let min_row = if row >= top { top } else { 0 };
                    if row > min_row {
                        let wrap_col = if self.modes.left_right_margin_mode {
                            self.grid.horizontal_margins().right
                        } else {
                            self.grid.cols().saturating_sub(1)
                        };
                        self.grid.set_cursor(row - 1, wrap_col);
                    }
                } else if self.modes.grapheme_cluster_mode {
                    // Mode 2027: respect grapheme cluster boundaries
                    self.cursor_state().cursor_backward_graphemes(1);
                } else {
                    self.grid
                        .cursor_backward_margin(1, self.modes.left_right_margin_mode);
                }
            }
            0x09 => {
                // HT (Horizontal Tab)
                self.grid.tab_margin(self.modes.left_right_margin_mode);
            }
            0x0A..=0x0C => {
                // LF, VT, FF
                // In new line mode (LNM), LF also performs CR
                if self.modes.new_line_mode {
                    self.grid
                        .carriage_return_margin(self.modes.left_right_margin_mode);
                }
                // Per VT510: when DECLRMM is active, LF at the scroll boundary
                // scrolls only within horizontal margins (#7407).
                // Line feed, honoring DECLRMM left/right margins (#7687).
                self.margined_line_feed(self.modes.left_right_margin_mode);
            }
            0x0D => {
                // CR (Carriage Return)
                self.grid
                    .carriage_return_margin(self.modes.left_right_margin_mode);
            }
            0x0E => {
                // SO (Shift Out) - invoke G1 into GL
                self.charset.gl = GlMapping::G1;
            }
            0x0F => {
                // SI (Shift In) - invoke G0 into GL
                self.charset.gl = GlMapping::G0;
            }

            // C1 control codes (0x80-0x9F)
            // These are 8-bit equivalents of ESC + character sequences
            0x84 => {
                // IND (Index) - same as ESC D
                // Move cursor down, scroll if at bottom of scroll region
                // Per VT510: when DECLRMM is active, IND at the scroll boundary
                // scrolls only within horizontal margins (#7407).
                // Line feed, honoring DECLRMM left/right margins (#7687).
                self.margined_line_feed(self.modes.left_right_margin_mode);
            }
            0x85 => {
                // NEL (Next Line) - same as ESC E
                // Move cursor to start of next line, scroll if needed
                self.grid
                    .carriage_return_margin(self.modes.left_right_margin_mode);
                // Per VT510: when DECLRMM is active, NEL at the scroll boundary
                // scrolls only within horizontal margins (#7407).
                // Line feed, honoring DECLRMM left/right margins (#7687).
                self.margined_line_feed(self.modes.left_right_margin_mode);
            }
            0x88 => {
                // HTS (Horizontal Tab Set) - same as ESC H
                // Set a tab stop at current column
                self.grid.set_tab_stop();
            }
            0x8D => {
                // RI (Reverse Index) - same as ESC M
                // Move cursor up, scroll down if at top of scroll region
                // Per VT510: when DECLRMM is active, RI at the scroll boundary
                // scrolls only within horizontal margins (#7407).
                self.grid
                    .reverse_line_feed_margined(self.modes.left_right_margin_mode);
            }
            0x8E => {
                // SS2 (Single Shift 2) - same as ESC N
                // Use G2 for next character only
                self.charset.single_shift = SingleShift::Ss2;
            }
            0x8F => {
                // SS3 (Single Shift 3) - same as ESC O
                // Use G3 for next character only
                self.charset.single_shift = SingleShift::Ss3;
            }

            _ => {}
        }
    }

    /// Dispatch CSI (Control Sequence Introducer) escape sequences.
    fn csi_dispatch(
        &mut self,
        params: &Provenance<[u16], Pty>,
        intermediates: &Provenance<[u8], Pty>,
        final_byte: u8,
    ) {
        // Mark BEFORE dispatch. The opening `?2026h` observes the old inactive
        // level and therefore starts clean; a redundant `?2026h` inside an
        // already-dirty window cannot launder the dirty bit.
        self.note_sync_open_action();
        self.sync_absolute_row_metadata();
        let params = params.as_ref();
        let intermediates = intermediates.as_ref();
        // VT52 mode does not recognize CSI sequences — ESC [ is not a valid
        // VT52 escape. Silently ignore any CSI that arrives while in VT52 mode.
        if self.modes.vt52_mode {
            return;
        }
        // Mint a response capability for this dispatch frame. The token is
        // zero-sized and exists only for the duration of this CSI sequence;
        // downstream handlers that may push to the PTY response buffer must
        // receive `&cap` explicitly. See `response_capability.rs` (CF-003).
        //
        // #7994 note: engine consultation for response_capability is
        // performed at the `send_response` sink rather than at mint time —
        // denying the mint would suppress all CSI handling (cursor moves,
        // SGR, etc.), not just the response emission. See
        // `response_capability::mint_for_dispatch_with_engine` for the
        // engine-consulting variant, reserved for contexts that want to
        // short-circuit the whole dispatch. The default dispatch path
        // mints unconditionally and the engine gates individual response
        // sites (see `handler::TerminalHandler::send_response`).
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        // Fast path: no intermediates (vast majority of CSI sequences).
        // Handles SGR, cursor moves, erase, scroll, insert/delete inline —
        // avoids function call chain through csi_dispatch_with_intermediates.
        if intermediates.is_empty() {
            self.csi_dispatch_no_intermediates(&cap, params, final_byte);
            return;
        }
        // Slow path: sequences with intermediates (DEC private, CSI > etc.)
        // Per VT spec, sequences with unrecognized intermediates must be silently
        // ignored — they must NOT fall through to standard CSI handlers, which
        // would misinterpret e.g. CSI # h as CSI h (ANSI set mode).
        let _ = self.csi_dispatch_with_intermediates(&cap, params, intermediates, final_byte);
    }

    /// Handle CSI sequences with subparameter information.
    ///
    /// This is called when the parser detects colon-separated subparameters
    /// (e.g., `ESC[4:3m` for curly underline). The `subparam_mask` indicates
    /// which params were preceded by a colon.
    fn csi_dispatch_with_subparams(
        &mut self,
        params: &Provenance<[u16], Pty>,
        intermediates: &Provenance<[u8], Pty>,
        final_byte: u8,
        subparam_mask: u32,
    ) {
        self.note_sync_open_action();
        if self.modes.vt52_mode {
            return;
        }
        // For SGR (Select Graphic Rendition), handle subparameters specially
        if final_byte == b'm' && intermediates.as_ref().is_empty() {
            self.sgr_style()
                .handle_sgr_with_subparams(params.as_ref(), subparam_mask);
            return;
        }

        // For all other sequences, fall back to normal dispatch
        self.csi_dispatch(params, intermediates, final_byte);
    }

    /// Dispatch ESC (Escape) sequences.
    fn esc_dispatch(&mut self, intermediates: &Provenance<[u8], Pty>, final_byte: u8) {
        self.note_sync_open_action();
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        self.esc_dispatch_core(&cap, intermediates.as_ref(), final_byte);
    }

    /// Dispatch OSC (Operating System Command) escape sequences.
    fn osc_dispatch(&mut self, params: &Provenance<[&[u8]], Pty>) {
        self.note_sync_open_action();
        self.sync_absolute_row_metadata();
        // VT52 mode has no OSC sequences — silently ignore.
        if self.modes.vt52_mode {
            return;
        }
        self.transient.last_osc_bel_terminated = false;
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        self.osc_dispatch_inner(&cap, params.as_ref());
    }

    /// Dispatch OSC with terminator info for response echo (#7548).
    fn osc_dispatch_with_terminator(
        &mut self,
        params: &Provenance<[&[u8]], Pty>,
        bel_terminated: bool,
    ) {
        self.note_sync_open_action();
        self.sync_absolute_row_metadata();
        // VT52 mode has no OSC sequences — silently ignore.
        if self.modes.vt52_mode {
            return;
        }
        self.transient.last_osc_bel_terminated = bel_terminated;
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        self.osc_dispatch_inner(&cap, params.as_ref());
    }

    /// Begin processing a DCS (Device Control String) sequence.
    fn dcs_hook(
        &mut self,
        params: &Provenance<[u16], Pty>,
        intermediates: &Provenance<[u8], Pty>,
        final_byte: u8,
    ) {
        // VT52 mode has no DCS sequences — silently ignore.
        if self.modes.vt52_mode {
            return;
        }
        self.dcs_hook_inner(params.as_ref(), intermediates.as_ref(), final_byte);
    }

    /// Accumulate data bytes for the current DCS sequence.
    fn dcs_put(&mut self, byte: u8) {
        self.dcs_put_inner(byte);
    }

    /// Bulk-accumulate DCS data bytes (the parser's DcsPassthrough fast path
    /// hands over nearly whole PTY chunks): budget accounting, data caps, and
    /// the Sixel pixel-allocation sampling run once per run instead of once
    /// per byte. Parity with per-byte `dcs_put` is documented and tested at
    /// `dcs_put_bulk_inner` (handler_dcs.rs).
    fn dcs_put_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        self.dcs_put_bulk_inner(data.as_ref());
    }

    /// Finalize a DCS sequence after receiving the String Terminator (ST).
    ///
    /// DCS unhook may produce responses for DECRQSS/XTGETTCAP; mint a
    /// capability here so downstream handlers can thread it.
    ///
    /// `canceled` is `true` for a CAN/SUB abort: the Sixel branch then DISCARDS
    /// the half-decoded image rather than rendering it (DECRQSS/XTGETTCAP are
    /// unaffected — their finalization is idempotent on an empty buffer).
    fn dcs_unhook(&mut self, canceled: bool) {
        self.note_sync_open_action();
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        self.dcs_unhook_inner(&cap, canceled);
    }

    fn apc_start(&mut self) {
        // VT52 mode has no APC sequences — silently ignore.
        if self.modes.vt52_mode {
            return;
        }
        // Release global budget from any abandoned prior DCS sequence.
        // Without this, an incomplete DCS (no ST) followed by APC leaks
        // its sequence_bytes permanently, eventually exhausting
        // MAX_DCS_GLOBAL_BUDGET and silently dropping all DCS (#7269).
        self.dcs.total_bytes = self.dcs.total_bytes.saturating_sub(self.dcs.sequence_bytes);
        self.dcs.sequence_bytes = 0;
        // Abort an abandoned Sixel decoder before clearing dcs_type.
        // Uses abort() instead of unhook() to avoid a transient 64MB
        // allocation for a copy that's immediately dropped. (#7453)
        #[cfg(feature = "sixel")]
        if matches!(self.dcs.dcs_type, super::super::DcsType::Sixel) {
            self.sixel.decoder.abort();
        }
        self.dcs.dcs_type = super::super::DcsType::None;
        self.dcs.data.clear(); // Reuse dcs_data buffer for APC
    }

    fn apc_put(&mut self, byte: u8) {
        // Accumulate APC data bytes
        // Limit to prevent DoS (same as OSC limit).
        // Track against global DCS budget so APC memory is visible
        // to the budget system (shares the dcs.data buffer).
        if self.dcs.total_bytes >= super::super::MAX_DCS_GLOBAL_BUDGET {
            return;
        }
        // Always count bytes against the budget, even when the data vec
        // is capped. Otherwise APC flooding past the cap goes untracked
        // and the budget system cannot throttle it.
        self.dcs.total_bytes += 1;
        self.dcs.sequence_bytes += 1;
        // Allow up to 4MB per APC sequence for Kitty graphics (#7688).
        // The global DCS budget (10MB) still caps total memory.
        if self.dcs.data.len() < 4 * 1024 * 1024 {
            self.dcs.data.push(byte);
        }
    }

    fn apc_put_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        // Bulk equivalent of `apc_put` over a contiguous run, doing the budget
        // accounting ONCE and a single `extend_from_slice`. This MUST stay
        // byte-identical to calling `apc_put` per byte (see the
        // `apc_put_bulk_crosses_caps_parity` test):
        //
        //  - The per-byte path stops the instant total_bytes >= the global
        //    budget, so at most `MAX_DCS_GLOBAL_BUDGET - total_bytes` of this
        //    run are ever counted; bytes past that are neither counted nor
        //    pushed.
        //  - Of the counted bytes, the per-byte path pushes byte k iff
        //    `data.len() + k < 4 MiB`, i.e. exactly the leading prefix until the
        //    per-sequence cap is reached.
        let data = data.as_ref();
        let countable = data
            .len()
            .min(super::super::MAX_DCS_GLOBAL_BUDGET.saturating_sub(self.dcs.total_bytes));
        if countable == 0 {
            return;
        }
        self.dcs.total_bytes += countable;
        self.dcs.sequence_bytes += countable;
        let push_len = countable.min((4usize * 1024 * 1024).saturating_sub(self.dcs.data.len()));
        if push_len > 0 {
            self.dcs.data.extend_from_slice(&data[..push_len]);
        }
    }

    fn apc_end(&mut self) {
        self.note_sync_open_action();
        // Kitty graphics (APC 'G'): parse the accumulated payload and handle it
        // (transmit/store, transmit-and-display, put, delete). `parse_kitty_command`
        // returns an OWNED command (payload cloned), so the borrow on `dcs.data` is
        // released before `handle_kitty_command` mutates the grid/store.
        let cmd = if self.dcs.data.first() == Some(&b'G') {
            crate::terminal::kitty_graphics::parse_kitty_command(&self.dcs.data)
        } else {
            None
        };
        if let Some(cmd) = cmd {
            self.handle_kitty_command(cmd);
        }
        // Release APC bytes from the global DCS budget.
        self.dcs.total_bytes = self.dcs.total_bytes.saturating_sub(self.dcs.sequence_bytes);
        self.dcs.sequence_bytes = 0;
        // Clear the buffer and reclaim memory from large APC payloads
        // (same policy as DCS unhook and OSC dispatch — see #7272).
        self.dcs.data.clear();
        if self.dcs.data.capacity() > 4096 {
            self.dcs.data.shrink_to(128);
        }
    }
}

/// Kitty graphics (APC `G`) command handling. An inherent impl (not part of
/// `ActionSink`); called from `apc_end` above.
impl TerminalHandler<'_> {
    /// Handle one parsed Kitty graphics command, assembling CHUNKED
    /// transmissions (`m=1`) first; the per-action handling is
    /// `handle_complete_kitty_command`, which also lists what is not
    /// implemented. The first `m=1` chunk seeds the pending command (moved in
    /// whole, payload included); continuation chunks append their payload; the
    /// `m=0` chunk finalizes and dispatches the whole image. Non-chunked
    /// commands dispatch immediately. The accumulated payload is bounded by
    /// `MAX_KITTY_IMAGE_BYTES` (overflow aborts the transfer). Takes the command
    /// BY VALUE so the assembled payload can flow into the image store without a
    /// multi-MiB copy.
    fn handle_kitty_command(&mut self, cmd: KittyCommand) {
        if self.transient.kitty_pending.is_some() || cmd.more {
            // Bound the assembled payload BEFORE appending (read current len first
            // to avoid borrowing across the abort reset).
            let cur_len = self
                .transient
                .kitty_pending
                .as_ref()
                .map_or(0, |p| p.payload.len());
            // Fail closed BEFORE touching the buffer: overflow past the cap, or an
            // armed alloc fault (M7 FAULT-INJECT), aborts the transfer.
            if crate::fault::triggered("kitty.chunk_alloc")
                || cur_len.saturating_add(cmd.payload.len()) > MAX_KITTY_IMAGE_BYTES
            {
                self.transient.kitty_pending = None; // abort the overflowing transfer
                return;
            }
            let Some(pending) = self.transient.kitty_pending.as_mut() else {
                // FIRST chunk (`cmd.more` is true here): seed the accumulator
                // with the owned command — metadata AND payload move in, with
                // no metadata clone and no re-append copy of the first chunk.
                self.transient.kitty_pending = Some(cmd);
                return;
            };
            // Continuation chunk. FALLIBLE ALLOCATION (M7): reserve before
            // extending so a real OOM degrades to a dropped transfer instead of
            // aborting the process.
            if pending.payload.try_reserve(cmd.payload.len()).is_ok() {
                pending.payload.extend_from_slice(&cmd.payload);
            } else {
                self.transient.kitty_pending = None; // OOM: drop the transfer, fail closed
                return;
            }
            if cmd.more {
                return; // more chunks to come
            }
            // Final chunk: take the assembled command out and dispatch it.
            if let Some(assembled) = self.transient.kitty_pending.take() {
                self.handle_complete_kitty_command(assembled);
            }
            return;
        }
        self.handle_complete_kitty_command(cmd);
    }

    /// Handle one COMPLETE (chunk-assembled) Kitty graphics command.
    ///
    /// What the engine does, by action:
    ///
    ///   * `a=t` / `a=T` transmit — decode, store by `i=` (or by an id the
    ///     terminal assigns an `I=` number), and for `a=T` show it as a put
    ///     would;
    ///   * `a=p` put — show a stored image laid out by the put's own
    ///     `c=`/`r=`/`z=`/`x=`/`y=`/`w=`/`h=` (only one of `c=`/`r=` keeps the
    ///     image's aspect): at the cursor (kitty's cursor policy, `C=1` to keep
    ///     it); as a VIRTUAL placement for `U=1`, drawn where the client prints
    ///     Unicode placeholders, which name it by image id (foreground colour)
    ///     and placement id (underline colour); or RELATIVE to a parent
    ///     placement for `P=`/`Q=`, `H=`/`V=` cells from the parent's top-left,
    ///     moving with the parent and deleted with it. A placement id (`p=`)
    ///     names one placement of an image, so a later put with the same pair
    ///     MOVES it;
    ///   * `a=d` delete — the placements a selector addresses (only the
    ///     id-addressed selectors reach virtual placements), their data too
    ///     under an uppercase selector; `d=f`/`F` one animation frame;
    ///   * `a=q` query — answered per medium availability;
    ///   * `a=f` frame — load frame data: a new frame, or frame `r=` edited,
    ///     composed at `x=`/`y=` onto frame `c=`, the edited frame or the
    ///     background colour `Y=`, alpha-blended unless `X=1`;
    ///   * `a=c` compose — a rectangle of frame `r=` onto frame `c=`;
    ///   * `a=a` animation control — `c=` makes a frame the current one,
    ///     everywhere the image shows.
    ///
    /// Transmits, puts, frames and compositions that name an `i=`/`I=` are
    /// answered as kitty answers them (`OK`, or an error), subject to `q=`.
    /// The non-direct mediums work only when the host installs the opt-in
    /// resolver.
    ///
    /// Not built, each with what it does instead: animation PLAYBACK (`a=a`
    /// `s=`, frame gaps and loop counts are accepted and ignored — a frame
    /// changes only when `c=` selects it, since nothing in the engine advances
    /// frames on a clock); composition onto a PNG frame or of PNG frame data
    /// (answered `ENOTSUPPORTED`: the engine carries no image codec, so it
    /// composes raw `f=24`/`f=32` pixels only); a placement relative to a
    /// VIRTUAL placement (answered `ENOTSUPPORTED`: that parent sits wherever
    /// its placeholders are printed, which moves with the text, while a
    /// placement is stamped into cells); in-cell pixel offsets (`X=`/`Y=` on a
    /// put are ignored, so the image starts at its cell's corner, less than a
    /// cell from where kitty starts it); and ordering images against EACH
    /// OTHER (a cell holds one image, so a later placement wins the cells it
    /// covers — `z<0` orders an image only against text). A `C=1` placement is
    /// clipped at the screen's last row instead of hanging below it.
    fn handle_complete_kitty_command(&mut self, cmd: KittyCommand) {
        match cmd.action {
            KittyAction::Delete => self.delete_kitty_placements(&cmd),
            KittyAction::Display => self.put_kitty(&cmd),
            KittyAction::Transmit | KittyAction::TransmitAndDisplay => self.transmit_kitty(cmd),
            // Support probe: report OK (we support core transmit/display). The
            // success response is suppressed by q>=1 — that Query then falls to
            // the arm below (no response). Echo the id (i=) or number (I=) the
            // client used.
            KittyAction::Query if cmd.quiet == 0 => self.answer_kitty_query(&cmd),
            KittyAction::Query => {}
            KittyAction::Frame => self.load_kitty_frame(cmd),
            KittyAction::Compose => self.compose_kitty_frames(&cmd),
            KittyAction::Animate => self.control_kitty_animation(&cmd),
        }
    }

    /// `a=p`: show a stored image (by `i=` or `I=`), laid out by the put's
    /// own keys ([`show_kitty`](Self::show_kitty)).
    fn put_kitty(&mut self, cmd: &KittyCommand) {
        let Some(id) = self.resolve_kitty_id(cmd) else {
            self.kitty_reply(cmd, None, "ENOENT:no such image");
            return;
        };
        let Some(stored) = self.transient.kitty_images.get(&id).cloned() else {
            self.kitty_reply(cmd, Some(id), "ENOENT:no such image");
            return;
        };
        let verdict = match self
            .kitty_placement_image(id, &stored, cmd)
            .and_then(|image| self.show_kitty(&image, id, cmd))
        {
            Ok(()) => "OK",
            Err(error) => error,
        };
        self.kitty_reply(cmd, Some(id), verdict);
    }

    /// Show `image`, image `id` laid out for this command, as the command
    /// asks: a virtual placement for `U=1`, relative to a parent for `P=`,
    /// else at the cursor.
    fn show_kitty(
        &mut self,
        image: &Arc<ImageData>,
        id: u32,
        cmd: &KittyCommand,
    ) -> Result<(), &'static str> {
        if cmd.virtual_placement {
            if cmd.parent_id.is_some() {
                return Err("EINVAL:a virtual placement cannot be relative");
            }
            self.add_kitty_virtual(id, cmd.placement.unwrap_or(0), image);
            return Ok(());
        }
        match cmd.parent_id {
            Some(parent) => self.place_kitty_relative(image, id, cmd, parent),
            None => {
                self.place_kitty(image, id, cmd);
                Ok(())
            }
        }
    }

    /// `a=t` / `a=T`: decode, store under `i=` (or an id the terminal assigns
    /// an `I=` number), and for `a=T` show it as a put would.
    fn transmit_kitty(&mut self, mut cmd: KittyCommand) {
        if cmd.id.is_some() && cmd.number.is_some() {
            self.kitty_reply(&cmd, None, "EINVAL:i and I are exclusive");
            return;
        }
        if !self.kitty_medium_works(cmd.medium) {
            // The same verdict the `a=q` probe gives this medium: the data was
            // never read, so "bad image data" would name the wrong cause.
            self.kitty_reply(&cmd, cmd.id, KITTY_MEDIUM_DISABLED);
            return;
        }
        let Some((image, crop_ok)) = self.build_kitty_image(&mut cmd) else {
            self.kitty_reply(&cmd, cmd.id, "EINVAL:bad image data");
            return;
        };
        let image = Arc::new(image);
        // `I=` alone: the terminal picks the id and reports it.
        let id = cmd.id.or_else(|| cmd.number.map(|_| self.fresh_kitty_id()));
        let mut verdict = "OK";
        if let Some(id) = id {
            if self.store_kitty_image(id, &image) {
                if let Some(number) = cmd.number {
                    self.transient.kitty_numbers.insert(number, id);
                }
            } else {
                verdict = "ENOSPC:image store full";
            }
        }
        if cmd.action == KittyAction::TransmitAndDisplay {
            let shown = if !crop_ok {
                Err("EINVAL:source rectangle outside the image")
            } else if cmd.virtual_placement && verdict != "OK" {
                // The store refused the image: no placeholder could draw it.
                Ok(())
            } else {
                // Shown even when the store refused it: the pixels are in
                // hand, only a later put by id cannot find them.
                self.show_kitty(&image, id.unwrap_or(0), &cmd)
            };
            if let Err(error) = shown
                && verdict == "OK"
            {
                verdict = error;
            }
        }
        self.kitty_reply(&cmd, id, verdict);
    }

    /// `a=q` (not quieted): the support probe.
    fn answer_kitty_query(&mut self, cmd: &KittyCommand) {
        use core::fmt::Write as _;
        // Answer the probe HONESTLY per the queried medium. Clients ask
        // `a=q` before committing to a transmission strategy (kitty's
        // icat probes `t=f` and falls back to direct on an error
        // reply), and this arm used to say OK unconditionally — so on
        // a session where the non-direct resolver was never installed
        // (`allow_kitty_file_transfer` is opt-in, default off) the
        // prober was told file/shm transfer works, and its real
        // transmits then failed as a SILENT fail-closed skip: an
        // advertised capability that drops every payload. Direct is
        // always real; the rest are exactly as real as the resolver.
        let verdict = if self.kitty_medium_works(cmd.medium) {
            "OK"
        } else {
            KITTY_MEDIUM_DISABLED
        };
        let mut r = crate::terminal::stack_response::StackResponse::<96>::new();
        if let Some(id) = cmd.id {
            let _ = write!(r, "\x1b_Gi={id};{verdict}\x1b\\");
        } else if let Some(n) = cmd.number {
            let _ = write!(r, "\x1b_GI={n};{verdict}\x1b\\");
        } else {
            let _ = write!(r, "\x1b_G;{verdict}\x1b\\");
        }
        // Route the reply through the single response sink (like every
        // other terminal response) so it is gated by the response
        // capability, the rate limiter and the buffer cap.
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        self.send_response(&cap, r.as_bytes());
    }

    /// Whether this session can read a transmission sent over `medium`: direct
    /// always, the file / temp-file / shared-memory mediums only when the host
    /// installed the opt-in resolver.
    fn kitty_medium_works(&self, medium: KittyMedium) -> bool {
        medium == KittyMedium::Direct || self.kitty_file_resolver.is_some()
    }

    /// The image id a command addresses: its `i=`, else the id of the newest
    /// image transmitted under its `I=` number.
    fn resolve_kitty_id(&self, cmd: &KittyCommand) -> Option<u32> {
        cmd.id.or_else(|| {
            cmd.number
                .and_then(|number| self.transient.kitty_numbers.get(&number).copied())
        })
    }

    /// A fresh id for an `I=` transmission: the next one past the last
    /// assignment that no stored image uses (0 is never an id).
    fn fresh_kitty_id(&mut self) -> u32 {
        let mut id = self.transient.kitty_last_assigned_id;
        loop {
            id = id.wrapping_add(1).max(1);
            if !self.transient.kitty_images.contains_key(&id) {
                break;
            }
        }
        self.transient.kitty_last_assigned_id = id;
        id
    }

    /// Answer a transmit / put / composition the way kitty does: only when the
    /// command named an image (`i=` or `I=`), `OK` unless `q>=1`, an error
    /// unless `q>=2`. `id` is the image the command resolved to, if any. A
    /// delete or an animation control is never answered.
    fn kitty_reply(&mut self, cmd: &KittyCommand, id: Option<u32>, verdict: &str) {
        self.kitty_reply_naming(cmd, id, None, verdict);
    }

    /// [`kitty_reply`](Self::kitty_reply) that also names a frame (`r=`), as
    /// kitty's answer to an `a=f` frame load does.
    fn kitty_reply_naming(
        &mut self,
        cmd: &KittyCommand,
        id: Option<u32>,
        frame: Option<usize>,
        verdict: &str,
    ) {
        use core::fmt::Write as _;
        let ok = verdict == "OK";
        if (cmd.id.is_none() && cmd.number.is_none()) || cmd.quiet >= 2 || (ok && cmd.quiet >= 1) {
            return;
        }
        let mut r = crate::terminal::stack_response::StackResponse::<128>::new();
        let _ = r.write_str("\x1b_G");
        let mut sep = "";
        if let Some(id) = id.or(cmd.id) {
            let _ = write!(r, "i={id}");
            sep = ",";
        }
        if let Some(number) = cmd.number {
            let _ = write!(r, "{sep}I={number}");
            sep = ",";
        }
        if let Some(placement) = cmd.placement {
            let _ = write!(r, "{sep}p={placement}");
            sep = ",";
        }
        if let Some(frame) = frame {
            let _ = write!(r, "{sep}r={frame}");
        }
        let _ = write!(r, ";{verdict}\x1b\\");
        // The single response sink, like the query arm (see there).
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        self.send_response(&cap, r.as_bytes());
    }

    /// Store `image` under `id` in the image store, returning whether it fit.
    ///
    /// A base transmit stores the frame in TWO slots (`kitty_images[id]` and
    /// `kitty_frames[id][0]`), each charged to the global byte budget. The store
    /// is capped by count (`MAX_KITTY_IMAGES`, a DoS bound; an existing id may
    /// always update) and by the GLOBAL budget (`MAX_KITTY_STORE_BYTES`,
    /// fail-closed), so the per-item caps can't multiply. Re-transmitting an id
    /// replaces its data and, as in kitty, REMOVES its placements — on screen
    /// (their pixels are the old image's) and virtual — with their relative
    /// placements, and its re-laid-out variants.
    fn store_kitty_image(&mut self, id: u32, image: &Arc<ImageData>) -> bool {
        let replacing = self.transient.kitty_images.contains_key(&id);
        let add = image.bytes.len().saturating_mul(2);
        let projected = self
            .transient
            .kitty_total_bytes
            .saturating_sub(self.kitty_bytes_held(id))
            .saturating_add(add);
        if !((self.transient.kitty_images.len() < MAX_KITTY_IMAGES || replacing)
            && projected <= MAX_KITTY_STORE_BYTES)
        {
            return false;
        }
        if replacing {
            self.transient.kitty_virtual.remove(&id);
            self.delete_kitty_where(&|tag, _| tag.image_id == id);
            // The delete may have freed the image already (a relative
            // placement of it lost its parent); whatever is left goes now.
            let held = self.kitty_bytes_held(id);
            let t = &mut *self.transient;
            t.kitty_variants.remove(&id);
            t.kitty_total_bytes = t.kitty_total_bytes.saturating_sub(held);
        }
        let t = &mut *self.transient;
        t.kitty_images.insert(id, Arc::clone(image));
        // A fresh base transmit resets the animation frame list to just this
        // frame (frame 1); `a=f` adds or edits frames, `a=a c=N` selects one.
        t.kitty_frames.insert(id, vec![Arc::clone(image)]);
        t.kitty_total_bytes = t.kitty_total_bytes.saturating_add(add);
        self.damage_kitty_placeholders(id);
        true
    }

    /// Bytes the store holds for `id`: its image slot, its frames and its
    /// placement variants (each slot counted, as the budget counts them).
    fn kitty_bytes_held(&self, id: u32) -> usize {
        let t = &self.transient;
        t.kitty_images.get(&id).map_or(0, |img| img.bytes.len())
            + t.kitty_frames.get(&id).map_or(0, |frames| {
                frames.iter().map(|f| f.bytes.len()).sum::<usize>()
            })
            + t.kitty_variants.get(&id).map_or(0, |vs| {
                vs.iter().map(|v| v.image.bytes.len()).sum::<usize>()
            })
    }

    /// Free everything the store holds for `id` — image, frames, variants and
    /// virtual placements — returning their bytes to the budget and forgetting
    /// every number that named it. Placements already on screen keep their own
    /// `Arc`s.
    fn free_kitty_image(&mut self, id: u32) {
        let held = self.kitty_bytes_held(id);
        let t = &mut *self.transient;
        t.kitty_images.remove(&id);
        t.kitty_frames.remove(&id);
        t.kitty_variants.remove(&id);
        t.kitty_virtual.remove(&id);
        t.kitty_numbers.retain(|_, named| *named != id);
        t.kitty_total_bytes = t.kitty_total_bytes.saturating_sub(held);
        self.damage_kitty_placeholders(id);
    }

    /// The image a put displays: the stored image itself when the put's layout
    /// (footprint, z-index, scaling, source rectangle) is the stored one's, else
    /// a re-laid-out VARIANT of it ([`kitty_variant`](Self::kitty_variant)).
    fn kitty_placement_image(
        &mut self,
        id: u32,
        stored: &Arc<ImageData>,
        cmd: &KittyCommand,
    ) -> Result<Arc<ImageData>, &'static str> {
        let (px_w, px_h) = raster_size(stored).ok_or("EINVAL:image has no pixel size")?;
        let layout = self
            .kitty_layout(cmd, px_w, px_h)
            .ok_or("EINVAL:source rectangle outside the image")?;
        self.kitty_variant(id, stored, layout)
    }

    /// `stored` (image `id`, or one of its frames) laid out as `layout`: itself
    /// when that is its layout, else a re-laid-out VARIANT — a footprint is part
    /// of the payload the renderer decodes, so a different one is a different
    /// `ImageData`.
    ///
    /// A variant copies the image's bytes, so it is charged to the global budget
    /// and capped per id (`MAX_KITTY_VARIANTS`); variants that nothing but the
    /// store still holds (no cell, no history row, no virtual placement, no
    /// frame in flight) are dropped before a new one is admitted, so a client
    /// that re-crops on every scroll (image viewers do) recycles rather than
    /// exhausts them. Over either bound it fails closed with `ENOSPC`.
    fn kitty_variant(
        &mut self,
        id: u32,
        stored: &Arc<ImageData>,
        layout: KittyLayout,
    ) -> Result<Arc<ImageData>, &'static str> {
        if layout == KittyLayout::of(stored) {
            return Ok(Arc::clone(stored));
        }
        let variants = self.transient.kitty_variants.entry(id).or_default();
        if let Some(variant) = variants
            .iter()
            .find(|v| Arc::ptr_eq(&v.source, stored) && layout == KittyLayout::of(&v.image))
        {
            return Ok(Arc::clone(&variant.image));
        }
        let mut released = 0usize;
        variants.retain(|v| {
            let live = Arc::strong_count(&v.image) > 1;
            if !live {
                released += v.image.bytes.len();
            }
            live
        });
        let count = variants.len();
        self.transient.kitty_total_bytes =
            self.transient.kitty_total_bytes.saturating_sub(released);
        let add = stored.bytes.len();
        if count >= MAX_KITTY_VARIANTS
            || self.transient.kitty_total_bytes.saturating_add(add) > MAX_KITTY_STORE_BYTES
        {
            return Err("ENOSPC:image store full");
        }
        // FALLIBLE ALLOCATION (M7): an OOM fails the put, never the process.
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(add)
            .map_err(|_| "ENOSPC:out of memory")?;
        bytes.extend_from_slice(&stored.bytes);
        let variant = Arc::new(layout.apply(ImageData {
            bytes,
            format: stored.format,
            cols: 0,
            rows: 0,
            z_index: 0,
            band_lift_px: 0,
            scaling: ImageScaling::Fit,
            source_rect: None,
        }));
        self.transient
            .kitty_variants
            .entry(id)
            .or_default()
            .push(KittyVariant {
                source: Arc::clone(stored),
                image: Arc::clone(&variant),
            });
        self.transient.kitty_total_bytes += add;
        Ok(variant)
    }

    /// A fresh tag for one placement act of image `image_id` under
    /// `placement_id`.
    fn next_kitty_tag(&mut self, image_id: u32, placement_id: u32) -> KittyPlacementTag {
        let serial = self.transient.kitty_placement_serial.wrapping_add(1).max(1);
        self.transient.kitty_placement_serial = serial;
        KittyPlacementTag {
            image_id,
            placement_id,
            serial: NonZeroU32::new(serial).unwrap_or(NonZeroU32::MIN),
        }
    }

    /// Stamp a Kitty placement of `image` (image id `id`, `0` for an anonymous
    /// transmit-and-display) at the cursor, under the command's placement id
    /// and cursor policy. A non-zero `p=` names ONE placement of the image, so
    /// displaying it again MOVES it — its relative placements with it — and a
    /// placement put again without `P=` is no longer relative.
    fn place_kitty(&mut self, image: &Arc<ImageData>, id: u32, cmd: &KittyCommand) {
        let placement_id = cmd.placement.unwrap_or(0);
        let moved = self.lift_kitty_placement(id, placement_id);
        let tag = self.next_kitty_tag(id, placement_id);
        let cursor = if cmd.cursor_stays {
            PlacementCursor::Unmoved
        } else {
            PlacementCursor::AfterImage
        };
        let at = self.grid.cursor_col();
        self.place_image_as(image, image.cols, image.rows, at, Some(tag), cursor);
        if moved {
            self.restamp_kitty_children(tag, 0);
        }
    }

    /// Take placement `placement_id` of image `id` off the screen to put it
    /// again, WITHOUT deleting its relative placements (they follow it), and
    /// forget whether it was relative. Returns whether it was on screen;
    /// `placement_id == 0` names no single placement, so nothing moves.
    fn lift_kitty_placement(&mut self, id: u32, placement_id: u32) -> bool {
        if placement_id == 0 {
            return false;
        }
        self.transient
            .kitty_relations
            .retain(|r| !(r.child.image_id == id && r.child.placement_id == placement_id));
        !self
            .clear_kitty_placements(&|tag, _| {
                tag.image_id == id && tag.placement_id == placement_id
            })
            .is_empty()
    }

    /// `P=`/`Q=`: stamp `image` (image `id`) relative to a parent placement —
    /// the newest placement of image `parent_image` on screen, the one with
    /// placement id `Q=` when given — `H=` columns and `V=` rows from the
    /// parent's top-left cell, keeping the cells on screen. The cursor never
    /// moves: the position is the parent's, not the cursor's. Refused as kitty
    /// refuses it: `ENOPARENT` when the parent is not on screen, `ECYCLE` when
    /// the placement would be its own ancestor, `ETOODEEP` past
    /// `MAX_KITTY_RELATIVE_DEPTH` parents.
    fn place_kitty_relative(
        &mut self,
        image: &Arc<ImageData>,
        id: u32,
        cmd: &KittyCommand,
        parent_image: u32,
    ) -> Result<(), &'static str> {
        let placement_id = cmd.placement.unwrap_or(0);
        let wanted = cmd.parent_placement;
        let parent = self.newest_kitty_placement(&|tag| {
            tag.image_id == parent_image && wanted.is_none_or(|q| tag.placement_id == q)
        });
        let Some((parent, (top, left))) =
            parent.and_then(|parent| Some((parent, self.kitty_origin(parent.serial)?)))
        else {
            let virtual_parent =
                self.transient
                    .kitty_virtual
                    .get(&parent_image)
                    .is_some_and(|list| {
                        list.iter()
                            .any(|v| wanted.is_none_or(|q| v.placement_id == q))
                    });
            return Err(if virtual_parent {
                "ENOTSUPPORTED:a virtual placement cannot be a parent"
            } else {
                "ENOPARENT:no such parent placement"
            });
        };
        self.check_kitty_ancestry(parent, id, placement_id)?;
        let (dx, dy) = (cmd.parent_dx.unwrap_or(0), cmd.parent_dy.unwrap_or(0));
        let moved = self.lift_kitty_placement(id, placement_id);
        let tag = self.next_kitty_tag(id, placement_id);
        self.stamp_kitty_at(image, top.saturating_add(dy), left.saturating_add(dx), tag);
        self.prune_kitty_relations();
        self.transient.kitty_relations.push(KittyRelation {
            child: tag,
            parent: KittyParent::of(parent),
            dx,
            dy,
        });
        if moved {
            self.restamp_kitty_children(tag, 0);
        }
        Ok(())
    }

    /// Refuse a relative placement of `(id, placement_id)` under `parent` that
    /// would be its own ancestor (`ECYCLE`: only a placement with an id can be
    /// put again, so only such a one can close a cycle) or sit under more than
    /// `MAX_KITTY_RELATIVE_DEPTH` parents (`ETOODEEP`).
    fn check_kitty_ancestry(
        &self,
        parent: KittyPlacementTag,
        id: u32,
        placement_id: u32,
    ) -> Result<(), &'static str> {
        let mut at = Some(parent);
        let mut depth = 0usize;
        while let Some(tag) = at {
            if placement_id != 0 && tag.image_id == id && tag.placement_id == placement_id {
                return Err("ECYCLE:the placement would be its own ancestor");
            }
            depth += 1;
            if depth > MAX_KITTY_RELATIVE_DEPTH {
                return Err("ETOODEEP:relative placements nest too deeply");
            }
            at = self
                .transient
                .kitty_relations
                .iter()
                .find(|r| r.child.serial == tag.serial)
                .and_then(|r| {
                    let up = r.parent;
                    self.newest_kitty_placement(&|t| up.names(t))
                });
        }
        Ok(())
    }

    /// The newest (highest-serial) Kitty placement on screen `selected`
    /// accepts.
    fn newest_kitty_placement(
        &self,
        selected: &dyn Fn(&KittyPlacementTag) -> bool,
    ) -> Option<KittyPlacementTag> {
        let mut newest: Option<KittyPlacementTag> = None;
        for row in 0..self.grid.rows() {
            for col in 0..self.grid.cols() {
                if let Some(tag) = self
                    .grid
                    .cell_extra(row, col)
                    .and_then(|extra| extra.image())
                    .and_then(|placed| placed.kitty)
                    && selected(&tag)
                    && newest.is_none_or(|n| tag.serial > n.serial)
                {
                    newest = Some(tag);
                }
            }
        }
        newest
    }

    /// The cell of placement `serial`'s top-left tile — `(row, col)`, off
    /// screen (negative) when that tile scrolled away — or `None` when no cell
    /// on screen shows it.
    fn kitty_origin(&self, serial: NonZeroU32) -> Option<(i32, i32)> {
        let (placed, row, col) = self.kitty_placed(serial)?;
        Some((
            i32::from(row) - i32::from(placed.cell_row),
            i32::from(col) - i32::from(placed.cell_col),
        ))
    }

    /// A cell on screen showing placement `serial`: its image reference and
    /// where it is.
    fn kitty_placed(&self, serial: NonZeroU32) -> Option<(ImageRef, u16, u16)> {
        (0..self.grid.rows()).find_map(|row| {
            (0..self.grid.cols()).find_map(|col| {
                self.grid
                    .cell_extra(row, col)
                    .and_then(|extra| extra.image())
                    .filter(|placed| placed.kitty.is_some_and(|tag| tag.serial == serial))
                    .map(|placed| (placed.clone(), row, col))
            })
        })
    }

    /// Stamp `image`'s footprint with its top-left tile at `(top, left)` —
    /// which may lie off screen — onto the cells that are on screen, damaging
    /// each.
    fn stamp_kitty_at(
        &mut self,
        image: &Arc<ImageData>,
        top: i32,
        left: i32,
        tag: KittyPlacementTag,
    ) {
        for cell_row in 0..image.rows {
            let Ok(row) = u16::try_from(top.saturating_add(i32::from(cell_row))) else {
                continue;
            };
            if row >= self.grid.rows() {
                break;
            }
            for cell_col in 0..image.cols {
                let Ok(col) = u16::try_from(left.saturating_add(i32::from(cell_col))) else {
                    continue;
                };
                if col >= self.grid.cols() {
                    break;
                }
                self.grid.set_cell_image(
                    row,
                    col,
                    ImageRef {
                        image: Arc::clone(image),
                        cell_row,
                        cell_col,
                        kitty: Some(tag),
                    },
                );
                self.grid.damage_mut().mark_cell(row, col);
            }
        }
    }

    /// Move every relative placement of `parent` — and theirs, depth-first —
    /// to its offset from the parent's CURRENT top-left. A child no longer on
    /// screen stays gone.
    fn restamp_kitty_children(&mut self, parent: KittyPlacementTag, depth: usize) {
        if depth >= MAX_KITTY_RELATIVE_DEPTH {
            return;
        }
        let Some((top, left)) = self.kitty_origin(parent.serial) else {
            return;
        };
        let children: Vec<KittyRelation> = self
            .transient
            .kitty_relations
            .iter()
            .filter(|r| r.parent.names(&parent))
            .copied()
            .collect();
        for child in children {
            let Some((placed, _, _)) = self.kitty_placed(child.child.serial) else {
                continue;
            };
            let serial = child.child.serial;
            self.clear_kitty_placements(&|tag, _| tag.serial == serial);
            self.stamp_kitty_at(
                &placed.image,
                top.saturating_add(child.dy),
                left.saturating_add(child.dx),
                child.child,
            );
            self.restamp_kitty_children(child.child, depth + 1);
        }
    }

    /// Forget the relative placements no cell on screen shows any more
    /// (scrolled away, or overwritten by text).
    fn prune_kitty_relations(&mut self) {
        if self.transient.kitty_relations.is_empty() {
            return;
        }
        let mut on_screen = HashSet::new();
        for row in 0..self.grid.rows() {
            for col in 0..self.grid.cols() {
                if let Some(tag) = self
                    .grid
                    .cell_extra(row, col)
                    .and_then(|extra| extra.image())
                    .and_then(|placed| placed.kitty)
                {
                    on_screen.insert(tag.serial);
                }
            }
        }
        self.transient
            .kitty_relations
            .retain(|r| on_screen.contains(&r.child.serial));
    }

    /// Record a VIRTUAL placement (`U=1`) of image `id`: `image` (the stored
    /// image, or a variant laid out for the put) is what a Unicode placeholder
    /// naming it draws. A placement id names one virtual placement, so putting
    /// it again replaces it; unnamed ones accumulate, newest last, up to
    /// `MAX_KITTY_VIRTUALS` per image. An image the store does not hold gets
    /// none: no placeholder could draw it.
    fn add_kitty_virtual(&mut self, id: u32, placement_id: u32, image: &Arc<ImageData>) {
        if !self.transient.kitty_images.contains_key(&id) {
            return;
        }
        let list = self.transient.kitty_virtual.entry(id).or_default();
        if placement_id != 0 {
            list.retain(|v| v.placement_id != placement_id);
        }
        if list.len() >= MAX_KITTY_VIRTUALS {
            list.remove(0);
        }
        list.push(KittyVirtual {
            placement_id,
            image: Arc::clone(image),
        });
        self.damage_kitty_placeholders(id);
    }

    /// Remove image `id`'s virtual placement `placement` — every one of them
    /// for `None`.
    fn remove_kitty_virtual(&mut self, id: u32, placement: Option<u32>) {
        let Some(list) = self.transient.kitty_virtual.get_mut(&id) else {
            return;
        };
        list.retain(|v| placement.is_some_and(|p| v.placement_id != p));
        if list.is_empty() {
            self.transient.kitty_virtual.remove(&id);
        }
        self.damage_kitty_placeholders(id);
    }

    /// Damage every Unicode placeholder cell on screen that names image `id`:
    /// what it draws just changed. Placeholders are text, so nothing else
    /// would repaint them.
    fn damage_kitty_placeholders(&mut self, id: u32) {
        let grid: &Grid = self.grid;
        let (rows, cols) = (grid.rows(), grid.cols());
        let cells: Vec<(u16, u16)> = grid
            .extras()
            .iter()
            .filter(|(at, extra)| {
                at.row < rows
                    && at.col < cols
                    && super::super::kitty_placeholder::decode_cell(grid, at.row, at.col, extra)
                        .is_some_and(|cell| cell.image_id == id)
            })
            .map(|(at, _)| (at.row, at.col))
            .collect();
        for (row, col) in cells {
            self.grid.damage_mut().mark_cell(row, col);
        }
    }

    /// Clear every VISIBLE Kitty placement whose tag `selected` accepts,
    /// damaging each cleared cell so the repaint erases it, and return the tags
    /// of the placements it cleared — nothing else: their relative placements
    /// stay ([`delete_kitty_where`](Self::delete_kitty_where) takes them too).
    ///
    /// Placements are `ImageRef`s stamped into cell extras, each carrying its
    /// placement's [`KittyPlacementTag`]; an image another protocol drew
    /// carries none and is never touched. The read side uses the
    /// non-allocating [`aterm_grid::Grid::cell_extra`]; `cell_extra_mut` is
    /// touched only for cells that actually match, so a sweep over a grid with
    /// no images allocates nothing. Rows already scrolled into scrollback keep
    /// their pixels — kitty deletion addresses the screen, and scrolled-away
    /// placements age out with their rows.
    fn clear_kitty_placements(
        &mut self,
        selected: &dyn Fn(&KittyPlacementTag, &ImageData) -> bool,
    ) -> Vec<KittyPlacementTag> {
        let mut cleared = Vec::new();
        let mut seen = HashSet::new();
        for row in 0..self.grid.rows() {
            for col in 0..self.grid.cols() {
                let hit = self
                    .grid
                    .cell_extra(row, col)
                    .and_then(|extra| extra.image())
                    .and_then(|placed| placed.kitty.filter(|tag| selected(tag, &placed.image)));
                if let Some(tag) = hit {
                    self.grid.cell_extra_mut(row, col).set_image(None);
                    self.grid.damage_mut().mark_cell(row, col);
                    if seen.insert(tag.serial) {
                        cleared.push(tag);
                    }
                }
            }
        }
        cleared
    }

    /// Delete the placements `selected` accepts and, with each, its relative
    /// placements (a relative placement lives as long as its parent), and
    /// return the ids of every image that lost a placement. The image of a
    /// relative placement deleted with its parent goes too — data and all —
    /// when it has no placement left, as the protocol says.
    fn delete_kitty_where(
        &mut self,
        selected: &dyn Fn(&KittyPlacementTag, &ImageData) -> bool,
    ) -> Vec<u32> {
        let mut gone = self.clear_kitty_placements(selected);
        let mut touched: Vec<u32> = Vec::new();
        for tag in &gone {
            push_unique(&mut touched, tag.image_id);
        }
        let mut orphans: Vec<u32> = Vec::new();
        for _ in 0..=MAX_KITTY_RELATIVE_DEPTH {
            if gone.is_empty() || self.transient.kitty_relations.is_empty() {
                break;
            }
            // A deleted placement is no longer anyone's child; its own
            // children go with it.
            let relations = std::mem::take(&mut self.transient.kitty_relations);
            let (children, rest): (Vec<KittyRelation>, Vec<KittyRelation>) = relations
                .into_iter()
                .filter(|r| !gone.iter().any(|g| g.serial == r.child.serial))
                .partition(|r| gone.iter().any(|g| r.parent.names(g)));
            self.transient.kitty_relations = rest;
            let serials: HashSet<NonZeroU32> = children.iter().map(|r| r.child.serial).collect();
            self.clear_kitty_placements(&|tag, _| serials.contains(&tag.serial));
            for child in &children {
                push_unique(&mut touched, child.child.image_id);
                push_unique(&mut orphans, child.child.image_id);
            }
            gone = children.iter().map(|r| r.child).collect();
        }
        for id in orphans {
            if !self.kitty_image_placed(id) {
                self.free_kitty_image(id);
            }
        }
        touched
    }

    /// The serials of the Kitty placements covering any visible cell in
    /// `rows × cols` — restricted to images at z-index `z` when given.
    fn kitty_serials_in(
        &self,
        rows: std::ops::Range<u16>,
        cols: std::ops::Range<u16>,
        z: Option<i32>,
    ) -> Vec<NonZeroU32> {
        let mut serials = Vec::new();
        for row in rows.start..rows.end.min(self.grid.rows()) {
            for col in cols.start..cols.end.min(self.grid.cols()) {
                if let Some(placed) = self.grid.cell_extra(row, col).and_then(|e| e.image())
                    && let Some(tag) = placed.kitty
                    && z.is_none_or(|z| placed.image.z_index == z)
                    && !serials.contains(&tag.serial)
                {
                    serials.push(tag.serial);
                }
            }
        }
        serials
    }

    /// Whether any visible cell still shows a placement of image `id`.
    fn kitty_image_on_screen(&self, id: u32) -> bool {
        (0..self.grid.rows()).any(|row| {
            (0..self.grid.cols()).any(|col| {
                self.grid
                    .cell_extra(row, col)
                    .and_then(|extra| extra.image())
                    .and_then(|placed| placed.kitty)
                    .is_some_and(|tag| tag.image_id == id)
            })
        })
    }

    /// Whether image `id` has a placement left: on screen, or virtual.
    fn kitty_image_placed(&self, id: u32) -> bool {
        self.transient.kitty_virtual.contains_key(&id) || self.kitty_image_on_screen(id)
    }

    /// `a=d`: delete the placements the `d=` selector addresses. Kitty
    /// semantics, on aterm's placement model:
    ///
    ///   * a LOWERCASE selector deletes placements and KEEPS the transmitted
    ///     data (the id stays placeable) — preview cyclers (yazi, icat) lean on
    ///     that; UPPERCASE also frees the data of every image the command named
    ///     or cleared that has no placement left (`A`: of every image with no
    ///     virtual placement);
    ///   * a selector addresses SPECIFIC placements — never license to clear
    ///     the whole store — and only KITTY placements: an iTerm2 or sixel
    ///     image is not a Kitty delete's to erase;
    ///   * only the id-addressed selectors (`i`, `n`, `r`) reach VIRTUAL
    ///     placements, which have no place on screen for the others to hit;
    ///   * a relative placement goes with its parent.
    ///
    /// `x=`/`y=` are 1-based cell coordinates (for `r`, the id range); `z=`
    /// defaults to 0. `f`/`F` delete one animation frame
    /// ([`delete_kitty_frame`](Self::delete_kitty_frame)); an unknown selector
    /// deletes nothing: recoverable, and never destructive.
    fn delete_kitty_placements(&mut self, cmd: &KittyCommand) {
        let selector = cmd.delete_target.unwrap_or('a');
        let free_data = selector.is_ascii_uppercase();
        // A 1-based coordinate key as a 0-based cell index (`None` for 0/absent).
        let cell = |v: Option<u32>| {
            v.and_then(|v| v.checked_sub(1))
                .and_then(|v| u16::try_from(v).ok())
        };
        let everything = 0..u16::MAX;
        // Ids the command NAMES: their data goes under an uppercase selector
        // even when none of their placements was on screen.
        let mut named: Vec<u32> = Vec::new();
        let cleared = match selector.to_ascii_lowercase() {
            'a' => {
                self.delete_every_kitty_placement(free_data);
                return;
            }
            'i' | 'n' => {
                let id = if selector.eq_ignore_ascii_case(&'i') {
                    cmd.id
                } else {
                    cmd.number
                        .and_then(|number| self.transient.kitty_numbers.get(&number).copied())
                };
                let Some(id) = id else { return };
                // With `p=`, only that one placement of the image.
                let placement = cmd.placement;
                named.push(id);
                self.remove_kitty_virtual(id, placement);
                self.delete_kitty_where(&|tag, _| {
                    tag.image_id == id && placement.is_none_or(|p| tag.placement_id == p)
                })
            }
            'r' => {
                let (Some(lo), Some(hi)) = (cmd.x, cmd.y) else {
                    return;
                };
                named.extend(
                    self.transient
                        .kitty_images
                        .keys()
                        .copied()
                        .filter(|id| (lo..=hi).contains(id)),
                );
                for &id in &named {
                    self.remove_kitty_virtual(id, None);
                }
                self.delete_kitty_where(&|tag, _| (lo..=hi).contains(&tag.image_id))
            }
            'z' => {
                let z = cmd.z_index.unwrap_or(0);
                self.delete_kitty_where(&|_, image| image.z_index == z)
            }
            'f' => {
                self.delete_kitty_frame(cmd, free_data);
                return;
            }
            geometric => {
                let (rows, cols, z) = match geometric {
                    'c' => {
                        let (row, col) = (self.grid.cursor_row(), self.grid.cursor_col());
                        (row..row.saturating_add(1), col..col.saturating_add(1), None)
                    }
                    'p' | 'q' => {
                        let (Some(col), Some(row)) = (cell(cmd.x), cell(cmd.y)) else {
                            return;
                        };
                        let z = (geometric == 'q').then(|| cmd.z_index.unwrap_or(0));
                        (row..row.saturating_add(1), col..col.saturating_add(1), z)
                    }
                    'x' => {
                        let Some(col) = cell(cmd.x) else { return };
                        (everything.clone(), col..col.saturating_add(1), None)
                    }
                    'y' => {
                        let Some(row) = cell(cmd.y) else { return };
                        (row..row.saturating_add(1), everything.clone(), None)
                    }
                    // An unknown selector deletes nothing.
                    _ => return,
                };
                let serials = self.kitty_serials_in(rows, cols, z);
                if serials.is_empty() {
                    return;
                }
                self.delete_kitty_where(&|tag, _| serials.contains(&tag.serial))
            }
        };
        if free_data {
            for id in named.into_iter().chain(cleared) {
                if !self.kitty_image_placed(id) {
                    self.free_kitty_image(id);
                }
            }
        }
    }

    /// `d=a` / `d=A`: delete every placement on screen; `A` also frees every
    /// image that has no virtual placement (the one kind `a` cannot reach).
    fn delete_every_kitty_placement(&mut self, free_data: bool) {
        self.delete_kitty_where(&|_, _| true);
        if free_data {
            let unplaced: Vec<u32> = self
                .transient
                .kitty_images
                .keys()
                .copied()
                .filter(|id| !self.transient.kitty_virtual.contains_key(id))
                .collect();
            for id in unplaced {
                self.free_kitty_image(id);
            }
        }
    }

    /// `d=f` / `d=F`: delete frame `r=` of the image `i=`/`I=` names, as kitty
    /// does — `r=` is 1-based, absent or 0 is frame 1, past the end is the last
    /// frame — and the frames after it move up a number. The current frame
    /// stays the frame it was when that frame survives, else it is the frame
    /// now at its number, or the last; every placement of the image shows it.
    /// An image with a single frame keeps it under `f`; under `F` the image
    /// itself is deleted, placements and data.
    fn delete_kitty_frame(&mut self, cmd: &KittyCommand, image_too: bool) {
        let Some(id) = self.resolve_kitty_id(cmd) else {
            return;
        };
        let t = &mut *self.transient;
        let Some(frames) = t.kitty_frames.get_mut(&id) else {
            return;
        };
        if frames.len() < 2 {
            if image_too {
                self.remove_kitty_virtual(id, None);
                self.delete_kitty_where(&|tag, _| tag.image_id == id);
                self.free_kitty_image(id);
            }
            return;
        }
        let doomed = usize::try_from(cmd.rows.unwrap_or(0))
            .unwrap_or(usize::MAX)
            .clamp(1, frames.len())
            - 1;
        let shown = t
            .kitty_images
            .get(&id)
            .and_then(|shown| frames.iter().position(|f| Arc::ptr_eq(f, shown)))
            .unwrap_or(0);
        let removed = frames.remove(doomed);
        let shown = if shown > doomed { shown - 1 } else { shown };
        let current = Arc::clone(&frames[shown.min(frames.len() - 1)]);
        t.kitty_total_bytes = t.kitty_total_bytes.saturating_sub(removed.bytes.len());
        self.set_kitty_current_frame(id, current);
    }

    /// `a=a`: animation control. `c=` makes frame `c` (1-based) the image's
    /// current frame, shown by every placement of it. Frame gaps (`r=` with
    /// `z=`), the run state (`s=`) and loop counts (`v=`) drive PLAYBACK,
    /// which is not built, so they are accepted and ignored. Never answered,
    /// as in kitty.
    fn control_kitty_animation(&mut self, cmd: &KittyCommand) {
        let Some(id) = self.resolve_kitty_id(cmd) else {
            return;
        };
        let frame = cmd
            .columns
            .and_then(|n| usize::try_from(n).ok())
            .and_then(|n| n.checked_sub(1))
            .and_then(|index| self.transient.kitty_frames.get(&id)?.get(index).cloned());
        if let Some(frame) = frame {
            self.set_kitty_current_frame(id, frame);
        }
    }

    /// Make `frame` image `id`'s current frame (`kitty_images[id]`, charged
    /// like any slot) and show it everywhere the image shows.
    fn set_kitty_current_frame(&mut self, id: u32, frame: Arc<ImageData>) {
        let t = &mut *self.transient;
        if t.kitty_images
            .get(&id)
            .is_some_and(|shown| Arc::ptr_eq(shown, &frame))
        {
            return;
        }
        let new = frame.bytes.len();
        let old = t
            .kitty_images
            .insert(id, frame)
            .map_or(0, |img| img.bytes.len());
        t.kitty_total_bytes = t.kitty_total_bytes.saturating_sub(old).saturating_add(new);
        self.show_kitty_frame(id);
    }

    /// Point every placement of image `id` — on screen and virtual — at its
    /// CURRENT frame (`kitty_images[id]`), each keeping its own layout: kitty
    /// shows one frame of an image everywhere it shows. A placement whose
    /// re-laid-out copy does not fit the store keeps the frame it had.
    fn show_kitty_frame(&mut self, id: u32) {
        let Some(current) = self.transient.kitty_images.get(&id).cloned() else {
            return;
        };
        let mut memo: Vec<(Arc<ImageData>, Arc<ImageData>)> = Vec::new();
        for row in 0..self.grid.rows() {
            for col in 0..self.grid.cols() {
                let Some(placed) = self
                    .grid
                    .cell_extra(row, col)
                    .and_then(|extra| extra.image())
                    .filter(|placed| placed.kitty.is_some_and(|tag| tag.image_id == id))
                    .cloned()
                else {
                    continue;
                };
                let Some(image) = self.kitty_frame_for(id, &current, &placed.image, &mut memo)
                else {
                    continue;
                };
                if !Arc::ptr_eq(&image, &placed.image) {
                    self.grid
                        .set_cell_image(row, col, ImageRef { image, ..placed });
                    self.grid.damage_mut().mark_cell(row, col);
                }
            }
        }
        if let Some(virtuals) = self.transient.kitty_virtual.get(&id).cloned() {
            let shown = virtuals
                .into_iter()
                .map(|v| KittyVirtual {
                    image: self
                        .kitty_frame_for(id, &current, &v.image, &mut memo)
                        .unwrap_or(v.image),
                    ..v
                })
                .collect();
            self.transient.kitty_virtual.insert(id, shown);
        }
        self.damage_kitty_placeholders(id);
    }

    /// Frame `current` of image `id` laid out as `shown` is — one copy per
    /// distinct `shown`, remembered in `memo` — or `None` when that copy does
    /// not fit the store.
    fn kitty_frame_for(
        &mut self,
        id: u32,
        current: &Arc<ImageData>,
        shown: &Arc<ImageData>,
        memo: &mut Vec<(Arc<ImageData>, Arc<ImageData>)>,
    ) -> Option<Arc<ImageData>> {
        if let Some((_, done)) = memo.iter().find(|(from, _)| Arc::ptr_eq(from, shown)) {
            return Some(Arc::clone(done));
        }
        let image = self
            .kitty_variant(id, current, KittyLayout::of(shown))
            .ok()?;
        memo.push((Arc::clone(shown), Arc::clone(&image)));
        Some(image)
    }

    /// `a=f`: load frame data into the image `i=`/`I=` names — a new frame, or
    /// frame `r=` (1-based) edited when it exists — and answer as kitty does,
    /// naming the frame (`r=`). See [`build_kitty_frame`](Self::build_kitty_frame)
    /// for how the frame is made. Editing the current frame shows the edit
    /// everywhere the image shows.
    fn load_kitty_frame(&mut self, mut cmd: KittyCommand) {
        let Some(id) = self.resolve_kitty_id(&cmd) else {
            self.kitty_reply(&cmd, None, "ENOENT:no such image");
            return;
        };
        let Some(count) = self.transient.kitty_frames.get(&id).map(Vec::len) else {
            self.kitty_reply(&cmd, Some(id), "ENOENT:no such image");
            return;
        };
        // Kitty: `r=` names a frame to edit; absent, 0 or past the end, a new
        // frame is made.
        let edit = cmd
            .rows
            .and_then(|n| usize::try_from(n).ok())
            .filter(|&n| (1..=count).contains(&n))
            .map(|n| n - 1);
        let number = edit.map_or(count + 1, |index| index + 1);
        let verdict = match self.build_kitty_frame(id, &mut cmd, edit) {
            Ok(frame) => self.commit_kitty_frame(id, frame, edit),
            Err(error) => error,
        };
        self.kitty_reply_naming(&cmd, Some(id), Some(number), verdict);
    }

    /// Make an `a=f` frame of image `id`: its data (`s=`×`v=` raw pixels, or a
    /// PNG) placed at `x=`/`y=` on a canvas the root frame's size — the frame
    /// being edited (`edit`), else frame `c=` when given, else the background
    /// colour `Y=` (RGBA, default transparent black) — alpha-blended unless
    /// `X=1` overwrites. The frame keeps the root frame's layout. Data that is
    /// the whole canvas on a transparent background needs no canvas at all,
    /// so it may be a PNG; anything else is composed, which needs raw pixels
    /// on both sides.
    fn build_kitty_frame(
        &self,
        id: u32,
        cmd: &mut KittyCommand,
        edit: Option<usize>,
    ) -> Result<ImageData, &'static str> {
        let frames = self
            .transient
            .kitty_frames
            .get(&id)
            .ok_or("ENOENT:no such image")?;
        let root = frames.first().ok_or("ENOENT:no such image")?;
        let (canvas_w, canvas_h) = raster_size(root).ok_or("EINVAL:image has no pixel size")?;
        let at = (cmd.x.unwrap_or(0), cmd.y.unwrap_or(0));
        let base = cmd.columns.filter(|&n| n != 0);
        let background = cmd.cell_y_offset.unwrap_or(0);
        let blend = cmd.cell_x_offset != Some(1);
        // On `a=f` these keys place the data in the animation; none of them
        // lays the frame out.
        cmd.x = None;
        cmd.y = None;
        cmd.crop_width = None;
        cmd.crop_height = None;
        cmd.columns = None;
        cmd.rows = None;
        cmd.z_index = None;
        let (data, _) = self.build_kitty_image(cmd).ok_or("EINVAL:bad image data")?;
        let (data_w, data_h) = raster_size(&data).ok_or("EINVAL:bad image data")?;
        if data_w > canvas_w || data_h > canvas_h {
            return Err("EINVAL:frame larger than the image");
        }
        let whole = edit.is_none()
            && base.is_none()
            && at == (0, 0)
            && (data_w, data_h) == (canvas_w, canvas_h)
            && (background == 0 || !blend);
        if whole {
            return Ok(KittyLayout::of(root).apply(data));
        }
        if !matches!(data.format, ImageFormat::RawRgba8 { .. }) {
            return Err(KITTY_NEEDS_RAW);
        }
        let (Ok(width), Ok(height)) = (u16::try_from(canvas_w), u16::try_from(canvas_h)) else {
            return Err("ENOSPC:frame too large");
        };
        let size = usize::from(width) * usize::from(height) * 4;
        if size > MAX_KITTY_IMAGE_BYTES {
            return Err("ENOSPC:frame too large");
        }
        let under = match (edit, base) {
            (Some(index), _) => Some(frames.get(index).ok_or("EINVAL:no such frame")?),
            (None, Some(n)) => Some(
                usize::try_from(n - 1)
                    .ok()
                    .and_then(|index| frames.get(index))
                    .ok_or("EINVAL:no such frame")?,
            ),
            (None, None) => None,
        };
        // FALLIBLE ALLOCATION (M7): an OOM fails the frame, never the process.
        let mut canvas = Vec::new();
        canvas
            .try_reserve_exact(size)
            .map_err(|_| "ENOSPC:out of memory")?;
        match under {
            Some(under) if under.format == (ImageFormat::RawRgba8 { width, height }) => {
                canvas.extend_from_slice(&under.bytes);
            }
            Some(_) => return Err(KITTY_NEEDS_RAW),
            None => {
                for _ in 0..usize::from(width) * usize::from(height) {
                    canvas.extend_from_slice(&background.to_be_bytes());
                }
            }
        }
        let to_usize = |v: u32| usize::try_from(v).unwrap_or(usize::MAX);
        kitty_graphics::compose_rgba(
            &mut canvas,
            usize::from(width),
            (to_usize(at.0), to_usize(at.1)),
            &data.bytes,
            to_usize(data_w),
            (0, 0, to_usize(data_w), to_usize(data_h)),
            blend,
        );
        Ok(raw_frame(canvas, width, height, root))
    }

    /// Store `frame` as image `id`'s frame `edit` (0-based), or as a new last
    /// frame, charged to the budget and capped by `MAX_KITTY_FRAMES`. An edit
    /// of the current frame is shown everywhere the image shows. Returns the
    /// verdict to answer with.
    fn commit_kitty_frame(
        &mut self,
        id: u32,
        frame: ImageData,
        edit: Option<usize>,
    ) -> &'static str {
        let add = frame.bytes.len();
        let frame = Arc::new(frame);
        let t = &mut *self.transient;
        let total = t.kitty_total_bytes;
        let Some(frames) = t.kitty_frames.get_mut(&id) else {
            return "ENOENT:no such image";
        };
        let Some(index) = edit else {
            // Per-id frame-count cap AND the global byte budget (fail-closed):
            // a frame that would push the store over MAX_KITTY_STORE_BYTES is
            // refused so the caps can't multiply into a resident OOM.
            if frames.len() >= MAX_KITTY_FRAMES || total.saturating_add(add) > MAX_KITTY_STORE_BYTES
            {
                return "ENOSPC:no room for another frame";
            }
            frames.push(frame);
            t.kitty_total_bytes = total.saturating_add(add);
            return "OK";
        };
        let Some(slot) = frames.get_mut(index) else {
            return "EINVAL:no such frame";
        };
        let projected = total.saturating_sub(slot.bytes.len()).saturating_add(add);
        if projected > MAX_KITTY_STORE_BYTES {
            return "ENOSPC:image store full";
        }
        let old = std::mem::replace(slot, Arc::clone(&frame));
        t.kitty_total_bytes = projected;
        if t.kitty_images
            .get(&id)
            .is_some_and(|shown| Arc::ptr_eq(shown, &old))
        {
            self.set_kitty_current_frame(id, frame);
        }
        "OK"
    }

    /// `a=c`: compose a `w=`×`h=` rectangle (default: the whole image) of
    /// frame `r=` at `X=`/`Y=` onto frame `c=` at `x=`/`y=`, alpha-blended
    /// unless `C=1` overwrites, and answer as kitty does: `ENOENT` for a
    /// missing frame, `EINVAL` for a rectangle out of bounds or overlapping
    /// itself within one frame.
    fn compose_kitty_frames(&mut self, cmd: &KittyCommand) {
        let Some(id) = self.resolve_kitty_id(cmd) else {
            self.kitty_reply(cmd, None, "ENOENT:no such image");
            return;
        };
        let verdict = match self.composed_kitty_frame(id, cmd) {
            Ok((index, frame)) => self.commit_kitty_frame(id, frame, Some(index)),
            Err(error) => error,
        };
        self.kitty_reply(cmd, Some(id), verdict);
    }

    /// The frame `a=c` makes (see [`compose_kitty_frames`](Self::compose_kitty_frames))
    /// and the index it replaces.
    fn composed_kitty_frame(
        &self,
        id: u32,
        cmd: &KittyCommand,
    ) -> Result<(usize, ImageData), &'static str> {
        let frames = self
            .transient
            .kitty_frames
            .get(&id)
            .ok_or("ENOENT:no such image")?;
        let pick = |n: Option<u32>| {
            let index = usize::try_from(n?).ok()?.checked_sub(1)?;
            Some((index, frames.get(index)?))
        };
        let (src_index, src) = pick(cmd.rows).ok_or("ENOENT:no such source frame")?;
        let (dst_index, dst) = pick(cmd.columns).ok_or("ENOENT:no such destination frame")?;
        let ImageFormat::RawRgba8 { width, height } = dst.format else {
            return Err(KITTY_NEEDS_RAW);
        };
        if src.format != dst.format {
            return Err(KITTY_NEEDS_RAW);
        }
        let (image_w, image_h) = (u64::from(width), u64::from(height));
        let span = |v: Option<u32>, whole: u64| v.filter(|&v| v != 0).map_or(whole, u64::from);
        let (w, h) = (
            span(cmd.crop_width, image_w),
            span(cmd.crop_height, image_h),
        );
        let corner = |v: Option<u32>| u64::from(v.unwrap_or(0));
        let (dst_x, dst_y) = (corner(cmd.x), corner(cmd.y));
        let (src_x, src_y) = (corner(cmd.cell_x_offset), corner(cmd.cell_y_offset));
        if dst_x + w > image_w || dst_y + h > image_h {
            return Err("EINVAL:the destination rectangle is out of bounds");
        }
        if src_x + w > image_w || src_y + h > image_h {
            return Err("EINVAL:the source rectangle is out of bounds");
        }
        let overlaps = |a: u64, b: u64, len: u64| a.max(b) < a.min(b) + len;
        if src_index == dst_index && overlaps(src_x, dst_x, w) && overlaps(src_y, dst_y, h) {
            return Err("EINVAL:the rectangles overlap in one frame");
        }
        let mut canvas = Vec::new();
        canvas
            .try_reserve_exact(dst.bytes.len())
            .map_err(|_| "ENOSPC:out of memory")?;
        canvas.extend_from_slice(&dst.bytes);
        // Every value is bounded by a u16 image side, so the casts are exact.
        let to_usize = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
        kitty_graphics::compose_rgba(
            &mut canvas,
            usize::from(width),
            (to_usize(dst_x), to_usize(dst_y)),
            &src.bytes,
            usize::from(width),
            (to_usize(src_x), to_usize(src_y), to_usize(w), to_usize(h)),
            !cmd.cursor_stays,
        );
        Ok((dst_index, raw_frame(canvas, width, height, dst)))
    }

    /// The layout (footprint, z-index, scaling, source rectangle) a transmit or
    /// put asks for, for a raster of `px_w × px_h` pixels, or `None` when its
    /// source rectangle (`x=`/`y=`/`w=`/`h=`) misses the raster.
    ///
    /// NATURAL SIZE vs a requested cell box: `c=`/`r=` are the client asking
    /// for the image scaled over that many cells, so the renderer FITS it to the
    /// footprint; with only one of them the other follows from the (cropped)
    /// raster's aspect, as kitty computes it. With NEITHER given the footprint
    /// is merely the (cropped) raster's pixel size rounded UP to whole cells,
    /// and scaling back out to it would magnify by up to a cell — the same
    /// rounding noise the sixel path is `PixelExact` to avoid. Kitty's own
    /// reference terminal draws an un-sized transmission one image pixel to one
    /// device pixel. The footprint is clamped to the grid so a huge image can't
    /// request an enormous cell span.
    fn kitty_layout(&self, cmd: &KittyCommand, px_w: u32, px_h: u32) -> Option<KittyLayout> {
        let cropped = cmd.x.is_some()
            || cmd.y.is_some()
            || cmd.crop_width.is_some()
            || cmd.crop_height.is_some();
        let (x, y, vis_w, vis_h) = SourceRect {
            x: cmd.x.unwrap_or(0),
            y: cmd.y.unwrap_or(0),
            width: cmd.crop_width.unwrap_or(0),
            height: cmd.crop_height.unwrap_or(0),
        }
        .clamp_to(px_w, px_h)?;
        // A rectangle that IS the whole raster is no crop at all, so a put that
        // spells it out still reuses the stored image.
        let source_rect = (cropped && (vis_w, vis_h) != (px_w, px_h)).then_some(SourceRect {
            x,
            y,
            width: vis_w,
            height: vis_h,
        });
        let cell_w = u32::from(self.iterm2.cell_px.0.max(1));
        let cell_h = u32::from(self.iterm2.cell_px.1.max(1));
        // `c=0`/`r=0` are kitty's "unspecified".
        let (cols, rows) = (
            cmd.columns.filter(|&c| c != 0),
            cmd.rows.filter(|&r| r != 0),
        );
        // The span, in cells of `to_cell` px, that `cells` cells of `from_cell`
        // px make along the other axis at the raster's aspect `to_px:from_px`.
        let aspect = |cells: u32, from_cell: u32, from_px: u32, to_px: u32, to_cell: u32| {
            let span = (u64::from(cells) * u64::from(from_cell) * u64::from(to_px))
                .div_ceil(u64::from(from_px) * u64::from(to_cell));
            u32::try_from(span).unwrap_or(u32::MAX)
        };
        let (fit_cols, fit_rows) = match (cols, rows) {
            (Some(c), Some(r)) => (c, r),
            (Some(c), None) => (c, aspect(c, cell_w, vis_w, vis_h, cell_h)),
            (None, Some(r)) => (aspect(r, cell_h, vis_h, vis_w, cell_w), r),
            (None, None) => (vis_w.div_ceil(cell_w), vis_h.div_ceil(cell_h)),
        };
        Some(KittyLayout {
            cols: u16::try_from(fit_cols.max(1))
                .unwrap_or(u16::MAX)
                .min(self.grid.cols())
                .max(1),
            rows: u16::try_from(fit_rows.max(1))
                .unwrap_or(u16::MAX)
                .min(self.grid.rows())
                .max(1),
            // Kitty z=: negative draws behind text. iTerm2/Sixel + z=0 default to 0.
            z_index: cmd.z_index.unwrap_or(0),
            scaling: if cols.is_none() && rows.is_none() {
                ImageScaling::PixelExact
            } else {
                ImageScaling::Fit
            },
            source_rect,
        })
    }

    /// Build an [`ImageData`] from a single-chunk Kitty transmit command, or
    /// `None` if the payload is missing/oversized or the dimensions are
    /// invalid. PNG keeps its bytes (the renderer decodes); raw `f=32`/`f=24`
    /// become `RawRgba8` (RGB expands to opaque RGBA). The layout is the
    /// command's ([`kitty_layout`](Self::kitty_layout)); the flag is `false`
    /// when its source rectangle missed the raster, in which case the image is
    /// laid out whole — the DATA is still good to store, only that display is
    /// not.
    ///
    /// `&mut cmd` so the direct-uncompressed payload (the common `kitten icat`
    /// case, up to 4 MiB) MOVES into the image instead of being memcpy'd; the
    /// caller must not read `cmd.payload` afterwards. All other fields are
    /// left untouched.
    fn build_kitty_image(&self, cmd: &mut KittyCommand) -> Option<(ImageData, bool)> {
        use crate::terminal::kitty_graphics::{KittyFormat, png_dimensions, rgb_to_rgba};
        if cmd.payload.is_empty() {
            return None;
        }
        // The TRANSMITTED bytes: for `t=d` (direct) the payload IS the data; for the
        // non-direct mediums (`t=f` file / `t=t` temp-file / `t=s` shared memory) the
        // payload is a PATH/name, and the host RESOLVER does the I/O + security policy
        // and hands back the bytes. No resolver (the default) or a rejection ⇒ skip
        // cleanly (fail-closed) — the engine never reads files/shm itself. The host is
        // responsible for bounding what it reads to MAX_KITTY_IMAGE_BYTES.
        //
        // o=z (RFC 1950 zlib): the transmitted bytes (direct payload or resolved file
        // contents) may be compressed — inflate first, bounded so a decompression bomb
        // is rejected (fail closed).
        let payload: Vec<u8> = if cmd.medium == KittyMedium::Direct {
            if cmd.compressed {
                aterm_codec::inflate::zlib_decompress(&cmd.payload, MAX_KITTY_IMAGE_BYTES).ok()?
            } else {
                // Zero-copy: the (chunk-assembled) payload moves straight into
                // the image bytes. The emptiness guard above already ran.
                std::mem::take(&mut cmd.payload)
            }
        } else {
            let resolver = self.kitty_file_resolver.as_ref()?;
            let name = std::str::from_utf8(&cmd.payload).ok()?;
            let bytes = resolver(cmd.medium, name)?;
            if bytes.is_empty() || bytes.len() > MAX_KITTY_IMAGE_BYTES {
                return None;
            }
            if cmd.compressed {
                aterm_codec::inflate::zlib_decompress(&bytes, MAX_KITTY_IMAGE_BYTES).ok()?
            } else {
                bytes
            }
        };
        if payload.is_empty() || payload.len() > MAX_KITTY_IMAGE_BYTES {
            return None;
        }
        let (format, px_w, px_h, bytes) = match cmd.format {
            KittyFormat::Png => {
                // Pixel dims from the PNG header (for the footprint), else explicit s/v.
                let (w, h) = png_dimensions(&payload).or_else(|| cmd.width.zip(cmd.height))?;
                (ImageFormat::Png, w, h, payload)
            }
            KittyFormat::Rgba => {
                let (w, h) = (cmd.width?, cmd.height?);
                if payload.len() != (w as usize).checked_mul(h as usize)?.checked_mul(4)? {
                    return None; // malformed raw buffer
                }
                let fmt = ImageFormat::RawRgba8 {
                    width: u16::try_from(w).ok()?,
                    height: u16::try_from(h).ok()?,
                };
                (fmt, w, h, payload)
            }
            KittyFormat::Rgb => {
                let (w, h) = (cmd.width?, cmd.height?);
                if payload.len() != (w as usize).checked_mul(h as usize)?.checked_mul(3)? {
                    return None;
                }
                let fmt = ImageFormat::RawRgba8 {
                    width: u16::try_from(w).ok()?,
                    height: u16::try_from(h).ok()?,
                };
                (fmt, w, h, rgb_to_rgba(&payload))
            }
        };
        let (layout, crop_ok) = match self.kitty_layout(cmd, px_w, px_h) {
            Some(layout) => (layout, true),
            None => {
                let whole = KittyCommand {
                    columns: cmd.columns,
                    rows: cmd.rows,
                    z_index: cmd.z_index,
                    ..KittyCommand::default()
                };
                (self.kitty_layout(&whole, px_w, px_h)?, false)
            }
        };
        let image = layout.apply(ImageData {
            bytes,
            format,
            cols: 0,
            rows: 0,
            z_index: 0,
            band_lift_px: 0,
            scaling: ImageScaling::Fit,
            source_rect: None,
        });
        Some((image, crop_ok))
    }
}

/// The verdict for a transmission over a medium this session cannot read
/// (the non-direct mediums without the opt-in resolver) — the `a=q` probe's
/// answer and a transmit's alike.
const KITTY_MEDIUM_DISABLED: &str = "ENOTSUPPORTED:medium disabled (allow_kitty_file_transfer)";

/// The verdict for a frame composition the engine cannot do: it carries no
/// image codec, so it composes raw (`f=24`/`f=32`) pixels only.
const KITTY_NEEDS_RAW: &str = "ENOTSUPPORTED:frame composition needs raw pixels (f=24 or f=32)";

/// How one Kitty placement lays its image out: the fields of [`ImageData`] a
/// transmit or put decides (the rest is the payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KittyLayout {
    cols: u16,
    rows: u16,
    z_index: i32,
    scaling: ImageScaling,
    source_rect: Option<SourceRect>,
}

impl KittyLayout {
    /// The layout `image` has.
    const fn of(image: &ImageData) -> Self {
        Self {
            cols: image.cols,
            rows: image.rows,
            z_index: image.z_index,
            scaling: image.scaling,
            source_rect: image.source_rect,
        }
    }

    /// `image` with this layout.
    fn apply(self, image: ImageData) -> ImageData {
        ImageData {
            cols: self.cols,
            rows: self.rows,
            z_index: self.z_index,
            scaling: self.scaling,
            source_rect: self.source_rect,
            ..image
        }
    }
}

/// A raw RGBA8 frame of `width × height` pixels, laid out as `like`.
fn raw_frame(bytes: Vec<u8>, width: u16, height: u16, like: &ImageData) -> ImageData {
    KittyLayout::of(like).apply(ImageData {
        bytes,
        format: ImageFormat::RawRgba8 { width, height },
        cols: 0,
        rows: 0,
        z_index: 0,
        band_lift_px: 0,
        scaling: ImageScaling::Fit,
        source_rect: None,
    })
}

/// Push `id` onto `ids` unless it is there already.
fn push_unique(ids: &mut Vec<u32>, id: u32) {
    if !ids.contains(&id) {
        ids.push(id);
    }
}

/// The pixel size of a stored Kitty raster (the store holds PNG and raw RGBA
/// only).
fn raster_size(image: &ImageData) -> Option<(u32, u32)> {
    match image.format {
        ImageFormat::RawRgba8 { width, height } => Some((u32::from(width), u32::from(height))),
        ImageFormat::Png => crate::terminal::kitty_graphics::png_dimensions(&image.bytes),
        ImageFormat::Unknown => None,
    }
}

/// Maximum Kitty images retained in the per-screen store (DoS bound).
const MAX_KITTY_IMAGES: usize = 256;
/// Maximum animation frames retained per Kitty image (DoS bound).
const MAX_KITTY_FRAMES: usize = 128;
/// Maximum re-laid-out placement variants retained per Kitty image (DoS bound;
/// each is also charged to `MAX_KITTY_STORE_BYTES`).
const MAX_KITTY_VARIANTS: usize = 16;
/// Maximum virtual (`U=1`) placements retained per Kitty image; the oldest
/// unnamed one goes first.
const MAX_KITTY_VIRTUALS: usize = 8;
/// Maximum chain of parents above a relative placement. Kitty requires at
/// least 8.
const MAX_KITTY_RELATIVE_DEPTH: usize = 16;
/// Maximum decoded bytes for a single Kitty image (matches the APC payload cap).
const MAX_KITTY_IMAGE_BYTES: usize = 4 * 1024 * 1024;
/// GLOBAL byte budget for the whole Kitty image store (images + animation frames),
/// summed across every stored slot. Without this ceiling the three per-item caps
/// (`MAX_KITTY_IMAGES` × `MAX_KITTY_FRAMES` × `MAX_KITTY_IMAGE_BYTES`) multiply to
/// ~128 GiB of resident data — a hard OOM DoS from untrusted PTY output. Mirrors
/// the DCS global budget and the inline-image cap, which both bound their totals.
const MAX_KITTY_STORE_BYTES: usize = 64 * 1024 * 1024;

/// CSI dispatch fast paths extracted from ActionSink::csi_dispatch.
impl TerminalHandler<'_> {
    /// Fast-path CSI dispatch for sequences without intermediates.
    ///
    /// Single match on `final_byte` covers SGR, cursor moves, erase, scroll,
    /// and insert/delete — the top ~15 CSI sequences by frequency. Avoids the
    /// previous 3-function call chain for non-SGR sequences.
    #[inline]
    fn csi_dispatch_no_intermediates(
        &mut self,
        cap: &super::super::response_capability::ResponseCapability,
        params: &[u16],
        final_byte: u8,
    ) {
        match final_byte {
            b'm' => self.csi_dispatch_sgr_fast(params),
            // Top 5 cursor ops — inlined to avoid csi_dispatch_standard_core call
            b'A' => {
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid.cursor_up(n);
            }
            b'B' => {
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid.cursor_down(n);
            }
            b'C' => {
                let n = params.first().copied().unwrap_or(1).max(1);
                if self.modes.grapheme_cluster_mode {
                    self.cursor_state().cursor_forward_graphemes(n);
                } else {
                    self.grid
                        .cursor_forward_margin(n, self.modes.left_right_margin_mode);
                }
            }
            b'D' => {
                let n = params.first().copied().unwrap_or(1).max(1);
                if self.modes.grapheme_cluster_mode {
                    self.cursor_state().cursor_backward_graphemes(n);
                } else {
                    self.grid
                        .cursor_backward_margin(n, self.modes.left_right_margin_mode);
                }
            }
            b'H' | b'f' => {
                let row = params.first().copied().unwrap_or(1).saturating_sub(1);
                let col = params.get(1).copied().unwrap_or(1).saturating_sub(1);
                let (actual_row, actual_col) = if self.modes.origin_mode {
                    let region = self.grid.scroll_region();
                    let r = region.top.saturating_add(row).min(region.bottom);
                    if self.modes.left_right_margin_mode {
                        let margins = self.grid.horizontal_margins();
                        let c = margins.left.saturating_add(col).min(margins.right);
                        (r, c)
                    } else {
                        (r, col)
                    }
                } else {
                    (row, col)
                };
                self.grid.set_cursor(actual_row, actual_col);
            }
            // Remaining standard ops — delegate to avoid bloating this function
            _ => {
                let _ = self.csi_dispatch_standard_core(cap, params, final_byte);
            }
        }
    }

    /// SGR fast-path dispatch (extracted from csi_dispatch for clarity).
    #[inline]
    #[allow(
        clippy::too_many_lines,
        reason = "SGR dispatch table with many attribute codes"
    )]
    fn csi_dispatch_sgr_fast(&mut self, params: &[u16]) {
        // Ultra-fast: SGR 0 (reset) and bare CSI m
        // Both empty params and explicit 0 are SGR reset — use reset_sgr()
        // to preserve DECSCA protection attribute (#7321).
        // Must also clear underline color to match the CSI 0 m path (#7254).
        if params.is_empty() || (params.len() == 1 && params[0] == 0) {
            self.style.reset_sgr();
            self.transient.current_underline_color = None;
            self.transient.update_has_transient_extras();
            // Reset BCE cursor template when SGR is fully default (#7522).
            self.grid
                .set_cursor_template(crate::grid::Cell::EMPTY, None);
            return;
        }
        // Single-param basic colors — ANSI 8/16 and default fg/bg reset
        if params.len() == 1 {
            let p = params[0];
            match p {
                30..=37 | 90..=97 => {
                    let index =
                        crate::terminal::sgr_color_u8(if p >= 90 { p - 90 + 8 } else { p - 30 });
                    self.style.fg = PackedColor::indexed(index);
                    // Was: build an L1 probe `Style`, scan the 4-way L1, then the
                    // 256-entry indexed L2, then intern on miss — all to compute a
                    // `StyleId` no production reader consumes (see
                    // `SgrStyleHandler::apply_style_change`). What the writers
                    // actually need is the colour cache, and they need it on EVERY
                    // path: the old cache-hit branches called
                    // `update_fg_cache_indexed`, which does NOT refresh
                    // `cached_has_style_extras`, so an RGB→indexed fg change that
                    // happened to hit L1/L2 left that flag stale-true and the next
                    // write set HAS_EXTRAS on a cell with no extras. One
                    // unconditional refresh removes the divergence.
                    self.style.update_cached_colors();
                    return;
                }
                39 => {
                    self.style.fg = PackedColor::DEFAULT_FG;
                    self.style.update_cached_colors();
                    return;
                }
                40..=47 | 100..=107 => {
                    self.style.bg =
                        PackedColor::indexed(crate::terminal::sgr_color_u8(if p >= 100 {
                            p - 100 + 8
                        } else {
                            p - 40
                        }));
                    self.style.update_cached_colors();
                    // Update BCE cursor template for background change (#7522).
                    self.grid.set_cursor_template(
                        crate::grid::Cell::bce_blank(self.style.cached_colors()),
                        self.style.bce_bg_rgb(),
                    );
                    return;
                }
                49 => {
                    self.style.bg = PackedColor::DEFAULT_BG;
                    self.style.update_cached_colors();
                    // Reset BCE cursor template when bg returns to default (#7522).
                    self.grid.set_cursor_template(
                        crate::grid::Cell::bce_blank(self.style.cached_colors()),
                        self.style.bce_bg_rgb(),
                    );
                    return;
                }
                _ => {} // Non-color single params fall through to handle_sgr
            }
        }
        self.csi_dispatch_sgr_extended(params);
    }

    #[inline(never)]
    fn csi_dispatch_sgr_extended(&mut self, params: &[u16]) {
        // 5-param truecolor fg/bg — bat, delta, vim truecolor output
        if params.len() == 5 && params[1] == 2 {
            if params[0] == 38 {
                self.style.fg = PackedColor::rgb(
                    params[2].min(255) as u8,
                    params[3].min(255) as u8,
                    params[4].min(255) as u8,
                );
                self.style.update_cached_colors();
                return;
            }
            if params[0] == 48 {
                self.style.bg = PackedColor::rgb(
                    params[2].min(255) as u8,
                    params[3].min(255) as u8,
                    params[4].min(255) as u8,
                );
                self.style.update_cached_colors();
                // Update BCE cursor template for truecolor bg change (#7522).
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                return;
            }
        }
        // 3-param 256-color fg/bg
        if params.len() == 3 && params[1] == 5 {
            let index = crate::terminal::sgr_color_u8(params[2]);
            if params[0] == 38 {
                self.style.fg = PackedColor::indexed(index);
                // See the ANSI-fg arm above: one unconditional cache refresh
                // replaces the L1/L2/intern ladder AND its stale-flag divergence.
                self.style.update_cached_colors();
                return;
            }
            if params[0] == 48 {
                self.style.bg = PackedColor::indexed(index);
                self.style.update_cached_colors();
                // Update BCE cursor template for 256-color bg change (#7522).
                self.grid.set_cursor_template(
                    crate::grid::Cell::bce_blank(self.style.cached_colors()),
                    self.style.bce_bg_rgb(),
                );
                return;
            }
        }
        self.sgr_style().handle_sgr(params);
    }
}

/// True iff `data` contains a run of >= 4 identical bytes — the only content
/// `write_ascii_bulk_fast`'s run-detection scan can turn into a `write_cell_run`
/// splat. Prose (the cat-a-file case) has none, so this cheaply proves the scan
/// would be wasted and the caller can blast the whole run instead.
///
/// Branch-free windowed fold (bitwise `&`, no early exit) so LLVM auto-vectorizes
/// it to NEON/SSE byte compares — the sanctioned no-`std::arch` idiom that stays
/// inside the always-on Trust verification gate (cf. `aterm-parser::simd`).
#[inline]
#[allow(
    clippy::needless_bitwise_bool,
    reason = "bitwise & keeps the body branch-free so LLVM vectorizes it; the lazy && form measured SLOWER (does not vectorize) — this fold is the whole point"
)]
fn has_run_of_4(data: &[u8]) -> bool {
    let mut found = false;
    for w in data.windows(4) {
        found |= (w[0] == w[1]) & (w[1] == w[2]) & (w[2] == w[3]);
    }
    found
}

/// Bulk ASCII write helpers extracted from `print_ascii_bulk`.
impl TerminalHandler<'_> {
    /// Fast-path bulk ASCII writer for data that passed all precondition checks.
    ///
    /// Selects between three strategies:
    /// - **Cell-run path**: same byte repeated N times uses `write_cell_run`
    ///   (memset-like fill, avoids per-cell branch overhead)
    /// - **Blast path**: default style (no colors, no flags) uses `write_ascii_blast`
    /// - **Styled path**: non-default style uses `write_ascii_run_styled`
    ///
    /// All paths update `last_graphic_char` for the REP (repeat) sequence.
    fn write_ascii_bulk_fast(&mut self, data: &[u8]) {
        let flags = if self.style.protected {
            self.style.flags.union(CellFlags::PROTECTED)
        } else {
            self.style.flags
        };
        let is_default = self.style.is_default();
        let colors = self.style.cached_colors();

        // Real terminal output is dominated by short fragments between
        // color/reset/newline boundaries, and cat-a-file prose has no long
        // identical runs — for both, the scalar 4+-run scan below finds nothing
        // to splat and just blasts the whole run anyway. Skip it: short chunks,
        // OR longer runs a fast vectorized check proves are run-free, blast
        // directly. Byte-identical (the no-run scan path already blasts); the
        // check is safe (a false "run-free" would only forgo a splat, never
        // corrupt output) and its safe autovectorizable form stays in the Trust gate.
        if data.len() <= 64 || !has_run_of_4(data) {
            if is_default {
                let written = self.grid.write_ascii_blast(data);
                if written > 0 {
                    if let Some(&last) = data.get(written.saturating_sub(1)) {
                        self.transient.last_graphic_char = Some(last as char);
                    }
                }
            } else {
                let mut last_byte: Option<u8> = None;
                self.grid
                    .write_ascii_run_styled_packed(data, colors, flags, &mut last_byte);
                if let Some(b) = last_byte {
                    self.transient.last_graphic_char = Some(b as char);
                }
            }
            return;
        }

        // Single-pass scan: find runs of 4+ identical bytes AND mixed segments
        // in one traversal. Previous two-pass approach (scan_identical_run then
        // scan_mixed_segment) re-scanned the same bytes for diverse content.
        let mut pos = 0;
        while pos < data.len() {
            let byte = data[pos];
            let mut run_end = pos + 1;
            while run_end < data.len() && data[run_end] == byte {
                run_end += 1;
            }

            if run_end - pos >= 4 {
                let run_len = run_end - pos;
                let mut last_byte: Option<u8> = None;
                if is_default {
                    self.grid.write_cell_run(
                        byte,
                        run_len,
                        crate::grid::PackedColors::DEFAULT,
                        CellFlags::empty(),
                        &mut last_byte,
                    );
                } else {
                    self.grid
                        .write_cell_run(byte, run_len, colors, flags, &mut last_byte);
                }
                if let Some(b) = last_byte {
                    self.transient.last_graphic_char = Some(b as char);
                }
                pos = run_end;
                continue;
            }

            // Mixed segment: accumulate until we hit a 4+ run.
            let seg_start = pos;
            pos = run_end;
            while pos < data.len() {
                let b = data[pos];
                let mut r = pos + 1;
                while r < data.len() && data[r] == b {
                    r += 1;
                }
                if r - pos >= 4 {
                    break;
                }
                pos = r;
            }
            let segment = &data[seg_start..pos];

            if is_default {
                let written = self.grid.write_ascii_blast(segment);
                if written > 0 {
                    if let Some(&last) = segment.get(written.saturating_sub(1)) {
                        self.transient.last_graphic_char = Some(last as char);
                    }
                }
            } else {
                let mut last_byte: Option<u8> = None;
                self.grid
                    .write_ascii_run_styled_packed(segment, colors, flags, &mut last_byte);
                if let Some(b) = last_byte {
                    self.transient.last_graphic_char = Some(b as char);
                }
            }
        }
    }

    /// Bulk ASCII writer for styles that need `CellExtras` overflow.
    ///
    /// Handles RGB colors, hyperlinks, underline colors, and extended flags
    /// in bulk instead of falling back to per-character processing. Writes
    /// cells via `write_ascii_run_with_extras` which does bulk cell writes
    /// followed by batch extras application — 4-5x faster than per-char.
    fn write_ascii_bulk_with_extras(&mut self, data: &[u8]) {
        // Use pre-computed packed colors from CurrentStyle.
        let colors = self.style.cached_colors();
        let flags = if self.style.protected {
            self.style.flags.union(CellFlags::PROTECTED)
        } else {
            self.style.flags
        };

        let fg_rgb = if self.style.fg.is_rgb() {
            let (r, g, b) = self.style.fg.rgb_components();
            Some([r, g, b])
        } else {
            None
        };
        let bg_rgb = if self.style.bg.is_rgb() {
            let (r, g, b) = self.style.bg.rgb_components();
            Some([r, g, b])
        } else {
            None
        };
        let extended_flags_bits = if self.style.flags.has_extended_flags() {
            self.style.flags.extended_flags().bits()
        } else {
            0
        };

        let mut last_byte: Option<u8> = None;
        self.grid.write_ascii_run_with_extras(
            data,
            colors,
            flags,
            fg_rgb,
            bg_rgb,
            self.transient.current_underline_color,
            extended_flags_bits,
            self.transient.current_hyperlink.as_ref(),
            self.transient.current_hyperlink_id.as_ref(),
            &mut last_byte,
        );

        if let Some(b) = last_byte {
            self.transient.last_graphic_char = Some(b as char);
        }
    }
}

#[cfg(test)]
mod bulk_ascii_fast_tests {
    use super::has_run_of_4;
    use crate::terminal::Terminal;

    #[test]
    fn has_run_of_4_detects_exact_boundary() {
        assert!(!has_run_of_4(b""));
        assert!(!has_run_of_4(b"abc"));
        assert!(!has_run_of_4(b"aaab")); // only 3 identical
        assert!(!has_run_of_4(b"the quick brown fox jumps over 0123456789"));
        assert!(has_run_of_4(b"aaaa")); // exactly 4
        assert!(has_run_of_4(b"xy    zw")); // 4 spaces mid-string
        assert!(has_run_of_4(b"prefix______suffix")); // run at an offset
        assert!(has_run_of_4(b"tailaaaa")); // run at the end
    }

    /// The bulk writer (which for run-free >64 data now blasts directly instead
    /// of scanning) must render byte-identically to the per-byte oracle — for
    /// long run-free prose AND long data containing a splat-worthy run.
    fn assert_bulk_matches_per_byte(line: &[u8]) {
        let mut bulk = Terminal::new(24, 80);
        bulk.process(line);
        let mut per_byte = Terminal::new(24, 80);
        for &b in line {
            per_byte.process(&[b]);
        }
        assert_eq!(
            bulk.visible_content(),
            per_byte.visible_content(),
            "bulk vs per-byte render mismatch"
        );
    }

    #[test]
    fn long_run_free_prose_renders_identically() {
        // >64 bytes, no 4+ identical run — takes the new fast-reject blast path.
        assert_bulk_matches_per_byte(
            b"the quick brown fox jumps over the lazy dog 0123456789 and some more text\r\n",
        );
    }

    #[test]
    fn long_data_with_runs_still_renders_identically() {
        // >64 bytes WITH 4+ runs — takes the unchanged run-detection scan path.
        assert_bulk_matches_per_byte(
            b"indent:        code====line with runs and normal prose padding to exceed 64B\r\n",
        );
        assert_bulk_matches_per_byte(
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n",
        );
    }
}

#[cfg(test)]
mod kitty_display_tests {
    use crate::terminal::Terminal;

    /// Build an APC `G` Kitty graphics sequence (`ESC _ G <control> ; <b64> ESC \`).
    fn apc_g(control: &str, raw_payload: &[u8]) -> Vec<u8> {
        let mut v = b"\x1b_G".to_vec();
        v.extend_from_slice(control.as_bytes());
        if !raw_payload.is_empty() {
            v.push(b';');
            v.extend_from_slice(
                aterm_codec::base64::encode(raw_payload)
                    .expect("encode")
                    .as_bytes(),
            );
        }
        v.extend_from_slice(b"\x1b\\");
        v
    }

    /// A 1x1-cell RGBA image: cell_px 10x20 + s=10,v=20 -> ceil(10/10)=1 col, 1 row.
    fn one_cell_rgba() -> Vec<u8> {
        vec![0u8; 10 * 20 * 4]
    }

    #[test]
    fn transmit_and_display_places_inline_image() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=T,f=32,s=10,v=20", &one_cell_rgba()));
        let frame = term.cell_frame(24, 80);
        assert!(
            !frame.images[0].is_empty(),
            "a=T must place an inline image via the shared image pipeline"
        );
    }

    #[test]
    fn store_then_put_displays_only_after_put() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        // a=t stores under id=5 WITHOUT displaying.
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=5", &one_cell_rgba()));
        assert!(
            term.cell_frame(24, 80).images[0].is_empty(),
            "a=t alone does not display"
        );
        // a=p puts the stored image at the cursor.
        term.process(&apc_g("a=p,i=5", b""));
        assert!(
            !term.cell_frame(24, 80).images[0].is_empty(),
            "a=p displays the stored image"
        );
    }

    /// Bare `a=d` clears visible placements but KEEPS the store (spec: only an
    /// UPPERCASE selector frees data), so `a=p` still displays; `d=A` is the
    /// form that empties the store and makes `a=p` a no-op. (This test used to
    /// pin the opposite — bare delete nuking the store — which is exactly the
    /// defect that broke preview cyclers.)
    #[test]
    fn bare_delete_keeps_store_but_uppercase_all_frees_it() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=5", &one_cell_rgba()));
        term.process(&apc_g("a=d", b""));
        term.process(&apc_g("a=p,i=5", b"")); // store kept -> still placeable
        assert!(
            !term.cell_frame(24, 80).images[0].is_empty(),
            "bare a=d keeps the store; a=p still displays"
        );
        term.process(&apc_g("a=d,d=A", b""));
        term.process(&apc_g("a=p,i=5", b"")); // store freed -> nothing to place
        assert!(
            term.cell_frame(24, 80).images[0].is_empty(),
            "d=A freed the store; a=p cannot display"
        );
    }

    #[test]
    fn delete_by_id_removes_only_that_image() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=5", &one_cell_rgba()));
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=6", &one_cell_rgba()));
        // d=I,i=5 (UPPERCASE: placements + data) deletes only image 5; image 6
        // still displays. Lowercase d=i keeps the data by spec — covered by
        // tests/kitty_graphics_delete.rs.
        term.process(&apc_g("a=d,d=I,i=5", b""));
        assert!(
            term.cell_frame(24, 80).images[0].is_empty(),
            "deleted id 5 cannot be put"
        );
        term.process(&apc_g("a=p,i=5", b""));
        assert!(term.cell_frame(24, 80).images[0].is_empty(), "id 5 is gone");
        term.process(&apc_g("a=p,i=6", b""));
        assert!(
            !term.cell_frame(24, 80).images[0].is_empty(),
            "id 6 survived the targeted delete"
        );
    }

    #[test]
    fn chunked_transmit_display_assembles_and_places() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        let full = one_cell_rgba(); // 800 bytes = 10*20*4
        // First chunk carries the control + m=1; continuations carry m=1 / m=0
        // (their payloads are appended using the FIRST chunk's metadata).
        term.process(&apc_g("a=T,f=32,s=10,v=20,m=1", &full[0..400]));
        assert!(
            term.cell_frame(24, 80).images[0].is_empty(),
            "must not place a partial (mid-chunk) image"
        );
        term.process(&apc_g("m=1", &full[400..600]));
        term.process(&apc_g("m=0", &full[600..800]));
        assert!(
            !term.cell_frame(24, 80).images[0].is_empty(),
            "assembled image places on the final (m=0) chunk"
        );
    }

    #[test]
    fn unicode_placeholder_places_stored_image_via_fg_id_and_diacritics() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        // Store a 1-cell RGBA image under id 5 WITHOUT displaying it (a=t).
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=5", &one_cell_rgba()));
        assert!(
            term.cell_frame(24, 80).images[0].is_empty(),
            "a=t stores but does not display"
        );
        // Now draw it virtually: fg = indexed 5 (image-id low), then the placeholder
        // U+10EEEE + row diacritic (U+0305 -> 0) + col diacritic (U+0305 -> 0).
        let mut seq = b"\x1b[38;5;5m".to_vec();
        let mut buf = [0u8; 4];
        for c in ['\u{10EEEE}', '\u{0305}', '\u{0305}'] {
            seq.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        term.process(&seq);

        let frame = term.cell_frame(24, 80);
        assert!(
            !frame.images[0].is_empty(),
            "a Unicode placeholder cell must place the stored image (id from fg)"
        );
        let (col, iref) = &frame.images[0][0];
        assert_eq!(*col, 0, "placeholder was written at column 0");
        assert_eq!(
            (iref.cell_row, iref.cell_col),
            (0, 0),
            "row/col diacritics 0x0305 both decode to tile (0, 0)"
        );
    }

    #[test]
    fn animation_frame_transmit_and_select_switches_displayed_frame() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        // Base transmit (frame 1): a 1-cell RGBA image of all-zero bytes, id 7.
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=7", &one_cell_rgba()));
        // a=f: append a 2nd frame with DISTINCT pixels (all 0xAB).
        let frame2 = vec![0xABu8; 10 * 20 * 4];
        term.process(&apc_g("a=f,f=32,s=10,v=20,i=7", &frame2));

        // Display via a placeholder (it reads the CURRENT frame from the store live).
        let mut seq = b"\x1b[38;5;7m".to_vec();
        let mut buf = [0u8; 4];
        for c in ['\u{10EEEE}', '\u{0305}', '\u{0305}'] {
            seq.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        term.process(&seq);

        // Frame 1 is current: the placeholder shows the all-zero base frame.
        let f = term.cell_frame(24, 80);
        assert_eq!(
            f.images[0][0].1.image.bytes[0], 0x00,
            "frame 1 = base (zeros)"
        );

        // a=a c=2: select frame 2 → the SAME placeholder now shows the 0xAB frame.
        term.process(&apc_g("a=a,i=7,c=2", b""));
        let f = term.cell_frame(24, 80);
        assert_eq!(
            f.images[0][0].1.image.bytes[0], 0xAB,
            "a=a c=2 must switch the displayed frame to frame 2"
        );

        // a=a c=1: back to frame 1.
        term.process(&apc_g("a=a,i=7,c=1", b""));
        assert_eq!(
            term.cell_frame(24, 80).images[0][0].1.image.bytes[0],
            0x00,
            "a=a c=1 returns to frame 1"
        );

        // `r=` (with `z=`) names the frame whose GAP changes: it selects
        // nothing. The engine before 2026-09-28 selected frames with `r=`.
        term.process(&apc_g("a=a,i=7,r=2,z=80", b""));
        assert_eq!(
            term.cell_frame(24, 80).images[0][0].1.image.bytes[0],
            0x00,
            "a=a r=2 z=80 changes a gap and leaves frame 1 shown"
        );
    }

    /// The first pixel byte of what the placeholder at (0, 0) draws — the
    /// image's displayed frame — or `None` when it draws nothing.
    fn shown_frame(term: &mut Terminal) -> Option<u8> {
        term.cell_frame(24, 80).images[0]
            .first()
            .map(|(_, iref)| iref.image.bytes[0])
    }

    /// Image 7 with three frames whose pixels are all `0x01`, `0x02`, `0x03`,
    /// drawn by a placeholder at (0, 0), frame `shown` selected.
    fn three_frames(shown: u32) -> Terminal {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=7", &[1u8; 800]));
        term.process(&apc_g("a=f,f=32,s=10,v=20,i=7", &[2u8; 800]));
        term.process(&apc_g("a=f,f=32,s=10,v=20,i=7", &[3u8; 800]));
        term.process(&apc_g(&format!("a=a,i=7,c={shown}"), b""));
        let mut seq = b"\x1b[38;5;7m".to_vec();
        let mut buf = [0u8; 4];
        for c in ['\u{10EEEE}', '\u{0305}', '\u{0305}'] {
            seq.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        term.process(&seq);
        term
    }

    /// `d=f` deletes frame `r=` (1-based; absent is frame 1, past the end the
    /// last) and the later frames move up a number; the displayed frame stays
    /// the one it was while it survives, else it is the frame now at its
    /// number. Each deleted frame returns its bytes to the budget.
    #[test]
    fn frame_delete_removes_the_numbered_frame_as_kitty_does() {
        const FRAME: usize = 800;
        // The shown frame survives a delete before it and stays shown.
        let mut term = three_frames(3);
        assert_eq!(shown_frame(&mut term), Some(3));
        let before = term.transient.kitty_total_bytes;
        term.process(&apc_g("a=d,d=f,i=7,r=2", b""));
        assert_eq!(term.transient.kitty_frames[&7].len(), 2, "frame 2 went");
        assert_eq!(shown_frame(&mut term), Some(3), "frame 3 is still shown");
        assert_eq!(
            term.transient.kitty_total_bytes,
            before - FRAME,
            "the frame's bytes are returned"
        );
        term.process(&apc_g("a=a,i=7,c=2", b""));
        assert_eq!(
            shown_frame(&mut term),
            Some(3),
            "old frame 3 is frame 2 now"
        );

        // Deleting the shown frame shows the frame that takes its number.
        let mut term = three_frames(2);
        term.process(&apc_g("a=d,d=f,i=7,r=2", b""));
        assert_eq!(shown_frame(&mut term), Some(3));

        // ...or the last frame, when the shown one was the last.
        let mut term = three_frames(3);
        term.process(&apc_g("a=d,d=f,i=7,r=99", b""));
        assert_eq!(shown_frame(&mut term), Some(2), "r= past the end: the last");

        // No r=: frame 1, and the old frame 2 is the root.
        let mut term = three_frames(1);
        term.process(&apc_g("a=d,d=f,i=7", b""));
        assert_eq!(shown_frame(&mut term), Some(2));
        term.process(&apc_g("a=d,d=f,i=7", b""));
        assert_eq!(term.transient.kitty_frames[&7].len(), 1);
        assert_eq!(shown_frame(&mut term), Some(3));
        let lone = term.transient.kitty_total_bytes;
        assert_eq!(lone, 2 * FRAME, "one frame, charged in its two slots");

        // A lone frame: `f` keeps it, `F` deletes the image.
        term.process(&apc_g("a=d,d=f,i=7", b""));
        assert_eq!(shown_frame(&mut term), Some(3), "d=f keeps a lone frame");
        assert_eq!(term.transient.kitty_total_bytes, lone);
        term.process(&apc_g("a=d,d=F,i=7", b""));
        assert_eq!(shown_frame(&mut term), None, "d=F deleted the image");
        assert_eq!(term.transient.kitty_total_bytes, 0);
    }

    /// On `a=f` the keys a transmit lays out by are ANIMATION keys: `x=`/`y=`
    /// place the data on the canvas, `c=` names the frame it is drawn over, `r=`
    /// the frame edited (a new one here: frame 2 does not exist yet) and `z=`
    /// the gap. None of them crops, sizes or orders the frame, which takes its
    /// root frame's layout, so the placeholder that shows it after `a=a` draws
    /// it where it drew frame 1.
    #[test]
    fn animation_frame_keys_place_the_data_and_keep_the_root_layout() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=7", &one_cell_rgba()));
        let frame2 = vec![0xABu8; 10 * 20 * 4];
        term.process(&apc_g(
            "a=f,f=32,s=10,v=20,i=7,x=2,y=3,c=1,r=2,z=-40",
            &frame2,
        ));
        term.process(&apc_g("a=a,i=7,c=2", b""));
        let mut seq = b"\x1b[38;5;7m".to_vec();
        let mut buf = [0u8; 4];
        for c in ['\u{10EEEE}', '\u{0305}', '\u{0305}'] {
            seq.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        term.process(&seq);
        let f = term.cell_frame(24, 80);
        let shown = &f.images[0][0].1.image;
        assert_eq!(
            shown.bytes.len(),
            10 * 20 * 4,
            "the frame is the whole canvas"
        );
        assert_eq!(shown.bytes[0], 0, "left of x=2: frame 1's pixels (c=1)");
        let at = (3 * 10 + 2) * 4;
        assert_eq!(shown.bytes[at], 0xAB, "the data starts at (x=2, y=3)");
        assert_eq!(
            shown.source_rect, None,
            "x=/y= on a=f do not crop the frame"
        );
        assert_eq!(
            (shown.cols, shown.rows, shown.z_index, shown.scaling),
            (1, 1, 0, aterm_grid::ImageScaling::PixelExact),
            "c=/r=/z= on a=f do not lay the frame out: it takes frame 1's layout"
        );
    }

    #[test]
    fn unicode_placeholder_unknown_id_places_nothing() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        // No image stored under id 9 -> the placeholder resolves to nothing (no panic).
        let mut seq = b"\x1b[38;5;9m".to_vec();
        let mut buf = [0u8; 4];
        for c in ['\u{10EEEE}', '\u{0305}', '\u{0305}'] {
            seq.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        term.process(&seq);
        assert!(
            term.cell_frame(24, 80).images[0].is_empty(),
            "a placeholder referencing an unknown image id places nothing (fail closed)"
        );
    }

    #[test]
    fn non_direct_mediums_skip_cleanly_never_garbage() {
        // t=f (file) / t=t (temp) / t=s (shared mem): the payload is a PATH/name, not
        // pixels. aterm must NOT read host files/shm off an escape and must NOT decode
        // the path bytes as an image — it skips cleanly (no placement, no panic).
        for medium in ["f", "t", "s"] {
            let mut term = Terminal::new(24, 80);
            term.set_cell_pixel_size(10, 20);
            // A plausible path payload, declared as a 10x20 RGBA image via a non-direct
            // medium. Without the skip, the path bytes would be mis-decoded.
            let control = format!("a=T,f=32,s=10,v=20,t={medium}");
            term.process(&apc_g(&control, b"/tmp/some/image.png"));
            assert!(
                term.cell_frame(24, 80).images[0].is_empty(),
                "t={medium} (non-direct medium) must skip cleanly — no image placed"
            );
        }
    }

    #[test]
    fn file_medium_places_image_via_host_resolver() {
        use crate::terminal::kitty_graphics::KittyMedium;
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        // Host opts in by installing a resolver: it serves a 1-cell RGBA image for the
        // path "img.rgba" via `t=f`, rejects everything else (its security policy).
        term.set_kitty_file_resolver(|medium, name| {
            (medium == KittyMedium::File && name == "img.rgba").then(|| vec![0u8; 10 * 20 * 4])
        });
        // a=T with t=f and the path as payload → the resolver supplies the bytes.
        term.process(&apc_g("a=T,f=32,s=10,v=20,t=f", b"img.rgba"));
        assert!(
            !term.cell_frame(24, 80).images[0].is_empty(),
            "t=f via the host resolver must place the image"
        );
        // A path the resolver rejects (its policy) → fail closed, nothing placed.
        let mut term2 = Terminal::new(24, 80);
        term2.set_cell_pixel_size(10, 20);
        term2.set_kitty_file_resolver(|_, _| None);
        term2.process(&apc_g("a=T,f=32,s=10,v=20,t=f", b"/etc/passwd"));
        assert!(
            term2.cell_frame(24, 80).images[0].is_empty(),
            "a resolver that rejects the path places nothing (fail closed)"
        );
    }

    #[test]
    fn shared_memory_medium_routes_through_resolver() {
        use crate::terminal::kitty_graphics::KittyMedium;
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        // t=s (shared memory) also routes through the resolver — the engine is medium-
        // agnostic; the host implements shm_open etc. behind the same seam.
        term.set_kitty_file_resolver(|medium, name| {
            (medium == KittyMedium::SharedMemory && name == "/aterm-shm")
                .then(|| vec![0u8; 10 * 20 * 4])
        });
        term.process(&apc_g("a=T,f=32,s=10,v=20,t=s", b"/aterm-shm"));
        assert!(
            !term.cell_frame(24, 80).images[0].is_empty(),
            "t=s via the host resolver must place the image"
        );
    }

    #[test]
    fn compressed_o_z_payload_is_inflated_and_placed() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        // zlib(800 zero bytes) — a one-cell 10x20 RGBA image, transmitted with o=z.
        let compressed: &[u8] = &[
            0x78, 0xda, 0x63, 0x60, 0x18, 0x05, 0xa3, 0x60, 0x14, 0xe0, 0x02, 0x00, 0x03, 0x20,
            0x00, 0x01,
        ];
        term.process(&apc_g("a=T,f=32,s=10,v=20,o=z", compressed));
        assert!(
            !term.cell_frame(24, 80).images[0].is_empty(),
            "o=z compressed payload must be inflated and placed via the shared pipeline"
        );
    }

    #[test]
    fn malformed_o_z_payload_is_rejected_not_panic() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        // o=z but the payload is not a valid zlib stream — must drop, never panic.
        term.process(&apc_g("a=T,f=32,s=10,v=20,o=z", &[1, 2, 3, 4, 5, 6]));
        assert!(
            term.cell_frame(24, 80).images[0].is_empty(),
            "a corrupt o=z stream must be rejected (fail closed)"
        );
    }

    #[test]
    fn armed_alloc_fault_aborts_chunked_transfer_fail_closed() {
        use crate::fault;
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        let full = one_cell_rgba();
        // With the chunk-allocation fault armed, the first chunk fails closed: the
        // transfer is dropped, no partial buffer accumulates, and nothing panics.
        fault::with_armed("kitty.chunk_alloc", || {
            term.process(&apc_g("a=T,f=32,s=10,v=20,m=1", &full[0..400]));
            term.process(&apc_g("m=1", &full[400..600]));
            term.process(&apc_g("m=0", &full[600..800]));
        });
        assert!(
            term.cell_frame(24, 80).images[0].is_empty(),
            "an armed alloc fault must abort the transfer (fail closed), placing nothing"
        );
        // After disarming, a fresh transfer assembles and places normally — the fault
        // left no corrupt pending state behind (graceful, recoverable degradation).
        term.process(&apc_g("a=T,f=32,s=10,v=20,m=1", &full[0..400]));
        term.process(&apc_g("m=1", &full[400..600]));
        term.process(&apc_g("m=0", &full[600..800]));
        assert!(
            !term.cell_frame(24, 80).images[0].is_empty(),
            "after disarming, the accumulator recovers and places the assembled image"
        );
    }

    #[test]
    fn query_reports_ok() {
        let mut term = Terminal::new(24, 80);
        // a=q support probe with id=3 -> _Gi=3;OK (q=0 default).
        term.process(&apc_g("a=q,i=3", b""));
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b_Gi=3;OK\x1b\\"
        );
        // q=2 suppresses the response.
        term.process(&apc_g("a=q,i=4,q=2", b""));
        assert!(
            term.take_response().is_none(),
            "q=2 suppresses the query OK"
        );
    }

    /// The probe answers per MEDIUM: a `t=f` query on a session with no
    /// non-direct resolver installed (the default — `allow_kitty_file_transfer`
    /// is opt-in) must ERROR so the prober falls back to direct, instead of the
    /// old unconditional OK that promised a capability whose every transmit was
    /// then dropped by the fail-closed skip. Installing the resolver flips the
    /// same probe to OK; direct stays OK throughout.
    #[test]
    fn query_answers_per_medium_availability() {
        let mut term = Terminal::new(24, 80);
        term.process(&apc_g("a=q,i=3,t=f", b""));
        let reply = term.take_response().unwrap_or_default();
        let reply = String::from_utf8_lossy(&reply).into_owned();
        assert!(
            reply.starts_with("\x1b_Gi=3;ENOTSUPPORTED"),
            "no resolver -> t=f errors: {reply:?}"
        );
        term.process(&apc_g("a=q,i=3,t=d", b""));
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b_Gi=3;OK\x1b\\",
            "direct is always real"
        );
        term.set_kitty_file_resolver(|_, _| None);
        term.process(&apc_g("a=q,i=3,t=f", b""));
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b_Gi=3;OK\x1b\\",
            "with the resolver installed the same probe is OK"
        );
    }

    /// A transmit over a medium the session cannot read is answered with the
    /// verdict the `a=q` probe gives that medium — the data was never read, so
    /// `EINVAL:bad image data` would name the wrong cause. Once the resolver is
    /// installed, a path it refuses IS bad data.
    #[test]
    fn transmit_over_a_disabled_medium_answers_as_the_probe_does() {
        let mut term = Terminal::new(24, 80);
        term.process(&apc_g("a=q,i=5,t=f", b"/no/such/file"));
        let probe = term.take_response().expect("the probe is answered");
        term.process(&apc_g("a=t,i=5,t=f", b"/no/such/file"));
        let transmit = term.take_response().expect("the transmit is answered");
        assert_eq!(
            String::from_utf8_lossy(&transmit),
            String::from_utf8_lossy(&probe),
            "a disabled medium: the transmit's verdict is the probe's"
        );
        term.set_kitty_file_resolver(|_, _| None);
        term.process(&apc_g("a=t,i=5,t=f", b"/no/such/file"));
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b_Gi=5;EINVAL:bad image data\x1b\\",
            "with the resolver installed, a refused path is bad data"
        );
    }

    /// No delete selector is answered, found or not (kitty sends no reply to
    /// `a=d`).
    #[test]
    fn deletes_are_never_answered() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=4,q=1", &one_cell_rgba()));
        assert!(term.take_response().is_none(), "q=1 quiets the OK");
        for delete in ["a=d,d=i,i=4", "a=d,d=I,i=4", "a=d,d=i,i=99", "a=d,d=n,I=9"] {
            term.process(&apc_g(delete, b""));
            assert!(term.take_response().is_none(), "{delete} must not answer");
        }
    }

    #[test]
    fn malformed_raw_buffer_is_rejected() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        // s/v claim 10x20 RGBA (800 bytes) but only 4 bytes of payload -> rejected.
        term.process(&apc_g("a=T,f=32,s=10,v=20", &[1, 2, 3, 4]));
        assert!(
            term.cell_frame(24, 80).images[0].is_empty(),
            "a mismatched raw RGBA length must not place an image"
        );
    }

    /// The bulk `apc_put_bulk` override (used by the parser's APC fast path) must
    /// accumulate byte-for-byte identical `dcs.data` and budget counters as
    /// calling `apc_put` once per byte — even when a single run straddles BOTH
    /// the 4 MiB per-sequence cap AND the 10 MiB global DCS budget. Analogous to
    /// the parser's `osc_over_capacity_truncation_parity`.
    #[test]
    fn apc_put_bulk_crosses_caps_parity() {
        use crate::parser::ActionSink;
        use crate::terminal::MAX_DCS_GLOBAL_BUDGET;
        use aterm_provenance::pty_wrap_ref;

        const PER_SEQ_CAP: usize = 4 * 1024 * 1024;

        // Pre-seed to a state straddling both limits: dcs.data is 100 bytes below
        // the per-sequence cap, and total_bytes is 200 below the global budget.
        // A 300-byte run then crosses BOTH boundaries: the per-seq cap stops
        // pushes after 100 bytes, and the global budget stops counting after 200.
        let seed_len = PER_SEQ_CAP - 100;
        let seed_total = MAX_DCS_GLOBAL_BUDGET - 200;
        let run: Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();

        // `false` => per-byte reference (apc_put); `true` => bulk path
        // (apc_put_bulk), fed in three uneven chunks so a mid-stream boundary
        // crossing is exercised ACROSS calls too.
        fn drive(
            bulk: bool,
            run: &[u8],
            seed_len: usize,
            seed_total: usize,
        ) -> (Vec<u8>, usize, usize) {
            let mut term = Terminal::new(24, 80);
            let (_parser, mut h) = term.split_for_process();
            h.dcs.data = vec![0xAB; seed_len];
            h.dcs.total_bytes = seed_total;
            h.dcs.sequence_bytes = seed_total;
            if bulk {
                for chunk in [&run[..150], &run[150..151], &run[151..]] {
                    h.apc_put_bulk(pty_wrap_ref(chunk));
                }
            } else {
                for &b in run {
                    h.apc_put(b);
                }
            }
            (h.dcs.data.clone(), h.dcs.total_bytes, h.dcs.sequence_bytes)
        }

        let (data_byte, total_byte, seq_byte) = drive(false, &run, seed_len, seed_total);
        let (data_bulk, total_bulk, seq_bulk) = drive(true, &run, seed_len, seed_total);

        assert_eq!(
            data_byte, data_bulk,
            "bulk and per-byte APC paths must accumulate byte-identical data across both caps"
        );
        assert_eq!(
            (total_byte, seq_byte),
            (total_bulk, seq_bulk),
            "bulk and per-byte APC paths must charge the global budget identically"
        );
        // Sanity: both boundaries were actually crossed.
        assert_eq!(data_bulk.len(), PER_SEQ_CAP, "per-sequence cap reached");
        assert_eq!(total_bulk, MAX_DCS_GLOBAL_BUDGET, "global budget reached");
    }

    /// Bulk/per-byte parity for the DCS accumulator (the `dcs_put_bulk`
    /// override): identical `dcs.data` and budget counters across BOTH the
    /// per-type data cap and the global DCS budget, for every non-sixel DCS
    /// type. Sibling of `apc_put_bulk_crosses_caps_parity`. `Unknown` has no
    /// push without a registered callback but must still COUNT bytes (#7367).
    #[test]
    fn dcs_put_bulk_crosses_caps_parity() {
        use crate::parser::ActionSink;
        use crate::terminal::{DcsType, MAX_DCS_GLOBAL_BUDGET};
        use aterm_provenance::pty_wrap_ref;

        // (dcs_type, per-type data cap; 0 = nothing pushed without a callback).
        let cases: &[(DcsType, usize)] = &[
            (DcsType::Decrqss, 256),
            (DcsType::Xtgettcap, 1024),
            (DcsType::Unknown, 0),
        ];

        // `false` => per-byte reference (dcs_put); `true` => bulk path
        // (dcs_put_bulk), fed in three uneven chunks so a mid-stream boundary
        // crossing is exercised ACROSS calls too.
        fn drive(
            bulk: bool,
            dcs_type: DcsType,
            seed_len: usize,
            seed_total: usize,
            run: &[u8],
        ) -> (Vec<u8>, usize, usize) {
            let mut term = Terminal::new(24, 80);
            let (_parser, mut h) = term.split_for_process();
            h.dcs.dcs_type = dcs_type;
            h.dcs.data = vec![0xAB; seed_len];
            h.dcs.total_bytes = seed_total;
            h.dcs.sequence_bytes = seed_total;
            if bulk {
                for chunk in [&run[..150], &run[150..151], &run[151..]] {
                    h.dcs_put_bulk(pty_wrap_ref(chunk));
                }
            } else {
                for &b in run {
                    h.dcs_put(b);
                }
            }
            (h.dcs.data.clone(), h.dcs.total_bytes, h.dcs.sequence_bytes)
        }

        for &(dcs_type, cap) in cases {
            // Seed the data 100 bytes below its per-type cap (when it has one)
            // and the budget 200 below full: a 300-byte run crosses BOTH — the
            // cap stops pushes after 100 bytes, the budget stops counting at 200.
            let seed_len = cap.saturating_sub(100);
            let seed_total = MAX_DCS_GLOBAL_BUDGET - 200;
            let run: Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();
            let per_byte = drive(false, dcs_type, seed_len, seed_total, &run);
            let bulk = drive(true, dcs_type, seed_len, seed_total, &run);
            assert_eq!(
                per_byte, bulk,
                "bulk and per-byte DCS paths must match for {dcs_type:?}"
            );
            assert_eq!(per_byte.1, MAX_DCS_GLOBAL_BUDGET, "global budget reached");
            if cap > 0 {
                assert_eq!(per_byte.0.len(), cap, "per-type data cap reached");
            }
        }
    }

    /// Bulk/per-byte parity for the Sixel DCS path: for an in-budget stream
    /// the run-sampled pixel-allocation charge must land on identical budget
    /// counters and an identical decoder allocation.
    #[cfg(feature = "sixel")]
    #[test]
    fn dcs_put_bulk_sixel_parity() {
        use crate::parser::ActionSink;
        use aterm_provenance::pty_wrap_ref;

        // A small but real sixel body: raster declaration, a color register,
        // and painted bands so the decoder actually allocates pixels.
        let mut body = b"\"1;1;60;24#0;2;100;0;0".to_vec();
        for _ in 0..4 {
            body.extend_from_slice(b"#0!60~-");
        }

        fn drive(bulk: bool, body: &[u8]) -> (usize, usize, usize) {
            let mut term = Terminal::new(24, 80);
            let (_parser, mut h) = term.split_for_process();
            h.dcs_hook_inner(&[], &[], b'q'); // Sixel
            if bulk {
                let mid = body.len() / 2;
                h.dcs_put_bulk(pty_wrap_ref(&body[..mid]));
                h.dcs_put_bulk(pty_wrap_ref(&body[mid..]));
            } else {
                for &b in body {
                    h.dcs_put(b);
                }
            }
            (
                h.dcs.total_bytes,
                h.dcs.sequence_bytes,
                h.sixel.decoder.pixel_alloc_bytes(),
            )
        }

        let per_byte = drive(false, &body);
        let bulk = drive(true, &body);
        assert_eq!(per_byte, bulk, "sixel bulk/per-byte budget + alloc parity");
        assert!(bulk.2 > 0, "the stream must actually allocate pixels");
    }

    /// A `w x h` raw RGBA payload (`f=32`), exactly `w*h*4` bytes as `build_kitty_image`
    /// requires.
    fn raw_rgba(w: usize, h: usize) -> Vec<u8> {
        vec![0u8; w * h * 4]
    }

    /// The running `kitty_total_bytes` budget tracks every stored slot precisely:
    /// a base transmit charges TWO slots (kitty_images[id] + kitty_frames[id][0]),
    /// a frame charges one, and delete/clear decrement back to zero without drift.
    #[test]
    fn kitty_total_bytes_accounting_is_precise() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        let img = raw_rgba(4, 4); // 64 bytes
        let n = img.len();

        // Base transmit (a=t, no display): stored in images[1] AND frames[1][0].
        term.process(&apc_g("a=t,f=32,s=4,v=4,i=1", &img));
        assert_eq!(
            term.transient.kitty_total_bytes,
            2 * n,
            "base transmit = 2 slots"
        );

        // a=f appends one animation frame to id 1: +1 slot.
        term.process(&apc_g("a=f,f=32,s=4,v=4,i=1", &img));
        assert_eq!(
            term.transient.kitty_total_bytes,
            3 * n,
            "frame adds one slot"
        );

        // A second image in another id.
        term.process(&apc_g("a=t,f=32,s=4,v=4,i=2", &img));
        assert_eq!(term.transient.kitty_total_bytes, 5 * n);

        // Data-freeing deletes are the UPPERCASE selectors; lowercase keeps the
        // store (and therefore the budget) by spec. Delete just id 1's DATA
        // (its image + its 2 frames = 3 slots freed).
        term.process(&apc_g("a=d,d=I,i=1", b""));
        assert_eq!(
            term.transient.kitty_total_bytes,
            2 * n,
            "delete id 1 frees 3 slots"
        );

        // Uppercase delete-all clears the counter to zero.
        term.process(&apc_g("a=d,d=A", b""));
        assert_eq!(
            term.transient.kitty_total_bytes, 0,
            "delete-all resets the budget"
        );
        assert!(term.transient.kitty_images.is_empty());
        assert!(term.transient.kitty_frames.is_empty());
    }

    /// Regression: RIS (`ESC c`, full reset) clears the Kitty graphics store and
    /// drops the global byte budget in lockstep — matching kitty/xterm and the
    /// `a=d` delete-all path. If the budget survived RIS it would drift from the
    /// now-empty store and could wrongly reject a later valid in-budget image, and
    /// a stale id could still display after the reset.
    #[test]
    fn ris_clears_kitty_store_and_budget() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        let img = raw_rgba(4, 4);

        term.process(&apc_g("a=t,f=32,s=4,v=4,i=1", &img));
        term.process(&apc_g("a=f,f=32,s=4,v=4,i=1", &img));
        term.process(&apc_g("a=t,f=32,s=4,v=4,i=2", &img));
        assert!(
            term.transient.kitty_total_bytes > 0,
            "images stored before RIS"
        );
        assert!(!term.transient.kitty_images.is_empty());

        // RIS — full terminal reset.
        term.process(b"\x1bc");

        assert_eq!(term.transient.kitty_total_bytes, 0, "RIS resets the budget");
        assert!(
            term.transient.kitty_images.is_empty(),
            "RIS clears stored images"
        );
        assert!(
            term.transient.kitty_frames.is_empty(),
            "RIS clears animation frames"
        );

        // The freed budget admits a fresh image post-reset (no stale rejection).
        term.process(&apc_g("a=t,f=32,s=4,v=4,i=3", &img));
        assert!(
            term.transient.kitty_total_bytes > 0,
            "post-RIS image stores cleanly"
        );
    }

    /// Regression (round-6): RIS must also drop the IN-FLIGHT chunked-transmit
    /// accumulator (`kitty_pending`), not only the store. A partial `m=1` transfer
    /// left alive across RIS was silently merged into the FIRST post-reset Kitty
    /// command (`handle_kitty_command` branches on `kitty_pending.is_some() ||
    /// cmd.more`), gluing the new payload onto the stale chunk and finalizing it with
    /// the pre-reset metadata — corrupting a legitimate post-reset image and retaining
    /// its bytes across a reset that must free everything.
    #[test]
    fn ris_clears_in_flight_kitty_chunk_accumulator() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        let img = raw_rgba(4, 4);

        // A FIRST chunk with m=1 seeds kitty_pending and waits for further chunks.
        term.process(&apc_g("a=T,f=32,s=4,v=4,m=1", &img));
        assert!(
            term.transient.kitty_pending.is_some(),
            "precondition: an m=1 chunk leaves an in-flight transfer pending"
        );

        // RIS — full reset must abandon the partial transfer.
        term.process(b"\x1bc");
        assert!(
            term.transient.kitty_pending.is_none(),
            "RIS must clear the in-flight chunked-transmit accumulator"
        );
    }

    /// Regression: the Kitty store enforces a GLOBAL byte budget so the per-item
    /// caps (MAX_KITTY_IMAGES × MAX_KITTY_FRAMES × MAX_KITTY_IMAGE_BYTES) can no
    /// longer multiply into a multi-GiB resident OOM. Once adding an image would
    /// exceed MAX_KITTY_STORE_BYTES, the transfer is rejected (fail-closed) and
    /// the counter is left untouched; existing images remain.
    #[test]
    fn kitty_store_global_budget_rejects_overflow() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);

        // 3_000_000 bytes per image (base64 ≈ 4_000_000, just under the 4 MiB APC
        // buffer cap); each base transmit charges 2 slots (~6 MB), so the running
        // budget crosses the 64 MiB cap after ~11 distinct ids.
        let img = raw_rgba(1000, 750); // 3_000_000 bytes
        let per_slot = img.len();
        let mut stored = 0usize;
        let mut id = 1u32;
        loop {
            let before = term.transient.kitty_total_bytes;
            term.process(&apc_g(&format!("a=t,f=32,s=1000,v=750,i={id}"), &img));
            if term.transient.kitty_images.contains_key(&id) {
                // Accepted: budget grew by exactly two slots.
                assert_eq!(term.transient.kitty_total_bytes, before + 2 * per_slot);
                stored += 1;
                id += 1;
                assert!(
                    term.transient.kitty_total_bytes <= super::MAX_KITTY_STORE_BYTES,
                    "the running budget must never exceed the global cap"
                );
            } else {
                // Rejected (fail-closed): would have exceeded the cap; counter and
                // store are untouched, and we never blew past the cap.
                assert_eq!(
                    term.transient.kitty_total_bytes, before,
                    "a rejected over-budget transfer must not change the counter"
                );
                assert!(
                    before + 2 * per_slot > super::MAX_KITTY_STORE_BYTES,
                    "rejection only when adding the image would exceed the cap"
                );
                break;
            }
            assert!(id < 100, "must reject before storing absurdly many images");
        }
        // We actually filled the store up to the global cap (not the count cap).
        assert!(stored >= 1 && stored < super::MAX_KITTY_IMAGES);
        assert_eq!(term.transient.kitty_images.len(), stored);
    }

    /// NATURAL SIZE IS PIXEL-EXACT. `c=`/`r=` are the client asking for the
    /// image scaled over that many cells; with NEITHER given the footprint is
    /// only the raster's pixel size rounded UP to whole cells, and scaling back
    /// out to fill it magnifies the picture by up to a cell and interpolates
    /// away its 1-px features — the same rounding noise the sixel path carries
    /// `PixelExact` to avoid. Kitty's own reference terminal draws an un-sized
    /// transmission one image pixel to one device pixel.
    #[test]
    fn un_sized_transmit_is_pixel_exact_but_an_explicit_cell_box_is_fitted() {
        // 15x25 px at a 10x20 cell: a 2x2-cell footprint that is NOT the raster,
        // so the two policies differ on every pixel of it.
        let raster = vec![0u8; 15 * 25 * 4];
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=T,f=32,s=15,v=25", &raster));
        let frame = term.cell_frame(24, 80);
        let placed = frame.images[0].first().expect("a=T places an image");
        assert_eq!((placed.1.image.cols, placed.1.image.rows), (2, 2));
        assert_eq!(
            placed.1.image.scaling,
            aterm_grid::ImageScaling::PixelExact,
            "a transmission with neither c= nor r= is drawn at its natural size, \
             1:1 from the top-left of the footprint"
        );

        // The SAME raster with an explicit cell box is the opposite: the client
        // named cells, so the renderer fits the raster to them.
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=T,f=32,s=15,v=25,c=4,r=3", &raster));
        let frame = term.cell_frame(24, 80);
        let placed = frame.images[0].first().expect("a=T places an image");
        assert_eq!((placed.1.image.cols, placed.1.image.rows), (4, 3));
        assert_eq!(
            placed.1.image.scaling,
            aterm_grid::ImageScaling::Fit,
            "c=/r= asked for the image over that many cells: fit it to them"
        );
    }

    /// The one image placed at `(row, col)` of the visible grid, if any.
    fn image_at(term: &Terminal, row: usize, col: usize) -> Option<aterm_grid::ImageRef> {
        term.images_row(row)
            .into_iter()
            .find(|(c, _)| *c == col)
            .map(|(_, image)| image)
    }

    /// A put lays the image out by ITS OWN keys — kitty's placement model —
    /// not by the transmit's: `c=`/`r=` give it a cell box, `x=`/`y=`/`w=`/`h=`
    /// crop it. Such a put gets a re-laid-out variant (a footprint is part of
    /// the payload); an identical put reuses that variant, and a put with no
    /// keys reuses the stored image itself.
    #[test]
    fn put_lays_the_image_out_by_its_own_keys() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        // 20x40 px = a 2x2-cell natural footprint.
        term.process(&apc_g("a=t,f=32,s=20,v=40,i=1,q=2", &raw_rgba(20, 40)));
        let stored = std::sync::Arc::clone(&term.transient.kitty_images[&1]);

        term.process(b"\x1b[1;1H");
        term.process(&apc_g("a=p,i=1,q=2", b""));
        let natural = image_at(&term, 0, 0).expect("placed");
        assert!(
            std::sync::Arc::ptr_eq(&natural.image, &stored),
            "a key-less put shows the stored image itself"
        );

        term.process(b"\x1b[5;1H");
        term.process(&apc_g("a=p,i=1,c=4,r=3,q=2", b""));
        let boxed = image_at(&term, 4, 0).expect("placed");
        assert_eq!(
            (boxed.image.cols, boxed.image.rows),
            (4, 3),
            "the put's cell box"
        );
        assert_eq!(boxed.image.scaling, aterm_grid::ImageScaling::Fit);
        assert_eq!(
            (stored.cols, stored.rows),
            (2, 2),
            "the stored image keeps its own layout"
        );
        term.process(b"\x1b[10;1H");
        term.process(&apc_g("a=p,i=1,c=4,r=3,q=2", b""));
        let again = image_at(&term, 9, 0).expect("placed");
        assert!(
            std::sync::Arc::ptr_eq(&again.image, &boxed.image),
            "an identical put reuses the variant (one decode for both)"
        );

        term.process(b"\x1b[15;1H");
        term.process(&apc_g("a=p,i=1,x=10,y=20,w=10,h=20,q=2", b""));
        let cropped = image_at(&term, 14, 0).expect("placed");
        assert_eq!(
            cropped.image.source_rect,
            Some(aterm_grid::SourceRect {
                x: 10,
                y: 20,
                width: 10,
                height: 20,
            }),
            "the crop rides the placement to the renderer"
        );
        assert_eq!(
            (cropped.image.cols, cropped.image.rows),
            (1, 1),
            "an un-sized crop's footprint is the CROPPED raster's"
        );
        assert_eq!(cropped.image.scaling, aterm_grid::ImageScaling::PixelExact);

        // A crop that misses the raster is refused, and says so.
        term.process(&apc_g("a=p,i=1,x=99,y=0", b""));
        let reply = String::from_utf8(term.take_response().unwrap_or_default()).unwrap_or_default();
        assert!(reply.starts_with("\x1b_Gi=1;EINVAL"), "{reply:?}");
    }

    /// Variants are charged to the store's budget and capped per id, but one
    /// nothing shows any more is recycled — a client that re-crops on every
    /// scroll never runs out. Keeping them all ON SCREEN is the control: that
    /// does hit the cap, and the put is refused rather than evicting pixels.
    #[test]
    fn put_variants_are_budgeted_recycled_and_capped() {
        let mut term = Terminal::new(40, 80);
        term.set_cell_pixel_size(10, 20);
        let img = raw_rgba(20, 40);
        term.process(&apc_g("a=t,f=32,s=20,v=40,i=1,q=2", &img));
        let base = term.transient.kitty_total_bytes;
        // Each put re-crops and lands on row 0, overwriting the previous one.
        for x in 0..(2 * super::MAX_KITTY_VARIANTS as u32) {
            term.process(b"\x1b[1;1H");
            term.process(&apc_g(&format!("a=p,i=1,x={},w=1,h=1", x % 20), b""));
            let reply = term.take_response().unwrap_or_default();
            assert_eq!(reply, b"\x1b_Gi=1;OK\x1b\\", "put {x}");
        }
        assert!(
            term.transient.kitty_total_bytes <= base + super::MAX_KITTY_VARIANTS * img.len(),
            "variants stay charged within their cap"
        );

        // Control: distinct variants all kept visible, one per row.
        let mut term = Terminal::new(40, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=32,s=20,v=40,i=1,q=2", &img));
        let mut refused = None;
        for x in 0..=super::MAX_KITTY_VARIANTS as u32 {
            term.process(format!("\x1b[{};1H", x + 1).as_bytes());
            term.process(&apc_g(&format!("a=p,i=1,x={x},w=1,h=1"), b""));
            let reply =
                String::from_utf8(term.take_response().unwrap_or_default()).unwrap_or_default();
            if reply.contains("ENOSPC") {
                refused = Some(x);
                break;
            }
        }
        assert_eq!(
            refused,
            Some(super::MAX_KITTY_VARIANTS as u32),
            "the variant past the cap, with every earlier one still on screen, is refused"
        );
    }

    /// `I=` alone: the terminal assigns the id and reports it with the number;
    /// later puts by number find it. `q=1` keeps the OK quiet, `q=2` an error.
    #[test]
    fn number_transmit_assigns_an_id_and_answers_with_both() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=32,s=10,v=20,I=77", &one_cell_rgba()));
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b_Gi=1,I=77;OK\x1b\\"
        );
        term.process(&apc_g("a=p,I=77,q=1", b""));
        assert!(term.take_response().is_none(), "q=1 silences the OK");
        assert!(
            !term.cell_frame(24, 80).images[0].is_empty(),
            "placed by number"
        );

        term.process(&apc_g("a=t,f=32,s=10,v=20,I=78,i=3", &one_cell_rgba()));
        let reply = String::from_utf8(term.take_response().unwrap_or_default()).unwrap_or_default();
        assert!(reply.contains(";EINVAL"), "i and I together: {reply:?}");

        term.process(&apc_g("a=p,i=9", b""));
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b_Gi=9;ENOENT:no such image\x1b\\"
        );
        term.process(&apc_g("a=p,i=9,q=2", b""));
        assert!(term.take_response().is_none(), "q=2 silences errors too");
        // No i=/I= at all: nothing to answer to.
        term.process(&apc_g("a=T,f=32,s=10,v=20", &one_cell_rgba()));
        assert!(term.take_response().is_none());
    }

    /// Re-transmitting an id replaces its data AND removes its placements,
    /// whose pixels are the old image's (kitty's semantics).
    #[test]
    fn retransmitting_an_id_removes_its_old_placements() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=T,f=32,s=10,v=20,i=4,q=2", &one_cell_rgba()));
        assert!(image_at(&term, 0, 0).is_some());
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=4,q=2", &one_cell_rgba()));
        assert!(
            image_at(&term, 0, 0).is_none(),
            "the old placement of id 4 went with its data"
        );
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=5,q=2", &one_cell_rgba()));
        term.process(b"\x1b[3;1H");
        term.process(&apc_g("a=p,i=4,q=2", b""));
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=5,q=2", &one_cell_rgba()));
        assert!(
            image_at(&term, 2, 0).is_some(),
            "control: another id's transmit leaves id 4's placement alone"
        );
    }

    /// Kitty's cursor policy: after a placement the cursor sits just past the
    /// image's right edge on its LAST row (the next line's start when that is
    /// past the margin); `C=1` leaves it where it was. iTerm2's policy — a
    /// fresh line below the image — is the control.
    #[test]
    fn kitty_cursor_lands_after_the_image_and_c1_keeps_it() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(b"\x1b[2;3H");
        term.process(&apc_g("a=T,f=32,s=10,v=20,c=4,r=3,q=2", &one_cell_rgba()));
        let at = term.cursor();
        assert_eq!((at.row, at.col), (3, 6), "row 1 + 3 - 1, col 2 + 4");

        term.process(b"\x1b[10;78H");
        term.process(&apc_g("a=T,f=32,s=10,v=20,c=3,r=1,q=2", &one_cell_rgba()));
        let at = term.cursor();
        assert_eq!(
            (at.row, at.col),
            (10, 0),
            "past the right margin: next line"
        );

        term.process(b"\x1b[15;5H");
        term.process(&apc_g(
            "a=T,f=32,s=10,v=20,c=2,r=2,C=1,q=2",
            &one_cell_rgba(),
        ));
        let at = term.cursor();
        assert_eq!((at.row, at.col), (14, 4), "C=1: the cursor does not move");
        assert!(image_at(&term, 15, 5).is_some(), "but the image is placed");

        // Control: iTerm2 places, then starts a fresh line.
        let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
        let b64 = aterm_codec::base64::encode(&png).expect("encode");
        term.process(b"\x1b[20;1H");
        term.process(format!("\x1b]1337;File=inline=1;width=2;height=1:{b64}\x1b\\").as_bytes());
        let at = term.cursor();
        assert_eq!((at.row, at.col), (20, 0), "iTerm2: the line below");
    }

    /// `U=1` is a VIRTUAL placement: shown only where the client prints
    /// Unicode placeholders. Stamping it at the cursor too would draw the
    /// picture twice (yazi transmits exactly this way) and move the cursor
    /// under the client's feet.
    #[test]
    fn virtual_placement_stamps_nothing_and_keeps_the_cursor() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(b"\x1b[4;4H");
        term.process(&apc_g(
            "a=T,U=1,f=32,s=10,v=20,i=6,c=2,r=1,q=2",
            &one_cell_rgba(),
        ));
        assert!(
            (0..24).all(|row| term.images_row(row).is_empty()),
            "no direct placement"
        );
        let at = term.cursor();
        assert_eq!((at.row, at.col), (3, 3), "cursor untouched");
        assert!(
            term.transient.kitty_images.contains_key(&6),
            "stored for placeholders"
        );
        // Control: the same transmit without U=1 does stamp.
        term.process(&apc_g(
            "a=T,f=32,s=10,v=20,i=7,c=2,r=1,q=2",
            &one_cell_rgba(),
        ));
        assert!(image_at(&term, 3, 3).is_some());
    }

    /// A Unicode placeholder cell for image `id` (indexed foreground) and
    /// virtual placement `pid` (indexed underline colour; none for `0`),
    /// showing tile (`row`, `col`) — both below 4.
    fn placeholder(id: u8, pid: u8, row: usize, col: usize) -> Vec<u8> {
        const MARKS: [char; 4] = ['\u{0305}', '\u{030D}', '\u{030E}', '\u{0310}'];
        let mut seq = format!("\x1b[38;5;{id}m").into_bytes();
        if pid != 0 {
            seq.extend_from_slice(format!("\x1b[58;5;{pid}m").as_bytes());
        }
        let mut buf = [0u8; 4];
        for c in ['\u{10EEEE}', MARKS[row], MARKS[col]] {
            seq.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        }
        seq.extend_from_slice(b"\x1b[0m");
        seq
    }

    /// The reply the last command drew, as text.
    fn reply(term: &mut Terminal) -> String {
        String::from_utf8(term.take_response().unwrap_or_default()).unwrap_or_default()
    }

    /// `a=a c=N` shows frame N EVERYWHERE the image shows: a placement at the
    /// cursor switches too, keeping its own layout, and the placeholder cells
    /// naming the image are damaged so they repaint. The engine before
    /// 2026-09-28 re-pointed only the store: a direct placement kept its frame,
    /// and nothing was damaged.
    #[test]
    fn a_selected_frame_shows_in_every_placement_and_repaints_placeholders() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=7,q=2", &[1u8; 800]));
        term.process(&apc_g("a=f,f=32,s=10,v=20,i=7,q=2", &[2u8; 800]));
        // A direct placement with a layout of its own (a 2x1 cell box).
        term.process(&apc_g("a=p,i=7,c=2,r=1,q=2", b""));
        // A placeholder on row 5, the cursor parked far from it.
        term.process(b"\x1b[6;1H");
        term.process(&placeholder(7, 0, 0, 0));
        term.process(b"\x1b[21;1H");
        let _ = term.cell_frame(24, 80);
        term.take_damage();

        term.process(&apc_g("a=a,i=7,c=2", b""));
        let placed = image_at(&term, 0, 0).expect("still placed");
        assert_eq!(
            placed.image.bytes[0], 2,
            "the direct placement shows frame 2"
        );
        assert_eq!(
            (placed.image.cols, placed.image.rows),
            (2, 1),
            "...in its own layout"
        );
        assert!(
            term.grid().damage().is_row_damaged(5),
            "the placeholder's row repaints"
        );
        assert_eq!(term.cell_frame(24, 80).images[5][0].1.image.bytes[0], 2);
    }

    /// `U=1` put: a VIRTUAL placement with its own id and cell box, which a
    /// placeholder names in its underline colour. A placement id with no
    /// virtual placement draws nothing; no underline colour picks the newest
    /// one. The engine before 2026-09-28 stored nothing for a `U=1` put and
    /// read no underline colour, so every placeholder drew the transmit's
    /// 2x2 footprint.
    #[test]
    fn virtual_placements_carry_their_own_id_and_layout() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        // 20x40 px: a 2x2 natural footprint.
        term.process(&apc_g("a=t,f=32,s=20,v=40,i=5,q=2", &raw_rgba(20, 40)));
        term.process(&apc_g("a=p,U=1,i=5,p=3,c=4,r=1", b""));
        assert_eq!(reply(&mut term), "\x1b_Gi=5,p=3;OK\x1b\\");
        assert!(
            (0..24).all(|row| term.images_row(row).is_empty()),
            "a virtual placement stamps nothing"
        );
        term.process(b"\x1b[1;1H");
        term.process(&placeholder(5, 3, 0, 1));
        term.process(b"\x1b[2;1H");
        term.process(&placeholder(5, 9, 0, 0));
        term.process(b"\x1b[3;1H");
        term.process(&placeholder(5, 0, 0, 0));
        let frame = term.cell_frame(24, 80);
        let (_, named) = &frame.images[0][0];
        assert_eq!((named.image.cols, named.image.rows), (4, 1), "p=3's box");
        assert_eq!((named.cell_row, named.cell_col), (0, 1));
        assert!(
            frame.images[1].is_empty(),
            "no virtual placement 9: nothing"
        );
        let (_, any) = &frame.images[2][0];
        assert!(
            std::sync::Arc::ptr_eq(&any.image, &named.image),
            "no placement id: the newest virtual placement"
        );

        // Putting p=3 again replaces it.
        term.process(&apc_g("a=p,U=1,i=5,p=3,c=3,r=2,q=2", b""));
        let frame = term.cell_frame(24, 80);
        assert_eq!(
            (
                frame.images[0][0].1.image.cols,
                frame.images[0][0].1.image.rows
            ),
            (3, 2)
        );
        assert_eq!(term.transient.kitty_virtual[&5].len(), 1);
    }

    /// Only the id-addressed delete selectors reach a virtual placement —
    /// it has no place on screen for the others to hit — and `d=A` keeps the
    /// data of an image that still has one.
    #[test]
    fn deletes_reach_virtual_placements_by_id_only() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=32,s=20,v=40,i=5,q=2", &raw_rgba(20, 40)));
        term.process(&apc_g("a=p,U=1,i=5,p=3,c=4,r=1,q=2", b""));
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=6,q=2", &one_cell_rgba()));
        term.process(&placeholder(5, 3, 0, 0));
        let cols = |term: &mut Terminal| {
            term.cell_frame(24, 80).images[0]
                .first()
                .map(|(_, image)| image.image.cols)
        };

        for selector in ["a", "c", "p,x=1,y=1", "x,x=1", "y,y=1", "z"] {
            term.process(&apc_g(&format!("a=d,d={selector}"), b""));
            assert_eq!(cols(&mut term), Some(4), "d={selector} leaves it");
        }
        term.process(&apc_g("a=d,d=A", b""));
        assert!(term.transient.kitty_images.contains_key(&5), "d=A keeps 5");
        assert!(!term.transient.kitty_images.contains_key(&6), "d=A frees 6");
        assert_eq!(cols(&mut term), Some(4));

        term.process(&apc_g("a=d,d=i,i=5,p=3", b""));
        assert_eq!(cols(&mut term), None, "d=i with p= deletes that one");
        assert!(
            term.transient.kitty_images.contains_key(&5),
            "lowercase keeps data"
        );
        term.process(&apc_g("a=p,U=1,i=5,p=3,c=4,r=1,q=2", b""));
        term.process(&apc_g("a=d,d=I,i=5", b""));
        assert!(
            !term.transient.kitty_images.contains_key(&5),
            "d=I frees it"
        );
        assert_eq!(term.transient.kitty_total_bytes, 0, "and its budget");
    }

    /// `P=`/`Q=`: a placement relative to a parent — `H=`/`V=` cells from the
    /// parent's top-left, the cursor untouched — moves with the parent and is
    /// deleted with it, and kitty's errors come back for a missing parent, a
    /// cycle, a virtual parent and a relative virtual placement. The engine
    /// before 2026-09-28 read none of these keys and drew the child at the
    /// cursor.
    #[test]
    fn relative_placements_follow_their_parent() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        for id in 1..=3 {
            term.process(&apc_g(
                &format!("a=t,f=32,s=10,v=20,i={id},q=2"),
                &one_cell_rgba(),
            ));
        }
        let tag_at = |term: &Terminal, row, col| {
            image_at(term, row, col)
                .and_then(|image| image.kitty)
                .map(|tag| (tag.image_id, tag.placement_id))
        };
        term.process(b"\x1b[3;4H");
        term.process(&apc_g("a=p,i=1,p=1,q=2", b""));
        term.process(b"\x1b[10;10H");
        term.process(&apc_g("a=p,i=2,p=5,P=1,Q=1,H=2,V=1", b""));
        assert_eq!(reply(&mut term), "\x1b_Gi=2,p=5;OK\x1b\\");
        assert_eq!(tag_at(&term, 3, 5), Some((2, 5)), "(2,3) + (V=1, H=2)");
        assert_eq!(tag_at(&term, 9, 9), None, "not at the cursor");
        let at = term.cursor();
        assert_eq!((at.row, at.col), (9, 9), "the cursor does not move");

        // Moving the parent moves the child.
        term.process(b"\x1b[6;1H");
        term.process(&apc_g("a=p,i=1,p=1,q=2", b""));
        assert_eq!(tag_at(&term, 2, 3), None, "the parent left (2,3)");
        assert_eq!(tag_at(&term, 3, 5), None, "the child left (3,5)");
        assert_eq!(tag_at(&term, 6, 2), Some((2, 5)), "(5,0) + (1, 2)");

        // Kitty's refusals.
        term.process(&apc_g("a=p,i=3,P=9", b""));
        assert!(reply(&mut term).contains(";ENOPARENT"));
        term.process(&apc_g("a=p,i=1,p=1,P=2,Q=5", b""));
        assert!(reply(&mut term).contains(";ECYCLE"));
        term.process(&apc_g("a=p,U=1,i=3,p=7,q=2", b""));
        term.process(&apc_g("a=p,i=1,P=3,Q=7", b""));
        assert!(
            reply(&mut term).contains(";ENOTSUPPORTED"),
            "virtual parent"
        );
        term.process(&apc_g("a=p,U=1,i=3,P=1", b""));
        assert!(
            reply(&mut term).contains(";EINVAL"),
            "a relative virtual placement"
        );
        assert_eq!(tag_at(&term, 6, 2), Some((2, 5)), "refusals moved nothing");

        // Deleting the parent deletes the child, and the child's image goes
        // with its last placement.
        term.process(&apc_g("a=d,d=i,i=1", b""));
        assert_eq!(tag_at(&term, 6, 2), None, "the child went with its parent");
        assert!(
            term.transient.kitty_images.contains_key(&1),
            "d=i keeps the parent's data"
        );
        assert!(
            !term.transient.kitty_images.contains_key(&2),
            "the child's image went"
        );
        assert!(term.transient.kitty_relations.is_empty());
    }

    /// Relative placements nest, and too deep a chain is refused.
    #[test]
    fn relative_chains_are_bounded() {
        let mut term = Terminal::new(40, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=1,q=2", &one_cell_rgba()));
        term.process(&apc_g("a=p,i=1,p=1,q=2", b""));
        for p in 2..=u32::try_from(super::MAX_KITTY_RELATIVE_DEPTH).unwrap_or(0) + 1 {
            term.process(&apc_g(&format!("a=p,i=1,p={p},P=1,Q={},V=1", p - 1), b""));
            assert_eq!(reply(&mut term), format!("\x1b_Gi=1,p={p};OK\x1b\\"));
        }
        let deepest = super::MAX_KITTY_RELATIVE_DEPTH + 1;
        term.process(&apc_g(&format!("a=p,i=1,p=99,P=1,Q={deepest}"), b""));
        assert!(reply(&mut term).contains(";ETOODEEP"));
    }

    /// `a=f` composes as kitty does: data at `x=`/`y=` over a transparent
    /// canvas, over frame `c=`, or into frame `r=` in place; the answer names
    /// the frame. PNG data that must be composed is refused, not stored as a
    /// frame of the wrong size. The engine before 2026-09-28 appended every
    /// `a=f` payload whole, so a 1-pixel patch became a 1-pixel frame.
    #[test]
    fn frames_are_composed_on_a_canvas() {
        const RED: [u8; 4] = [255, 0, 0, 255];
        const GREEN: [u8; 4] = [0, 255, 0, 255];
        const BLUE: [u8; 4] = [0, 0, 255, 255];
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=32,s=2,v=1,i=7,q=1", &[RED, GREEN].concat()));
        let frame = |term: &Terminal, n: usize| term.transient.kitty_frames[&7][n].bytes.clone();

        term.process(&apc_g("a=f,f=32,s=1,v=1,i=7,x=1", &BLUE));
        assert_eq!(reply(&mut term), "\x1b_Gi=7,r=2;OK\x1b\\");
        assert_eq!(
            frame(&term, 1),
            [[0; 4], BLUE].concat(),
            "over transparent black"
        );

        term.process(&apc_g("a=f,f=32,s=1,v=1,i=7,x=1,c=1,q=1", &BLUE));
        assert_eq!(frame(&term, 2), [RED, BLUE].concat(), "over frame 1");

        term.process(&apc_g(
            "a=f,f=32,s=1,v=1,i=7,Y=4278190335,q=1",
            &[0, 0, 0, 0],
        ));
        assert_eq!(
            frame(&term, 3),
            [RED, RED].concat(),
            "the background Y= shows through"
        );

        // r=1 edits the root — the current frame — in place.
        term.process(&apc_g("a=f,f=32,s=1,v=1,i=7,r=1,X=1", &[0, 0, 255, 0]));
        assert_eq!(reply(&mut term), "\x1b_Gi=7,r=1;OK\x1b\\");
        assert_eq!(
            frame(&term, 0),
            [[0, 0, 255, 0], GREEN].concat(),
            "X=1 overwrites"
        );
        assert_eq!(term.transient.kitty_images[&7].bytes, frame(&term, 0));
        assert_eq!(
            term.transient.kitty_frames[&7].len(),
            4,
            "no frame was added"
        );

        // Refusals: too large, a PNG patch, a missing base frame.
        term.process(&apc_g("a=f,f=32,s=3,v=1,i=7", &[0; 12]));
        assert!(reply(&mut term).contains(";EINVAL"), "wider than the image");
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1u32.to_be_bytes());
        png.extend_from_slice(&1u32.to_be_bytes());
        term.process(&apc_g("a=f,f=100,i=7,x=1", &png));
        assert!(reply(&mut term).contains(";ENOTSUPPORTED"), "a PNG patch");
        term.process(&apc_g("a=f,f=32,s=1,v=1,i=7,c=9", &BLUE));
        assert!(
            reply(&mut term).contains(";EINVAL"),
            "no frame 9 to draw over"
        );
        assert_eq!(term.transient.kitty_frames[&7].len(), 4);
    }

    /// `a=c` composes a rectangle of one frame onto another, and refuses what
    /// kitty refuses. The engine before 2026-09-28 ignored `a=c` entirely.
    #[test]
    fn compose_copies_a_rectangle_between_frames() {
        const RED: [u8; 4] = [255, 0, 0, 255];
        const GREEN: [u8; 4] = [0, 255, 0, 255];
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=32,s=2,v=1,i=7,q=2", &[RED, GREEN].concat()));
        term.process(&apc_g("a=f,f=32,s=2,v=1,i=7,q=2", &[GREEN, GREEN].concat()));
        term.process(&apc_g("a=c,i=7,r=1,c=2,w=1,h=1,x=1", b""));
        assert_eq!(reply(&mut term), "\x1b_Gi=7;OK\x1b\\");
        assert_eq!(
            term.transient.kitty_frames[&7][1].bytes,
            [GREEN, RED].concat(),
            "frame 1's (0,0) onto frame 2's (1,0)"
        );
        term.process(&apc_g("a=c,i=7,r=1,c=2,w=2,x=1", b""));
        assert!(reply(&mut term).contains(";EINVAL"), "out of bounds");
        term.process(&apc_g("a=c,i=7,r=9,c=2", b""));
        assert!(reply(&mut term).contains(";ENOENT"), "no frame 9");
        term.process(&apc_g("a=c,i=7,r=2,c=2,w=2", b""));
        assert!(reply(&mut term).contains(";EINVAL"), "overlapping itself");
    }

    /// Only one of `c=`/`r=`: the other follows from the image's aspect, as
    /// kitty computes it; `c=0`/`r=0` are unspecified. The engine before
    /// 2026-09-28 took the missing one from the natural size, and read `c=0`
    /// as a one-column box.
    #[test]
    fn one_cell_dimension_keeps_the_aspect() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        // 20x40 px: natural 2x2 cells; 4 columns = 40 px wide → 80 px tall.
        term.process(&apc_g("a=T,f=32,s=20,v=40,c=4", &raw_rgba(20, 40)));
        let placed = image_at(&term, 0, 0).expect("placed");
        assert_eq!((placed.image.cols, placed.image.rows), (4, 4));
        term.process(b"\x1b[10;1H");
        term.process(&apc_g("a=T,f=32,s=20,v=40,r=1", &raw_rgba(20, 40)));
        let placed = image_at(&term, 9, 0).expect("placed");
        assert_eq!(
            (placed.image.cols, placed.image.rows),
            (1, 1),
            "20 px tall → 10 px wide"
        );
        term.process(b"\x1b[15;1H");
        term.process(&apc_g("a=T,f=32,s=20,v=40,c=0,r=0", &raw_rgba(20, 40)));
        let placed = image_at(&term, 14, 0).expect("placed");
        assert_eq!((placed.image.cols, placed.image.rows), (2, 2));
        assert_eq!(placed.image.scaling, aterm_grid::ImageScaling::PixelExact);
    }
}
