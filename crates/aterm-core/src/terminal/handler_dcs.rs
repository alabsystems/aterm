// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! DCS routing and state behind `ActionSink::{dcs_hook, dcs_put, dcs_unhook}`.

use super::super::{DcsType, MAX_DCS_GLOBAL_BUDGET};
use super::TerminalHandler;
use super::handler_decrsps::DATA_CAP as RESTORE_DATA_CAP;

impl TerminalHandler<'_> {
    /// Begin a DCS sequence (`ESC P <params> <intermediates> <final> <data> ST`).
    pub(super) fn dcs_hook_inner(&mut self, params: &[u16], intermediates: &[u8], final_byte: u8) {
        self.dcs.total_bytes = self.dcs.total_bytes.saturating_sub(self.dcs.sequence_bytes);
        self.dcs.data.clear();
        self.dcs.sequence_bytes = 0;

        if matches!(self.dcs.dcs_type, DcsType::Sixel) {
            self.sixel.decoder.abort();
        }

        if intermediates == *b"$" && final_byte == b'q' {
            self.dcs.dcs_type = DcsType::Decrqss;
        } else if params == [1] && intermediates == *b"$" && final_byte == b't' {
            self.dcs.dcs_type = DcsType::Decrsps { discarded: false };
        } else if intermediates.is_empty() && final_byte == b'q' {
            self.dcs.dcs_type = DcsType::Sixel;
            self.sixel.decoder.hook();
        } else if intermediates == *b"+" && final_byte == b'q' {
            self.dcs.dcs_type = DcsType::Xtgettcap;
        } else {
            self.dcs.dcs_type = DcsType::Unknown;
        }
    }

    /// Accumulate one DCS data byte for the type `dcs_hook` identified.
    pub(super) fn dcs_put_inner(&mut self, byte: u8) {
        if self.dcs.total_bytes >= MAX_DCS_GLOBAL_BUDGET {
            self.discard_cursor_restore();
            return; // Global budget exceeded: drop the byte.
        }

        match self.dcs.dcs_type {
            DcsType::Decrqss => {
                self.dcs.total_bytes += 1;
                self.dcs.sequence_bytes += 1;
                if self.dcs.data.len() < 256 {
                    self.dcs.data.push(byte);
                }
            }
            DcsType::Sixel => {
                self.dcs.total_bytes += 1;
                self.dcs.sequence_bytes += 1;
                let alloc_before = self.sixel.decoder.pixel_alloc_bytes();
                self.sixel.decoder.put(byte);
                let alloc_after = self.sixel.decoder.pixel_alloc_bytes();
                if alloc_after > alloc_before {
                    let delta = alloc_after - alloc_before;
                    self.dcs.total_bytes += delta;
                    self.dcs.sequence_bytes += delta;
                    if self.dcs.total_bytes > MAX_DCS_GLOBAL_BUDGET {
                        self.sixel.decoder.abort();
                        self.dcs.total_bytes -= alloc_after;
                        self.dcs.sequence_bytes -= alloc_after;
                    }
                }
            }
            DcsType::Xtgettcap => {
                self.dcs.total_bytes += 1;
                self.dcs.sequence_bytes += 1;
                if self.dcs.data.len() < 1024 {
                    self.dcs.data.push(byte);
                }
            }
            DcsType::Decrsps { .. } => {
                self.dcs.total_bytes += 1;
                self.dcs.sequence_bytes += 1;
                if self.dcs.data.len() < RESTORE_DATA_CAP {
                    self.dcs.data.push(byte);
                } else {
                    self.discard_cursor_restore();
                }
            }
            DcsType::Unknown | DcsType::None => {
                self.dcs.total_bytes += 1;
                self.dcs.sequence_bytes += 1;
            }
        }
    }

    /// Bulk [`dcs_put_inner`](Self::dcs_put_inner) over a contiguous run of DCS data.
    pub(super) fn dcs_put_bulk_inner(&mut self, data: &[u8]) {
        let countable = data
            .len()
            .min(MAX_DCS_GLOBAL_BUDGET.saturating_sub(self.dcs.total_bytes));
        if countable < data.len() {
            self.discard_cursor_restore();
        }
        if countable == 0 {
            return;
        }
        let data = &data[..countable];
        self.dcs.total_bytes += countable;
        self.dcs.sequence_bytes += countable;

        match self.dcs.dcs_type {
            DcsType::Decrqss => self.push_dcs_data_capped(data, 256),
            DcsType::Sixel => {
                let alloc_before = self.sixel.decoder.pixel_alloc_bytes();
                for &byte in data {
                    self.sixel.decoder.put(byte);
                }
                let alloc_after = self.sixel.decoder.pixel_alloc_bytes();
                if alloc_after > alloc_before {
                    let delta = alloc_after - alloc_before;
                    self.dcs.total_bytes += delta;
                    self.dcs.sequence_bytes += delta;
                    if self.dcs.total_bytes > MAX_DCS_GLOBAL_BUDGET {
                        self.sixel.decoder.abort();
                        self.dcs.total_bytes -= alloc_after;
                        self.dcs.sequence_bytes -= alloc_after;
                    }
                }
            }
            DcsType::Xtgettcap => self.push_dcs_data_capped(data, 1024),
            DcsType::Decrsps { .. } => {
                if data.len() > RESTORE_DATA_CAP.saturating_sub(self.dcs.data.len()) {
                    self.discard_cursor_restore();
                }
                self.push_dcs_data_capped(data, RESTORE_DATA_CAP);
            }
            DcsType::Unknown | DcsType::None => {}
        }
    }

    /// A mutating restore never accepts a retained prefix after any byte was dropped.
    fn discard_cursor_restore(&mut self) {
        if matches!(self.dcs.dcs_type, DcsType::Decrsps { .. }) {
            self.dcs.dcs_type = DcsType::Decrsps { discarded: true };
        }
    }

    /// Append the leading prefix of `data` that fits under `cap` into `dcs.data`.
    #[inline]
    fn push_dcs_data_capped(&mut self, data: &[u8], cap: usize) {
        let n = data.len().min(cap.saturating_sub(self.dcs.data.len()));
        if n > 0 {
            self.dcs.data.extend_from_slice(&data[..n]);
        }
    }

    /// Finish a DCS string at ST: answer a query, restore cursor presentation, or place Sixel.
    pub(super) fn dcs_unhook_inner(
        &mut self,
        cap: &super::super::response_capability::ResponseCapability,
        canceled: bool,
    ) {
        match self.dcs.dcs_type {
            DcsType::Decrqss if !canceled => {
                self.handle_decrqss(cap);
            }
            DcsType::Sixel => {
                if canceled {
                    self.sixel.decoder.abort();
                } else if let Some(image) = self.sixel.decoder.unhook() {
                    // The image is the raster `dcs_put` charged, cut to its painted extent in
                    // place: unhook allocates nothing that charge does not already cover.
                    self.place_sixel_image(image);
                    self.sixel.next_id += 1;
                }
            }
            DcsType::Xtgettcap if !canceled => {
                self.handle_xtgettcap(cap);
            }
            DcsType::Decrsps { discarded: false } if !canceled => self.handle_decrsps(),
            DcsType::Decrqss
            | DcsType::Xtgettcap
            | DcsType::Decrsps { .. }
            | DcsType::Unknown
            | DcsType::None => {}
        }

        self.dcs.dcs_type = DcsType::None;
        self.dcs.total_bytes = self.dcs.total_bytes.saturating_sub(self.dcs.sequence_bytes);
        self.dcs.sequence_bytes = 0;
        self.dcs.data.clear();
        if self.dcs.data.capacity() > 4096 {
            self.dcs.data.shrink_to(128);
        }
    }
}

#[cfg(test)]
#[path = "handler_dcs_tests.rs"]
mod tests;
