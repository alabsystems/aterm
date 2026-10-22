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
    /// tmux control mode (`DCS 1000 p`) and SSH conductor (`DCS 2000 p`), is
    /// consumed and ignored as `Unknown`. A tmux passthrough wrapper
    /// (`DCS tmux; … ST`) never reaches here: the parser reads its payload
    /// unwrapped, as tmux does (`aterm_parser::TmuxPassthrough`).
    pub(super) fn dcs_hook_inner(&mut self, _params: &[u16], intermediates: &[u8], final_byte: u8) {
        // Release an abandoned sequence's budget (dcs_hook with no dcs_unhook after a parser
        // reset), or its bytes would leak.
        self.dcs.total_bytes = self.dcs.total_bytes.saturating_sub(self.dcs.sequence_bytes);
        self.dcs.data.clear();
        self.dcs.sequence_bytes = 0;

        // A Sixel cut off by a parser reset (no dcs_unhook) leaves its decoder active: stop
        // it, so no stale image comes out later.
        if matches!(self.dcs.dcs_type, DcsType::Sixel) {
            // Frees the pixel buffer without composing the image `unhook` would return.
            self.sixel.decoder.abort();
        }

        if intermediates == *b"$" && final_byte == b'q' {
            self.dcs.dcs_type = DcsType::Decrqss;
        } else if intermediates.is_empty() && final_byte == b'q' {
            // Sixel: DCS Ps1 ; Ps2 ; Ps3 q <sixel-data> ST
            self.dcs.dcs_type = DcsType::Sixel;
            self.sixel.decoder.hook();
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
    #[inline]
    fn push_dcs_data_capped(&mut self, data: &[u8], cap: usize) {
        let n = data.len().min(cap.saturating_sub(self.dcs.data.len()));
        if n > 0 {
            self.dcs.data.extend_from_slice(&data[..n]);
        }
    }

    /// Finish a DCS string at ST: answer DECRQSS or XTGETTCAP, or place the Sixel image. A
    /// string cut short by CAN/SUB, or broken off by an ESC that does not begin ST
    /// (`canceled`), is discarded: no reply, no image, as xterm answers or applies a DCS
    /// only at ST. Then reset DCS state and release the sequence's budget.
    pub(super) fn dcs_unhook_inner(
        &mut self,
        cap: &super::super::response_capability::ResponseCapability,
        canceled: bool,
    ) {
        match self.dcs.dcs_type {
            // A canceled query is not answered: the handback's own CAN ends
            // one its holder left torn, and the reply would reach the next
            // holder (the shell) as typed input; and one an ESC broke off was
            // never asked whole.
            DcsType::Decrqss if !canceled => {
                self.handle_decrqss(cap);
            }
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

    /// `bytes` fed in two reads cut at `cut`.
    fn fed(bytes: &[u8], cut: usize) -> Terminal {
        let mut t = Terminal::new(24, 80);
        let (head, tail) = bytes.split_at(cut);
        t.process(head);
        t.process(tail);
        t
    }

    /// A DCS is answered or applied only at ST, as xterm answers it. An ESC that
    /// does not begin ST breaks the string off and cancels it, as CAN and SUB do,
    /// so a DECRQSS or XTGETTCAP query is not answered (`ESC P $ q m ESC [ 0 m`
    /// replied `ESC P 1 $ r 0 m ESC \`), and what the ESC begins runs as
    /// anywhere. The ESC and the byte after it in two reads decide the same way.
    #[test]
    fn a_query_broken_off_by_an_escape_is_not_answered() {
        for query in [&b"\x1bP$qm"[..], b"\x1bP+q544E"] {
            for brk in [
                &b"\x1b[5;7H"[..],
                b"\x1b]2;t\x07\x1b[5;7H",
                b"\x1b\x1b[5;7H",
            ] {
                let bytes = [query, brk].concat();
                for cut in 0..=bytes.len() {
                    let mut t = fed(&bytes, cut);
                    let shown = String::from_utf8_lossy(&bytes);
                    assert_eq!(t.take_response(), None, "{shown:?} cut {cut}: answered");
                    let cursor = t.cursor();
                    assert_eq!((cursor.row, cursor.col), (4, 6), "{shown:?} cut {cut}");
                }
            }
        }
    }

    /// `payload` in tmux's passthrough wrapper, every ESC doubled, as a program
    /// that finds `TMUX` in its environment writes it.
    fn tmux_wrapped(payload: &[u8]) -> Vec<u8> {
        let mut out = b"\x1bPtmux;".to_vec();
        for &byte in payload {
            if byte == 0x1B {
                out.push(0x1B);
            }
            out.push(byte);
        }
        out.extend_from_slice(b"\x1b\\");
        out
    }

    /// A DECRQSS or XTGETTCAP wrapped in a tmux passthrough that reached aterm
    /// itself (a tab that inherited `TMUX` with no tmux in it) is answered as
    /// the bare query is, however the bytes are cut: tmux 3.5a hands its own
    /// terminal the bare query for that wrapper (measured 2026-10-19), and the
    /// parser unwraps it the same way. RED before: the wrapper doubled the
    /// query's ST into `ESC ESC \`, which broke it off unanswered.
    #[test]
    fn a_query_wrapped_for_tmux_is_answered_as_the_bare_one() {
        for query in [&b"\x1bP$qm\x1b\\"[..], b"\x1bP+q544E\x1b\\"] {
            let bare = fed(query, 0).take_response();
            assert!(bare.is_some(), "{query:?}: the bare query is answered");
            let bytes = tmux_wrapped(query);
            for cut in 0..=bytes.len() {
                let shown = String::from_utf8_lossy(&bytes);
                assert_eq!(
                    fed(&bytes, cut).take_response(),
                    bare,
                    "{shown:?} cut {cut}"
                );
            }
        }
    }

    /// Negative control: ST ends a query complete however it is cut, the ESC in
    /// one read and the `\` in the next included, and so does the 8-bit ST.
    #[test]
    fn a_query_ended_at_st_is_answered_however_it_is_cut() {
        for (query, reply) in [
            (&b"\x1bP$qm"[..], &b"\x1bP1$r0m\x1b\\"[..]),
            (b"\x1bP+q544E", b"\x1bP1+r544E="),
        ] {
            for st in [&b"\x1b\\"[..], b"\x9c"] {
                let bytes = [query, st].concat();
                for cut in 0..=bytes.len() {
                    let response = fed(&bytes, cut).take_response();
                    assert!(
                        response.as_deref().is_some_and(|r| r.starts_with(reply)),
                        "{:?} cut {cut}: {response:?}",
                        String::from_utf8_lossy(&bytes)
                    );
                }
            }
        }
    }

    /// The strings the engine consumes and ignores (tmux control mode, the SSH
    /// conductor, DECDLD, a kitty remote-control command, the DCS
    /// synchronized-update pair) end with nothing at ST or broken off alike, and
    /// what breaks one off runs. A tmux passthrough wrapper, unwrapped, runs
    /// its payload (an OSC here) and ends at its own `ESC \`.
    #[test]
    fn a_consumed_string_broken_off_runs_what_breaks_it_off() {
        let heads: [&[u8]; 6] = [
            b"\x1bPtmux;",
            b"\x1bP1000p%begin 1 1 0\n",
            b"\x1bP2000pabc",
            b"\x1bP1;1{ @???",
            b"\x1bP@kitty-cmd{",
            b"\x1bP=1s",
        ];
        for head in heads {
            let bytes = [head, &b"\x1b\x1b]2;inner\x07\x1b\\after"[..]].concat();
            for cut in 0..=bytes.len() {
                let mut t = fed(&bytes, cut);
                let shown = String::from_utf8_lossy(&bytes);
                assert_eq!(t.title(), "inner", "{shown:?} cut {cut}");
                assert_eq!(
                    t.row_text(0).as_deref().map(str::trim_end),
                    Some("after"),
                    "{shown:?} cut {cut}"
                );
                assert_eq!(t.take_response(), None, "{shown:?} cut {cut}");
            }
        }
    }

    /// A wrapper head with no wrapper after it (a program that wrote `ESC P tmux;`
    /// and doubled nothing, a stray head in a file `cat` prints) leaves the shell's
    /// and the next program's sequences working: an ST-ended title is set and the
    /// text after it prints, a RIS ends the wrapper whole, and a query after BEL-ended
    /// prompts is answered. RED before: the head's wrapper stayed open past RIS, and
    /// its `ESC \` ended the wrapper instead of the title, which swallowed the text.
    #[test]
    fn a_stray_wrapper_head_breaks_no_sequence_after_it() {
        let head = b"\x1bPtmux;";
        let title = b"\x1b]2;t\x1b\\ok";
        for cut in 0..=head.len() + title.len() {
            let t = fed(&[&head[..], title].concat(), cut);
            assert_eq!(t.title(), "t", "cut {cut}");
            assert_eq!(t.row_text(0).as_deref().map(str::trim_end), Some("ok"));
            assert!(t.parser_is_ground(), "cut {cut}");
        }
        let mut t = Terminal::new(24, 80);
        t.process(head);
        t.process(b"\x1bc");
        assert!(t.parser_is_ground(), "RIS ends the wrapper");
        t.process(b"\x1b]2;after-ris\x1b\\hi");
        assert_eq!(t.title(), "after-ris");
        assert_eq!(t.row_text(0).as_deref().map(str::trim_end), Some("hi"));
        let mut t = Terminal::new(24, 80);
        t.process(head);
        for _ in 0..3 {
            t.process(b"\x1b]133;A\x07% \x1b]133;B\x07\r\n");
        }
        t.process(b"\x1bP+q544e\x1b\\");
        let response = t.take_response();
        assert!(
            response
                .as_deref()
                .is_some_and(|r| r.starts_with(b"\x1bP1+r544E=")),
            "{response:?}"
        );
        assert!(t.parser_is_ground());
    }

    /// A query torn at the ESC that may begin its ST is still open: the handback's
    /// `CAN` ends it, and it is not answered.
    #[test]
    fn a_handback_does_not_answer_a_query_torn_at_its_escape() {
        for query in [&b"\x1b[1m\x1bP$qm\x1b"[..], b"\x1bP+q544E\x1b"] {
            let mut t = Terminal::new(24, 80);
            t.process(query);
            assert_eq!(t.manual_handback().bytes, vec![0x18], "{query:?}");
            assert_eq!(t.take_response(), None, "{query:?}");
        }
    }

    /// The cells carrying an inline image.
    fn image_cells(t: &Terminal) -> usize {
        let grid = t.grid();
        (0..grid.rows())
            .flat_map(|r| (0..grid.cols()).map(move |c| (r, c)))
            .filter(|&(r, c)| grid.cell_extra(r, c).is_some_and(|e| e.image().is_some()))
            .count()
    }

    /// A sixel wrapped in a tmux passthrough that reached aterm itself is placed
    /// as the bare one is, however the bytes are cut. RED before: its doubled
    /// ST broke it off.
    #[test]
    fn a_sixel_wrapped_for_tmux_is_placed() {
        let sixel = b"\x1bPq#0;2;100;0;0#0~~~~~~-~~~~~~\x1b\\";
        let placed = image_cells(&fed(sixel, 0));
        assert!(placed > 0);
        let bytes = tmux_wrapped(sixel);
        for cut in 0..=bytes.len() {
            assert_eq!(image_cells(&fed(&bytes, cut)), placed, "cut {cut}");
        }
    }

    /// A sixel is placed only at ST: one broken off by an ESC is dropped, as one
    /// CAN or SUB cancels is. Negative control: ST places it, however it is cut.
    #[test]
    fn a_sixel_broken_off_by_an_escape_is_not_placed() {
        let sixel = b"\x1bPq#0;2;100;0;0#0~~~~~~-~~~~~~";
        for brk in [&b"\x1b[5;7H"[..], b"\x1b\x1b\\"] {
            let bytes = [&sixel[..], brk].concat();
            for cut in 0..=bytes.len() {
                assert_eq!(image_cells(&fed(&bytes, cut)), 0, "{brk:?} cut {cut}");
            }
        }
        for st in [&b"\x1b\\"[..], b"\x9c"] {
            let bytes = [&sixel[..], st].concat();
            for cut in 0..=bytes.len() {
                assert!(image_cells(&fed(&bytes, cut)) > 0, "{st:?} cut {cut}");
            }
        }
    }
}
