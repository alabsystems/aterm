// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `ActionSink` implementation, bulk ASCII writers, and Kitty graphics for `TerminalHandler`.

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
    #[inline]
    pub(super) fn stamp_print_anchor(&mut self) {
        let cursor = self.grid.cursor();
        let col = cursor
            .col
            .saturating_add(u16::from(self.grid.pending_wrap()));
        self.transient.print_anchor = Some((cursor.row, col));
        self.transient.print_anchor_seq = self.transient.print_anchor_seq.wrapping_add(1);
    }

    /// Print a run of printable ASCII outside VT52 addressing: `print_ascii_bulk`'s writer, which
    /// the unicode run writer also calls for the ASCII the parser joins between the characters
    /// of a run, wherever it does not write that ASCII itself.
    #[allow(
        clippy::inline_always,
        reason = "measured: as a call (plain #[inline] kept it one) a line of ASCII cost 1.2% more \
                  instructions than print_ascii_bulk's body inline"
    )]
    #[inline(always)]
    pub(super) fn write_ascii_print(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

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

        let origin = self.grid.output_damage_origin();
        if self.style.has_style_extras() || self.transient.has_transient_extras {
            self.write_ascii_bulk_with_extras(data);
        } else {
            self.write_ascii_bulk_fast(data);
        }
        self.transient.last_combining_was_zwj = false;
        self.grid.damage_selection_output(origin);
        self.stamp_print_anchor();
    }
}

