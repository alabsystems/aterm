// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `ActionSink` for `TerminalHandler`: parser actions (print, C0/C1, CSI, DCS, APC) dispatched
//! to typed handlers, the bulk ASCII writers, and the Kitty graphics protocol (APC `G`).

use crate::grid::{CellFlags, PackedColor};
use crate::parser::ActionSink;
use aterm_provenance::{Provenance, Pty};
use aterm_types::charset::{GlMapping, SingleShift};

use super::super::handler_osc_1337::PlacementCursor;
use super::super::kitty_floating::{self, MAX_KITTY_FLOATING, MAX_KITTY_RELATIVE_DEPTH};
use super::super::transient_state::{
    KittyAnimation, KittyFloatParent, KittyFloating, KittyFrame, KittyParent, KittyPlayState,
    KittyRelation, KittyVariant, KittyVirtual, TransientState,
};
use super::{TerminalHandler, Vt52CursorState};
use crate::terminal::kitty_graphics::{self, DuringLoad, KittyAction, KittyCommand, KittyMedium};
use aterm_grid::{
    Grid, ImageData, ImageFormat, ImageRef, ImageScaling, KittyPlacementTag, SourceRect,
};
use std::borrow::Cow;
use std::collections::HashSet;
use std::num::NonZeroU32;
use std::sync::Arc;

impl TerminalHandler<'_> {
    /// Stamp the echo anchor: one past the last glyph of this print action's run (a glyph at
    /// the last column with the wrap deferred reports the column past it), plus a monotonic
    /// action count. Called once per print action; observability only, nothing decides on it.
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

    /// Fast path for a run of printable ASCII (0x20-0x7E): the bulk writers when charset,
    /// modes and margins allow, else per-character `write_char`.
    fn print_ascii_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        self.note_sync_open_action();
        let mut data = data.as_ref();
        // VT52 direct cursor addressing (ESC Y) takes the next two printables as row and
        // column; the rest of the run is text, written as if it came in the next read.
        while self.transient.vt52_cursor_state != Vt52CursorState::None {
            let Some((&byte, rest)) = data.split_first() else {
                return;
            };
            self.print(byte as char);
            data = rest;
        }
        if data.is_empty() {
            return;
        }

        // Charset translation, insert mode and no-autowrap act per position, so they go per
        // character. So does a run starting outside a DECSLRM span: the run writers clamp at
        // the right margin only from a start inside it.
        if !self.charset.is_ascii_passthrough()
            || self.modes.insert_mode
            || !self.modes.auto_wrap
            || self.grid.cursor_outside_margin_span()
        {
            for &byte in data {
                self.write_char(byte as char);
            }
            self.stamp_print_anchor();
            return;
        }

        // Selection custody: output damages the rows it overwrites (`write_char` brackets the
        // per-character paths). One bracket per run: the absolute frame is invariant, so a run
        // that wraps and scrolls is still named by its first and last cursor row.
        let origin = self.grid.output_damage_origin();
        // Both flags are cached at mutation time.
        if self.style.has_style_extras() || self.transient.has_transient_extras {
            self.write_ascii_bulk_with_extras(data);
        } else {
            self.write_ascii_bulk_fast(data);
        }
        // A printed glyph that does not fold ends a ZWJ continuation, as it
        // does in `write_char`: an emoji later written next to the ZWJ's cell
        // opens its own cell whichever writer printed this run.
        self.transient.last_combining_was_zwj = false;
        self.grid.damage_selection_output(origin);
        self.stamp_print_anchor();
    }

    /// Fast path: print a run of decoded non-ASCII characters, amortizing
    /// per-character overhead (charset, clipboard, style checks) over the run.
    /// Falls back to per-character for complex cases.
    fn print_unicode_bulk(&mut self, chars: &Provenance<[char], Pty>) {
        self.note_sync_open_action();
        let mut chars = chars.as_ref();
        // VT52 cursor addressing takes the next two characters; the rest of the
        // run is text (see `print_ascii_bulk`).
        while self.transient.vt52_cursor_state != Vt52CursorState::None {
            let Some((&c, rest)) = chars.split_first() else {
                return;
            };
            self.print(c);
            chars = rest;
        }
        if chars.is_empty() {
            return;
        }

        // Selection custody, as in `print_ascii_bulk`: one bracket for the run.
        // `write_unicode_bulk`'s per-character fallbacks go through
        // `write_char`, which brackets itself; the extra record is idempotent.
        let origin = self.grid.output_damage_origin();
        self.write_unicode_bulk(chars);
        self.grid.damage_selection_output(origin);
        self.stamp_print_anchor();
    }

    /// Execute a C0 or C1 control character (CAN/SUB are the parser's).
    fn execute(&mut self, byte: u8) {
        self.note_sync_open_action();
        // Per VT220 spec: a control character arriving mid-sequence cancels
        // any in-progress ESC Y cursor addressing (VT52 mode).
        self.transient.vt52_cursor_state = Vt52CursorState::None;

        // Per VT220 spec: SS2/SS3 single-shift is cleared on any control
        // character, not just on the next graphic character.
        self.charset.clear_single_shift();

        match byte {
            // C0 control codes (0x00-0x1F)
            0x07 => self.handle_bell(),
            0x08 => {
                // BS. Per VT510: when DECLRMM is active, the "left margin" for BS
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
                } else {
                    self.grid
                        .cursor_backward_margin(1, self.modes.left_right_margin_mode);
                }
            }
            0x09 => self.grid.tab_margin(self.modes.left_right_margin_mode), // HT
            0x0A..=0x0C => {
                // LF, VT, FF; in new line mode (LNM) also CR.
                if self.modes.new_line_mode {
                    self.grid
                        .carriage_return_margin(self.modes.left_right_margin_mode);
                }
                // Line feed; with DECLRMM active, scrolling at the boundary stays
                // within the horizontal margins (VT510).
                self.margined_line_feed(self.modes.left_right_margin_mode);
            }
            0x0D => self
                .grid
                .carriage_return_margin(self.modes.left_right_margin_mode), // CR
            0x0E => self.charset.gl = GlMapping::G1, // SO: G1 into GL
            0x0F => self.charset.gl = GlMapping::G0, // SI: G0 into GL

            // C1 control codes (0x80-0x9F): 8-bit forms of ESC sequences.
            0x84 => {
                // IND, as ESC D.
                self.margined_line_feed(self.modes.left_right_margin_mode);
            }
            0x85 => {
                // NEL, as ESC E.
                self.grid
                    .carriage_return_margin(self.modes.left_right_margin_mode);
                self.margined_line_feed(self.modes.left_right_margin_mode);
            }
            0x88 => self.grid.set_tab_stop(), // HTS - same as ESC H
            0x8D => {
                // RI, as ESC M.
                self.grid
                    .reverse_line_feed_margined(self.modes.left_right_margin_mode);
            }
            0x8E => self.charset.single_shift = SingleShift::Ss2, // SS2 - same as ESC N
            0x8F => self.charset.single_shift = SingleShift::Ss3, // SS3 - same as ESC O

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
        // Mark before dispatch. The opening `?2026h` observes the old inactive
        // level and therefore starts clean; a redundant `?2026h` inside an
        // already-dirty window cannot launder the dirty bit.
        self.note_sync_open_action();
        self.sync_absolute_row_metadata();
        let params = params.as_ref();
        let intermediates = intermediates.as_ref();
        // VT52 has no CSI (ESC [ is not a VT52 escape).
        if self.modes.vt52_mode {
            return;
        }
        // A zero-sized response capability for this dispatch (see `response_capability.rs`).
        // The engine gates each response at `send_response`, not the mint: a denied mint would
        // suppress all CSI handling.
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        // Fast path: no intermediates (the vast majority of CSI sequences).
        if intermediates.is_empty() {
            self.csi_dispatch_no_intermediates(&cap, params, final_byte);
            return;
        }
        // With intermediates: an unrecognized one is ignored, never falling through to the
        // plain handler (`CSI # h` is not `CSI h`).
        let _ = self.csi_dispatch_with_intermediates(&cap, params, intermediates, final_byte);
    }

    /// CSI with colon subparameters (`ESC[4:3m`); `subparam_mask` marks the params preceded by
    /// a colon.
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
        // Only SGR takes subparameters; everything else dispatches normally.
        if final_byte == b'm' && intermediates.as_ref().is_empty() {
            self.sgr_style()
                .handle_sgr_with_subparams(params.as_ref(), subparam_mask);
            return;
        }
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
        self.osc_dispatch_with_terminator(params, false);
    }

    /// Dispatch OSC with terminator info for response echo.
    fn osc_dispatch_with_terminator(
        &mut self,
        params: &Provenance<[&[u8]], Pty>,
        bel_terminated: bool,
    ) {
        self.note_sync_open_action();
        self.sync_absolute_row_metadata();
        // VT52 has no OSC.
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
        // VT52 has no DCS.
        if self.modes.vt52_mode {
            return;
        }
        self.dcs_hook_inner(params.as_ref(), intermediates.as_ref(), final_byte);
    }

    /// Accumulate data bytes for the current DCS sequence.
    fn dcs_put(&mut self, byte: u8) {
        self.dcs_put_inner(byte);
    }

    /// Bulk-accumulate DCS data (the parser's passthrough fast path hands over near-whole PTY
    /// chunks): budget, caps and Sixel allocation sampling run once per run. Parity with
    /// `dcs_put` is at `dcs_put_bulk_inner`.
    fn dcs_put_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        self.dcs_put_bulk_inner(data.as_ref());
    }

    /// Finish a DCS string, minting the response capability DECRQSS/XTGETTCAP answers need.
    /// `canceled` (CAN/SUB, or an ESC that does not begin ST) discards the string: a
    /// half-decoded sixel is dropped and a query is not answered.
    fn dcs_unhook(&mut self, canceled: bool) {
        self.note_sync_open_action();
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        self.dcs_unhook_inner(&cap, canceled);
    }

    fn apc_start(&mut self) {
        // VT52 has no APC.
        if self.modes.vt52_mode {
            return;
        }
        // Release an abandoned DCS's budget, or a DCS without ST followed by APC would leak
        // its sequence_bytes.
        self.dcs.total_bytes = self.dcs.total_bytes.saturating_sub(self.dcs.sequence_bytes);
        self.dcs.sequence_bytes = 0;
        // Drop an abandoned Sixel decoder before clearing dcs_type, without the full-size
        // copy `unhook` makes.
        if matches!(self.dcs.dcs_type, super::super::DcsType::Sixel) {
            self.sixel.decoder.abort();
        }
        self.dcs.dcs_type = super::super::DcsType::None;
        self.dcs.data.clear(); // Reuse dcs_data buffer for APC
    }

    fn apc_put(&mut self, byte: u8) {
        // APC shares `dcs.data`, so it counts against the global DCS budget.
        if self.dcs.total_bytes >= super::super::MAX_DCS_GLOBAL_BUDGET {
            return;
        }
        // Count every byte, even past the data cap, so the budget throttles an APC flood.
        self.dcs.total_bytes += 1;
        self.dcs.sequence_bytes += 1;
        // Up to 4 MiB per APC sequence for Kitty graphics; the global budget caps the total.
        if self.dcs.data.len() < 4 * 1024 * 1024 {
            self.dcs.data.push(byte);
        }
    }

    fn apc_put_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        // Bulk `apc_put`: byte-identical to it
        // (`bulk_string_accumulators_cross_caps_like_per_byte`). Only
        // `MAX_DCS_GLOBAL_BUDGET - total_bytes` of the run are counted; bytes past that are
        // neither counted nor pushed. Counted byte k is pushed iff `data.len() + k < 4 MiB`.
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

    /// End an APC string. One cut short by CAN/SUB (`canceled`) is discarded unparsed, and a
    /// kitty one also ends the chunked transfer in flight; either way the budget and buffer
    /// are released as for a complete one.
    fn apc_end(&mut self, canceled: bool) {
        self.note_sync_open_action();
        let kitty = self.dcs.data.first() == Some(&b'G');
        if kitty {
            self.transient.kitty_commands = self.transient.kitty_commands.wrapping_add(1);
        }
        if canceled && kitty {
            // A canceled kitty command was a chunk of any transfer in flight, which can no
            // longer complete intact: drop it, or the next complete command would be taken as
            // its continuation.
            self.transient.kitty_pending = None;
        }
        // `parse_kitty_command` returns an owned command, so the borrow on `dcs.data` ends
        // before `handle_kitty_command` mutates the grid and store.
        let cmd = if !canceled && kitty {
            crate::terminal::kitty_graphics::parse_kitty_command(&self.dcs.data)
        } else {
            None
        };
        if let Some(cmd) = cmd {
            self.handle_kitty_command(cmd);
        }
        self.dcs.total_bytes = self.dcs.total_bytes.saturating_sub(self.dcs.sequence_bytes);
        self.dcs.sequence_bytes = 0;
        // Clear the buffer and reclaim memory from a large payload, as DCS unhook does.
        self.dcs.data.clear();
        if self.dcs.data.capacity() > 4096 {
            self.dcs.data.shrink_to(128);
        }
    }
}

