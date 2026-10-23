// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the pinned astream-term models, read off the fold itself.

use astream_term::Record;
use astream_term::screen::{Color, fold};

/// The capabilities, probed once per call (each is a fold of a few bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// EL/ED from a pending wrap clear the parked glyph and reset the wrap (xterm util.c
    /// `ClearRight`: "with the right part cleared, we can't be wrapping").
    pub erase_resets_wrap: bool,
    /// `ED 3` erases saved lines only (xterm `do_erase_display` case 3).
    pub ed3_spares_screen: bool,
    /// A line scrolled in takes the current SGR background (xterm screen.c `ScrnClearLines` fills
    /// with `xtermColorPair`).
    pub bce_on_scroll: bool,
    /// VT and FF are line feeds (xterm `CASE_VMOT`: "form feed, line feed, vertical tab" all
    /// `xtermIndex`).
    pub vt_ff_feed: bool,
    /// IND, NEL and RI (`ESC D`, `ESC E`, `ESC M`).
    pub index_ops: bool,
    /// ICH, DCH, IL, DL and ECH (`CSI @`, `P`, `L`, `M`, `X`).
    pub insert_delete: bool,
    /// DECSC saves, and DECRC restores, the SGR rendition with the position (xterm
    /// `CursorSave`/`CursorRestore` save `flags` and the colours).
    pub decsc_saves_pen: bool,
    /// One alternate buffer that persists across `?47` switches (xterm keeps it; only `?1047`
    /// clears it on leaving and `?1049` on entering).
    pub alt_persists: bool,
    /// RIS keeps that buffer and its DECSC slot, as xterm's full reset does (charproc.c
    /// `ReallyReset`: `FromAlternate(xw, True)` clears the buffer only if it is up, and
    /// `CursorSave` writes the main slot alone).
    pub ris_keeps_alt: bool,
    /// The alternate-screen switches clear as xterm's do: `?1049h` ends in `ClearScreen` even when
    /// the alternate screen is already up, and `?1047l` runs it before leaving; `ClearScreen`
    /// resets the pending wrap (charproc.c `srm_OPT_ALTBUF_CURSOR`, `srm_OPT_ALTBUF`; util.c
    /// `ClearScreen`).
    pub alt_clears: bool,
    /// Malformed UTF-8 becomes one U+FFFD per maximal subpart (Unicode 3.9, the WHATWG decoder,
    /// aterm): `C0 80` is two replacements.
    pub utf8_subparts: bool,
    /// Origin mode (DECOM, `?6`): CUP and VPA count rows from the top margin and stop at the bottom
    /// one, `?6` homes, DECSC saves it (xterm `CursorSet`, `DECSC_FLAGS`).
    pub origin_mode: bool,
    /// Insert mode (IRM, `CSI 4 h`): each glyph shifts the line right first (xterm `WriteText` runs
    /// `InsertChar`).
    pub insert_mode: bool,
    /// SU and SD (`CSI n S`, `CSI n T`): the region scrolls, the cursor and its pending wrap stay
    /// (xterm `xtermScroll`, `RevScroll`).
    pub scroll_up_down: bool,
    /// Newline mode (LNM, `CSI 20 h`): LF, VT and FF return the carriage too (xterm `CASE_VMOT`
    /// under `LINEFEED`).
    pub newline_mode: bool,
    /// G0/G1 designation of ASCII and DEC Special Graphics (`ESC ( 0`, `ESC ) 0`), SO/SI, xterm's
    /// table, and DECSC saving the sets.
    pub charsets: bool,
}

fn line(cols: u16, rows: u16, bytes: &[u8], row: u16) -> String {
    fold(cols, rows, &[Record::Out(bytes.to_vec())]).line_text(row)
}

impl Caps {
    pub fn probe() -> Caps {
        let bce = fold(4, 1, &[Record::Out(b"\x1b[44m\n".to_vec())]);
        let pen = fold(4, 1, &[Record::Out(b"\x1b[31m\x1b7\x1b[0m\x1b8X".to_vec())]);
        let vt = fold(4, 3, &[Record::Out(b"\x0b".to_vec())]);
        Caps {
            erase_resets_wrap: line(2, 1, b"ab\x1b[K", 0) == "a",
            ed3_spares_screen: line(4, 1, b"x\x1b[3J", 0) == "x",
            bce_on_scroll: bce.cell(0, 0).map(|c| c.bg) == Some(Color::Indexed(4)),
            vt_ff_feed: vt.cursor() == (1, 0),
            index_ops: line(4, 2, b"a\x1b[1;1H\x1bM", 1) == "a",
            insert_delete: line(4, 1, b"ab\x1b[1G\x1b[@", 0) == " ab",
            decsc_saves_pen: pen.cell(0, 0).map(|c| c.fg) == Some(Color::Indexed(1)),
            alt_persists: line(4, 1, b"\x1b[?47hX\x1b[?47l\x1b[?47h", 0) == "X",
            ris_keeps_alt: line(4, 1, b"\x1b[?47hX\x1b[?47l\x1bc\x1b[?47h", 0) == "X",
            alt_clears: fold(2, 2, &[Record::Out(b"ab\x1b[?1049h".to_vec())]).cursor() == (0, 1)
                && line(4, 1, b"\x1b[?1049hX\x1b[?1049h", 0).is_empty(),
            utf8_subparts: line(4, 1, b"\xc0\x80", 0) == "\u{fffd}\u{fffd}",
            origin_mode: line(4, 4, b"\x1b[2;3r\x1b[?6h\x1b[1;1HX", 1) == "X",
            insert_mode: line(4, 1, b"ab\x1b[1G\x1b[4hX", 0) == "Xab",
            scroll_up_down: line(4, 2, b"a\x1b[S", 0).is_empty()
                && line(4, 2, b"a\x1b[T", 1) == "a",
            newline_mode: line(4, 2, b"\x1b[20hab\nc", 1) == "c",
            charsets: line(4, 1, b"\x1b(0q", 0) == "\u{2500}",
        }
    }
}