impl ActionSink for TerminalHandler<'_> {
    fn print(&mut self, c: char) {
        self.note_sync_open_action();
        match self.transient.vt52_cursor_state {
            Vt52CursorState::WaitingRow => {
                let row = (c as u8).saturating_sub(32);
                self.transient.vt52_cursor_state = Vt52CursorState::WaitingCol(row);
                return;
            }
            Vt52CursorState::WaitingCol(row) => {
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

    fn print_ascii_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        self.note_sync_open_action();
        let mut data = data.as_ref();
        while self.transient.vt52_cursor_state != Vt52CursorState::None {
            let Some((&byte, rest)) = data.split_first() else {
                return;
            };
            self.print(byte as char);
            data = rest;
        }
        self.write_ascii_print(data);
    }

    fn print_unicode_bulk(&mut self, chars: &Provenance<[char], Pty>) {
        self.note_sync_open_action();
        let mut chars = chars.as_ref();
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

        let origin = self.grid.output_damage_origin();
        self.write_unicode_bulk(chars);
        self.grid.damage_selection_output(origin);
        self.stamp_print_anchor();
    }

    const TAKES_LINES: bool = true;

    fn print_lines(&mut self, lines: &Provenance<[u8], Pty>) {
        self.print_line_run(lines.as_ref());
    }

    fn execute(&mut self, byte: u8) {
        self.note_sync_open_action();
        self.transient.vt52_cursor_state = Vt52CursorState::None;

        self.charset.clear_single_shift();

        match byte {
            0x07 => self.handle_bell(),
            0x08 => {
                // BS.
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
                        self.grid.set_cursor_following_content(row - 1, wrap_col);
                    }
                } else {
                    self.grid
                        .cursor_backward_margin(1, self.modes.left_right_margin_mode);
                }
            }
            0x09 => self.grid.tab_margin(self.modes.left_right_margin_mode), // HT
            0x0A..=0x0C => {
                // xterm `CASE_VMOT`: the index, then LNM's carriage return.
                self.margined_line_feed(self.modes.left_right_margin_mode);
                if self.modes.new_line_mode {
                    self.grid
                        .carriage_return_margin(self.modes.left_right_margin_mode);
                }
            }
            0x0D => self
                .grid
                .carriage_return_margin(self.modes.left_right_margin_mode), // CR
            0x0E => self.charset.gl = GlMapping::G1, // SO: G1 into GL
            0x0F => self.charset.gl = GlMapping::G0, // SI: G0 into GL

            0x84 => {
                self.margined_line_feed(self.modes.left_right_margin_mode);
            }
            0x85 => {
                // NEL, as ESC E: the index, then the carriage return.
                self.margined_line_feed(self.modes.left_right_margin_mode);
                self.grid
                    .carriage_return_margin(self.modes.left_right_margin_mode);
            }
            0x88 => self.grid.set_tab_stop(), // HTS - same as ESC H
            0x8D => {
                self.grid
                    .reverse_line_feed_margined(self.modes.left_right_margin_mode);
            }
            0x8E => self.charset.single_shift = SingleShift::Ss2, // SS2 - same as ESC N
            0x8F => self.charset.single_shift = SingleShift::Ss3, // SS3 - same as ESC O

            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &Provenance<[u16], Pty>,
        intermediates: &Provenance<[u8], Pty>,
        final_byte: u8,
    ) {
        self.note_sync_open_action();
        self.sync_absolute_row_metadata();
        let params = params.as_ref();
        let intermediates = intermediates.as_ref();
        if self.modes.vt52_mode {
            return;
        }
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        if intermediates.is_empty() {
            self.csi_dispatch_no_intermediates(&cap, params, final_byte);
            return;
        }
        let _ = self.csi_dispatch_with_intermediates(&cap, params, intermediates, final_byte);
    }

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
        if final_byte == b'm' && intermediates.as_ref().is_empty() {
            self.sgr_style()
                .handle_sgr_with_subparams(params.as_ref(), subparam_mask);
            return;
        }
        self.csi_dispatch(params, intermediates, final_byte);
    }

    fn esc_dispatch(&mut self, intermediates: &Provenance<[u8], Pty>, final_byte: u8) {
        self.note_sync_open_action();
        // As the CSI and OSC dispatchers do: a RIS moves the marks, which must first catch up with
        // the rows the batch's output already moved (a region scroll's pending splice).
        self.sync_absolute_row_metadata();
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        self.esc_dispatch_core(&cap, intermediates.as_ref(), final_byte);
    }

    fn osc_dispatch(&mut self, params: &Provenance<[&[u8]], Pty>) {
        self.osc_dispatch_with_terminator(params, false);
    }

    fn osc_dispatch_with_terminator(
        &mut self,
        params: &Provenance<[&[u8]], Pty>,
        bel_terminated: bool,
    ) {
        self.note_sync_open_action();
        self.sync_absolute_row_metadata();
        if self.modes.vt52_mode {
            return;
        }
        self.transient.last_osc_bel_terminated = bel_terminated;
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        self.osc_dispatch_inner(&cap, params.as_ref());
    }

    fn dcs_hook(
        &mut self,
        params: &Provenance<[u16], Pty>,
        intermediates: &Provenance<[u8], Pty>,
        final_byte: u8,
    ) {
        if self.modes.vt52_mode {
            return;
        }
        self.dcs_hook_inner(params.as_ref(), intermediates.as_ref(), final_byte);
    }

    fn dcs_put(&mut self, byte: u8) {
        self.dcs_put_inner(byte);
    }

    fn dcs_put_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        self.dcs_put_bulk_inner(data.as_ref());
    }

    fn dcs_unhook(&mut self, canceled: bool) {
        self.note_sync_open_action();
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        self.dcs_unhook_inner(&cap, canceled);
    }

    fn apc_start(&mut self) {
        if self.modes.vt52_mode {
            return;
        }
        self.dcs.total_bytes = self.dcs.total_bytes.saturating_sub(self.dcs.sequence_bytes);
        self.dcs.sequence_bytes = 0;
        if matches!(self.dcs.dcs_type, super::super::DcsType::Sixel) {
            self.sixel.decoder.abort();
        }
        self.dcs.dcs_type = super::super::DcsType::None;
        self.dcs.data.clear(); // Reuse dcs_data buffer for APC
    }

    fn apc_put(&mut self, byte: u8) {
        if self.dcs.total_bytes >= super::super::MAX_DCS_GLOBAL_BUDGET {
            return;
        }
        self.dcs.total_bytes += 1;
        self.dcs.sequence_bytes += 1;
        if self.dcs.data.len() < 4 * 1024 * 1024 {
            self.dcs.data.push(byte);
        }
    }

    fn apc_put_bulk(&mut self, data: &Provenance<[u8], Pty>) {
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

    fn apc_end(&mut self, canceled: bool) {
        self.note_sync_open_action();
        let kitty = self.dcs.data.first() == Some(&b'G');
        if kitty {
            self.transient.kitty_commands = self.transient.kitty_commands.wrapping_add(1);
        }
        if canceled && kitty {
            self.transient.kitty_pending = None;
        }
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
        self.dcs.data.clear();
        if self.dcs.data.capacity() > 4096 {
            self.dcs.data.shrink_to(128);
        }
    }
}