/// Kitty graphics (APC `G`), called from `apc_end`.
impl TerminalHandler<'_> {
    /// Handle one parsed Kitty command, assembling chunked transfers (`m=1`). As in kitty, a
    /// command naming both `i=` and `I=` (`EINVAL`) and a query without `i=` are handled
    /// first, never as chunks. Only a first chunk kitty would hold
    /// ([`kitty_would_hold`](Self::kitty_would_hold)) opens a load; while one is in flight,
    /// [`KittyCommand::during_load`] says whether a command continues it, ends it, or runs
    /// beside it. A prompt or command start (`dispatch_shell_mark`) and RIS also end it. The
    /// payload is bounded by `MAX_KITTY_IMAGE_BYTES`.
    fn handle_kitty_command(&mut self, cmd: KittyCommand) {
        if cmd.id.is_some() && cmd.number.is_some() {
            self.kitty_reply(&cmd, None, "EINVAL:i and I are exclusive");
            return;
        }
        if cmd.action == KittyAction::Query && cmd.id.is_none() {
            self.handle_complete_kitty_command(cmd);
            return;
        }
        let during_load = cmd.during_load();
        if self.transient.kitty_pending.is_some() {
            match during_load {
                DuringLoad::Continues => {}
                DuringLoad::Ends => self.transient.kitty_pending = None,
                DuringLoad::Beside => {
                    self.handle_complete_kitty_command(cmd);
                    return;
                }
            }
        }
        if self.transient.kitty_pending.is_some()
            || (cmd.more && during_load == DuringLoad::Continues && self.kitty_would_hold(&cmd))
        {
            let cur_len = self
                .transient
                .kitty_pending
                .as_ref()
                .map_or(0, |p| p.payload.len());
            // Fail closed before touching the buffer: overflow or an armed alloc fault aborts.
            if crate::fault::triggered("kitty.chunk_alloc")
                || cur_len.saturating_add(cmd.payload.len()) > MAX_KITTY_IMAGE_BYTES
            {
                self.transient.kitty_pending = None; // abort the overflowing transfer
                return;
            }
            let Some(pending) = self.transient.kitty_pending.as_mut() else {
                // First chunk: the owned command seeds the accumulator without a copy.
                self.transient.kitty_pending = Some(cmd);
                return;
            };
            // Reserve fallibly: an OOM drops the transfer, not the process.
            if pending.payload.try_reserve(cmd.payload.len()).is_ok() {
                pending.payload.extend_from_slice(&cmd.payload);
                // A chunk's `q=` holds for the whole load's answer, as in kitty.
                if cmd.quiet != 0 {
                    pending.quiet = cmd.quiet;
                }
            } else {
                self.transient.kitty_pending = None; // OOM: drop the transfer, fail closed
                return;
            }
            if cmd.more {
                return; // more chunks to come
            }
            if let Some(assembled) = self.transient.kitty_pending.take() {
                self.handle_complete_kitty_command(assembled);
            }
            return;
        }
        self.handle_complete_kitty_command(cmd);
    }

    /// Whether kitty would hold a load for `cmd`, a first chunk (`m=1`) of a direct transmit,
    /// query or frame: it declares a loadable size and, for a frame, names a stored image.
    /// kitty refuses any other such chunk at once. Only first chunks carry `s=`/`v=`.
    fn kitty_would_hold(&self, cmd: &KittyCommand) -> bool {
        cmd.declares_loadable_size()
            && (cmd.action != KittyAction::Frame
                || self
                    .resolve_kitty_id(cmd)
                    .is_some_and(|id| self.transient.kitty_frames.contains_key(&id)))
    }

    /// Handle one complete Kitty command. A command naming `i=`/`I=` is answered as kitty
    /// answers ([`kitty_reply`](Self::kitty_reply)); the text after an error code is aterm's.
    /// Placements are layers: each covered cell stacks the tile in kitty's draw order (z-index,
    /// then first transmit, then placement order: [`aterm_grid::ImageRef::draw_key`]), so
    /// deleting one uncovers what lay beneath; an iTerm2 or sixel image takes its cells whole.
    ///
    /// Where aterm differs from kitty:
    ///
    ///   * a `C=1` placement is clipped at the last row instead of hanging below it;
    ///   * a cell holds at most `MAX_IMAGE_LAYERS` (16) layers; a 17th pushes out the lowest;
    ///   * an unnamed placement put exactly over an unnamed one of the same image replaces it
    ///     ([`replace_covered_kitty`](Self::replace_covered_kitty)); kitty keeps both, which
    ///     differs only where the newer is translucent;
    ///   * at most `MAX_KITTY_FLOATING` (16) placements relative to a virtual one, and
    ///     `MAX_KITTY_VARIANTS` (16) differently laid-out placements of one image shown at
    ///     once; past either, `ENOSPC`;
    ///   * animations play on the live screen only: in history an image keeps its frame;
    ///   * a web embed plays animations only if its page calls the `advance_kitty_animations`
    ///     export, and no page in this repository does.
    fn handle_complete_kitty_command(&mut self, cmd: KittyCommand) {
        match cmd.action {
            KittyAction::Delete => {
                self.delete_kitty_placements(&cmd);
                self.prune_kitty_floating();
            }
            KittyAction::Display => self.put_kitty(&cmd),
            KittyAction::Transmit | KittyAction::TransmitAndDisplay => self.transmit_kitty(cmd),
            // The support probe; `q>=1` suppresses its answer.
            KittyAction::Query if cmd.quiet == 0 => self.answer_kitty_query(&cmd),
            KittyAction::Query => {}
            KittyAction::Frame => self.load_kitty_frame(cmd),
            KittyAction::Compose => self.compose_kitty_frames(&cmd),
            KittyAction::Animate => self.control_kitty_animation(&cmd),
        }
    }

    /// `a=p`: show a stored image (`i=` or `I=`), laid out by the put's own keys.
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

    /// Show `image` (image `id`, laid out for `cmd`): virtual for `U=1`, relative for `P=`,
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
        let shown = match cmd.parent_id {
            Some(parent) => self.place_kitty_relative(image, id, cmd, parent),
            None => {
                self.place_kitty(image, id, cmd);
                Ok(())
            }
        };
        // A placement put again elsewhere may leave its relatives floating without a parent.
        self.prune_kitty_floating();
        shown
    }

    /// `a=t`/`a=T`: decode and store under `i=` (or a fresh id for `I=`); `a=T` also shows it.
    fn transmit_kitty(&mut self, mut cmd: KittyCommand) {
        if !self.kitty_medium_works(cmd.medium) {
            // The probe's verdict for this medium: the data was never read.
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
                // Shown even when the store refused it: only a later put by id fails.
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

    /// `a=q` (not quieted): the support probe, answered even with no `i=` (echoing any `I=`;
    /// kitty answers none). It names only whether the medium works: a size kitty refuses is
    /// still `OK` on the direct medium (kitty: `EINVAL`), and opens no load.
    fn answer_kitty_query(&mut self, cmd: &KittyCommand) {
        use core::fmt::Write as _;
        // Clients probe before choosing a medium (kitty's icat falls back to direct on an
        // error), so the answer names whether this one works.
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
        // The single response sink: capability, rate limiter and buffer cap apply.
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        self.send_response(&cap, r.as_bytes());
    }

    /// Whether this session can read a transmission over `medium`: direct always, the others
    /// only with the host's opt-in resolver.
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

    /// Answer as kitty does: only a command naming an image (`i=` or `I=`); `OK` unless
    /// `q>=1`, an error unless `q>=2`; naming the frame (`r=`) of an `a=f` or `a=a`. `id` is
    /// the image it resolved to. Callers decide what is answered at all: deletes never, an
    /// animation control only for a missing image.
    fn kitty_reply(&mut self, cmd: &KittyCommand, id: Option<u32>, verdict: &str) {
        let frame = matches!(cmd.action, KittyAction::Frame | KittyAction::Animate)
            .then_some(cmd.rows)
            .flatten()
            .filter(|&frame| frame != 0)
            .and_then(|frame| usize::try_from(frame).ok());
        self.kitty_reply_naming(cmd, id, frame, verdict);
    }

    /// [`kitty_reply`](Self::kitty_reply) naming frame `frame` (`r=`), as kitty's answer to a
    /// frame load does.
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
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        self.send_response(&cap, r.as_bytes());
    }

    /// Store `image` under `id`, returning whether it fit. A base transmit fills two slots
    /// (`kitty_images[id]` and the root of `kitty_frames[id]`), each charged to the budget.
    /// The store is capped by count (`MAX_KITTY_IMAGES`; an existing id may always update)
    /// and by `MAX_KITTY_STORE_BYTES`. Re-transmitting an id replaces its data and, as in
    /// kitty, removes its placements (on screen and virtual, with their relatives) and
    /// variants.
    fn store_kitty_image(&mut self, id: u32, image: &Arc<ImageData>) -> bool {
        let replacing = self.transient.kitty_images.contains_key(&id);
        let add = image.bytes.len().saturating_mul(2);
        let fits = |t: &TransientState| {
            t.kitty_total_bytes
                .saturating_sub(kitty_bytes_held(t, id))
                .saturating_add(add)
                <= MAX_KITTY_STORE_BYTES
        };
        if !((self.transient.kitty_images.len() < MAX_KITTY_IMAGES || replacing)
            && self.reclaim_kitty_variants(None, &fits))
        {
            return false;
        }
        // Sent again, an image keeps its place among images of one z-index.
        let image_serial = match self.transient.kitty_frames.get(&id) {
            Some(animation) => animation.image_serial,
            None => self.next_kitty_serial(),
        };
        if replacing {
            self.transient.kitty_virtual.remove(&id);
            self.delete_kitty_where(&|tag, _| tag.image_id == id);
            self.prune_kitty_floating();
            // The delete may already have freed the image (an orphaned relative placement).
            let held = kitty_bytes_held(self.transient, id);
            let t = &mut *self.transient;
            let released = kitty_payload_held(t, id);
            charge_released_kitty(t, released);
            t.kitty_variants.remove(&id);
            t.kitty_total_bytes = t.kitty_total_bytes.saturating_sub(held);
        }
        let t = &mut *self.transient;
        t.kitty_images.insert(id, Arc::clone(image));
        // A base transmit resets the animation to this one frame, stopped.
        t.kitty_frames.insert(
            id,
            KittyAnimation::new(Arc::clone(image), image_serial, t.process_now),
        );
        t.kitty_total_bytes = t.kitty_total_bytes.saturating_add(add);
        self.damage_kitty_placeholders(id);
        true
    }

    /// Free everything the store holds for `id` (image, frames, variants, virtual placements)
    /// and forget every number naming it. Placements on either grid keep their own `Arc`s, so
    /// the bytes are charged to both screens' live-image budgets ([`charge_released_kitty`]).
    fn free_kitty_image(&mut self, id: u32) {
        let held = kitty_bytes_held(self.transient, id);
        let t = &mut *self.transient;
        let released = kitty_payload_held(t, id);
        charge_released_kitty(t, released);
        t.kitty_images.remove(&id);
        t.kitty_frames.remove(&id);
        t.kitty_variants.remove(&id);
        t.kitty_virtual.remove(&id);
        t.kitty_numbers.retain(|_, named| *named != id);
        t.kitty_total_bytes = t.kitty_total_bytes.saturating_sub(held);
        self.damage_kitty_placeholders(id);
    }

    /// Whether the Kitty store holds `image` for image `id` (as its image, a frame or a
    /// variant), so `MAX_KITTY_STORE_BYTES` already bounds it.
    pub(in crate::terminal) fn kitty_store_holds(&self, id: u32, image: &Arc<ImageData>) -> bool {
        let t = &*self.transient;
        t.kitty_images
            .get(&id)
            .is_some_and(|held| Arc::ptr_eq(held, image))
            || t.kitty_frames.get(&id).is_some_and(|animation| {
                animation
                    .frames
                    .iter()
                    .any(|frame| Arc::ptr_eq(&frame.image, image))
            })
            || t.kitty_variants
                .get(&id)
                .is_some_and(|variants| variants.iter().any(|v| Arc::ptr_eq(&v.image, image)))
    }

    /// Every image the Kitty store holds (images, frames, variants), by address.
    pub(in crate::terminal) fn kitty_store_images(&self) -> HashSet<*const ImageData> {
        let t = &*self.transient;
        t.kitty_images
            .values()
            .map(Arc::as_ptr)
            .chain(
                t.kitty_frames
                    .values()
                    .flat_map(|animation| animation.frames.iter().map(|f| Arc::as_ptr(&f.image))),
            )
            .chain(
                t.kitty_variants
                    .values()
                    .flatten()
                    .map(|v| Arc::as_ptr(&v.image)),
            )
            .collect()
    }

    /// The image a put displays: the stored one when the put's layout is its layout, else a
    /// re-laid-out variant ([`kitty_variant`](Self::kitty_variant)).
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

    /// `stored` (image `id`, or one of its frames) laid out as `layout`: itself when that is its
    /// layout, else a re-laid-out copy (the footprint is part of the payload the renderer
    /// decodes). A copy is charged to the budget and capped per id (`MAX_KITTY_VARIANTS`); one
    /// no placement shows is a cache entry, reused and reclaimed first
    /// ([`reclaim_kitty_variants`](Self::reclaim_kitty_variants)), so viewers that re-crop on
    /// every scroll recycle rather than exhaust them. Over a bound with every copy shown, the
    /// put fails closed with `ENOSPC`.
    fn kitty_variant(
        &mut self,
        id: u32,
        stored: &Arc<ImageData>,
        layout: KittyLayout,
    ) -> Result<Arc<ImageData>, &'static str> {
        if layout == KittyLayout::of(stored) {
            return Ok(Arc::clone(stored));
        }
        if let Some(variant) = self.transient.kitty_variants.get(&id).and_then(|variants| {
            variants
                .iter()
                .find(|v| Arc::ptr_eq(&v.source, stored) && layout == KittyLayout::of(&v.image))
        }) {
            return Ok(Arc::clone(&variant.image));
        }
        let add = stored.bytes.len();
        let counted = |t: &TransientState| {
            t.kitty_variants
                .get(&id)
                .is_none_or(|variants| variants.len() < MAX_KITTY_VARIANTS)
        };
        let fits =
            |t: &TransientState| t.kitty_total_bytes.saturating_add(add) <= MAX_KITTY_STORE_BYTES;
        if !(self.reclaim_kitty_variants(Some(id), &counted)
            && self.reclaim_kitty_variants(None, &fits))
        {
            return Err("ENOSPC:image store full");
        }
        // Fallible allocation: an OOM fails the put, never the process.
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(add)
            .map_err(|_| "ENOSPC:out of memory")?;
        bytes.extend_from_slice(&stored.bytes);
        let variant = Arc::new(layout.image(bytes, stored.format));
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

    /// Every image a Kitty placement shows now (on screen, virtual or floating), by address:
    /// a re-laid-out copy one of them shows is more than a cache entry.
    fn kitty_images_shown(&self) -> HashSet<*const ImageData> {
        let t = &*self.transient;
        self.kitty_layers()
            .map(|(_, _, layer)| Arc::as_ptr(&layer.image))
            .chain(
                t.kitty_virtual
                    .values()
                    .flatten()
                    .map(|v| Arc::as_ptr(&v.image)),
            )
            .chain(t.kitty_floating.iter().map(|f| Arc::as_ptr(&f.image)))
            .collect()
    }

    /// Drop the re-laid-out copies no placement shows (image `only`'s, or every image's),
    /// oldest first, until `enough` holds; returns whether it does.
    fn reclaim_kitty_variants(
        &mut self,
        only: Option<u32>,
        enough: &dyn Fn(&TransientState) -> bool,
    ) -> bool {
        if enough(self.transient) {
            return true;
        }
        let shown = self.kitty_images_shown();
        let t = &mut *self.transient;
        let mut ids: Vec<u32> = match only {
            Some(id) => vec![id],
            None => t.kitty_variants.keys().copied().collect(),
        };
        ids.sort_unstable();
        for id in ids {
            let mut at = 0;
            while !enough(t) {
                let Some(variants) = t.kitty_variants.get_mut(&id) else {
                    break;
                };
                let Some(variant) = variants.get(at) else {
                    break;
                };
                if shown.contains(&Arc::as_ptr(&variant.image)) {
                    at += 1;
                    continue;
                }
                let gone = variants.remove(at);
                t.kitty_total_bytes = t.kitty_total_bytes.saturating_sub(gone.image.bytes.len());
                // The parked grid may show it: no check here sees that grid.
                charge_released_kitty(t, gone.image.payload_bytes());
            }
        }
        t.kitty_variants.retain(|_, variants| !variants.is_empty());
        enough(t)
    }

    /// The next Kitty serial: placements and images first transmitted share
    /// one count, so each orders by when it happened.
    fn next_kitty_serial(&mut self) -> NonZeroU32 {
        let serial = self.transient.kitty_serial.wrapping_add(1).max(1);
        self.transient.kitty_serial = serial;
        NonZeroU32::new(serial).unwrap_or(NonZeroU32::MIN)
    }

    /// A fresh tag for one placement act of image `image_id` under
    /// `placement_id`. An image the store does not hold (an anonymous
    /// `a=T`) is first transmitted now.
    fn next_kitty_tag(&mut self, image_id: u32, placement_id: u32) -> KittyPlacementTag {
        let image_serial = match self.transient.kitty_frames.get(&image_id) {
            Some(animation) => animation.image_serial,
            None => self.next_kitty_serial(),
        };
        KittyPlacementTag {
            image_id,
            placement_id,
            image_serial,
            serial: self.next_kitty_serial(),
        }
    }

    /// Stamp a Kitty placement of `image` (image `id`, `0` for an anonymous `a=T`) at the
    /// cursor, under the command's placement id and cursor policy. A non-zero `p=` names one
    /// placement, so displaying it again moves it (relatives with it) and, without `P=`, makes
    /// it no longer relative; an unnamed one put exactly over another replaces it
    /// ([`replace_covered_kitty`](Self::replace_covered_kitty)).
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
        self.replace_covered_kitty(tag);
    }

    /// An unnamed placement just stamped (`tag`) replaces every older unnamed placement of the
    /// same image it covers exactly (same top-left cell and [`kitty_same_spot`]), as a redraw
    /// in place: an anonymous `a=T` each frame, or a put that re-crops. Kitty keeps the older
    /// one underneath; here each would hold a laid-out copy and a layer per cell. The older
    /// one's relatives follow the new one.
    fn replace_covered_kitty(&mut self, tag: KittyPlacementTag) {
        if tag.placement_id != 0 {
            return;
        }
        let Some((placed, row, col)) = self.kitty_placed(tag.serial) else {
            return;
        };
        let origin = |layer: &ImageRef, row: u16, col: u16| {
            (
                i32::from(row) - i32::from(layer.cell_row),
                i32::from(col) - i32::from(layer.cell_col),
            )
        };
        let at = origin(&placed, row, col);
        let mut covered: Vec<NonZeroU32> = Vec::new();
        for (row, col, layer) in self.kitty_layers() {
            if let Some(old) = layer.kitty
                && old.image_id == tag.image_id
                && old.placement_id == 0
                && old.serial != tag.serial
                && !covered.contains(&old.serial)
                && origin(layer, row, col) == at
                && kitty_same_spot(&layer.image, &placed.image)
            {
                covered.push(old.serial);
            }
        }
        if covered.is_empty() {
            return;
        }
        self.clear_kitty_placements(&|old, _| covered.contains(&old.serial));
        let new = KittyParent::of(tag);
        for relation in &mut self.transient.kitty_relations {
            if let KittyParent::Serial { serial, .. } = relation.parent
                && covered.contains(&serial)
            {
                relation.parent = new;
            }
        }
        self.prune_kitty_relations();
    }

    /// Take placement `placement_id` of image `id` off the screen (or the floating list) to
    /// put it again, keeping its relatives, and forget whether it was relative. Returns
    /// whether it was on screen; `0` names no single placement, so nothing moves.
    fn lift_kitty_placement(&mut self, id: u32, placement_id: u32) -> bool {
        if placement_id == 0 {
            return false;
        }
        self.transient
            .kitty_relations
            .retain(|r| !(r.child.image_id == id && r.child.placement_id == placement_id));
        let floating = self.transient.kitty_floating.len();
        self.transient
            .kitty_floating
            .retain(|f| !(f.tag.image_id == id && f.tag.placement_id == placement_id));
        if self.transient.kitty_floating.len() != floating {
            self.grid.damage_mut().mark_full();
        }
        !self
            .clear_kitty_placements(&|tag, _| {
                tag.image_id == id && tag.placement_id == placement_id
            })
            .is_empty()
    }

    /// `P=`/`Q=`: stamp `image` (image `id`) `H=` columns and `V=` rows from the top-left of a
    /// parent placement (the newest of image `parent_image` on screen, or placement `Q=`). The
    /// cursor never moves. A parent not on screen may be virtual, or relative to one: the
    /// placement then floats ([`place_kitty_floating`](Self::place_kitty_floating)). Refused
    /// as kitty refuses it: `ECYCLE`, or `ETOODEEP` past `MAX_KITTY_RELATIVE_DEPTH` parents.
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
            return self.place_kitty_floating(image, id, cmd, parent_image);
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
        self.replace_covered_kitty(tag);
        Ok(())
    }

    /// `P=`/`Q=` naming a virtual placement, or a placement relative to one: keep it beside the
    /// grid ([`kitty_floating`]), placed each frame `H=`/`V=` cells from where its parent
    /// shows (for a virtual parent, the top-left of its placeholders) and deleted with it. The
    /// cursor never moves. Refused as kitty refuses it (`ENOPARENT`, `ECYCLE`, `ETOODEEP` past
    /// `MAX_KITTY_RELATIVE_DEPTH`), and `ENOSPC` past `MAX_KITTY_FLOATING`. An unnamed one put
    /// again with the same parent, offset and [`kitty_same_spot`] layout replaces the older.
    fn place_kitty_floating(
        &mut self,
        image: &Arc<ImageData>,
        id: u32,
        cmd: &KittyCommand,
        parent_image: u32,
    ) -> Result<(), &'static str> {
        let wanted = cmd.parent_placement;
        let names = |tag: &KittyPlacementTag| {
            tag.image_id == parent_image && wanted.is_none_or(|q| tag.placement_id == q)
        };
        let t = &*self.transient;
        let parent = if let Some(v) = t
            .kitty_virtual
            .get(&parent_image)
            .and_then(|list| list.iter().rev().find(|v| names(&v.tag)))
        {
            KittyFloatParent::Virtual(KittyParent::of(v.tag))
        } else if let Some(f) = t.kitty_floating.iter().rev().find(|f| names(&f.tag)) {
            KittyFloatParent::Floating(KittyParent::of(f.tag))
        } else {
            return Err("ENOPARENT:no such parent placement");
        };
        let placement_id = cmd.placement.unwrap_or(0);
        self.check_kitty_float_ancestry(parent, id, placement_id)?;
        let (dx, dy) = (cmd.parent_dx.unwrap_or(0), cmd.parent_dy.unwrap_or(0));
        let replaces = |f: &KittyFloating| {
            f.tag.image_id == id
                && f.tag.placement_id == placement_id
                && (placement_id != 0
                    || (f.parent == parent
                        && (f.dx, f.dy) == (dx, dy)
                        && kitty_same_spot(&f.image, image)))
        };
        let t = &*self.transient;
        if t.kitty_floating.iter().filter(|f| !replaces(f)).count() >= MAX_KITTY_FLOATING {
            return Err("ENOSPC:too many relative placements");
        }
        self.lift_kitty_placement(id, placement_id);
        let tag = self.next_kitty_tag(id, placement_id);
        let mut covered: Vec<NonZeroU32> = Vec::new();
        self.transient.kitty_floating.retain(|f| {
            let gone = replaces(f);
            if gone {
                covered.push(f.tag.serial);
            }
            !gone
        });
        for f in &mut self.transient.kitty_floating {
            if let KittyFloatParent::Floating(KittyParent::Serial { serial, .. }) = f.parent
                && covered.contains(&serial)
            {
                f.parent = KittyFloatParent::Floating(KittyParent::of(tag));
            }
        }
        self.transient.kitty_floating.push(KittyFloating {
            tag,
            image: Arc::clone(image),
            parent,
            dx,
            dy,
        });
        self.grid.damage_mut().mark_full();
        Ok(())
    }

    /// [`check_kitty_ancestry`](Self::check_kitty_ancestry) for a floating
    /// placement of `(id, placement_id)` under `parent`.
    fn check_kitty_float_ancestry(
        &self,
        parent: KittyFloatParent,
        id: u32,
        placement_id: u32,
    ) -> Result<(), &'static str> {
        let t = &*self.transient;
        let is_self = |tag: KittyPlacementTag| {
            placement_id != 0 && tag.image_id == id && tag.placement_id == placement_id
        };
        let mut at = Some(parent);
        let mut depth = 0usize;
        while let Some(parent) = at {
            depth += 1;
            if depth > MAX_KITTY_RELATIVE_DEPTH {
                return Err("ETOODEEP:relative placements nest too deeply");
            }
            at = match parent {
                KittyFloatParent::Virtual(p) => {
                    if kitty_floating::named_virtual(t, p).is_some_and(|v| is_self(v.tag)) {
                        return Err("ECYCLE:the placement would be its own ancestor");
                    }
                    None
                }
                KittyFloatParent::Floating(p) => {
                    let Some(f) =
                        kitty_floating::named_floating(t, p).map(|i| &t.kitty_floating[i])
                    else {
                        break;
                    };
                    if is_self(f.tag) {
                        return Err("ECYCLE:the placement would be its own ancestor");
                    }
                    Some(f.parent)
                }
            };
        }
        Ok(())
    }

    /// Delete the floating placements whose parent is gone (and theirs), and the data of each
    /// image left with no placement.
    fn prune_kitty_floating(&mut self) {
        for _ in 0..=MAX_KITTY_IMAGES {
            let mut orphans: Vec<u32> = Vec::new();
            // A pass per level of parents gone: at most MAX_KITTY_FLOATING.
            loop {
                let t = &*self.transient;
                let dead: Vec<bool> = t
                    .kitty_floating
                    .iter()
                    .map(|f| !kitty_floating::parent_exists(t, f.parent))
                    .collect();
                if !dead.contains(&true) {
                    break;
                }
                let mut dead = dead.into_iter();
                self.transient.kitty_floating.retain(|f| {
                    let gone = dead.next().unwrap_or(false);
                    if gone {
                        push_unique(&mut orphans, f.tag.image_id);
                    }
                    !gone
                });
            }
            if orphans.is_empty() {
                return;
            }
            self.grid.damage_mut().mark_full();
            let mut freed = false;
            for id in orphans {
                if !self.kitty_image_placed(id) {
                    // Its virtual placements go too, which may orphan more.
                    self.free_kitty_image(id);
                    freed = true;
                }
            }
            if !freed {
                return;
            }
        }
    }

    /// Refuse a relative placement of `(id, placement_id)` under `parent` that would be its
    /// own ancestor (`ECYCLE`; only a named placement can close a cycle) or sit under more
    /// than `MAX_KITTY_RELATIVE_DEPTH` parents (`ETOODEEP`).
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

    /// Every Kitty placement layer on screen, with its cell `(row, col)`, in
    /// no particular order. Walks the extras that exist, not every cell.
    fn kitty_layers(&self) -> impl Iterator<Item = (u16, u16, &ImageRef)> + '_ {
        let (rows, cols) = (self.grid.rows(), self.grid.cols());
        self.grid
            .extras()
            .iter()
            .filter(move |(at, _)| at.row < rows && at.col < cols)
            .flat_map(|(at, extra)| {
                extra
                    .images()
                    .iter()
                    .filter(|layer| layer.kitty.is_some())
                    .map(move |layer| (at.row, at.col, layer))
            })
    }

    /// The tag of every Kitty placement layer on screen, once per cell it
    /// covers.
    fn kitty_tags_on_screen(&self) -> impl Iterator<Item = KittyPlacementTag> + '_ {
        self.kitty_layers().filter_map(|(_, _, layer)| layer.kitty)
    }

    /// The newest (highest-serial) Kitty placement on screen `selected`
    /// accepts.
    fn newest_kitty_placement(
        &self,
        selected: &dyn Fn(&KittyPlacementTag) -> bool,
    ) -> Option<KittyPlacementTag> {
        self.kitty_tags_on_screen()
            .filter(|tag| selected(tag))
            .max_by_key(|tag| tag.serial)
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
        self.kitty_layers()
            .find(|(_, _, layer)| layer.kitty.is_some_and(|tag| tag.serial == serial))
            .map(|(row, col, layer)| (layer.clone(), row, col))
    }

    /// Stack `image`'s footprint with its top-left tile at `(top, left)` —
    /// which may lie off screen — onto the cells that are on screen, damaging
    /// each.
    fn stamp_kitty_at(
        &mut self,
        image: &Arc<ImageData>,
        top: i32,
        left: i32,
        tag: KittyPlacementTag,
    ) {
        self.admit_live_image(image, Some(tag));
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
                self.grid.stack_cell_image(
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
    /// to its offset from the parent's current top-left. A child no longer on
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
        let on_screen: HashSet<NonZeroU32> =
            self.kitty_tags_on_screen().map(|tag| tag.serial).collect();
        self.transient
            .kitty_relations
            .retain(|r| on_screen.contains(&r.child.serial));
    }

    /// Record a virtual placement (`U=1`) of image `id`, drawn by Unicode placeholders naming
    /// it. A placement id names one, so putting it again replaces it; unnamed ones accumulate
    /// up to `MAX_KITTY_VIRTUALS`. An image not stored gets none.
    fn add_kitty_virtual(&mut self, id: u32, placement_id: u32, image: &Arc<ImageData>) {
        if !self.transient.kitty_images.contains_key(&id) {
            return;
        }
        let tag = self.next_kitty_tag(id, placement_id);
        let list = self.transient.kitty_virtual.entry(id).or_default();
        if placement_id != 0 {
            list.retain(|v| v.tag.placement_id != placement_id);
        }
        if list.len() >= MAX_KITTY_VIRTUALS {
            list.remove(0);
        }
        list.push(KittyVirtual {
            tag,
            image: Arc::clone(image),
        });
        self.damage_kitty_placeholders(id);
        // An unnamed virtual placement pushed out orphans its relatives.
        self.prune_kitty_floating();
    }

    /// Remove image `id`'s virtual placement `placement` — every one of them
    /// for `None`.
    fn remove_kitty_virtual(&mut self, id: u32, placement: Option<u32>) {
        let Some(list) = self.transient.kitty_virtual.get_mut(&id) else {
            return;
        };
        list.retain(|v| placement.is_some_and(|p| v.tag.placement_id != p));
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

    /// Clear every visible Kitty placement whose tag `selected` accepts, damaging each cleared
    /// cell, and return the cleared tags. Relative placements stay
    /// ([`delete_kitty_where`](Self::delete_kitty_where) takes them too). Only layers carrying
    /// a [`KittyPlacementTag`] are touched, and only matching cells are mutated, so a sweep
    /// over an imageless grid allocates nothing. Rows already in scrollback keep their pixels:
    /// kitty deletion addresses the screen.
    fn clear_kitty_placements(
        &mut self,
        selected: &dyn Fn(&KittyPlacementTag, &ImageData) -> bool,
    ) -> Vec<KittyPlacementTag> {
        let hit = |layer: &ImageRef| layer.kitty.is_some_and(|tag| selected(&tag, &layer.image));
        let mut hits: Vec<(u16, u16, KittyPlacementTag)> = self
            .kitty_layers()
            .filter(|(_, _, layer)| hit(layer))
            .filter_map(|(row, col, layer)| Some((row, col, layer.kitty?)))
            .collect();
        // Row-major, so the placements come back in the order the screen
        // shows them, whatever order the extras map keeps.
        hits.sort_by_key(|&(row, col, tag)| (row, col, tag.serial));
        let mut cleared = Vec::new();
        let mut seen = HashSet::new();
        let mut last_cell = None;
        for (row, col, tag) in hits {
            if last_cell != Some((row, col)) {
                last_cell = Some((row, col));
                self.grid
                    .cell_extra_mut(row, col)
                    .retain_images(|layer| !hit(layer));
                self.grid.damage_mut().mark_cell(row, col);
            }
            if seen.insert(tag.serial) {
                cleared.push(tag);
            }
        }
        cleared
    }

    /// Delete every floating placement whose tag `selected` accepts, and
    /// return their tags; their relatives go with them at the next prune
    /// ([`prune_kitty_floating`](Self::prune_kitty_floating)).
    fn clear_kitty_floating(
        &mut self,
        selected: &dyn Fn(&KittyPlacementTag, &ImageData) -> bool,
    ) -> Vec<KittyPlacementTag> {
        let mut gone = Vec::new();
        self.transient.kitty_floating.retain(|f| {
            let hit = selected(&f.tag, &f.image);
            if hit {
                gone.push(f.tag);
            }
            !hit
        });
        if !gone.is_empty() {
            self.grid.damage_mut().mark_full();
        }
        gone
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
        for tag in gone.iter().chain(&self.clear_kitty_floating(selected)) {
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
        for (_, _, layer) in self
            .kitty_layers()
            .filter(|&(row, col, _)| rows.contains(&row) && cols.contains(&col))
        {
            if let Some(tag) = layer.kitty
                && z.is_none_or(|z| layer.image.z_index == z)
                && !serials.contains(&tag.serial)
            {
                serials.push(tag.serial);
            }
        }
        // A floating placement is hit where it shows now.
        let (screen_rows, screen_cols) = (
            i32::from(rows.end.min(self.grid.rows())),
            i32::from(cols.end.min(self.grid.cols())),
        );
        let origins = kitty_floating::floating_origins(self.grid, self.transient);
        for (f, origin) in self.transient.kitty_floating.iter().zip(origins) {
            let Some((top, left)) = origin else {
                continue;
            };
            let spans = |start: i32, len: u16, lo: u16, hi: i32| {
                start < hi && start.saturating_add(i32::from(len)) > i32::from(lo)
            };
            if spans(top, f.image.rows, rows.start, screen_rows)
                && spans(left, f.image.cols, cols.start, screen_cols)
                && z.is_none_or(|z| f.image.z_index == z)
                && !serials.contains(&f.tag.serial)
            {
                serials.push(f.tag.serial);
            }
        }
        serials
    }

    /// Whether any visible cell still shows a placement of image `id`.
    fn kitty_image_on_screen(&self, id: u32) -> bool {
        self.kitty_tags_on_screen().any(|tag| tag.image_id == id)
    }

    /// Whether image `id` has a placement left: on screen, virtual, or
    /// floating.
    fn kitty_image_placed(&self, id: u32) -> bool {
        self.transient.kitty_virtual.contains_key(&id)
            || self.kitty_image_floating(id)
            || self.kitty_image_on_screen(id)
    }

    /// Whether image `id` has a floating placement.
    fn kitty_image_floating(&self, id: u32) -> bool {
        self.transient
            .kitty_floating
            .iter()
            .any(|f| f.tag.image_id == id)
    }

    /// `a=d`: delete the placements the `d=` selector addresses, with kitty's semantics:
    ///
    ///   * lowercase keeps the data (the id stays placeable; preview cyclers such as yazi
    ///     rely on it); uppercase also frees the data of each image named or cleared that
    ///     has no placement left (`A`: every image with no virtual placement);
    ///   * only Kitty placements are hit, never an iTerm2 or sixel image;
    ///   * only the id selectors (`i`, `n`, `r`) reach virtual placements;
    ///   * a relative placement goes with its parent.
    ///
    /// `x=`/`y=` are 1-based cells (for `r`, the id range); `z=` defaults to 0. `f`/`F` delete
    /// one frame ([`delete_kitty_frame`](Self::delete_kitty_frame)); an unknown selector
    /// deletes nothing.
    fn delete_kitty_placements(&mut self, cmd: &KittyCommand) {
        let selector = cmd.delete_target.unwrap_or('a');
        let free_data = selector.is_ascii_uppercase();
        // A 1-based coordinate key as a 0-based cell index (`None` for 0/absent).
        let cell = |v: Option<u32>| {
            v.and_then(|v| v.checked_sub(1))
                .and_then(|v| u16::try_from(v).ok())
        };
        let everything = 0..u16::MAX;
        // Ids the command names: their data goes under an uppercase selector
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

    /// `d=f`/`d=F`: delete frame `r=` (1-based; absent or 0 is frame 1, past the end the last)
    /// of the image `i=`/`I=` names; later frames move up a number. The current frame stays
    /// if it survives, else it is the frame now at its number, or the last. A lone frame
    /// survives `f`; under `F` the image itself is deleted.
    fn delete_kitty_frame(&mut self, cmd: &KittyCommand, image_too: bool) {
        let Some(id) = self.resolve_kitty_id(cmd) else {
            return;
        };
        let Some(shown) = self.shown_kitty_frame(id) else {
            return;
        };
        let t = &mut *self.transient;
        let Some(animation) = t.kitty_frames.get_mut(&id) else {
            return;
        };
        let frames = &mut animation.frames;
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
        let removed = frames.remove(doomed);
        let shown = if shown > doomed { shown - 1 } else { shown };
        let current = Arc::clone(&frames[shown.min(frames.len() - 1)].image);
        t.kitty_total_bytes = t
            .kitty_total_bytes
            .saturating_sub(removed.image.bytes.len());
        charge_released_kitty(t, removed.image.payload_bytes());
        let now = t.process_now;
        self.set_kitty_current_frame(id, current, now);
    }

    /// `a=a`, in kitty's order: `r=` with `z=` sets frame `r`'s gap (negative: gapless); `c=`
    /// makes frame `c` current everywhere; `s=` stops (1), runs waiting at the last frame (2)
    /// or loops (3), restarting the loop count; `v=` sets the plays (`1` forever, `n` n−1
    /// times). Answered only with `ENOENT` for an image not stored, as in kitty.
    fn control_kitty_animation(&mut self, cmd: &KittyCommand) {
        let id = self.resolve_kitty_id(cmd);
        let now = self.transient.process_now;
        let Some((id, animation)) =
            id.and_then(|id| Some((id, self.transient.kitty_frames.get_mut(&id)?)))
        else {
            self.kitty_reply(cmd, id, "ENOENT:no such image");
            return;
        };
        let count = animation.frames.len();
        let frame = |n: Option<u32>| {
            usize::try_from(n?)
                .ok()?
                .checked_sub(1)
                .filter(|&index| index < count)
        };
        if let Some(index) = frame(cmd.rows)
            && let Some(gap) = cmd.z_index.filter(|&gap| gap != 0)
        {
            animation.frames[index].gap_ms = u32::try_from(gap).unwrap_or(0);
        }
        let select = frame(cmd.columns).map(|index| Arc::clone(&animation.frames[index].image));
        // `s=0` names no state: kitty ignores it.
        if let Some(state) = cmd.width.filter(|&state| state != 0) {
            let was = animation.state;
            animation.state = match state {
                1 => KittyPlayState::Stopped,
                2 => KittyPlayState::Loading,
                3 => KittyPlayState::Running,
                _ => was,
            };
            if was == KittyPlayState::Stopped && animation.state != KittyPlayState::Stopped {
                animation.shown_at = now;
            }
            animation.loops = 0;
        }
        if let Some(count) = cmd.height.filter(|&count| count != 0) {
            animation.max_loops = count - 1;
        }
        if let Some(frame) = select {
            self.set_kitty_current_frame(id, frame, now);
        }
    }

    /// The index of the frame image `id` shows (its `kitty_images` slot is
    /// that frame's image).
    fn shown_kitty_frame(&self, id: u32) -> Option<usize> {
        let shown = self.transient.kitty_images.get(&id)?;
        let animation = self.transient.kitty_frames.get(&id)?;
        Some(
            animation
                .frames
                .iter()
                .position(|frame| Arc::ptr_eq(&frame.image, shown))
                .unwrap_or(0),
        )
    }

    /// Play every running Kitty animation shown on screen up to `now`, as kitty's
    /// `scan_active_animations` does: an expired frame gives way to the next one with a gap,
    /// the last to the first while loops remain (or, loading, the last stays up). Returns
    /// whether a frame changed and when the next is due; `None` when nothing plays on screen,
    /// so an idle terminal schedules nothing.
    pub(in crate::terminal) fn advance_kitty_animations(
        &mut self,
        now: aterm_time::Instant,
    ) -> (bool, Option<aterm_time::Instant>) {
        let playing: Vec<u32> = self
            .transient
            .kitty_frames
            .iter()
            .filter(|(_, animation)| animation.plays())
            .map(|(&id, _)| id)
            .collect();
        let mut advanced = false;
        let mut next: Option<aterm_time::Instant> = None;
        for id in playing {
            if !self.kitty_image_shown(id) {
                continue;
            }
            let Some(mut index) = self.shown_kitty_frame(id) else {
                continue;
            };
            let Some(animation) = self.transient.kitty_frames.get_mut(&id) else {
                continue;
            };
            let gap = |animation: &KittyAnimation, index: usize| {
                std::time::Duration::from_millis(u64::from(animation.frames[index].gap_ms))
            };
            let mut due = animation.shown_at + gap(animation, index);
            if now >= due {
                let mut rests = false;
                loop {
                    let following = (index + 1) % animation.frames.len();
                    if following == 0 {
                        if animation.state == KittyPlayState::Loading {
                            rests = true;
                            break;
                        }
                        animation.loops = animation.loops.saturating_add(1);
                        if animation.max_loops != 0 && animation.loops >= animation.max_loops {
                            rests = true;
                            break;
                        }
                    }
                    index = following;
                    if animation.frames[index].gap_ms != 0 {
                        break;
                    }
                }
                if rests {
                    continue;
                }
                let frame = Arc::clone(&animation.frames[index].image);
                due = now + gap(animation, index);
                self.set_kitty_current_frame(id, frame, now);
                advanced = true;
            }
            next = Some(next.map_or(due, |next| next.min(due)));
        }
        (advanced, next)
    }

    /// Whether image `id` shows anywhere on screen: a placement, a floating
    /// placement with a tile on screen, or a Unicode placeholder naming it.
    fn kitty_image_shown(&self, id: u32) -> bool {
        if self.kitty_image_on_screen(id) || !self.kitty_floating_cells(id).is_empty() {
            return true;
        }
        if !self.transient.kitty_virtual.contains_key(&id) {
            return false;
        }
        let grid: &Grid = self.grid;
        let (rows, cols) = (grid.rows(), grid.cols());
        grid.extras().iter().any(|(at, extra)| {
            at.row < rows
                && at.col < cols
                && super::super::kitty_placeholder::decode_cell(grid, at.row, at.col, extra)
                    .is_some_and(|cell| cell.image_id == id)
        })
    }

    /// Make `frame` image `id`'s current frame (`kitty_images[id]`, charged
    /// like any slot) and show it everywhere the image shows; a frame that
    /// changes is up since `now`, the instant its gap runs from.
    fn set_kitty_current_frame(
        &mut self,
        id: u32,
        frame: Arc<ImageData>,
        now: aterm_time::Instant,
    ) {
        let t = &mut *self.transient;
        if t.kitty_images
            .get(&id)
            .is_some_and(|shown| Arc::ptr_eq(shown, &frame))
        {
            return;
        }
        if let Some(animation) = t.kitty_frames.get_mut(&id) {
            animation.shown_at = now;
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
    /// current frame (`kitty_images[id]`), each keeping its own layout: kitty
    /// shows one frame of an image everywhere it shows. A placement whose
    /// re-laid-out copy does not fit the store keeps the frame it had.
    fn show_kitty_frame(&mut self, id: u32) {
        let Some(current) = self.transient.kitty_images.get(&id).cloned() else {
            return;
        };
        let mut memo: Vec<(Arc<ImageData>, Arc<ImageData>)> = Vec::new();
        let shown: Vec<(u16, u16, NonZeroU32, Arc<ImageData>)> = self
            .kitty_layers()
            .filter_map(|(row, col, layer)| {
                let tag = layer.kitty.filter(|tag| tag.image_id == id)?;
                Some((row, col, tag.serial, Arc::clone(&layer.image)))
            })
            .collect();
        for (row, col, serial, placed) in shown {
            let Some(image) = self.kitty_frame_for(id, &current, &placed, &mut memo) else {
                continue;
            };
            if Arc::ptr_eq(&image, &placed) {
                continue;
            }
            // The frame keeps the placement's layout, z-index included, so the
            // layer keeps its place in the cell's stack.
            if let Some(layer) = self
                .grid
                .cell_extra_mut(row, col)
                .images_mut()
                .iter_mut()
                .find(|layer| layer.kitty.is_some_and(|tag| tag.serial == serial))
            {
                layer.image = image;
            }
            self.grid.damage_mut().mark_cell(row, col);
        }
        // One placement at a time, in place: a copy made for one is shown
        // before the next is made, so making room never takes it back.
        let virtuals = self.transient.kitty_virtual.get(&id).map_or(0, Vec::len);
        for index in 0..virtuals {
            let t = &*self.transient;
            let Some(placed) = t.kitty_virtual.get(&id).and_then(|list| list.get(index)) else {
                break;
            };
            let placed = Arc::clone(&placed.image);
            if let Some(image) = self.kitty_frame_for(id, &current, &placed, &mut memo)
                && let Some(v) = self
                    .transient
                    .kitty_virtual
                    .get_mut(&id)
                    .and_then(|list| list.get_mut(index))
            {
                v.image = image;
            }
        }
        if self.kitty_image_floating(id) {
            for index in 0..self.transient.kitty_floating.len() {
                let f = &self.transient.kitty_floating[index];
                if f.tag.image_id != id {
                    continue;
                }
                let placed = Arc::clone(&f.image);
                if let Some(image) = self.kitty_frame_for(id, &current, &placed, &mut memo) {
                    self.transient.kitty_floating[index].image = image;
                }
            }
            for (row, col) in self.kitty_floating_cells(id) {
                self.grid.damage_mut().mark_cell(row, col);
            }
        }
        self.damage_kitty_placeholders(id);
    }

    /// The cells on screen showing a floating placement of image `id`.
    fn kitty_floating_cells(&self, id: u32) -> Vec<(u16, u16)> {
        let t = &*self.transient;
        if !self.kitty_image_floating(id) {
            return Vec::new();
        }
        kitty_floating::floating_tiles(self.grid, t)
            .into_iter()
            .filter(|tile| t.kitty_floating[tile.index].tag.image_id == id)
            .map(|tile| (tile.row, tile.col))
            .collect()
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

    /// `a=f`: load a new frame, or edit frame `r=` (1-based) when it exists, into the image
    /// `i=`/`I=` names, and answer naming the frame. An edit of the current frame shows
    /// everywhere.
    fn load_kitty_frame(&mut self, mut cmd: KittyCommand) {
        let Some(id) = self.resolve_kitty_id(&cmd) else {
            self.kitty_reply(&cmd, None, "ENOENT:no such image");
            return;
        };
        let Some(count) = self
            .transient
            .kitty_frames
            .get(&id)
            .map(|animation| animation.frames.len())
        else {
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
        // `z=` is the frame's gap: kitty's 40 ms for a new frame without one, a
        // negative one gapless; `z=0` leaves an edited frame's gap alone.
        let gap = cmd.z_index.filter(|&gap| gap != 0);
        let verdict = match self.build_kitty_frame(id, &mut cmd, edit) {
            Ok(frame) => self.commit_kitty_frame(id, frame, edit, gap),
            Err(error) => error,
        };
        self.kitty_reply_naming(&cmd, Some(id), Some(number), verdict);
    }

    /// Make an `a=f` frame of image `id`: its data (`s=`×`v=` raw pixels, or a PNG) placed at
    /// `x=`/`y=` on a root-sized canvas (the frame being edited, else frame `c=`, else colour
    /// `Y=`, default transparent), alpha-blended unless `X=1`. It keeps the root's layout.
    /// Data that is the whole canvas on a transparent background is kept as sent; anything
    /// else is composed into raw pixels ([`kitty_rgba`]).
    fn build_kitty_frame(
        &self,
        id: u32,
        cmd: &mut KittyCommand,
        edit: Option<usize>,
    ) -> Result<ImageData, &'static str> {
        let frames = &self
            .transient
            .kitty_frames
            .get(&id)
            .ok_or("ENOENT:no such image")?
            .frames;
        let root = &frames.first().ok_or("ENOENT:no such image")?.image;
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
        cmd.cell_x_offset = None;
        cmd.cell_y_offset = None;
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
            return Ok(KittyLayout::of(root).image(data.bytes, data.format));
        }
        let (Ok(width), Ok(height)) = (u16::try_from(canvas_w), u16::try_from(canvas_h)) else {
            return Err("ENOSPC:frame too large");
        };
        let size = usize::from(width) * usize::from(height) * 4;
        if size > MAX_KITTY_IMAGE_BYTES {
            return Err("ENOSPC:frame too large");
        }
        let under = match (edit, base) {
            (Some(index), _) => Some(&frames.get(index).ok_or("EINVAL:no such frame")?.image),
            (None, Some(n)) => Some(
                &usize::try_from(n - 1)
                    .ok()
                    .and_then(|index| frames.get(index))
                    .ok_or("EINVAL:no such frame")?
                    .image,
            ),
            (None, None) => None,
        };
        let pixels = kitty_rgba(&data)?.pixels;
        // Fallible allocation: an OOM fails the frame, never the process.
        let mut canvas = Vec::new();
        canvas
            .try_reserve_exact(size)
            .map_err(|_| "ENOSPC:out of memory")?;
        match under {
            Some(under) => {
                let under = kitty_rgba(under)?;
                if under.size != (width, height) {
                    return Err("EINVAL:the frames differ in size");
                }
                canvas.extend_from_slice(&under.pixels);
            }
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
            &pixels,
            to_usize(data_w),
            (0, 0, to_usize(data_w), to_usize(data_h)),
            blend,
        );
        Ok(raw_frame(canvas, width, height, root))
    }

    /// Store `frame` as image `id`'s frame `edit` (0-based) or a new last frame, charged to the
    /// budget and capped by `MAX_KITTY_FRAMES`, with gap `gap` (`z=`: absent is kitty's 40 ms
    /// for a new frame and unchanged for an edit; negative is gapless). An edit of the current
    /// frame shows everywhere the image shows. Returns the verdict to answer with.
    fn commit_kitty_frame(
        &mut self,
        id: u32,
        frame: ImageData,
        edit: Option<usize>,
        gap: Option<i32>,
    ) -> &'static str {
        let add = frame.bytes.len();
        let frame = Arc::new(frame);
        let t = &mut *self.transient;
        let total = t.kitty_total_bytes;
        let now = t.process_now;
        let Some(animation) = t.kitty_frames.get_mut(&id) else {
            return "ENOENT:no such image";
        };
        let gap_ms = |gap: i32| u32::try_from(gap).unwrap_or(0);
        let Some(index) = edit else {
            // Fail closed at the per-id frame cap or the store budget.
            if animation.frames.len() >= MAX_KITTY_FRAMES
                || total.saturating_add(add) > MAX_KITTY_STORE_BYTES
            {
                return "ENOSPC:no room for another frame";
            }
            animation.frames.push(KittyFrame {
                image: frame,
                gap_ms: gap.map_or(KITTY_DEFAULT_GAP_MS, gap_ms),
            });
            t.kitty_total_bytes = total.saturating_add(add);
            return "OK";
        };
        let Some(slot) = animation.frames.get_mut(index) else {
            return "EINVAL:no such frame";
        };
        let projected = total
            .saturating_sub(slot.image.bytes.len())
            .saturating_add(add);
        if projected > MAX_KITTY_STORE_BYTES {
            return "ENOSPC:image store full";
        }
        if let Some(gap) = gap {
            slot.gap_ms = gap_ms(gap);
        }
        let old = std::mem::replace(&mut slot.image, Arc::clone(&frame));
        t.kitty_total_bytes = projected;
        // The old frame may still show where the edit does not re-point it:
        // on the parked grid, which no store check sees.
        charge_released_kitty(t, old.payload_bytes());
        if t.kitty_images
            .get(&id)
            .is_some_and(|shown| Arc::ptr_eq(shown, &old))
        {
            self.set_kitty_current_frame(id, frame, now);
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
            Ok((index, frame)) => self.commit_kitty_frame(id, frame, Some(index), None),
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
        let frames = &self
            .transient
            .kitty_frames
            .get(&id)
            .ok_or("ENOENT:no such image")?
            .frames;
        let pick = |n: Option<u32>| {
            let index = usize::try_from(n?).ok()?.checked_sub(1)?;
            Some((index, &frames.get(index)?.image))
        };
        let (src_index, src) = pick(cmd.rows).ok_or("ENOENT:no such source frame")?;
        let (dst_index, dst) = pick(cmd.columns).ok_or("ENOENT:no such destination frame")?;
        let src_rgba = kitty_rgba(src)?;
        let dst_rgba = kitty_rgba(dst)?;
        let (width, height) = dst_rgba.size;
        if src_rgba.size != dst_rgba.size {
            return Err("EINVAL:the frames differ in size");
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
            .try_reserve_exact(dst_rgba.pixels.len())
            .map_err(|_| "ENOSPC:out of memory")?;
        canvas.extend_from_slice(&dst_rgba.pixels);
        // Every value is bounded by a u16 image side, so the casts are exact.
        let to_usize = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
        kitty_graphics::compose_rgba(
            &mut canvas,
            usize::from(width),
            (to_usize(dst_x), to_usize(dst_y)),
            &src_rgba.pixels,
            usize::from(width),
            (to_usize(src_x), to_usize(src_y), to_usize(w), to_usize(h)),
            !cmd.cursor_stays,
        );
        Ok((dst_index, raw_frame(canvas, width, height, dst)))
    }

    /// The layout a transmit or put asks for, for a `px_w × px_h` raster, or `None` when its
    /// source rectangle (`x=`/`y=`/`w=`/`h=`) misses the raster.
    ///
    /// `X=`/`Y=` start a put's image that many pixels into its first cell (held below a cell),
    /// and the footprint grows to keep the image whole, by kitty's `update_dest_rect`
    /// arithmetic; a transmit or virtual placement takes no offset. `c=`/`r=` ask for the image
    /// fitted over that many cells, one alone taking the other from the aspect. With neither,
    /// the image is `PixelExact` (one image pixel per device pixel, as kitty draws it): fitting
    /// it to the rounded-up footprint would magnify by up to a cell. The footprint is clamped
    /// to the grid.
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
        // A rectangle that is the whole raster is no crop at all, so a put that
        // spells it out still reuses the stored image.
        let source_rect = (cropped && (vis_w, vis_h) != (px_w, px_h)).then_some(SourceRect {
            x,
            y,
            width: vis_w,
            height: vis_h,
        });
        let cell_w = u32::from(self.iterm2.cell_px.0.max(1));
        let cell_h = u32::from(self.iterm2.cell_px.1.max(1));
        // Only a placement shown in cells starts inside one.
        let in_cells = matches!(
            cmd.action,
            KittyAction::Display | KittyAction::TransmitAndDisplay
        ) && !cmd.virtual_placement;
        let offset = |key: Option<u32>, cell: u32| {
            let px = if in_cells {
                key.unwrap_or(0).min(cell - 1)
            } else {
                0
            };
            u16::try_from(px).unwrap_or(u16::MAX)
        };
        let cell_offset_px = (
            offset(cmd.cell_x_offset, cell_w),
            offset(cmd.cell_y_offset, cell_h),
        );
        let (off_x, off_y) = (u32::from(cell_offset_px.0), u32::from(cell_offset_px.1));
        // `c=0`/`r=0` are kitty's "unspecified".
        let (cols, rows) = (
            cmd.columns.filter(|&c| c != 0),
            cmd.rows.filter(|&r| r != 0),
        );
        // The span, in cells of `to_cell` px, that `len` px make along the
        // other axis at the raster's aspect `to_px:from_px`.
        let aspect = |len: u32, from_px: u32, to_px: u32, to_cell: u32| {
            let span = (u64::from(len) * u64::from(to_px))
                .div_ceil(u64::from(from_px) * u64::from(to_cell));
            u32::try_from(span).unwrap_or(u32::MAX)
        };
        // Client lengths (`c=`/`r=`, a PNG header's size) plus an offset can overflow, so the
        // sums saturate; the footprint is clamped to the grid below.
        let (fit_cols, fit_rows) = match (cols, rows) {
            (Some(c), Some(r)) => (c, r),
            (Some(c), None) => (
                c,
                aspect(
                    c.saturating_mul(cell_w).saturating_add(off_x),
                    vis_w,
                    vis_h,
                    cell_h,
                ),
            ),
            (None, Some(r)) => (
                aspect(
                    r.saturating_mul(cell_h).saturating_add(off_y),
                    vis_h,
                    vis_w,
                    cell_w,
                ),
                r,
            ),
            (None, None) => (
                vis_w.saturating_add(off_x).div_ceil(cell_w),
                vis_h.saturating_add(off_y).div_ceil(cell_h),
            ),
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
            // Kitty z=: negative draws behind text.
            z_index: cmd.z_index.unwrap_or(0),
            scaling: if cols.is_none() && rows.is_none() {
                ImageScaling::PixelExact
            } else {
                ImageScaling::Fit
            },
            source_rect,
            cell_offset_px,
        })
    }

    /// Build an [`ImageData`] from a complete Kitty transmit or frame command, or `None` if it
    /// declares a size kitty does not load ([`KittyCommand::declares_loadable_size`]), or the
    /// payload or dimensions are bad. PNG keeps its bytes; raw `f=32`/`f=24` become `RawRgba8`.
    /// The flag is `false` when the source rectangle missed the raster: the image is then laid
    /// out whole, still good to store but not to display.
    ///
    /// Takes `&mut cmd` so a direct uncompressed payload moves into the image without a copy;
    /// the caller must not read `cmd.payload` afterwards.
    fn build_kitty_image(&self, cmd: &mut KittyCommand) -> Option<(ImageData, bool)> {
        use crate::terminal::kitty_graphics::{KittyFormat, png_dimensions, rgb_to_rgba};
        // The same size rule that decides whether a first chunk opens a load
        // (`kitty_would_hold`), asked before a file is resolved or a stream inflated.
        if cmd.payload.is_empty() || !cmd.declares_loadable_size() {
            return None;
        }
        // `t=d` carries the data. The other mediums (`t=f`/`t=t`/`t=s`) carry a name the host
        // resolver reads under its own policy; with no resolver, or a refusal, nothing loads:
        // the engine never reads files or shared memory itself. `o=z` data is inflated with a
        // bound, so a decompression bomb is refused.
        let payload: Vec<u8> = if cmd.medium == KittyMedium::Direct {
            if cmd.compressed {
                aterm_codec::inflate::zlib_decompress(&cmd.payload, MAX_KITTY_IMAGE_BYTES).ok()?
            } else {
                // Zero-copy: the assembled payload moves into the image.
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
            KittyFormat::Rgba | KittyFormat::Rgb => {
                let rgb = cmd.format == KittyFormat::Rgb;
                let (w, h) = (cmd.width?, cmd.height?);
                let bytes_per_px = if rgb { 3 } else { 4 };
                if payload.len()
                    != (w as usize)
                        .checked_mul(h as usize)?
                        .checked_mul(bytes_per_px)?
                {
                    return None; // malformed raw buffer
                }
                let fmt = ImageFormat::RawRgba8 {
                    width: u16::try_from(w).ok()?,
                    height: u16::try_from(h).ok()?,
                };
                (fmt, w, h, if rgb { rgb_to_rgba(&payload) } else { payload })
            }
        };
        let (layout, crop_ok) = match self.kitty_layout(cmd, px_w, px_h) {
            Some(layout) => (layout, true),
            None => {
                let whole = KittyCommand {
                    columns: cmd.columns,
                    rows: cmd.rows,
                    z_index: cmd.z_index,
                    cell_x_offset: cmd.cell_x_offset,
                    cell_y_offset: cmd.cell_y_offset,
                    virtual_placement: cmd.virtual_placement,
                    ..KittyCommand::default()
                };
                (self.kitty_layout(&whole, px_w, px_h)?, false)
            }
        };
        Some((layout.image(bytes, format), crop_ok))
    }
}

/// The verdict for a transmission over a medium this session cannot read
/// (the non-direct mediums without the opt-in resolver) — the `a=q` probe's
/// answer and a transmit's alike.
const KITTY_MEDIUM_DISABLED: &str = "ENOTSUPPORTED:medium disabled (allow_kitty_file_transfer)";

/// The verdict for a PNG frame that does not decode, as kitty spells it.
const KITTY_BAD_PNG: &str = "EBADPNG:the PNG does not decode";

/// How one Kitty placement lays its image out: the fields of [`ImageData`] a
/// transmit or put decides (the rest is the payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KittyLayout {
    cols: u16,
    rows: u16,
    z_index: i32,
    scaling: ImageScaling,
    source_rect: Option<SourceRect>,
    cell_offset_px: (u16, u16),
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
            cell_offset_px: image.cell_offset_px,
        }
    }

    /// The image of `bytes` in `format`, with this layout.
    fn image(self, bytes: Vec<u8>, format: ImageFormat) -> ImageData {
        ImageData {
            bytes,
            format,
            cols: self.cols,
            rows: self.rows,
            z_index: self.z_index,
            band_lift_px: 0,
            scaling: self.scaling,
            source_rect: self.source_rect,
            cell_offset_px: self.cell_offset_px,
        }
    }
}

/// A stored Kitty frame's pixels as packed RGBA8, and their size.
struct KittyRgba<'a> {
    pixels: Cow<'a, [u8]>,
    size: (u16, u16),
}

/// The pixels of a stored Kitty frame as packed RGBA8: a raw frame's own
/// bytes, a PNG decoded, for a composition. A frame too large to compose is
/// refused before it is decoded.
fn kitty_rgba(frame: &ImageData) -> Result<KittyRgba<'_>, &'static str> {
    let (width, height) = raster_size(frame).ok_or(KITTY_BAD_PNG)?;
    let (Ok(width), Ok(height)) = (u16::try_from(width), u16::try_from(height)) else {
        return Err("ENOSPC:frame too large");
    };
    if usize::from(width) * usize::from(height) * 4 > MAX_KITTY_IMAGE_BYTES {
        return Err("ENOSPC:frame too large");
    }
    let pixels = match frame.format {
        ImageFormat::RawRgba8 { .. } => Cow::Borrowed(frame.bytes.as_slice()),
        ImageFormat::Png => {
            let (pixels, w, h) =
                aterm_png::decode_rgba8(&frame.bytes, u32::from(u16::MAX)).ok_or(KITTY_BAD_PNG)?;
            if (w, h) != (usize::from(width), usize::from(height)) {
                return Err(KITTY_BAD_PNG);
            }
            Cow::Owned(pixels)
        }
        ImageFormat::Unknown => return Err(KITTY_BAD_PNG),
    };
    Ok(KittyRgba {
        pixels,
        size: (width, height),
    })
}

/// A raw RGBA8 frame of `width × height` pixels, laid out as `like`.
fn raw_frame(bytes: Vec<u8>, width: u16, height: u16, like: &ImageData) -> ImageData {
    KittyLayout::of(like).image(bytes, ImageFormat::RawRgba8 { width, height })
}

/// Push `id` onto `ids` unless it is there already.
fn push_unique(ids: &mut Vec<u32>, id: u32) {
    if !ids.contains(&id) {
        ids.push(id);
    }
}

/// Whether `a` and `b` draw the same pixels of their cells when placed at the
/// same cell: one footprint, z-index, scaling and in-cell offset, and a
/// source rectangle of one size, wherever in the image it starts.
fn kitty_same_spot(a: &ImageData, b: &ImageData) -> bool {
    let drawn = |image: &ImageData| {
        let whole = KittyLayout {
            source_rect: None,
            ..KittyLayout::of(image)
        };
        let size = image
            .source_rect
            .map(|rect| (rect.width, rect.height))
            .or_else(|| raster_size(image));
        (whole, size)
    };
    drawn(a) == drawn(b)
}

/// Bytes the store holds for image `id`: its image slot, its frames and its
/// placement variants (each slot counted, as the budget counts them).
fn kitty_bytes_held(t: &TransientState, id: u32) -> usize {
    t.kitty_images.get(&id).map_or(0, |img| img.bytes.len())
        + t.kitty_frames.get(&id).map_or(0, |animation| {
            animation
                .frames
                .iter()
                .map(|frame| frame.image.bytes.len())
                .sum::<usize>()
        })
        + t.kitty_variants.get(&id).map_or(0, |vs| {
            vs.iter().map(|v| v.image.bytes.len()).sum::<usize>()
        })
}

/// The payload bytes of the distinct images the store holds for `id` — its
/// image (one of its frames), frames and variants — as the live-image budget
/// counts them.
fn kitty_payload_held(t: &TransientState, id: u32) -> usize {
    let mut seen: HashSet<*const ImageData> = HashSet::new();
    t.kitty_images
        .get(&id)
        .into_iter()
        .chain(
            t.kitty_frames
                .get(&id)
                .into_iter()
                .flat_map(|animation| animation.frames.iter().map(|frame| &frame.image)),
        )
        .chain(
            t.kitty_variants
                .get(&id)
                .into_iter()
                .flatten()
                .map(|v| &v.image),
        )
        .filter(|image| seen.insert(Arc::as_ptr(image)))
        .fold(0usize, |sum, image| {
            sum.saturating_add(image.payload_bytes())
        })
}

/// Charge `bytes` of images the Kitty store lets go to both screens'
/// `live_image_charge`: a placement on either grid may still show them (the
/// store's checks see only the active grid), and from now on they count
/// against that screen's `MAX_LIVE_IMAGE_BYTES`.
fn charge_released_kitty(t: &mut TransientState, bytes: usize) {
    for charge in &mut t.live_image_charge {
        *charge = charge.saturating_add(bytes);
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
/// The gap of a new animation frame that names none, as kitty gives it.
const KITTY_DEFAULT_GAP_MS: u32 = 40;
/// Maximum re-laid-out placement variants retained per Kitty image (DoS bound;
/// each is also charged to `MAX_KITTY_STORE_BYTES`).
const MAX_KITTY_VARIANTS: usize = 16;
/// Maximum virtual (`U=1`) placements retained per Kitty image; the oldest
/// unnamed one goes first.
const MAX_KITTY_VIRTUALS: usize = 8;
/// Maximum decoded bytes for a single Kitty image (matches the APC payload cap).
const MAX_KITTY_IMAGE_BYTES: usize = 4 * 1024 * 1024;
/// Byte budget for the whole Kitty store (images, frames, variants). Without it the per-item
/// caps multiply to ~128 GiB of memory from untrusted PTY output.
const MAX_KITTY_STORE_BYTES: usize = 64 * 1024 * 1024;

/// CSI dispatch fast paths for `ActionSink::csi_dispatch`.
impl TerminalHandler<'_> {
    /// CSI without intermediates: SGR and the hottest cursor moves inline, the rest via
    /// `csi_dispatch_standard_core`.
    #[inline]
    fn csi_dispatch_no_intermediates(
        &mut self,
        cap: &super::super::response_capability::ResponseCapability,
        params: &[u16],
        final_byte: u8,
    ) {
        match final_byte {
            b'm' => self.csi_dispatch_sgr_fast(params),
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
                self.grid
                    .cursor_forward_margin(n, self.modes.left_right_margin_mode);
            }
            b'D' => {
                let n = params.first().copied().unwrap_or(1).max(1);
                self.grid
                    .cursor_backward_margin(n, self.modes.left_right_margin_mode);
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
            _ => {
                let _ = self.csi_dispatch_standard_core(cap, params, final_byte);
            }
        }
    }

    /// SGR fast path: reset and single-param 8/16-colour or default fg/bg;
    /// everything else goes to `csi_dispatch_sgr_extended`.
    #[inline]
    #[allow(
        clippy::too_many_lines,
        reason = "SGR dispatch table with many attribute codes"
    )]
    fn csi_dispatch_sgr_fast(&mut self, params: &[u16]) {
        // SGR 0 / bare CSI m: `reset_sgr` keeps the DECSCA protection attribute; the
        // underline colour is cleared too.
        if params.is_empty() || (params.len() == 1 && params[0] == 0) {
            self.style.reset_sgr();
            self.transient.current_underline_color = None;
            self.transient.update_has_transient_extras();
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
                    // Refresh unconditionally: it also keeps `has_style_extras()` in step
                    // after an RGB→indexed change.
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
                    // Re-arms the BCE cursor template for the bg change.
                    self.sgr_style().apply_style_change();
                    return;
                }
                49 => {
                    self.style.bg = PackedColor::DEFAULT_BG;
                    self.sgr_style().apply_style_change();
                    return;
                }
                _ => {} // Non-color single params fall through to handle_sgr
            }
        }
        self.csi_dispatch_sgr_extended(params);
    }

    #[inline(never)]
    fn csi_dispatch_sgr_extended(&mut self, params: &[u16]) {
        // 5-param truecolor fg/bg.
        if params.len() == 5 && params[1] == 2 {
            let rgb = PackedColor::rgb(
                params[2].min(255) as u8,
                params[3].min(255) as u8,
                params[4].min(255) as u8,
            );
            if params[0] == 38 {
                self.style.fg = rgb;
                self.style.update_cached_colors();
                return;
            }
            if params[0] == 48 {
                self.style.bg = rgb;
                // Re-arms the BCE cursor template for the bg change.
                self.sgr_style().apply_style_change();
                return;
            }
        }
        // 3-param 256-color fg/bg
        if params.len() == 3 && params[1] == 5 {
            let index = crate::terminal::sgr_color_u8(params[2]);
            if params[0] == 38 {
                self.style.fg = PackedColor::indexed(index);
                // Unconditional cache refresh, as in the ANSI-fg arm.
                self.style.update_cached_colors();
                return;
            }
            if params[0] == 48 {
                self.style.bg = PackedColor::indexed(index);
                // Re-arms the BCE cursor template for the bg change.
                self.sgr_style().apply_style_change();
                return;
            }
        }
        self.sgr_style().handle_sgr(params);
    }
}

