// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! DCS routing and state behind `ActionSink::{dcs_hook, dcs_put, dcs_unhook}`.

use super::super::{DcsType, MAX_DCS_GLOBAL_BUDGET};
use super::TerminalHandler;

impl TerminalHandler<'_> {
    /// Begin a DCS sequence (`ESC P <params> <intermediates> <final> <data> ST`):
    /// identify its type and prepare state for `dcs_put`; `dcs_unhook` ends it.
    ///
    /// Handled: DECRQSS (`DCS $ q Pt ST`), Sixel (`DCS Ps q <data> ST`) and
    /// XTGETTCAP (`DCS + q Pt ST`). Everything else, including DECDLD (`{`),
    /// tmux control mode (`DCS 1000 p`), tmux passthrough (`DCS tmux;`) and
    /// SSH conductor (`DCS 2000 p`), is consumed and ignored as `Unknown`.
    pub(super) fn dcs_hook_inner(&mut self, _params: &[u16], intermediates: &[u8], final_byte: u8) {
        // Release an abandoned sequence's budget (dcs_hook with no dcs_unhook after a parser
        // reset), or its bytes would leak.
        self.dcs.total_bytes = self.dcs.total_bytes.saturating_sub(self.dcs.sequence_bytes);
        self.dcs.data.clear();
        self.dcs.sequence_bytes = 0;

        // A Sixel cut off by a parser reset (no dcs_unhook) leaves its decoder active: stop
        // it, so no stale image comes out later.
        #[cfg(feature = "sixel")]
        if matches!(self.dcs.dcs_type, DcsType::Sixel) {
            // Frees the pixel buffer without composing the image `unhook` would return.
            self.sixel.decoder.abort();
        }

        if intermediates == *b"$" && final_byte == b'q' {
            self.dcs.dcs_type = DcsType::Decrqss;
        } else if intermediates.is_empty() && final_byte == b'q' {
            // Sixel: DCS Ps1 ; Ps2 ; Ps3 q <sixel-data> ST
            #[cfg(feature = "sixel")]
            {
                self.dcs.dcs_type = DcsType::Sixel;
                self.sixel.decoder.hook();
            }
            #[cfg(not(feature = "sixel"))]
            {
                self.dcs.dcs_type = DcsType::Unknown;
            }
        } else if intermediates == *b"+" && final_byte == b'q' {
            self.dcs.dcs_type = DcsType::Xtgettcap;
        } else {
            self.dcs.dcs_type = DcsType::Unknown;
        }
    }

    /// Accumulate one DCS data byte for the type `dcs_hook` identified: DECRQSS and XTGETTCAP
    /// keep their parameter string (capped), Sixel feeds the decoder. Every byte counts
    /// against `MAX_DCS_GLOBAL_BUDGET`.
    pub(super) fn dcs_put_inner(&mut self, byte: u8) {
        if self.dcs.total_bytes >= MAX_DCS_GLOBAL_BUDGET {
            return; // Global budget exceeded: drop the byte.
        }

        match self.dcs.dcs_type {
            DcsType::Decrqss => {
                // Count every byte, even past the data cap, so a flood stays visible to
                // the budget.
                self.dcs.total_bytes += 1;
                self.dcs.sequence_bytes += 1;
                // Accumulate the parameter string (Pt) up to 256 bytes.
                if self.dcs.data.len() < 256 {
                    self.dcs.data.push(byte);
                }
            }
            #[cfg(feature = "sixel")]
            DcsType::Sixel => {
                self.dcs.total_bytes += 1;
                self.dcs.sequence_bytes += 1;
                // Pixel allocation is charged too.
                let alloc_before = self.sixel.decoder.pixel_alloc_bytes();
                self.sixel.decoder.put(byte);
                let alloc_after = self.sixel.decoder.pixel_alloc_bytes();
                if alloc_after > alloc_before {
                    let delta = alloc_after - alloc_before;
                    self.dcs.total_bytes += delta;
                    self.dcs.sequence_bytes += delta;
                    if self.dcs.total_bytes > MAX_DCS_GLOBAL_BUDGET {
                        self.sixel.decoder.abort();
                        // Release pixel bytes from budget (stream bytes stay charged).
                        self.dcs.total_bytes -= alloc_after;
                        self.dcs.sequence_bytes -= alloc_after;
                    }
                }
            }
            DcsType::Xtgettcap => {
                // Counted even past the data cap, as for DECRQSS.
                self.dcs.total_bytes += 1;
                self.dcs.sequence_bytes += 1;
                // Accumulate hex-encoded capability names (Pt) up to 1024 bytes.
                if self.dcs.data.len() < 1024 {
                    self.dcs.data.push(byte);
                }
            }
            DcsType::Unknown | DcsType::None => {
                // Counted though nothing keeps them, so unknown DCS cannot bypass the budget.
                self.dcs.total_bytes += 1;
                self.dcs.sequence_bytes += 1;
            }
        }
    }

    /// Bulk [`dcs_put_inner`](Self::dcs_put_inner) over a run of DCS data (the parser's
    /// passthrough fast path delivers near-whole PTY chunks): budget accounting, data caps and
    /// Sixel allocation sampling run once per run. Byte-identical to the per-byte path (the
    /// `dcs_put_bulk_*_parity` tests): at most `MAX_DCS_GLOBAL_BUDGET - total_bytes` bytes are
    /// counted (and fed to a Sixel decoder), and of those the prefix that fits the per-type
    /// cap is pushed to `dcs.data`.
    ///
    /// One deviation: when Sixel pixel allocation crosses the budget, the decoder is stopped at
    /// the end of the run rather than at the offending byte, an overshoot bounded by one parser
    /// run and the decoder's own caps. The budget release matches (stream bytes stay charged).
    pub(super) fn dcs_put_bulk_inner(&mut self, data: &[u8]) {
        let countable = data
            .len()
            .min(MAX_DCS_GLOBAL_BUDGET.saturating_sub(self.dcs.total_bytes));
        if countable == 0 {
            return;
        }
        let data = &data[..countable];
        self.dcs.total_bytes += countable;
        self.dcs.sequence_bytes += countable;

        match self.dcs.dcs_type {
            // Accumulate the parameter string (Pt) up to 256 bytes.
            DcsType::Decrqss => self.push_dcs_data_capped(data, 256),
            #[cfg(feature = "sixel")]
            DcsType::Sixel => {
                // Pixel allocation is charged to the budget, sampled once per run.
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
                        // Release pixel bytes from budget (stream bytes stay charged).
                        self.dcs.total_bytes -= alloc_after;
                        self.dcs.sequence_bytes -= alloc_after;
                    }
                }
            }
            // Accumulate hex-encoded capability names (Pt) up to 1024 bytes.
            DcsType::Xtgettcap => self.push_dcs_data_capped(data, 1024),
            // Bytes were already counted above; nothing keeps them.
            DcsType::Unknown | DcsType::None => {}
        }
    }

    /// Append the leading prefix of `data` that fits under `cap` into
    /// `dcs.data` — the bulk equivalent of the per-byte `len() < cap` push.
    fn push_dcs_data_capped(&mut self, data: &[u8], cap: usize) {
        let n = data.len().min(cap.saturating_sub(self.dcs.data.len()));
        if n > 0 {
            self.dcs.data.extend_from_slice(&data[..n]);
        }
    }

    /// Finish a DCS string at ST: answer DECRQSS or XTGETTCAP, or place the Sixel image. A
    /// string cut short by CAN/SUB (`canceled`) is discarded: no reply, no image. Then reset
    /// DCS state and release the sequence's budget.
    pub(super) fn dcs_unhook_inner(
        &mut self,
        cap: &super::super::response_capability::ResponseCapability,
        canceled: bool,
    ) {
        match self.dcs.dcs_type {
            // A canceled query is not answered: the handback's own CAN ends
            // one its holder left torn, and the reply would reach the next
            // holder (the shell) as typed input.
            DcsType::Decrqss if !canceled => {
                self.handle_decrqss(cap);
            }
            #[cfg(feature = "sixel")]
            DcsType::Sixel => {
                if canceled {
                    // Drop the half-decoded image without a copy: a canceled control
                    // string produces no output (VT500).
                    self.sixel.decoder.abort();
                } else if let Some(image) = self.sixel.decoder.unhook() {
                    // Placed through the inline-image path OSC 1337 `File=` uses; each
                    // placed sixel gets a distinct id.
                    self.place_sixel_image(&image);
                    self.sixel.next_id += 1;
                }
            }
            DcsType::Xtgettcap if !canceled => {
                self.handle_xtgettcap(cap);
            }
            // A canceled query, or nothing to do.
            DcsType::Decrqss | DcsType::Xtgettcap | DcsType::Unknown | DcsType::None => {}
        }

        // Release by sequence_bytes, not data.len(): Sixel bytes go to the decoder, not data.
        self.dcs.dcs_type = DcsType::None;
        self.dcs.total_bytes = self.dcs.total_bytes.saturating_sub(self.dcs.sequence_bytes);
        self.dcs.sequence_bytes = 0;
        self.dcs.data.clear();
        // Shrink retained capacity after large sequences (as osc_data does).
        if self.dcs.data.capacity() > 4096 {
            self.dcs.data.shrink_to(128);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::terminal::Terminal;

    /// The handback's `CAN` ends a query its holder left torn. Answering it
    /// would type the reply into the next holder's input (the shell's).
    #[test]
    fn a_handback_does_not_answer_a_torn_query() {
        let mut t = Terminal::new(24, 80);
        t.process(b"\x1b[1m\x1bP$qm");
        let h = t.foreground_handback_scoped(false).expect("torn DECRQSS");
        assert_eq!(h.bytes, vec![0x18]);
        assert_eq!(t.take_response(), None);

        let mut t = Terminal::new(24, 80);
        t.process(b"\x1bP+q544E");
        assert_eq!(t.manual_handback().bytes, vec![0x18]);
        assert_eq!(t.take_response(), None);
    }
}