impl TerminalHandler<'_> {
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
            if crate::fault::triggered("kitty.chunk_alloc")
                || cur_len.saturating_add(cmd.payload.len()) > MAX_KITTY_IMAGE_BYTES
            {
                self.transient.kitty_pending = None; // abort the overflowing transfer
                return;
            }
            let Some(pending) = self.transient.kitty_pending.as_mut() else {
                self.transient.kitty_pending = Some(cmd);
                return;
            };
            if pending.payload.try_reserve(cmd.payload.len()).is_ok() {
                pending.payload.extend_from_slice(&cmd.payload);
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

    fn kitty_would_hold(&self, cmd: &KittyCommand) -> bool {
        cmd.declares_loadable_size()
            && (cmd.action != KittyAction::Frame
                || self
                    .resolve_kitty_id(cmd)
                    .is_some_and(|id| self.transient.kitty_frames.contains_key(&id)))
    }

    fn handle_complete_kitty_command(&mut self, cmd: KittyCommand) {
        match cmd.action {
            KittyAction::Delete => {
                self.delete_kitty_placements(&cmd);
                self.prune_kitty_floating();
            }
            KittyAction::Display => self.put_kitty(&cmd),
            KittyAction::Transmit | KittyAction::TransmitAndDisplay => self.transmit_kitty(cmd),
            KittyAction::Query if cmd.quiet == 0 => self.answer_kitty_query(&cmd),
            KittyAction::Query => {}
            KittyAction::Frame => self.load_kitty_frame(cmd),
            KittyAction::Compose => self.compose_kitty_frames(&cmd),
            KittyAction::Animate => self.control_kitty_animation(&cmd),
        }
    }

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
        self.prune_kitty_floating();
        shown
    }

    fn transmit_kitty(&mut self, mut cmd: KittyCommand) {
        if !self.kitty_medium_works(cmd.medium) {
            self.kitty_reply(&cmd, cmd.id, KITTY_MEDIUM_DISABLED);
            return;
        }
        let Some((image, crop_ok)) = self.build_kitty_image(&mut cmd) else {
            self.kitty_reply(&cmd, cmd.id, "EINVAL:bad image data");
            return;
        };
        let image = Arc::new(image);
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
                Ok(())
            } else {
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

    fn answer_kitty_query(&mut self, cmd: &KittyCommand) {
        use core::fmt::Write as _;
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
        let cap = super::super::response_capability::ResponseCapability::mint_for_dispatch();
        self.send_response(&cap, r.as_bytes());
    }

    #[inline]
    fn kitty_medium_works(&self, medium: KittyMedium) -> bool {
        medium == KittyMedium::Direct || self.kitty_file_resolver.installed()
    }

    #[inline]
    fn resolve_kitty_id(&self, cmd: &KittyCommand) -> Option<u32> {
        cmd.id.or_else(|| {
            cmd.number
                .and_then(|number| self.transient.kitty_numbers.get(&number).copied())
        })
    }

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

    fn kitty_reply(&mut self, cmd: &KittyCommand, id: Option<u32>, verdict: &str) {
        let frame = matches!(cmd.action, KittyAction::Frame | KittyAction::Animate)
            .then_some(cmd.rows)
            .flatten()
            .filter(|&frame| frame != 0)
            .and_then(|frame| usize::try_from(frame).ok());
        self.kitty_reply_naming(cmd, id, frame, verdict);
    }

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
        let image_serial = match self.transient.kitty_frames.get(&id) {
            Some(animation) => animation.image_serial,
            None => self.next_kitty_serial(),
        };
        if replacing {
            self.transient.kitty_virtual.remove(&id);
            self.delete_kitty_history_where(&|tag, _| tag.image_id == id);
            self.prune_kitty_floating();
            let held = kitty_bytes_held(self.transient, id);
            let t = &mut *self.transient;
            let released = kitty_payload_held(t, id);
            charge_released_kitty(t, released);
            t.kitty_variants.remove(&id);
            t.kitty_total_bytes = t.kitty_total_bytes.saturating_sub(held);
        }
        let t = &mut *self.transient;
        t.kitty_images.insert(id, Arc::clone(image));
        t.kitty_frames.insert(
            id,
            KittyAnimation::new(Arc::clone(image), image_serial, t.process_now),
        );
        t.kitty_total_bytes = t.kitty_total_bytes.saturating_add(add);
        self.damage_kitty_placeholders(id);
        true
    }

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

    /// Whether the Kitty store holds `image` for image `id`.
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
    #[inline]
    pub(in crate::terminal) fn kitty_store_images(&self) -> HashSet<*const ImageData> {
        self.transient.kitty_store_images()
    }

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
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(add)
            .map_err(|_| "ENOSPC:out of memory")?;
        bytes.extend_from_slice(&stored.bytes);
        bytes.shrink_to_fit();
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
                charge_released_kitty(t, gone.image.payload_bytes());
            }
        }
        t.kitty_variants.retain(|_, variants| !variants.is_empty());
        enough(t)
    }

    #[inline]
    fn next_kitty_serial(&mut self) -> NonZeroU32 {
        let serial = self.transient.kitty_serial.wrapping_add(1).max(1);
        self.transient.kitty_serial = serial;
        NonZeroU32::new(serial).unwrap_or(NonZeroU32::MIN)
    }

    #[inline]
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
        let history = self
            .clear_kitty_history(&|tag, _| tag.image_id == id && tag.placement_id == placement_id);
        let visible = self.clear_kitty_placements(&|tag, _| {
            tag.image_id == id && tag.placement_id == placement_id
        });
        !history.is_empty() || !visible.is_empty()
    }

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

    fn prune_kitty_floating(&mut self) {
        for _ in 0..=MAX_KITTY_IMAGES {
            let mut orphans: Vec<u32> = Vec::new();
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
                    self.free_kitty_image(id);
                    freed = true;
                }
            }
            if !freed {
                return;
            }
        }
    }

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

    fn kitty_tags_on_screen(&self) -> impl Iterator<Item = KittyPlacementTag> + '_ {
        self.kitty_layers().filter_map(|(_, _, layer)| layer.kitty)
    }

    fn newest_kitty_placement(
        &self,
        selected: &dyn Fn(&KittyPlacementTag) -> bool,
    ) -> Option<KittyPlacementTag> {
        self.kitty_tags_on_screen()
            .filter(|tag| selected(tag))
            .max_by_key(|tag| tag.serial)
    }

    fn kitty_origin(&self, serial: NonZeroU32) -> Option<(i32, i32)> {
        let (placed, row, col) = self.kitty_placed(serial)?;
        Some((
            i32::from(row) - i32::from(placed.cell_row),
            i32::from(col) - i32::from(placed.cell_col),
        ))
    }

    fn kitty_placed(&self, serial: NonZeroU32) -> Option<(ImageRef, u16, u16)> {
        self.kitty_layers()
            .find(|(_, _, layer)| layer.kitty.is_some_and(|tag| tag.serial == serial))
            .map(|(row, col, layer)| (layer.clone(), row, col))
    }

    fn stamp_kitty_at(
        &mut self,
        image: &Arc<ImageData>,
        top: i32,
        left: i32,
        tag: KittyPlacementTag,
    ) {
        self.admit_live_image(image, Some(tag));
        let cols = kitty_floating::visible_cell_range(left, image.cols, self.grid.cols());
        for cell_row in kitty_floating::visible_cell_range(top, image.rows, self.grid.rows()) {
            let Ok(row) = u16::try_from(top.saturating_add(i32::from(cell_row))) else {
                continue;
            };
            for cell_col in cols.clone() {
                let Ok(col) = u16::try_from(left.saturating_add(i32::from(cell_col))) else {
                    continue;
                };
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
        self.prune_kitty_floating();
    }

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

    fn clear_kitty_history(
        &mut self,
        selected: &dyn Fn(&KittyPlacementTag, &ImageData) -> bool,
    ) -> Vec<KittyPlacementTag> {
        let mut gone = Vec::new();
        self.grid.retain_history_images(&mut |span| {
            let Some(tag) = span.kitty else { return true };
            if !selected(&tag, &span.image) {
                return true;
            }
            if !gone
                .iter()
                .any(|old: &KittyPlacementTag| old.serial == tag.serial)
            {
                gone.push(tag);
            }
            false
        });
        gone
    }

    fn delete_kitty_where(
        &mut self,
        selected: &dyn Fn(&KittyPlacementTag, &ImageData) -> bool,
    ) -> Vec<u32> {
        self.delete_kitty_selected(selected, false)
    }

    fn delete_kitty_history_where(
        &mut self,
        selected: &dyn Fn(&KittyPlacementTag, &ImageData) -> bool,
    ) -> Vec<u32> {
        self.delete_kitty_selected(selected, true)
    }

    fn delete_kitty_selected(
        &mut self,
        selected: &dyn Fn(&KittyPlacementTag, &ImageData) -> bool,
        history: bool,
    ) -> Vec<u32> {
        let mut gone = self.clear_kitty_placements(selected);
        if history {
            gone.extend(self.clear_kitty_history(selected));
        }
        let mut touched: Vec<u32> = Vec::new();
        for tag in gone.iter().chain(&self.clear_kitty_floating(selected)) {
            push_unique(&mut touched, tag.image_id);
        }
        let mut orphans: Vec<u32> = Vec::new();
        for _ in 0..=MAX_KITTY_RELATIVE_DEPTH {
            if gone.is_empty() || self.transient.kitty_relations.is_empty() {
                break;
            }
            let relations = std::mem::take(&mut self.transient.kitty_relations);
            let (children, rest): (Vec<KittyRelation>, Vec<KittyRelation>) = relations
                .into_iter()
                .filter(|r| !gone.iter().any(|g| g.serial == r.child.serial))
                .partition(|r| gone.iter().any(|g| r.parent.names(g)));
            self.transient.kitty_relations = rest;
            let serials: HashSet<NonZeroU32> = children.iter().map(|r| r.child.serial).collect();
            self.clear_kitty_placements(&|tag, _| serials.contains(&tag.serial));
            self.clear_kitty_history(&|tag, _| serials.contains(&tag.serial));
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

    #[inline]
    fn kitty_image_on_screen(&self, id: u32) -> bool {
        self.kitty_tags_on_screen().any(|tag| tag.image_id == id)
    }

    #[inline]
    fn kitty_image_placed(&self, id: u32) -> bool {
        self.transient.kitty_virtual.contains_key(&id)
            || self.kitty_image_floating(id)
            || self.kitty_image_on_screen(id)
            || self
                .grid
                .history_any_image(&|span| span.kitty.is_some_and(|tag| tag.image_id == id))
    }

    #[inline]
    fn kitty_image_floating(&self, id: u32) -> bool {
        self.transient
            .kitty_floating
            .iter()
            .any(|f| f.tag.image_id == id)
    }

    fn delete_kitty_placements(&mut self, cmd: &KittyCommand) {
        let selector = cmd.delete_target.unwrap_or('a');
        let free_data = selector.is_ascii_uppercase();
        let cell = |v: Option<u32>| {
            v.and_then(|v| v.checked_sub(1))
                .and_then(|v| u16::try_from(v).ok())
        };
        let everything = 0..u16::MAX;
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
                let placement = cmd.placement;
                named.push(id);
                self.remove_kitty_virtual(id, placement);
                self.delete_kitty_history_where(&|tag, _| {
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
                self.delete_kitty_history_where(&|tag, _| (lo..=hi).contains(&tag.image_id))
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

    fn delete_every_kitty_placement(&mut self, free_data: bool) {
        self.delete_kitty_where(&|_, _| true);
        if free_data {
            let unplaced: Vec<u32> = self
                .transient
                .kitty_images
                .keys()
                .copied()
                .filter(|id| !self.kitty_image_placed(*id))
                .collect();
            for id in unplaced {
                self.free_kitty_image(id);
            }
        }
    }

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
                self.delete_kitty_history_where(&|tag, _| tag.image_id == id);
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

    /// Play every running Kitty animation shown on screen up to `now`.
    pub(in crate::terminal) fn advance_kitty_animations(
        &mut self,
        now: aterm_time::Instant,
    ) -> (bool, bool, Option<aterm_time::Instant>) {
        let playing: Vec<u32> = self
            .transient
            .kitty_frames
            .iter()
            .filter(|(_, animation)| animation.plays())
            .map(|(&id, _)| id)
            .collect();
        let mut advanced = false;
        let mut mutated = false;
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
                let prev_loops = animation.loops;
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
                if animation.loops != prev_loops {
                    mutated = true;
                }
                if rests {
                    continue;
                }
                let frame = Arc::clone(&animation.frames[index].image);
                due = now + gap(animation, index);
                self.set_kitty_current_frame(id, frame, now);
                advanced = true;
                mutated = true;
            }
            next = Some(next.map_or(due, |next| next.min(due)));
        }
        (advanced, mutated, next)
    }

    fn kitty_image_shown(&self, id: u32) -> bool {
        if self.kitty_image_on_screen(id)
            || (self.kitty_image_floating(id)
                && kitty_floating::floating_tiles(self.grid, self.transient, 0..self.grid.rows())
                    .any(|tile| self.transient.kitty_floating[tile.index].tag.image_id == id))
        {
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
        self.replace_shown_kitty_image(id, frame);
    }

    /// Make `frame` image `id`'s entry and repoint its placements to it, the
    /// animation's clock untouched.
    fn replace_shown_kitty_image(&mut self, id: u32, frame: Arc<ImageData>) {
        let t = &mut *self.transient;
        let new = frame.bytes.len();
        let old = t
            .kitty_images
            .insert(id, frame)
            .map_or(0, |img| img.bytes.len());
        t.kitty_total_bytes = t.kitty_total_bytes.saturating_sub(old).saturating_add(new);
        self.show_kitty_frame(id);
    }

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
            let transient = &*self.transient;
            let grid = &mut *self.grid;
            for tile in kitty_floating::floating_tiles(grid, transient, 0..grid.rows()) {
                if transient.kitty_floating[tile.index].tag.image_id == id {
                    grid.damage_mut().mark_cell(tile.row, tile.col);
                }
            }
        }
        self.damage_kitty_placeholders(id);
    }

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
        let edit = cmd
            .rows
            .and_then(|n| usize::try_from(n).ok())
            .filter(|&n| (1..=count).contains(&n))
            .map(|n| n - 1);
        let number = edit.map_or(count + 1, |index| index + 1);
        let gap = cmd.z_index.filter(|&gap| gap != 0);
        let verdict = match self.build_kitty_frame(id, &mut cmd, edit) {
            Ok(frame) => self.commit_kitty_frame(id, frame, edit, gap),
            Err(error) => error,
        };
        self.kitty_reply_naming(&cmd, Some(id), Some(number), verdict);
    }

    fn build_kitty_frame(
        &mut self,
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
        let root = Arc::clone(&frames.first().ok_or("ENOENT:no such image")?.image);
        let (canvas_w, canvas_h) = raster_size(&root).ok_or("EINVAL:image has no pixel size")?;
        let at = (cmd.x.unwrap_or(0), cmd.y.unwrap_or(0));
        let base = cmd.columns.filter(|&n| n != 0);
        let background = cmd.cell_y_offset.unwrap_or(0);
        let blend = cmd.cell_x_offset != Some(1);
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
            return Ok(KittyLayout::of(&root).image(data.bytes, data.format));
        }
        let (Ok(width), Ok(height)) = (u16::try_from(canvas_w), u16::try_from(canvas_h)) else {
            return Err("ENOSPC:frame too large");
        };
        let size = kitty_rgba_len(width, height)?;
        let under = match (edit, base) {
            (Some(index), _) => Some(index),
            (None, Some(n)) => Some(usize::try_from(n - 1).map_err(|_| "EINVAL:no such frame")?),
            (None, None) => None,
        };
        if let Some(index) = under {
            let under = &frames.get(index).ok_or("EINVAL:no such frame")?.image;
            if kitty_rgba_size(under)? != (width, height) {
                return Err("EINVAL:the frames differ in size");
            }
            self.decode_kitty_frame(id, index)?;
        }
        let pixels = kitty_rgba(&data)?;
        let mut canvas = Vec::new();
        canvas
            .try_reserve_exact(size)
            .map_err(|_| "ENOSPC:out of memory")?;
        match under {
            Some(index) => canvas.extend_from_slice(&kitty_rgba(self.kitty_frame(id, index)?)?),
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
        Ok(raw_frame(canvas, width, height, &root))
    }

    /// Frame `index` of image `id`.
    fn kitty_frame(&self, id: u32, index: usize) -> Result<&Arc<ImageData>, &'static str> {
        let animation = self
            .transient
            .kitty_frames
            .get(&id)
            .ok_or("ENOENT:no such image")?;
        Ok(&animation
            .frames
            .get(index)
            .ok_or("EINVAL:no such frame")?
            .image)
    }

    /// Store frame `index` of image `id` decoded if it is a PNG, before a
    /// compose or a frame load reads its pixels. A PNG costs a decode
    /// (`aterm_png` bounds its work by the image's size) on every read;
    /// stored decoded, the frame costs a copy, so a command repeated against
    /// one frame decodes it once. Only the store's representation changes:
    /// the pixels, and so the placements' pictures and the animation's clock,
    /// stay as they were. A PNG that does not decode keeps its bytes and is
    /// marked, so every later read refuses it without decoding it again.
    /// Refuses, decoding nothing, when the decoded frame would not fit the
    /// store.
    fn decode_kitty_frame(&mut self, id: u32, index: usize) -> Result<(), &'static str> {
        let t = &*self.transient;
        let slot = t
            .kitty_frames
            .get(&id)
            .ok_or("ENOENT:no such image")?
            .frames
            .get(index)
            .ok_or("EINVAL:no such frame")?;
        if !matches!(slot.image.format, ImageFormat::Png) {
            return Ok(());
        }
        if slot.undecodable {
            return Err(KITTY_BAD_PNG);
        }
        let frame = Arc::clone(&slot.image);
        let (width, height) = kitty_rgba_size(&frame)?;
        // The frame's slot and, when it is the frame shown, the image entry.
        let shown = t
            .kitty_images
            .get(&id)
            .is_some_and(|shown| Arc::ptr_eq(shown, &frame));
        let copies = if shown { 2 } else { 1 };
        let decoded = usize::from(width) * usize::from(height) * 4;
        let projected = t
            .kitty_total_bytes
            .saturating_sub(frame.bytes.len() * copies)
            .saturating_add(decoded * copies);
        if projected > MAX_KITTY_STORE_BYTES {
            return Err("ENOSPC:image store full");
        }
        let pixels = kitty_rgba(&frame).map(Cow::into_owned);
        let t = &mut *self.transient;
        let Some(slot) = t
            .kitty_frames
            .get_mut(&id)
            .and_then(|animation| animation.frames.get_mut(index))
        else {
            return Err("EINVAL:no such frame");
        };
        let pixels = match pixels {
            Ok(pixels) => pixels,
            Err(refused) => {
                slot.undecodable = true;
                return Err(refused);
            }
        };
        let decoded = Arc::new(raw_frame(pixels, width, height, &frame));
        slot.image = Arc::clone(&decoded);
        t.kitty_total_bytes = t
            .kitty_total_bytes
            .saturating_sub(frame.bytes.len())
            .saturating_add(decoded.bytes.len());
        charge_released_kitty(t, frame.payload_bytes());
        if shown {
            self.replace_shown_kitty_image(id, decoded);
        }
        Ok(())
    }

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
            if animation.frames.len() >= MAX_KITTY_FRAMES
                || total.saturating_add(add) > MAX_KITTY_STORE_BYTES
            {
                return "ENOSPC:no room for another frame";
            }
            animation.frames.push(KittyFrame {
                image: frame,
                gap_ms: gap.map_or(KITTY_DEFAULT_GAP_MS, gap_ms),
                undecodable: false,
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
        slot.undecodable = false;
        t.kitty_total_bytes = projected;
        charge_released_kitty(t, old.payload_bytes());
        if t.kitty_images
            .get(&id)
            .is_some_and(|shown| Arc::ptr_eq(shown, &old))
        {
            self.set_kitty_current_frame(id, frame, now);
        }
        "OK"
    }

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

    fn composed_kitty_frame(
        &mut self,
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
        let (width, height) = kitty_rgba_size(dst)?;
        if kitty_rgba_size(src)? != (width, height) {
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
        self.decode_kitty_frame(id, src_index)?;
        self.decode_kitty_frame(id, dst_index)?;
        let (src, dst) = (
            self.kitty_frame(id, src_index)?,
            self.kitty_frame(id, dst_index)?,
        );
        let (src_pixels, dst_pixels) = (kitty_rgba(src)?, kitty_rgba(dst)?);
        let mut canvas = Vec::new();
        canvas
            .try_reserve_exact(dst_pixels.len())
            .map_err(|_| "ENOSPC:out of memory")?;
        canvas.extend_from_slice(&dst_pixels);
        let to_usize = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
        kitty_graphics::compose_rgba(
            &mut canvas,
            usize::from(width),
            (to_usize(dst_x), to_usize(dst_y)),
            &src_pixels,
            usize::from(width),
            (to_usize(src_x), to_usize(src_y), to_usize(w), to_usize(h)),
            !cmd.cursor_stays,
        );
        Ok((dst_index, raw_frame(canvas, width, height, dst)))
    }

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
        let source_rect = (cropped && (vis_w, vis_h) != (px_w, px_h)).then_some(SourceRect {
            x,
            y,
            width: vis_w,
            height: vis_h,
        });
        let cell_w = u32::from(self.iterm2.cell_px.0.max(1));
        let cell_h = u32::from(self.iterm2.cell_px.1.max(1));
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
        let (cols, rows) = (
            cmd.columns.filter(|&c| c != 0),
            cmd.rows.filter(|&r| r != 0),
        );
        let aspect = |len: u32, from_px: u32, to_px: u32, to_cell: u32| {
            let span = (u64::from(len) * u64::from(to_px))
                .div_ceil(u64::from(from_px) * u64::from(to_cell));
            u32::try_from(span).unwrap_or(u32::MAX)
        };
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
            z_index: cmd.z_index.unwrap_or(0),
            scaling: match (cols, rows) {
                (None, None) => ImageScaling::PixelExact,
                (Some(_), Some(_)) => ImageScaling::Stretch,
                _ => ImageScaling::Fit,
            },
            source_rect,
            cell_offset_px,
        })
    }

    fn build_kitty_image(&self, cmd: &mut KittyCommand) -> Option<(ImageData, bool)> {
        use crate::terminal::kitty_graphics::{KittyFormat, png_dimensions, rgb_to_rgba};
        if cmd.payload.is_empty() || !cmd.declares_loadable_size() {
            return None;
        }
        let payload: Vec<u8> = if cmd.medium == KittyMedium::Direct {
            if cmd.compressed {
                aterm_codec::inflate::zlib_decompress(&cmd.payload, MAX_KITTY_IMAGE_BYTES).ok()?
            } else {
                std::mem::take(&mut cmd.payload)
            }
        } else {
            let cap = self.kitty_file_resolver.try_mint(cmd.medium)?;
            let name = std::str::from_utf8(&cmd.payload).ok()?;
            let bytes = self.kitty_file_resolver.resolve(cap, cmd.medium, name)?;
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
        let (format, px_w, px_h, mut bytes) = match cmd.format {
            KittyFormat::Png => {
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
        bytes.shrink_to_fit();
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
        if cmd.medium != KittyMedium::Direct {
            self.kitty_file_resolver.note_loaded();
        }
        Some((layout.image(bytes, format), crop_ok))
    }
}

const KITTY_MEDIUM_DISABLED: &str = "ENOTSUPPORTED:medium disabled (allow_kitty_file_transfer)";

const KITTY_BAD_PNG: &str = "EBADPNG:the PNG does not decode";

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
    #[inline]
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

    #[inline]
    const fn image(self, bytes: Vec<u8>, format: ImageFormat) -> ImageData {
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

/// Bound decoded frame bytes before allocation, including on 32-bit targets.
fn kitty_rgba_len(width: u16, height: u16) -> Result<usize, &'static str> {
    usize::from(width)
        .checked_mul(usize::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|&bytes| bytes <= MAX_KITTY_IMAGE_BYTES)
        .ok_or("ENOSPC:frame too large")
}

/// The pixel size [`kitty_rgba`] would decode `frame` to, read from its header
/// alone, so a command can refuse its arguments before it decodes anything.
fn kitty_rgba_size(frame: &ImageData) -> Result<(u16, u16), &'static str> {
    let (width, height) = raster_size(frame).ok_or(KITTY_BAD_PNG)?;
    let (Ok(width), Ok(height)) = (u16::try_from(width), u16::try_from(height)) else {
        return Err("ENOSPC:frame too large");
    };
    kitty_rgba_len(width, height)?;
    Ok((width, height))
}

#[cfg(test)]
thread_local! {
    /// PNG frames [`kitty_rgba`] has decoded on this thread.
    static KITTY_PNG_DECODES: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// `frame`'s RGBA8 pixels at [`kitty_rgba_size`], decoding a PNG.
fn kitty_rgba(frame: &ImageData) -> Result<Cow<'_, [u8]>, &'static str> {
    let (width, height) = kitty_rgba_size(frame)?;
    match frame.format {
        ImageFormat::RawRgba8 { .. } => Ok(Cow::Borrowed(frame.bytes.as_slice())),
        ImageFormat::Png => {
            #[cfg(test)]
            KITTY_PNG_DECODES.with(|n| n.set(n.get() + 1));
            let (pixels, w, h) =
                aterm_png::decode_rgba8(&frame.bytes, u32::from(u16::MAX)).ok_or(KITTY_BAD_PNG)?;
            if (w, h) != (usize::from(width), usize::from(height)) {
                return Err(KITTY_BAD_PNG);
            }
            Ok(Cow::Owned(pixels))
        }
        ImageFormat::Unknown => Err(KITTY_BAD_PNG),
    }
}

#[inline]
fn raw_frame(mut bytes: Vec<u8>, width: u16, height: u16, like: &ImageData) -> ImageData {
    bytes.shrink_to_fit();
    KittyLayout::of(like).image(bytes, ImageFormat::RawRgba8 { width, height })
}

#[inline]
fn push_unique(ids: &mut Vec<u32>, id: u32) {
    if !ids.contains(&id) {
        ids.push(id);
    }
}

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

#[inline]
fn charge_released_kitty(t: &mut TransientState, bytes: usize) {
    for charge in &mut t.live_image_charge {
        *charge = charge.saturating_add(bytes);
    }
    t.history_image_room = t.history_image_room.saturating_sub(bytes);
}

#[inline]
fn raster_size(image: &ImageData) -> Option<(u32, u32)> {
    match image.format {
        ImageFormat::RawRgba8 { width, height } => Some((u32::from(width), u32::from(height))),
        ImageFormat::Png => crate::terminal::kitty_graphics::png_dimensions(&image.bytes),
        ImageFormat::Unknown => None,
    }
}

pub(crate) const MAX_KITTY_IMAGES: usize = 256;
pub(crate) const MAX_KITTY_FRAMES: usize = 128;
const KITTY_DEFAULT_GAP_MS: u32 = 40;
pub(crate) const MAX_KITTY_VARIANTS: usize = 16;
pub(crate) const MAX_KITTY_VIRTUALS: usize = 8;
pub(crate) const MAX_KITTY_IMAGE_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_KITTY_STORE_BYTES: usize = 64 * 1024 * 1024;

impl TerminalHandler<'_> {
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

    #[inline]
    #[allow(
        clippy::too_many_lines,
        reason = "SGR dispatch table with many attribute codes"
    )]
    fn csi_dispatch_sgr_fast(&mut self, params: &[u16]) {
        if params.is_empty() || (params.len() == 1 && params[0] == 0) {
            self.style.reset_sgr();
            self.transient.current_underline_color = None;
            self.transient.update_has_transient_extras();
            self.grid
                .set_cursor_template(crate::grid::Cell::EMPTY, None);
            return;
        }
        if params.len() == 1 {
            let p = params[0];
            match p {
                30..=37 | 90..=97 => {
                    let index =
                        crate::terminal::sgr_color_u8(if p >= 90 { p - 90 + 8 } else { p - 30 });
                    self.style.fg = PackedColor::indexed(index);
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
                self.sgr_style().apply_style_change();
                return;
            }
        }
        if params.len() == 3 && params[1] == 5 {
            let index = crate::terminal::sgr_color_u8(params[2]);
            if params[0] == 38 {
                self.style.fg = PackedColor::indexed(index);
                self.style.update_cached_colors();
                return;
            }
            if params[0] == 48 {
                self.style.bg = PackedColor::indexed(index);
                self.sgr_style().apply_style_change();
                return;
            }
        }
        self.sgr_style().handle_sgr(params);
    }
}

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