/// True iff `data` holds a run of >= 4 identical bytes, the only content the run scan in
/// `write_ascii_bulk_fast` can splat. A branch-free windowed fold, so LLVM vectorizes it
/// without `std::arch` and it stays inside the Trust gate.
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

/// Bulk ASCII write helpers for `print_ascii_bulk`.
impl TerminalHandler<'_> {
    /// Bulk ASCII writer for data that passed every precondition: runs of 4+ identical bytes
    /// splat via `write_cell_run`, the rest is blasted (default style) or written styled
    /// (`write_ascii_run_styled_packed`). Records `last_graphic_char` for REP.
    fn write_ascii_bulk_fast(&mut self, data: &[u8]) {
        let flags = if self.style.protected {
            self.style.flags.union(CellFlags::PROTECTED)
        } else {
            self.style.flags
        };
        let is_default = self.style.is_default();
        let colors = self.style.cached_colors();

        // Most output is short or has no 4+ identical run: blast it whole. Byte-identical
        // either way; a false "run-free" only forgoes a splat.
        if data.len() <= 64 || !has_run_of_4(data) {
            self.write_ascii_segment(data, is_default, colors, flags);
            return;
        }

        // Single pass: runs of 4+ identical bytes splat, mixed segments
        // between them are written whole.
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
            self.write_ascii_segment(&data[seg_start..pos], is_default, colors, flags);
        }
    }

    /// Write a run-free `segment`: blast it when the style is default, else
    /// write it styled; either way record its last glyph for REP.
    #[inline]
    fn write_ascii_segment(
        &mut self,
        segment: &[u8],
        is_default: bool,
        colors: crate::grid::PackedColors,
        flags: CellFlags,
    ) {
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

    /// Bulk ASCII writer for styles that need `CellExtras` (RGB, hyperlinks, underline colour,
    /// extended flags), via `write_ascii_run_with_extras` instead of per-character writes.
    fn write_ascii_bulk_with_extras(&mut self, data: &[u8]) {
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
        let bg_rgb = self.style.bce_bg_rgb();
        let extended_flags_bits = self.style.flags.extended_flags().bits();

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

    /// The first pixel byte of the image placed at (0, 0) — its frame.
    fn placed_frame(term: &Terminal) -> Option<u8> {
        image_at(term, 0, 0).map(|image| image.image.bytes[0])
    }

    /// A clock reading `ms` after the moment the clock was made.
    fn clock() -> impl Fn(u64) -> crate::terminal::ClockReading {
        let base = crate::terminal::ClockReading::now();
        move |ms: u64| crate::terminal::ClockReading {
            monotonic: base
                .monotonic
                .checked_add(std::time::Duration::from_millis(ms))
                .expect("clock"),
            wall_ms: None,
        }
    }

    /// Image 7 placed at (0, 0) with four one-cell frames of pixels `1`..`4`,
    /// gaps 50 (set on the root with `a=a`), 100, gapless and kitty's default
    /// 40 ms, stopped; and a clock `ms` after the moment it was built.
    fn film() -> (Terminal, impl Fn(u64) -> crate::terminal::ClockReading) {
        let at = clock();
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        for control in [
            "a=t,f=32,s=10,v=20,i=7,q=2",
            "a=f,f=32,s=10,v=20,i=7,z=100,q=2",
            "a=f,f=32,s=10,v=20,i=7,z=-1,q=2",
            "a=f,f=32,s=10,v=20,i=7,q=2",
        ] {
            let pixel = u8::try_from(
                term.transient
                    .kitty_frames
                    .get(&7)
                    .map_or(0, |a| a.frames.len()),
            )
            .expect("few frames")
                + 1;
            term.process_at(&apc_g(control, &[pixel; 800]), at(0));
        }
        term.process_at(&apc_g("a=a,i=7,r=1,z=50", b""), at(0));
        term.process_at(&apc_g("a=p,i=7,C=1,q=2", b""), at(0));
        (term, at)
    }

    /// `s=3` plays each frame for its gap, skipping gapless ones and looping; the engine
    /// reports each step and the next due time, and nothing while stopped.
    #[test]
    fn a_running_animation_steps_its_frames_on_their_gaps() {
        use crate::terminal::KittyAnimationStep;
        let (mut term, at) = film();
        let step = |advanced, due: Option<u64>| KittyAnimationStep {
            advanced,
            next_frame_at: due.map(|ms| at(ms).monotonic),
        };
        assert_eq!(
            term.advance_kitty_animations(at(1000).monotonic),
            KittyAnimationStep::default(),
            "stopped: nothing plays, no wake"
        );
        assert_eq!(placed_frame(&term), Some(1));

        term.process_at(&apc_g("a=a,i=7,s=3", b""), at(1000));
        assert_eq!(reply(&mut term), "", "a=a is not answered");
        let mut play = |ms: u64| term.advance_kitty_animations(at(ms).monotonic);
        assert_eq!(play(1010), step(false, Some(1050)), "the root's 50 ms");
        assert_eq!(play(1050), step(true, Some(1150)), "frame 2, for 100 ms");
        assert_eq!(play(1149), step(false, Some(1150)));
        assert_eq!(play(1160), step(true, Some(1200)), "gapless 3 skipped: 4");
        assert_eq!(play(1200), step(true, Some(1250)), "round to the root");
        assert_eq!(placed_frame(&term), Some(1));

        // Stopping holds the frame shown and schedules nothing.
        term.process_at(&apc_g("a=a,i=7,s=1", b""), at(1210));
        assert_eq!(
            term.advance_kitty_animations(at(5000).monotonic),
            KittyAnimationStep::default()
        );
        assert_eq!(placed_frame(&term), Some(1));

        // An animation shown nowhere plays nowhere.
        term.process_at(&apc_g("a=a,i=7,s=3", b""), at(6000));
        term.process_at(&apc_g("a=d,d=i,i=7", b""), at(6000));
        assert_eq!(
            term.advance_kitty_animations(at(9000).monotonic),
            KittyAnimationStep::default()
        );

        // Kitty answers an animation command only for an image it lacks.
        term.process_at(&apc_g("a=a,i=99,s=3", b""), at(9000));
        assert_eq!(reply(&mut term), "\x1b_Gi=99;ENOENT:no such image\x1b\\");
    }

    /// `v=` bounds the loops, a new `s=` restarts the count, and `s=2` waits at the last frame
    /// until another arrives.
    #[test]
    fn loop_counts_and_loading_rest_on_the_last_frame() {
        use crate::terminal::KittyAnimationStep;
        let (mut term, at) = film();
        term.process_at(&apc_g("a=a,i=7,s=3,v=2", b""), at(0));
        let mut play = |ms: u64| term.advance_kitty_animations(at(ms).monotonic);
        assert!(play(50).advanced, "frame 2");
        assert!(play(150).advanced, "frame 4");
        let rest = play(190);
        assert_eq!(
            rest,
            KittyAnimationStep::default(),
            "one loop played: it rests on the last frame"
        );
        assert_eq!(placed_frame(&term), Some(4));

        term.process_at(&apc_g("a=a,i=7,s=2", b""), at(1000));
        let mut play = |ms: u64| term.advance_kitty_animations(at(ms).monotonic);
        assert!(!play(1010).advanced, "loading: still resting on the last");
        term.process_at(
            &apc_g("a=f,f=32,s=10,v=20,i=7,z=30,q=2", &[5u8; 800]),
            at(1020),
        );
        assert_eq!(
            term.advance_kitty_animations(at(1030).monotonic),
            KittyAnimationStep {
                advanced: true,
                next_frame_at: Some(at(1060).monotonic),
            },
            "the new frame plays"
        );
        assert_eq!(placed_frame(&term), Some(5));
    }

    /// `s=0` names no state, and kitty ignores it: an animation told to play
    /// twice (`v=3`) rests after its second time round though an `s=0` came in
    /// between.
    #[test]
    fn a_zero_state_changes_nothing() {
        use crate::terminal::KittyAnimationStep;
        let (mut term, at) = film();
        term.process_at(&apc_g("a=a,i=7,s=3,v=3", b""), at(0));
        let mut play = |ms: u64| term.advance_kitty_animations(at(ms).monotonic);
        for ms in [50, 150, 190] {
            assert!(play(ms).advanced, "the first time round, at {ms} ms");
        }
        term.process_at(&apc_g("a=a,i=7,s=0", b""), at(200));
        let mut play = |ms: u64| term.advance_kitty_animations(at(ms).monotonic);
        assert!(play(240).advanced && play(340).advanced, "the second");
        assert_eq!(play(380), KittyAnimationStep::default(), "then rests");
        assert_eq!(placed_frame(&term), Some(4));
    }

    /// Kitty's answer to an `a=a` or `a=f` command names the frame the command
    /// gave (`r=`), its error for a missing image too.
    #[test]
    fn frame_and_animation_errors_name_the_commands_frame() {
        let mut term = Terminal::new(24, 80);
        term.process(&apc_g("a=a,i=99,r=2,z=50", b""));
        assert_eq!(
            reply(&mut term),
            "\x1b_Gi=99,r=2;ENOENT:no such image\x1b\\"
        );
        term.process(&apc_g("a=f,f=32,s=1,v=1,i=99,r=3", &[0u8; 4]));
        assert_eq!(
            reply(&mut term),
            "\x1b_Gi=99,r=3;ENOENT:no such image\x1b\\"
        );
        term.process(&apc_g("a=a,i=99,s=3", b""));
        assert_eq!(
            reply(&mut term),
            "\x1b_Gi=99;ENOENT:no such image\x1b\\",
            "no frame named, none answered"
        );
    }

    /// An animation shown through a re-laid-out copy of each frame plays every
    /// frame while the renderers hold the frames they drew: a copy no placement
    /// shows is the store's to recycle, whoever else holds it.
    #[test]
    fn an_animation_plays_through_while_its_drawn_frames_are_held() {
        let at = clock();
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process_at(&apc_g("a=t,f=32,s=10,v=20,i=7,q=2", &[1u8; 800]), at(0));
        for pixel in 2..=20u8 {
            term.process_at(
                &apc_g("a=f,f=32,s=10,v=20,i=7,z=10,q=2", &[pixel; 800]),
                at(0),
            );
        }
        term.process_at(&apc_g("a=a,i=7,r=1,z=10", b""), at(0));
        // A two-cell box: every frame shows through a copy laid out for it.
        term.process_at(&apc_g("a=p,i=7,c=2,r=1,C=1,q=2", b""), at(0));
        term.process_at(&apc_g("a=a,i=7,s=3", b""), at(0));
        let mut held = Vec::new();
        let mut shown = Vec::new();
        for step in 1..=40u64 {
            term.advance_kitty_animations(at(step * 10).monotonic);
            let placed = image_at(&term, 0, 0).expect("placed").image;
            shown.push(placed.bytes[0]);
            held.push(placed);
        }
        let expected: Vec<u8> = (1..=40u8).map(|step| step % 20 + 1).collect();
        assert_eq!(shown, expected, "every frame, round and round");
    }

    /// `d=f` deletes frame `r=` as kitty does (see `delete_kitty_frame`) and returns its bytes
    /// to the budget.
    #[test]
    fn frame_delete_removes_the_numbered_frame_as_kitty_does() {
        const FRAME: usize = 800;
        // The shown frame survives a delete before it and stays shown.
        let mut term = three_frames(3);
        assert_eq!(shown_frame(&mut term), Some(3));
        let before = term.transient.kitty_total_bytes;
        term.process(&apc_g("a=d,d=f,i=7,r=2", b""));
        assert_eq!(
            term.transient.kitty_frames[&7].frames.len(),
            2,
            "frame 2 went"
        );
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
        assert_eq!(term.transient.kitty_frames[&7].frames.len(), 1);
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

    /// On `a=f`, `x=`/`y=`/`c=`/`r=`/`z=` are animation keys (canvas position, base frame,
    /// edited frame, gap), never layout: the frame keeps its root's layout and draws where
    /// frame 1 did.
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
    fn non_direct_mediums_skip_cleanly_never_garbage() {
        // t=f / t=t / t=s carry a path, not pixels: with no resolver nothing is read or
        // decoded, and nothing is placed.
        for medium in ["f", "t", "s"] {
            let mut term = Terminal::new(24, 80);
            term.set_cell_pixel_size(10, 20);
            let control = format!("a=T,f=32,s=10,v=20,t={medium}");
            term.process(&apc_g(&control, b"/tmp/some/image.png"));
            assert!(
                term.cell_frame(24, 80).images[0].is_empty(),
                "t={medium} (non-direct medium) must skip cleanly — no image placed"
            );
        }
    }

    /// A host that installs a resolver serves `t=f` files and `t=s` shared
    /// memory behind the same seam, under its own policy: a name it refuses
    /// places nothing (fail closed).
    #[test]
    fn non_direct_mediums_place_only_what_the_host_resolver_serves() {
        use crate::terminal::kitty_graphics::KittyMedium;
        for (medium, name, served) in [
            ("f", "img.rgba", true),
            ("s", "/aterm-shm", true),
            ("f", "/etc/passwd", false),
            ("s", "img.rgba", false),
        ] {
            let mut term = Terminal::new(24, 80);
            term.set_cell_pixel_size(10, 20);
            term.set_kitty_file_resolver(|medium, name| {
                matches!(
                    (medium, name),
                    (KittyMedium::File, "img.rgba") | (KittyMedium::SharedMemory, "/aterm-shm")
                )
                .then(|| vec![0u8; 10 * 20 * 4])
            });
            term.process(&apc_g(
                &format!("a=T,f=32,s=10,v=20,t={medium}"),
                name.as_bytes(),
            ));
            assert_eq!(
                !term.cell_frame(24, 80).images[0].is_empty(),
                served,
                "t={medium} {name}"
            );
        }
    }

    /// A payload that is not what its keys say is dropped, never placed and
    /// never a panic: a corrupt `o=z` stream, a raw buffer shorter than
    /// `s`x`v`.
    #[test]
    fn malformed_payloads_place_nothing() {
        for (control, payload) in [
            ("a=T,f=32,s=10,v=20,o=z", &[1u8, 2, 3, 4, 5, 6][..]),
            ("a=T,f=32,s=10,v=20", &[1, 2, 3, 4]),
        ] {
            let mut term = Terminal::new(24, 80);
            term.set_cell_pixel_size(10, 20);
            term.process(&apc_g(control, payload));
            assert!(term.cell_frame(24, 80).images[0].is_empty(), "{control}");
        }
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
        // After disarming, a fresh transfer assembles and places: no corrupt pending state.
        term.process(&apc_g("a=T,f=32,s=10,v=20,m=1", &full[0..400]));
        term.process(&apc_g("m=1", &full[400..600]));
        term.process(&apc_g("m=0", &full[600..800]));
        assert!(
            !term.cell_frame(24, 80).images[0].is_empty(),
            "after disarming, the accumulator recovers and places the assembled image"
        );
    }

    /// The probe answers per medium: `t=f` errors until the opt-in resolver is installed
    /// (so the prober falls back to direct); direct is always OK.
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
        term.process(&apc_g("a=q,i=3", b""));
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b_Gi=3;OK\x1b\\",
            "direct (the default medium) is always real"
        );
        term.process(&apc_g("a=q,i=4,q=2", b""));
        assert!(term.take_response().is_none(), "q=2 suppresses the OK");
        term.set_kitty_file_resolver(|_, _| None);
        term.process(&apc_g("a=q,i=3,t=f", b""));
        assert_eq!(
            term.take_response().unwrap_or_default(),
            b"\x1b_Gi=3;OK\x1b\\",
            "with the resolver installed the same probe is OK"
        );
    }

    /// A transmit over a medium the session cannot read is answered with the
    /// verdict the `a=q` probe gives that medium, since the data was never
    /// read. Once the resolver is installed, a path it refuses is bad data.
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

    /// The bulk accumulators (`apc_put_bulk`, the parser's APC fast path, and
    /// `dcs_put_bulk` for every non-sixel DCS type) must accumulate the same
    /// `dcs.data` and budget counters as per-byte `apc_put` / `dcs_put`, even
    /// when one run straddles both the per-sequence (or per-type) data cap and
    /// the global DCS budget. `Unknown` pushes nothing without a registered
    /// callback but must still count bytes.
    #[test]
    fn bulk_string_accumulators_cross_caps_like_per_byte() {
        use crate::parser::ActionSink;
        use crate::terminal::{DcsType, MAX_DCS_GLOBAL_BUDGET};
        use aterm_provenance::pty_wrap_ref;

        // (None = APC, else that DCS type; its data cap, 0 = nothing pushed).
        let cases: &[(Option<DcsType>, usize)] = &[
            (None, 4 * 1024 * 1024),
            (Some(DcsType::Decrqss), 256),
            (Some(DcsType::Xtgettcap), 1024),
            (Some(DcsType::Unknown), 0),
        ];
        // Seed the data 100 bytes below its cap and the budget 200 below full,
        // so a 300-byte run crosses both; the bulk path takes it in three
        // uneven chunks so a crossing falls between calls too.
        let run: Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();
        let drive = |bulk: bool, kind: Option<DcsType>, cap: usize| {
            let mut term = Terminal::new(24, 80);
            let (_parser, mut h) = term.split_for_process();
            if let Some(dcs_type) = kind {
                h.dcs.dcs_type = dcs_type;
            }
            h.dcs.data = vec![0xAB; cap.saturating_sub(100)];
            h.dcs.total_bytes = MAX_DCS_GLOBAL_BUDGET - 200;
            h.dcs.sequence_bytes = MAX_DCS_GLOBAL_BUDGET - 200;
            for chunk in [&run[..150], &run[150..151], &run[151..]] {
                match (bulk, kind) {
                    (true, None) => h.apc_put_bulk(pty_wrap_ref(chunk)),
                    (true, Some(_)) => h.dcs_put_bulk(pty_wrap_ref(chunk)),
                    (false, None) => chunk.iter().for_each(|&b| h.apc_put(b)),
                    (false, Some(_)) => chunk.iter().for_each(|&b| h.dcs_put(b)),
                }
            }
            (h.dcs.data.clone(), h.dcs.total_bytes, h.dcs.sequence_bytes)
        };
        for &(kind, cap) in cases {
            let per_byte = drive(false, kind, cap);
            assert_eq!(per_byte, drive(true, kind, cap), "{kind:?}");
            assert_eq!(
                per_byte.1, MAX_DCS_GLOBAL_BUDGET,
                "{kind:?}: budget reached"
            );
            if cap > 0 {
                assert_eq!(per_byte.0.len(), cap, "{kind:?}: data cap reached");
            }
        }
    }

    /// Bulk/per-byte parity for the Sixel DCS path: for an in-budget stream
    /// the run-sampled pixel-allocation charge must land on identical budget
    /// counters and an identical decoder allocation.
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

    /// RIS (`ESC c`) clears the Kitty store and its byte budget together, so a later in-budget
    /// image is admitted and a stale id no longer displays.
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

        // ...and the in-flight chunked transfer, so a partial `m=1` load is
        // not merged into the first post-reset command.
        term.process(&apc_g("a=T,f=32,s=4,v=4,m=1", &img));
        assert!(term.transient.kitty_pending.is_some(), "precondition");
        term.process(b"\x1bc");
        assert!(term.transient.kitty_pending.is_none());
    }

    /// The Kitty store enforces a global byte budget: once adding an image
    /// would exceed `MAX_KITTY_STORE_BYTES`, the transfer is rejected and the
    /// counter is unchanged; existing images remain.
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

    /// A transmit's footprint and scaling come from its keys, as kitty
    /// computes them: with neither `c=` nor `r=` (or both `0`, unspecified)
    /// the raster's pixel size rounded up to whole cells, drawn pixel-exact; a
    /// named cell box is fitted; one of `c=`/`r=` alone takes the other from
    /// the aspect. `(raster px, keys, (cols, rows), scaling)` at a 10x20 cell.
    #[test]
    fn a_transmits_footprint_and_scaling_follow_its_keys() {
        use aterm_grid::ImageScaling::{Fit, PixelExact};
        for ((w, h), keys, footprint, scaling) in [
            ((15, 25), "", (2, 2), PixelExact),
            ((15, 25), ",c=4,r=3", (4, 3), Fit),
            ((20, 40), ",c=4", (4, 4), Fit),
            ((20, 40), ",r=1", (1, 1), Fit),
            ((20, 40), ",c=0,r=0", (2, 2), PixelExact),
        ] {
            let mut term = Terminal::new(24, 80);
            term.set_cell_pixel_size(10, 20);
            term.process(&apc_g(
                &format!("a=T,f=32,s={w},v={h}{keys}"),
                &raw_rgba(w, h),
            ));
            let placed = image_at(&term, 0, 0).expect("a=T places an image");
            assert_eq!(
                ((placed.image.cols, placed.image.rows), placed.image.scaling),
                (footprint, scaling),
                "{w}x{h}{keys}"
            );
        }
    }

    /// The one image placed at `(row, col)` of the visible grid, if any.
    fn image_at(term: &Terminal, row: usize, col: usize) -> Option<aterm_grid::ImageRef> {
        term.images_row(row)
            .into_iter()
            .find(|(c, _)| *c == col)
            .map(|(_, image)| image)
    }

    /// The Kitty image ids layered in cell `(row, col)`, bottom first — the
    /// order the renderers draw them in (`0` for an image that is not Kitty's).
    fn layers_at(term: &Terminal, row: usize, col: usize) -> Vec<u32> {
        term.images_row(row)
            .into_iter()
            .filter(|(c, _)| *c == col)
            .map(|(_, image)| image.kitty.map_or(0, |tag| tag.image_id))
            .collect()
    }

    /// Kitty placements stack by z-index, first-sent order, then placement order; deleting
    /// the top uncovers the one beneath; an iTerm2 or sixel image takes a cell whole.
    #[test]
    fn overlapping_placements_stack_by_z_then_image_then_placement_order() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        for id in 1..=3 {
            term.process(&apc_g(
                &format!("a=t,f=32,s=20,v=20,i={id},q=2"),
                &raw_rgba(20, 20),
            ));
        }
        term.process(b"\x1b[1;1H");
        term.process(&apc_g("a=p,i=2,q=2", b""));
        term.process(b"\x1b[1;2H");
        term.process(&apc_g("a=p,i=1,q=2", b""));
        assert_eq!(layers_at(&term, 0, 0), [2], "only image 2 reaches col 0");
        assert_eq!(
            layers_at(&term, 0, 1),
            [1, 2],
            "one z: the image sent first lies lower"
        );
        assert_eq!(layers_at(&term, 0, 2), [1]);

        term.process(b"\x1b[1;2H");
        term.process(&apc_g("a=p,i=3,z=-1,q=2", b""));
        term.process(b"\x1b[1;2H");
        term.process(&apc_g("a=p,i=3,z=4,q=2", b""));
        assert_eq!(layers_at(&term, 0, 1), [3, 1, 2, 3], "then by z");
        term.process(b"\x1b[1;1H");
        term.process(&apc_g("a=p,i=1,q=2", b""));
        assert_eq!(
            layers_at(&term, 0, 1),
            [3, 1, 1, 2, 3],
            "one image, one z: the newer placement above"
        );

        // Deleting uncovers what lay beneath.
        term.process(&apc_g("a=d,d=i,i=2", b""));
        assert_eq!(layers_at(&term, 0, 0), [1]);
        assert_eq!(layers_at(&term, 0, 1), [3, 1, 1, 3]);
        term.process(&apc_g("a=d,d=z,z=4", b""));
        assert_eq!(layers_at(&term, 0, 1), [3, 1, 1]);

        // A layer ordering survives the trip into history and back into view:
        // 30 line feeds from row 0 of 24 push it 7 lines up into history.
        term.process(&[b'\n'; 30]);
        term.scroll_display(30);
        assert_eq!(term.grid().display_offset(), 7);
        assert_eq!(layers_at(&term, 0, 1), [3, 1, 1], "scrolled back");

        // An image that is not Kitty's owns the cells it covers.
        term.scroll_display(-30);
        term.process(b"\x1b[1;1H");
        term.process(&apc_g("a=p,i=1,q=2", b""));
        term.process(b"\x1b[1;1H");
        term.process(b"\x1b]1337;File=inline=1;width=1;height=1:AAAA\x07");
        assert_eq!(layers_at(&term, 0, 0), [0]);
    }

    /// Images of one z-index stack in first-sent order, whatever their ids; sending one again
    /// keeps its place.
    #[test]
    fn images_of_one_z_stack_in_the_order_they_were_first_sent() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        for id in [9, 4] {
            term.process(&apc_g(
                &format!("a=t,f=32,s=10,v=20,i={id},q=2"),
                &one_cell_rgba(),
            ));
        }
        for put in ["a=p,i=9,C=1,q=2", "a=p,i=4,C=1,q=2"] {
            term.process(b"\x1b[1;1H");
            term.process(&apc_g(put, b""));
        }
        term.process(&apc_g("a=T,f=32,s=10,v=20,C=1,q=2", &one_cell_rgba()));
        assert_eq!(layers_at(&term, 0, 0), [9, 4, 0]);

        term.process(&apc_g("a=t,f=32,s=10,v=20,i=9,q=2", &one_cell_rgba()));
        assert_eq!(layers_at(&term, 0, 0), [4, 0], "sent again: unplaced");
        term.process(&apc_g("a=p,i=9,C=1,q=2", b""));
        assert_eq!(layers_at(&term, 0, 0), [9, 4, 0], "and back in its place");
    }

    /// Redrawing an image exactly over its unnamed placement (only the crop corner may differ)
    /// replaces it, relatives following; a put that differs (z-index) still stacks.
    #[test]
    fn a_redraw_in_place_replaces_the_placement_it_covers() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        for frame in 1..=20u8 {
            term.process(b"\x1b[1;1H");
            term.process(&apc_g("a=T,f=32,s=10,v=20,c=2,r=1,C=1,q=2", &[frame; 800]));
        }
        let shown: Vec<u8> = term
            .images_row(0)
            .into_iter()
            .filter(|(col, _)| *col == 0)
            .map(|(_, image)| image.image.bytes[0])
            .collect();
        assert_eq!(shown, [20], "one layer: the newest frame");

        term.process(&apc_g("a=t,f=32,s=20,v=20,i=1,q=2", &raw_rgba(20, 20)));
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=3,q=2", &one_cell_rgba()));
        // Every crop 5 pixels square (one clamped narrower would not cover).
        for x in 0..16 {
            term.process(b"\x1b[3;1H");
            term.process(&apc_g(&format!("a=p,i=1,x={x},w=5,h=5,C=1,q=2"), b""));
        }
        assert_eq!(layers_at(&term, 2, 0), [1], "a re-crop replaces too");
        term.process(&apc_g("a=p,i=3,P=1,H=1,q=2", b""));
        assert_eq!(layers_at(&term, 2, 1), [3], "a child of the redrawn one");
        term.process(b"\x1b[3;1H");
        term.process(&apc_g("a=p,i=1,x=7,w=5,h=5,C=1,q=2", b""));
        assert_eq!(layers_at(&term, 2, 0), [1]);
        assert_eq!(layers_at(&term, 2, 1), [3], "follows its redraw");
        term.process(&apc_g("a=d,d=i,i=1", b""));
        assert_eq!(
            layers_at(&term, 2, 1),
            Vec::<u32>::new(),
            "and goes with it"
        );

        term.process(b"\x1b[5;1H");
        term.process(&apc_g("a=p,i=1,C=1,q=2", b""));
        term.process(&apc_g("a=p,i=1,z=1,C=1,q=2", b""));
        assert_eq!(layers_at(&term, 4, 0), [1, 1], "another z-index stacks");
    }

    /// Placements relative to a virtual one are capped (`MAX_KITTY_FLOATING`), a redraw
    /// replaces one, and a frame carries each cell's top `MAX_IMAGE_LAYERS`.
    #[test]
    fn floating_placements_are_capped_and_a_frame_keeps_each_cells_top_layers() {
        use aterm_grid::MAX_IMAGE_LAYERS;
        let cap = super::MAX_KITTY_FLOATING;
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        for id in (1..=2).chain(10..10 + MAX_IMAGE_LAYERS as u32) {
            term.process(&apc_g(
                &format!("a=t,f=32,s=10,v=20,i={id},q=2"),
                &one_cell_rgba(),
            ));
        }
        term.process(&apc_g("a=p,U=1,i=1,p=7,q=2", b""));
        term.process(b"\x1b[1;1H");
        term.process(&placeholder(1, 7, 0, 0));
        for _ in 0..40 {
            term.process(&apc_g("a=p,i=2,P=1,Q=7,c=20,r=4,z=1,q=2", b""));
        }
        assert_eq!(term.transient.kitty_floating.len(), 1, "a redraw replaces");
        let mut refused = 0;
        for h in 1..=cap + 3 {
            term.process(&apc_g(&format!("a=p,i=2,P=1,Q=7,H={h},c=20,r=4,z=1"), b""));
            if reply(&mut term).contains(";ENOSPC") {
                refused += 1;
            }
        }
        assert_eq!(term.transient.kitty_floating.len(), cap);
        assert_eq!(refused, 4, "every put past the cap");

        // Sixteen more images under them in cell (1, 19).
        for id in 10..10 + MAX_IMAGE_LAYERS as u32 {
            term.process(b"\x1b[2;20H");
            term.process(&apc_g(&format!("a=p,i={id},C=1,q=2"), b""));
        }
        let top = vec![(2, 0); MAX_IMAGE_LAYERS];
        assert_eq!(tags_at(&term, 1, 19), top, "the top layers, per row");
        let frame = term.cell_frame(24, 80);
        let batch: Vec<(u32, u32)> = frame.images[1]
            .iter()
            .filter(|(col, _)| *col == 19)
            .filter_map(|(_, image)| image.kitty)
            .map(|tag| (tag.image_id, tag.placement_id))
            .collect();
        assert_eq!(batch, top, "and per frame");
    }

    /// An animation shown only relative to a virtual placement plays only while its parent's
    /// placeholders show.
    #[test]
    fn an_animation_drawn_nowhere_through_a_virtual_parent_does_not_play() {
        use crate::terminal::KittyAnimationStep;
        let at = clock();
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        for control in [
            "a=t,f=32,s=10,v=20,i=1,q=2",
            "a=t,f=32,s=10,v=20,i=2,q=2",
            "a=f,f=32,s=10,v=20,i=2,z=30,q=2",
        ] {
            term.process_at(&apc_g(control, &one_cell_rgba()), at(0));
        }
        term.process_at(&apc_g("a=a,i=2,r=1,z=30", b""), at(0));
        term.process_at(&apc_g("a=p,U=1,i=1,p=7,q=2", b""), at(0));
        term.process_at(b"\x1b[2;3H", at(0));
        term.process_at(&placeholder(1, 7, 0, 0), at(0));
        term.process_at(&apc_g("a=p,i=2,P=1,Q=7,H=1,V=1,q=2", b""), at(0));
        term.process_at(&apc_g("a=a,i=2,s=3", b""), at(0));
        assert!(term.advance_kitty_animations(at(30).monotonic).advanced);

        term.process_at(b"\x1b[H\x1b[2J", at(40));
        for ms in [60, 90, 120, 150] {
            assert_eq!(
                term.advance_kitty_animations(at(ms).monotonic),
                KittyAnimationStep::default(),
                "drawn nowhere at {ms} ms"
            );
        }
        term.process_at(b"\x1b[2;3H", at(160));
        term.process_at(&placeholder(1, 7, 0, 0), at(160));
        assert!(
            term.advance_kitty_animations(at(170).monotonic).advanced,
            "shown again: it plays on"
        );
    }

    /// `X=`/`Y=` start a put's image inside its first cell and grow the footprint (and the
    /// cursor's move) to keep it whole; a virtual placement takes no offset.
    #[test]
    fn put_offsets_start_the_image_inside_its_first_cell() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=32,s=10,v=20,i=1,q=2", &raw_rgba(10, 20)));
        term.process(&apc_g("a=p,i=1,X=4,Y=6,q=2", b""));
        let placed = image_at(&term, 0, 0).expect("placed");
        assert_eq!(placed.image.cell_offset_px, (4, 6));
        assert_eq!(
            (placed.image.cols, placed.image.rows),
            (2, 2),
            "4+10 px wide and 6+20 tall need two cells each way"
        );
        let at = term.cursor();
        assert_eq!((at.row, at.col), (1, 2), "past the whole footprint");

        term.process(b"\x1b[5;1H");
        term.process(&apc_g("a=p,i=1,X=99,Y=99,q=2", b""));
        let clamped = image_at(&term, 4, 0).expect("placed");
        assert_eq!(clamped.image.cell_offset_px, (9, 19), "below a cell");

        // With `c=` alone the rows follow the aspect of the width plus the
        // offset: 2*10 + 5 = 25 px wide is 50 px tall, three rows.
        term.process(b"\x1b[10;1H");
        term.process(&apc_g("a=p,i=1,c=2,X=5,q=2", b""));
        let boxed = image_at(&term, 9, 0).expect("placed");
        assert_eq!((boxed.image.cols, boxed.image.rows), (2, 3));

        term.process(&apc_g("a=p,U=1,i=1,p=3,X=4,Y=6,q=2", b""));
        assert_eq!(
            term.transient.kitty_virtual[&1][0].image.cell_offset_px,
            (0, 0),
            "a virtual placement takes none"
        );
    }

    /// A put lays the image out by its own keys (`c=`/`r=` box, `x=`/`y=`/`w=`/`h=` crop),
    /// not the transmit's: a variant, reused by an identical put; a keyless put reuses the
    /// stored image.
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

    /// Variants are budgeted and capped per id, but unshown ones are recycled, so re-cropping
    /// on every scroll never runs out. Control: all kept on screen, the cap refuses the put.
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

    /// Re-transmitting an id replaces its data and removes its placements,
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

    /// Kitty's cursor policy: just past the image's right edge on its last row (or the next
    /// line's start); `C=1` keeps it. iTerm2's fresh line below the image is the control.
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

    /// `U=1` is a virtual placement, shown only where the client prints
    /// Unicode placeholders; stamping it at the cursor too would draw the
    /// picture twice and move the cursor.
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

    /// `a=a c=N` shows frame N everywhere the image shows: a placement at the
    /// cursor switches too, keeping its own layout, and the placeholder cells
    /// naming the image are damaged so they repaint.
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

        // `r=` (with `z=`) names the frame whose gap changes; it selects
        // nothing.
        term.process(&apc_g("a=a,i=7,r=1,z=80", b""));
        assert_eq!(term.cell_frame(24, 80).images[5][0].1.image.bytes[0], 2);
    }

    /// A `U=1` put has its own id and cell box, named by a placeholder's underline colour;
    /// an unknown placement id draws nothing, no underline colour picks the newest.
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

    /// `P=`/`Q=`: a relative placement moves and is deleted with its parent, the cursor
    /// untouched; kitty's errors come back for a missing parent, a cycle and virtual misuse.
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

    /// `(image id, placement id)` of each Kitty placement covering cell `(row, col)`, bottom
    /// first.
    fn tags_at(term: &Terminal, row: usize, col: usize) -> Vec<(u32, u32)> {
        term.images_row(row)
            .into_iter()
            .filter(|(c, _)| *c == col)
            .filter_map(|(_, image)| image.kitty)
            .map(|tag| (tag.image_id, tag.placement_id))
            .collect()
    }

    /// A placement relative to a virtual placement shows `H=`/`V=` cells from the top-left of
    /// its parent's placeholders, follows them (chains too), is kept but drawn nowhere with no
    /// placeholder on screen, and is deleted with its parent.
    #[test]
    fn placements_relative_to_a_virtual_placement_follow_its_placeholders() {
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        for id in 1..=3 {
            term.process(&apc_g(
                &format!("a=t,f=32,s=10,v=20,i={id},q=2"),
                &one_cell_rgba(),
            ));
        }
        term.process(&apc_g("a=p,U=1,i=1,p=7,q=2", b""));
        term.process(b"\x1b[4;6H");
        term.process(&placeholder(1, 7, 0, 1));
        term.process(b"\x1b[5;5H");
        term.process(&placeholder(1, 7, 1, 0));
        term.process(b"\x1b[20;20H");
        term.process(&apc_g("a=p,i=2,p=5,P=1,Q=7,H=2,V=1", b""));
        assert_eq!(reply(&mut term), "\x1b_Gi=2,p=5;OK\x1b\\");
        assert_eq!(
            tags_at(&term, 4, 6),
            [(2, 5)],
            "(top row 3, left col 4) + (V=1, H=2)"
        );
        let at = term.cursor();
        assert_eq!((at.row, at.col), (19, 19), "the cursor does not move");
        term.process(&apc_g("a=p,i=3,p=6,P=2,Q=5,V=2,q=2", b""));
        assert_eq!(tags_at(&term, 6, 6), [(3, 6)], "a chain follows in turn");

        // The placeholders move; the placements follow them.
        term.process(b"\x1b[4;1H\x1b[2K\x1b[5;1H\x1b[2K\x1b[10;1H");
        term.process(&placeholder(1, 7, 0, 0));
        assert_eq!(tags_at(&term, 4, 6), Vec::<(u32, u32)>::new());
        assert_eq!(tags_at(&term, 10, 2), [(2, 5)], "(9, 0) + (1, 2)");
        assert_eq!(tags_at(&term, 12, 2), [(3, 6)]);

        // With no placeholder on screen they are kept, and shown nowhere.
        term.process(b"\x1b[10;1H\x1b[2K");
        assert!(
            (0..24).all(|row| term.images_row(row).is_empty()),
            "nothing drawn"
        );
        assert_eq!(term.transient.kitty_floating.len(), 2, "but kept");
        term.process(b"\x1b[1;1H");
        term.process(&placeholder(1, 7, 0, 0));
        assert_eq!(tags_at(&term, 1, 2), [(2, 5)], "back with its parent");

        // A cycle through floating parents is refused, moving nothing.
        term.process(&apc_g("a=p,i=2,p=5,P=3,Q=6", b""));
        assert!(reply(&mut term).contains(";ECYCLE"));
        assert_eq!(tags_at(&term, 1, 2), [(2, 5)], "refusals moved nothing");

        // A cell a floating placement covers is a delete target there.
        term.process(&apc_g("a=d,d=p,x=3,y=4", b""));
        assert_eq!(
            tags_at(&term, 3, 2),
            Vec::<(u32, u32)>::new(),
            "(3, 6) went"
        );
        assert_eq!(tags_at(&term, 1, 2), [(2, 5)]);
        assert!(
            term.transient.kitty_images.contains_key(&3),
            "a lowercase delete keeps the data"
        );

        // Deleting the virtual parent deletes its relatives, and the image of
        // each left with no placement.
        term.process(&apc_g("a=d,d=i,i=1,p=7", b""));
        assert!(term.transient.kitty_floating.is_empty());
        assert!(
            !term.transient.kitty_images.contains_key(&2),
            "deleted with its parent, with no placement left: its image went"
        );
        assert!(
            term.transient.kitty_images.contains_key(&1),
            "d=i keeps its own data"
        );
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
    /// the frame.
    #[test]
    fn frames_are_composed_on_a_canvas() {
        const RED: [u8; 4] = [255, 0, 0, 255];
        const GREEN: [u8; 4] = [0, 255, 0, 255];
        const BLUE: [u8; 4] = [0, 0, 255, 255];
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=32,s=2,v=1,i=7,q=1", &[RED, GREEN].concat()));
        let frame = |term: &Terminal, n: usize| {
            term.transient.kitty_frames[&7].frames[n]
                .image
                .bytes
                .clone()
        };

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
            term.transient.kitty_frames[&7].frames.len(),
            4,
            "no frame was added"
        );

        // Refusals: too large, a PNG that does not decode, a missing base
        // frame.
        term.process(&apc_g("a=f,f=32,s=3,v=1,i=7", &[0; 12]));
        assert!(reply(&mut term).contains(";EINVAL"), "wider than the image");
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1u32.to_be_bytes());
        png.extend_from_slice(&1u32.to_be_bytes());
        term.process(&apc_g("a=f,f=100,i=7,x=1", &png));
        assert!(
            reply(&mut term).contains(";EBADPNG"),
            "a truncated PNG patch"
        );
        term.process(&apc_g("a=f,f=32,s=1,v=1,i=7,c=9", &BLUE));
        assert!(
            reply(&mut term).contains(";EINVAL"),
            "no frame 9 to draw over"
        );
        assert_eq!(term.transient.kitty_frames[&7].frames.len(), 4);
    }

    /// A `w × h` RGBA PNG of `pixels`, row-major.
    fn png(w: u32, h: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = aterm_png::Encoder::new(&mut out, w, h);
        enc.set_color(aterm_png::ColorType::Rgba);
        enc.set_depth(aterm_png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&pixels.concat()).expect("png data");
        drop(writer);
        out
    }

    /// PNG frames compose like raw ones (frame data, root, edit and `a=c`): decoded, composed
    /// and kept as raw pixels.
    #[test]
    fn png_frames_are_decoded_to_compose() {
        const RED: [u8; 4] = [255, 0, 0, 255];
        const GREEN: [u8; 4] = [0, 255, 0, 255];
        const BLUE: [u8; 4] = [0, 0, 255, 255];
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        term.process(&apc_g("a=t,f=100,i=7,q=1", &png(2, 1, &[RED, GREEN])));
        let frame = |term: &Terminal, n: usize| {
            term.transient.kitty_frames[&7].frames[n]
                .image
                .bytes
                .clone()
        };

        term.process(&apc_g("a=f,f=100,i=7,x=1", &png(1, 1, &[BLUE])));
        assert_eq!(reply(&mut term), "\x1b_Gi=7,r=2;OK\x1b\\");
        assert_eq!(frame(&term, 1), [[0; 4], BLUE].concat(), "PNG data");
        term.process(&apc_g("a=f,f=32,s=1,v=1,i=7,x=1,c=1", &BLUE));
        assert_eq!(reply(&mut term), "\x1b_Gi=7,r=3;OK\x1b\\");
        assert_eq!(frame(&term, 2), [RED, BLUE].concat(), "over the PNG root");
        term.process(&apc_g("a=f,f=100,i=7,r=1,X=1", &png(1, 1, &[BLUE])));
        assert_eq!(reply(&mut term), "\x1b_Gi=7,r=1;OK\x1b\\");
        assert_eq!(frame(&term, 0), [BLUE, GREEN].concat(), "the root edited");
        assert_eq!(
            term.transient.kitty_images[&7].bytes,
            frame(&term, 0),
            "the edited root is still the frame shown"
        );

        term.process(&apc_g("a=t,f=100,i=8,q=2", &png(2, 1, &[RED, GREEN])));
        term.process(&apc_g("a=f,f=100,i=8,q=2", &png(2, 1, &[BLUE, BLUE])));
        term.process(&apc_g("a=c,i=8,r=1,c=2,w=1,h=1,x=1", b""));
        assert_eq!(reply(&mut term), "\x1b_Gi=8;OK\x1b\\");
        assert_eq!(
            term.transient.kitty_frames[&8].frames[1].image.bytes,
            [BLUE, RED].concat(),
            "PNG frame 1's (0,0) onto PNG frame 2's (1,0)"
        );
    }

    /// `a=c` composes a rectangle of one frame onto another, and refuses what
    /// kitty refuses.
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
            term.transient.kitty_frames[&7].frames[1].image.bytes,
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

    /// A put's in-cell offset (`X=`/`Y=`) added to a client's huge length (`c=`/`r=`, or a PNG
    /// header's size) saturates, and the footprint is the grid's.
    #[test]
    fn huge_lengths_with_an_in_cell_offset_fill_the_grid() {
        // The footprint, in cells, of one transmit-and-display on a fresh grid.
        let footprint = |control: &str, payload: &[u8]| {
            let mut term = Terminal::new(24, 80);
            term.set_cell_pixel_size(10, 20);
            term.process(&apc_g(control, payload));
            let placed = image_at(&term, 0, 0).expect("placed");
            (placed.image.cols, placed.image.rows)
        };
        let max = u32::MAX;
        for control in [
            format!("a=T,f=32,s=10,v=20,c={max},X=5,Y=5,C=1,q=2"),
            format!("a=T,f=32,s=10,v=20,r={max},X=5,Y=5,C=1,q=2"),
        ] {
            assert_eq!(footprint(&control, &one_cell_rgba()), (80, 24), "{control}");
        }
        // A PNG header claiming u32::MAX x u32::MAX pixels (the engine reads no further).
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&max.to_be_bytes());
        png.extend_from_slice(&max.to_be_bytes());
        assert_eq!(footprint("a=T,f=100,X=5,Y=5,C=1,q=2", &png), (80, 24));
    }

    /// A whole image declaring a side over kitty's limit is refused, raw or PNG; one exactly at
    /// the limit is stored. Each refused payload is otherwise well-formed.
    #[test]
    fn a_whole_image_declaring_a_side_over_kittys_limit_is_refused() {
        let max = super::kitty_graphics::MAX_KITTY_DIMENSION;
        let side = usize::try_from(max).expect("fits");
        let one_px = png(1, 1, &[[0; 4]]);
        let cases: [(String, Vec<u8>, &str); 6] = [
            (
                format!("a=t,f=24,s={},v=1,i=7", max + 1),
                vec![0; (side + 1) * 3],
                "EINVAL",
            ),
            (format!("a=t,f=24,s={max},v=1,i=7"), vec![0; side * 3], "OK"),
            (
                format!("a=t,f=32,s=1,v={},i=7", max + 1),
                vec![0; (side + 1) * 4],
                "EINVAL",
            ),
            (format!("a=t,f=32,s=1,v={max},i=7"), vec![0; side * 4], "OK"),
            (
                format!("a=t,f=100,s={},v=1,i=7", max + 1),
                one_px.clone(),
                "EINVAL",
            ),
            (format!("a=t,f=100,s={max},v=1,i=7"), one_px, "OK"),
        ];
        for (control, payload, verdict) in cases {
            let mut term = Terminal::new(24, 80);
            term.set_cell_pixel_size(10, 20);
            term.process(&apc_g(&control, &payload));
            let answer = reply(&mut term);
            assert!(
                answer.starts_with(&format!("\x1b_Gi=7;{verdict}")),
                "{control}: {answer:?}"
            );
            assert_eq!(
                term.transient.kitty_images.contains_key(&7),
                verdict == "OK",
                "{control}: whether it was stored"
            );
        }
    }

    /// A zlib stream (RFC 1950) of `len` zero bytes, `len >= 1`: a literal zero, then
    /// 258-byte back-references at distance 1 (about 13 bits per 258 bytes).
    fn zlib_zeros(len: usize) -> Vec<u8> {
        // Deflate packs fields low bit first; a Huffman code goes out most
        // significant bit first.
        let mut bits: Vec<u8> = Vec::new();
        let mut field = |value: u32, n: u32| bits.extend((0..n).map(|i| ((value >> i) & 1) as u8));
        let code = |code: u32, n: u32| (0..n).fold(0, |r, i| r | ((code >> i) & 1) << (n - 1 - i));
        field(0b011, 3); // BFINAL, fixed Huffman
        field(code(0x30, 8), 8); // literal 0
        let mut left = len - 1;
        while left >= 258 {
            field(code(0b1100_0101, 8), 8); // length 258
            field(0, 5); // distance 1
            left -= 258;
        }
        for _ in 0..left {
            field(code(0x30, 8), 8);
        }
        field(0, 7); // end of block
        let mut out = vec![0x78, 0x01];
        out.extend(bits.chunks(8).map(|byte| {
            byte.iter()
                .enumerate()
                .fold(0u8, |acc, (i, &bit)| acc | (bit << i))
        }));
        // Adler-32 of zeros: `a` stays 1, `b` counts the bytes.
        let b = u32::try_from(len % 65521).expect("below 65521");
        out.extend_from_slice(&((b << 16) | 1).to_be_bytes());
        out
    }

    /// The payload bytes of the distinct images the visible screen shows.
    fn live_image_bytes(term: &Terminal) -> usize {
        let mut seen = std::collections::HashSet::new();
        let mut bytes = 0;
        for row in 0..usize::from(term.grid().rows()) {
            for (_, layer) in term.images_row(row) {
                if seen.insert(std::sync::Arc::as_ptr(&layer.image)) {
                    bytes += layer.image.payload_bytes();
                }
            }
        }
        bytes
    }

    /// Kitty images the store does not keep (sent without an id, or refused for room) still
    /// show, so they count against the screen's image budget.
    #[test]
    fn unstored_kitty_images_stay_within_the_live_budget() {
        use crate::terminal::handler_osc_1337::MAX_LIVE_IMAGE_BYTES;
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        let side = 1024;
        let raster = side * side * 4;
        let zeros = zlib_zeros(raster);
        assert_eq!(
            aterm_codec::inflate::zlib_decompress(&zeros, raster).map(|out| out.len()),
            Ok(raster),
            "the helper writes a valid stream"
        );
        for col in 1..=40 {
            term.process(format!("\x1b[1;{col}H").as_bytes());
            let control = format!("a=T,f=32,s={side},v={side},o=z,c=1,r=1,C=1,q=2");
            term.process(&apc_g(&control, &zeros));
            assert!(
                image_at(&term, 0, col - 1).is_some(),
                "the image just sent shows"
            );
            let live = live_image_bytes(&term);
            assert!(
                live <= MAX_LIVE_IMAGE_BYTES + raster,
                "after {col} images the screen holds {live} bytes of them"
            );
        }
    }

    /// Images the Kitty store frees while a parked screen still shows them count against that
    /// screen's budget.
    #[test]
    fn kitty_images_freed_behind_a_parked_screen_stay_within_the_live_budget() {
        use crate::terminal::handler_osc_1337::MAX_LIVE_IMAGE_BYTES;
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        let side = 1024;
        let raster = side * side * 4;
        let zeros = zlib_zeros(raster);
        let control = format!("a=T,i=1,f=32,s={side},v={side},o=z,c=1,r=1,C=1,q=2");
        for col in 1..=48 {
            term.process(format!("\x1b[1;{col}H").as_bytes());
            term.process(&apc_g(&control, &zeros));
            term.process(b"\x1b[?1049h");
            term.process(&apc_g("a=d,d=I,i=1,q=2", &[]));
            term.process(b"\x1b[?1049l");
            assert!(
                image_at(&term, 0, col - 1).is_some(),
                "the parked screen keeps the image the store freed"
            );
            let live = live_image_bytes(&term);
            assert!(
                live <= MAX_LIVE_IMAGE_BYTES + raster,
                "after {col} rounds the main screen holds {live} bytes of images"
            );
        }
    }

    /// The same for a frame the store replaces while the parked main screen still shows it.
    #[test]
    fn kitty_frames_replaced_behind_a_parked_screen_stay_within_the_live_budget() {
        use crate::terminal::handler_osc_1337::MAX_LIVE_IMAGE_BYTES;
        let mut term = Terminal::new(24, 80);
        term.set_cell_pixel_size(10, 20);
        let side = 1024;
        let raster = side * side * 4;
        let zeros = zlib_zeros(raster);
        let transmit = format!("a=T,i=1,f=32,s={side},v={side},o=z,c=1,r=1,C=1,q=2");
        let edit = format!("a=f,i=1,r=1,f=32,s={side},v={side},o=z,q=2");
        for col in 1..=48 {
            term.process(format!("\x1b[1;{col}H").as_bytes());
            if col == 1 {
                term.process(&apc_g(&transmit, &zeros));
            } else {
                term.process(&apc_g("a=p,i=1,c=1,r=1,C=1,q=2", &[]));
            }
            term.process(b"\x1b[?1049h");
            term.process(&apc_g(&edit, &zeros));
            term.process(b"\x1b[?1049l");
            assert!(
                image_at(&term, 0, col - 1).is_some(),
                "the parked screen keeps the frame the store replaced"
            );
            let live = live_image_bytes(&term);
            assert!(
                live <= MAX_LIVE_IMAGE_BYTES + raster,
                "after {col} rounds the main screen holds {live} bytes of images"
            );
        }
    }
}
