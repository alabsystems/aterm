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

use super::{TerminalHandler, Vt52CursorState};

impl ActionSink for TerminalHandler<'_> {
    fn print(&mut self, c: char) {
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
            return;
        }

        // Capture text for CopyToClipboard mode (OSC 1337) before fast-path write.
        // The fast paths bypass write_char, so we must capture here.
        if let Some(state) = self.clipboard.copy_state.as_mut() {
            for &byte in data {
                state.push(byte as char);
            }
        }

        // Check if style needs CellExtras overflow (RGB, hyperlinks, etc.).
        // Both flags cached at mutation time — no per-bulk-call overhead.
        if self.style.has_style_extras() || self.transient.has_transient_extras {
            self.write_ascii_bulk_with_extras(data);
        } else {
            self.write_ascii_bulk_fast(data);
        }
    }

    /// FAST PATH: Print a run of decoded non-ASCII characters.
    ///
    /// Called by the parser for consecutive multi-byte UTF-8 sequences.
    /// Amortizes per-character overhead (charset, clipboard, style checks)
    /// over the entire run. Falls back to per-character for complex cases.
    fn print_unicode_bulk(&mut self, chars: &Provenance<[char], Pty>) {
        let chars = chars.as_ref();
        // VT52 cursor addressing consumes characters specially
        if self.transient.vt52_cursor_state != Vt52CursorState::None {
            for &c in chars {
                self.print(c);
            }
            return;
        }

        self.write_unicode_bulk(chars);
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
    ///
    /// See `docs/ESCAPE_SEQUENCE_MATRIX.md` for complete control code coverage.
    fn execute(&mut self, byte: u8) {
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
                // Capture tab for CopyToClipboard (OSC 1337)
                if let Some(state) = self.clipboard.copy_state.as_mut() {
                    state.push('\t');
                }
                self.grid.tab_margin(self.modes.left_right_margin_mode);
            }
            0x0A..=0x0C => {
                // LF, VT, FF
                // Capture newline for CopyToClipboard (OSC 1337)
                if let Some(state) = self.clipboard.copy_state.as_mut() {
                    state.push('\n');
                }
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
                // Capture CR for CopyToClipboard (OSC 1337)
                if let Some(state) = self.clipboard.copy_state.as_mut() {
                    state.push('\r');
                }
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
                // Capture newline for CopyToClipboard (matches ESC D path)
                if let Some(state) = self.clipboard.copy_state.as_mut() {
                    state.push('\n');
                }
                // Per VT510: when DECLRMM is active, IND at the scroll boundary
                // scrolls only within horizontal margins (#7407).
                // Line feed, honoring DECLRMM left/right margins (#7687).
                self.margined_line_feed(self.modes.left_right_margin_mode);
            }
            0x85 => {
                // NEL (Next Line) - same as ESC E
                // Move cursor to start of next line, scroll if needed
                // Capture newline for CopyToClipboard (matches ESC E path)
                if let Some(state) = self.clipboard.copy_state.as_mut() {
                    state.push('\n');
                }
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
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        self.esc_dispatch_core(&cap, intermediates.as_ref(), final_byte);
    }

    /// Dispatch OSC (Operating System Command) escape sequences.
    fn osc_dispatch(&mut self, params: &Provenance<[&[u8]], Pty>) {
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

/// Kitty graphics (APC `G`) command handling — the KITTY-CORE display slice. An
/// inherent impl (not part of `ActionSink`); called from `apc_end` above.
impl TerminalHandler<'_> {
    /// Handle one parsed Kitty graphics command (KITTY-CORE display slice):
    /// delete (clear store), put/display (place a stored image), or transmit /
    /// transmit-and-display (decode, store by id, optionally place). Chunked
    /// (`m=1`), query, animation, and non-direct mediums are deferred — so
    /// `kitty_graphics` stays advertised FALSE until those land (no false advertise).
    /// Assemble CHUNKED Kitty transmissions (`m=1`) before handling. The first
    /// `m=1` chunk seeds the pending command (moved in whole, payload included);
    /// continuation chunks append their payload; the `m=0` chunk finalizes and
    /// dispatches the whole image. Non-chunked commands dispatch immediately.
    /// The accumulated payload is bounded by `MAX_KITTY_IMAGE_BYTES` (overflow
    /// aborts the transfer). Takes the command BY VALUE so the assembled
    /// payload can flow into the image store without a multi-MiB copy.
    fn handle_kitty_command(&mut self, cmd: crate::terminal::kitty_graphics::KittyCommand) {
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

    /// Clear every VISIBLE placement whose backing image `selected` accepts,
    /// damaging each cleared cell so the repaint erases it.
    ///
    /// Placements are `ImageRef` Arcs stamped into cell extras, so "delete a
    /// placement" is a visible-grid sweep matching on the shared `Arc` — ptr
    /// identity, because [`aterm_grid::ImageData`] carries no kitty id (the
    /// store map owns that association). The read side uses the non-allocating
    /// [`aterm_grid::Grid::cell_extra`]; `cell_extra_mut` is touched only for
    /// cells that actually match, so a sweep over a grid with no images
    /// allocates nothing. Rows already scrolled into scrollback keep their
    /// pixels — kitty deletion addresses the screen, and scrolled-away
    /// placements age out with their rows.
    fn clear_kitty_placements(
        &mut self,
        selected: &dyn Fn(&std::sync::Arc<aterm_grid::ImageData>) -> bool,
    ) {
        for row in 0..self.grid.rows() {
            for col in 0..self.grid.cols() {
                let hit = self
                    .grid
                    .cell_extra(row, col)
                    .and_then(|extra| extra.image())
                    .is_some_and(|placed| selected(&placed.image));
                if hit {
                    self.grid.cell_extra_mut(row, col).set_image(None);
                    self.grid.damage_mut().mark_cell(row, col);
                }
            }
        }
    }

    /// Handle one COMPLETE (chunk-assembled) Kitty graphics command:
    /// delete (placements per selector, data under uppercase selectors),
    /// put/display (place a stored image), transmit / transmit-and-display
    /// (decode, store by id, optionally place), query (answered per medium
    /// availability), animation frames (`a=f`, appended per id), and — when the
    /// host installs the opt-in resolver — the non-direct file/temp/shm
    /// mediums.
    ///
    /// `kitty_graphics` stays advertised FALSE (no false advertise) for the
    /// pieces still missing, which are no longer the ones an earlier version of
    /// this comment named: placement ids (`p=`) and delete-by-point/number
    /// (`x=`/`y=` are not even parsed), animation CONTROL (`a=a`), source
    /// cropping, z-index compositing between images, and Unicode placeholders.
    #[allow(
        clippy::too_many_lines,
        reason = "single per-action dispatch (transmit/frame/display/delete) with inline global-byte-budget accounting per arm"
    )]
    fn handle_complete_kitty_command(
        &mut self,
        mut cmd: crate::terminal::kitty_graphics::KittyCommand,
    ) {
        use crate::terminal::kitty_graphics::KittyAction;
        match cmd.action {
            KittyAction::Delete => {
                // Kitty delete semantics, on aterm's placement model (an image is
                // PLACED by stamping `ImageRef` Arcs into cell extras; the store
                // maps id -> Arc for later re-display). Two invariants the spec
                // sets and this arm now honors:
                //
                //   * a LOWERCASE selector deletes placements and KEEPS the
                //     transmitted data (the id stays placeable); UPPERCASE also
                //     frees the data. Preview cyclers (yazi, icat) lean on
                //     lowercase keeping data.
                //   * a selector addresses SPECIFIC placements — it is never
                //     license to clear the whole store.
                //
                // Neither held before: every selector except i/I nuked the entire
                // store, lowercase i destroyed data — and NOTHING ever cleared a
                // placed cell, so the "deleted" image stayed on screen while the
                // terminal forgot it had it. Delete was thus simultaneously too
                // destructive (all data gone) and not destructive enough (all
                // pixels kept).
                match cmd.delete_target {
                    // No d=, d=a, d=A: every visible placement; 'A' also frees
                    // the whole store.
                    None | Some('a' | 'A') => {
                        self.clear_kitty_placements(&|_| true);
                        if cmd.delete_target == Some('A') {
                            self.transient.kitty_images.clear();
                            self.transient.kitty_frames.clear();
                            self.transient.kitty_total_bytes = 0;
                        }
                    }
                    Some('i' | 'I') => {
                        if let Some(id) = cmd.id {
                            if let Some(img) = self.transient.kitty_images.get(&id).cloned() {
                                self.clear_kitty_placements(&|placed| {
                                    std::sync::Arc::ptr_eq(placed, &img)
                                });
                            }
                            if cmd.delete_target == Some('I') {
                                // Decrement the global byte budget by the bytes held
                                // in BOTH slots for this id (counted independently
                                // above).
                                let freed = self
                                    .transient
                                    .kitty_images
                                    .get(&id)
                                    .map(|img| img.bytes.len())
                                    .unwrap_or(0)
                                    + self
                                        .transient
                                        .kitty_frames
                                        .get(&id)
                                        .map(|fs| fs.iter().map(|f| f.bytes.len()).sum::<usize>())
                                        .unwrap_or(0);
                                self.transient.kitty_images.remove(&id);
                                self.transient.kitty_frames.remove(&id);
                                self.transient.kitty_total_bytes =
                                    self.transient.kitty_total_bytes.saturating_sub(freed);
                            }
                        }
                    }
                    // At the cursor: whichever image covers the cursor cell is
                    // cleared in full (ptr identity — one placement, all its
                    // cells); 'C' also frees that image's store entry.
                    Some('c' | 'C') => {
                        let at = self
                            .grid
                            .cell_extra(self.grid.cursor_row(), self.grid.cursor_col())
                            .and_then(|extra| extra.image())
                            .map(|placed| std::sync::Arc::clone(&placed.image));
                        if let Some(img) = at {
                            self.clear_kitty_placements(&|placed| {
                                std::sync::Arc::ptr_eq(placed, &img)
                            });
                            if cmd.delete_target == Some('C') {
                                let id = self
                                    .transient
                                    .kitty_images
                                    .iter()
                                    .find(|(_, stored)| std::sync::Arc::ptr_eq(stored, &img))
                                    .map(|(id, _)| *id);
                                if let Some(id) = id {
                                    let freed = img.bytes.len()
                                        + self
                                            .transient
                                            .kitty_frames
                                            .get(&id)
                                            .map(|fs| {
                                                fs.iter().map(|f| f.bytes.len()).sum::<usize>()
                                            })
                                            .unwrap_or(0);
                                    self.transient.kitty_images.remove(&id);
                                    self.transient.kitty_frames.remove(&id);
                                    self.transient.kitty_total_bytes =
                                        self.transient.kitty_total_bytes.saturating_sub(freed);
                                }
                            }
                        }
                    }
                    // Selectors this engine cannot address yet: by point (p/P —
                    // the parser reads no x=/y= keys), by number (n/N — numbers
                    // are not mapped to ids at transmit), by placement id (q/Q),
                    // by column/row/z (x/y/z). Deleting NOTHING is the honest
                    // fallback: it is recoverable, matches the advertised
                    // `kitty_graphics = false` posture, and is strictly closer to
                    // the spec than the previous behavior — which answered every
                    // one of these by destroying the entire store.
                    Some(_) => {}
                }
            }
            KittyAction::Display => {
                if let Some(id) = cmd.id
                    && let Some(image) = self.transient.kitty_images.get(&id).cloned()
                {
                    let (cols, rows) = (image.cols, image.rows);
                    let at = self.grid.cursor_col();
                    self.place_image(&image, cols, rows, at);
                }
            }
            KittyAction::Transmit | KittyAction::TransmitAndDisplay => {
                let Some(image) = self.build_kitty_image(&mut cmd) else {
                    return;
                };
                let image = std::sync::Arc::new(image);
                if let Some(id) = cmd.id {
                    // A base transmit stores the frame in TWO slots (kitty_images[id]
                    // and kitty_frames[id][0]); account each slot independently.
                    let add = image.bytes.len().saturating_mul(2);
                    // Bytes reclaimed by replacing an existing id's image + frames.
                    let freed = self
                        .transient
                        .kitty_images
                        .get(&id)
                        .map(|img| img.bytes.len())
                        .unwrap_or(0)
                        + self
                            .transient
                            .kitty_frames
                            .get(&id)
                            .map(|fs| fs.iter().map(|f| f.bytes.len()).sum::<usize>())
                            .unwrap_or(0);
                    let projected = self
                        .transient
                        .kitty_total_bytes
                        .saturating_sub(freed)
                        .saturating_add(add);
                    let store = &mut self.transient.kitty_images;
                    // Cap the store (count DoS bound); an existing id may always
                    // update. The GLOBAL byte budget (fail-closed) additionally
                    // rejects a transfer that would push the store over
                    // MAX_KITTY_STORE_BYTES, so the per-item caps can't multiply.
                    if (store.len() < MAX_KITTY_IMAGES || store.contains_key(&id))
                        && projected <= MAX_KITTY_STORE_BYTES
                    {
                        store.insert(id, std::sync::Arc::clone(&image));
                        // A fresh base transmit resets the animation frame list to
                        // just this frame (frame 1); `a=f` appends, `a=a r=N` selects.
                        self.transient
                            .kitty_frames
                            .insert(id, vec![std::sync::Arc::clone(&image)]);
                        self.transient.kitty_total_bytes = projected;
                    }
                }
                if cmd.action == KittyAction::TransmitAndDisplay {
                    let (cols, rows) = (image.cols, image.rows);
                    let at = self.grid.cursor_col();
                    self.place_image(&image, cols, rows, at);
                }
            }
            // Support probe: report OK (we support core transmit/display). The
            // success response is suppressed by q>=1 — that Query then falls to the
            // `_` arm (no response). Echo the id (i=) or number (I=) the client used.
            KittyAction::Query if cmd.quiet == 0 => {
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
                let medium_works = cmd.medium
                    == crate::terminal::kitty_graphics::KittyMedium::Direct
                    || self.kitty_file_resolver.is_some();
                let verdict = if medium_works {
                    "OK"
                } else {
                    "ENOTSUPPORTED:medium disabled (allow_kitty_file_transfer)"
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
                // other PTY reply — handler.rs::send_response) so the capability /
                // policy-ALLOW / rate-limit / buffer-cap gates apply. Writing
                // straight to `response_buffer` here bypassed all of them — e.g. a
                // host with `response any = Drop` could not suppress this echo.
                let cap =
                    super::super::response_capability::ResponseCapability::mint_for_dispatch();
                self.send_response(&cap, r.as_bytes());
            }
            // a=f: transmit an ANIMATION FRAME for an existing image — decode it like
            // a base transmit and append to the image's frame list (capped).
            KittyAction::Frame => {
                if let Some(id) = cmd.id
                    && self.transient.kitty_frames.contains_key(&id)
                    && let Some(frame) = self.build_kitty_image(&mut cmd)
                {
                    let add = frame.bytes.len();
                    let total = self.transient.kitty_total_bytes;
                    let frames = self.transient.kitty_frames.entry(id).or_default();
                    // Per-id frame-count cap AND the global byte budget (fail-closed):
                    // a frame that would push the store over MAX_KITTY_STORE_BYTES is
                    // dropped so the caps can't multiply into a resident OOM.
                    if frames.len() < MAX_KITTY_FRAMES
                        && total.saturating_add(add) <= MAX_KITTY_STORE_BYTES
                    {
                        frames.push(std::sync::Arc::new(frame));
                        self.transient.kitty_total_bytes = total.saturating_add(add);
                    }
                }
            }
            // a=a: animation control. The only frame-management action that does not
            // need a wall-clock timer (which is the renderer's job) is selecting the
            // CURRENT frame via `r=N` (1-based) — re-point `kitty_images[id]` at it so
            // every render path (direct + placeholder) shows frame N. Play/stop/gap
            // timing is left to the frame-pacing consumer.
            KittyAction::Animate => {
                if let Some(id) = cmd.id
                    && let Some(n) = cmd.rows
                    && n >= 1
                    && let Some(frames) = self.transient.kitty_frames.get(&id)
                    && let Some(frame) = frames.get((n - 1) as usize).cloned()
                {
                    // Re-pointing the kitty_images[id] slot at frame N adds no NEW
                    // allocation (the Arc is shared with kitty_frames[id][n-1]), but
                    // the per-slot byte accounting must track the slot swapping from
                    // its old frame's bytes to frame N's, so a later delete decrements
                    // precisely without drift.
                    let old = self
                        .transient
                        .kitty_images
                        .get(&id)
                        .map(|img| img.bytes.len())
                        .unwrap_or(0);
                    let new = frame.bytes.len();
                    self.transient.kitty_images.insert(id, frame);
                    self.transient.kitty_total_bytes = self
                        .transient
                        .kitty_total_bytes
                        .saturating_sub(old)
                        .saturating_add(new);
                }
            }
            // Quiet query, or unsupported actions: no response — degrade gracefully.
            _ => {}
        }
    }

    /// Build an [`aterm_grid::ImageData`] from a single-chunk Kitty transmit
    /// command, or `None` if the payload is missing/oversized or the dimensions are
    /// invalid. PNG keeps its bytes (the renderer decodes); raw `f=32`/`f=24`
    /// become `RawRgba8` (RGB expands to opaque RGBA). The cell footprint is the
    /// explicit `c`/`r`, else pixel size ÷ the renderer cell size (`iterm2.cell_px`)
    /// rounded up, clamped to the grid.
    ///
    /// `&mut cmd` so the direct-uncompressed payload (the common `kitten icat`
    /// case, up to 4 MiB) MOVES into the image instead of being memcpy'd; the
    /// caller must not read `cmd.payload` afterwards. All other fields are
    /// left untouched.
    fn build_kitty_image(
        &self,
        cmd: &mut crate::terminal::kitty_graphics::KittyCommand,
    ) -> Option<aterm_grid::ImageData> {
        use crate::terminal::kitty_graphics::{
            KittyFormat, KittyMedium, png_dimensions, rgb_to_rgba,
        };
        use aterm_grid::{ImageData, ImageFormat};
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
        let cell_w = u32::from(self.iterm2.cell_px.0.max(1));
        let cell_h = u32::from(self.iterm2.cell_px.1.max(1));
        let cols = cmd.columns.unwrap_or_else(|| px_w.div_ceil(cell_w)).max(1);
        let rows = cmd.rows.unwrap_or_else(|| px_h.div_ceil(cell_h)).max(1);
        // Clamp the footprint to the grid so a huge image can't request an enormous
        // cell span.
        let cols = u16::try_from(cols)
            .unwrap_or(u16::MAX)
            .min(self.grid.cols())
            .max(1);
        let rows = u16::try_from(rows)
            .unwrap_or(u16::MAX)
            .min(self.grid.rows())
            .max(1);
        Some(ImageData {
            bytes,
            format,
            cols,
            rows,
            // Kitty z=: negative draws behind text. iTerm2/Sixel + z=0 default to 0.
            z_index: cmd.z_index.unwrap_or(0),
            band_lift_px: 0,
        })
    }
}

/// Maximum Kitty images retained in the per-screen store (DoS bound).
const MAX_KITTY_IMAGES: usize = 256;
/// Maximum animation frames retained per Kitty image (DoS bound).
const MAX_KITTY_FRAMES: usize = 128;
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

        // a=a r=2: select frame 2 → the SAME placeholder now shows the 0xAB frame.
        term.process(&apc_g("a=a,i=7,r=2", b""));
        let f = term.cell_frame(24, 80);
        assert_eq!(
            f.images[0][0].1.image.bytes[0], 0xAB,
            "a=a r=2 must switch the displayed frame to frame 2"
        );

        // a=a r=1: back to frame 1.
        term.process(&apc_g("a=a,i=7,r=1", b""));
        assert_eq!(
            term.cell_frame(24, 80).images[0][0].1.image.bytes[0],
            0x00,
            "a=a r=1 returns to frame 1"
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
}