impl TerminalHandler<'_> {
    // Inlined into each caller, as is `write_ascii_segment`: a run of text lines writes through
    // both too (handler_line_run.rs), and with those second callers left to the inliner every
    // short print cost more (forcing both inline cut cursor_motion's instructions 4.5%).
    #[expect(
        clippy::inline_always,
        reason = "measured: left to the inliner beside the line-run caller, every short print cost more"
    )]
    #[inline(always)]
    pub(super) fn write_ascii_bulk_fast(&mut self, data: &[u8]) {
        let flags = if self.style.protected {
            self.style.flags.union(CellFlags::PROTECTED)
        } else {
            self.style.flags
        };
        let is_default = self.style.is_default();
        let colors = self.style.cached_colors();

        if data.len() <= 64 || !has_run_of_4(data) {
            self.write_ascii_segment(data, is_default, colors, flags);
            return;
        }

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

    // Inlined into each caller: see `write_ascii_bulk_fast`.
    #[expect(
        clippy::inline_always,
        reason = "measured: left to the inliner beside the line-run caller, every short print cost more"
    )]
    #[inline(always)]
    pub(super) fn write_ascii_segment(
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
            &mut last_byte,
        );

        if let Some(b) = last_byte {
            self.transient.last_graphic_char = Some(b as char);
        }
    }
}

#[cfg(test)]
#[path = "handler_actions_kitty_display_tests.rs"]
mod kitty_display_tests;
