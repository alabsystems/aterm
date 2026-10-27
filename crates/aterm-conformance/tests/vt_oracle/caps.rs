// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the pinned astream-term models, read off the fold itself.

use astream_term::Record;
use astream_term::screen::{Color, attr, fold};

/// The capabilities, probed once per call (each is a fold of a few bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// EL/ED from a pending wrap clear the parked glyph and reset the wrap.
    pub erase_resets_wrap: bool,
    /// `ED 3` erases saved lines only (xterm `do_erase_display` case 3).
    pub ed3_spares_screen: bool,
    /// A line scrolled in takes the current SGR background.
    pub bce_on_scroll: bool,
    /// VT and FF are line feeds.
    pub vt_ff_feed: bool,
    /// IND, NEL and RI (`ESC D`, `ESC E`, `ESC M`).
    pub index_ops: bool,
    /// ICH, DCH, IL, DL and ECH (`CSI @`, `P`, `L`, `M`, `X`).
    pub insert_delete: bool,
    /// DECSC saves, and DECRC restores, the SGR rendition with the position.
    pub decsc_saves_pen: bool,
    /// One alternate buffer that persists across `?47` switches.
    pub alt_persists: bool,
    /// RIS keeps that buffer and its DECSC slot, as xterm's full reset does.
    pub ris_keeps_alt: bool,
    /// The alternate-screen switches clear as xterm's do.
    pub alt_clears: bool,
    /// Malformed UTF-8 becomes one U+FFFD per maximal subpart.
    pub utf8_subparts: bool,
    /// Origin mode (DECOM, `?6`).
    pub origin_mode: bool,
    /// Insert mode (IRM, `CSI 4 h`): each glyph shifts the line right first.
    pub insert_mode: bool,
    /// SU and SD (`CSI n S`, `CSI n T`): the region scrolls, the cursor and its pending wrap stay.
    pub scroll_up_down: bool,
    /// Newline mode (LNM, `CSI 20 h`): LF, VT and FF return the carriage too.
    pub newline_mode: bool,
    /// G0/G1 designation of ASCII and DEC Special Graphics (`ESC ( 0`, `ESC ) 0`), SO/SI, and DECSC.
    pub charsets: bool,
    /// DECLRMM (`?69`) and DECSLRM (`CSI Pl ; Pr s`) left/right margins.
    pub lr_margins: bool,
    /// A resize of the width that keeps the cursor's next write: a wrap pending at the edge
    /// takes the next column on a widening, and a cursor whose own cell a narrowing cuts off is
    /// clamped onto the last column with no wrap, as xterm clamps it.
    pub width_resize: bool,
    /// East Asian wide glyphs take a lead cell and a spacer, and no operation leaves half of
    /// one: a wide glyph with one column left wraps and blanks that column.
    pub wide_glyphs: bool,
    /// SGR italic (3/23), blink (5, 6/25), concealed (8/28) and crossed-out (9/29) are
    /// attributes of their own, and 21 is an underline.
    pub sgr_attrs: bool,
    /// 24-bit colour: `38;2;r;g;b` and `48;2;r;g;b`, and the colon forms with and without a
    /// colour-space id, each channel clamped to 255.
    pub truecolour: bool,
}

fn line(cols: u16, rows: u16, bytes: &[u8], row: u16) -> String {
    fold(cols, rows, &[Record::Out(bytes.to_vec())]).line_text(row)
}

/// Row `row` after `before` at `cols`, a resize to `to` columns and `after`.
fn resized(cols: u16, rows: u16, before: &[u8], to: u16, after: &[u8], row: u16) -> String {
    let recs = [
        Record::Out(before.to_vec()),
        Record::Resize { cols: to, rows },
        Record::Out(after.to_vec()),
    ];
    fold(cols, rows, &recs).line_text(row)
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
            lr_margins: line(4, 1, b"\x1b[?69h\x1b[1;2sabc", 0) == "c",
            width_resize: resized(5, 2, b"abcde", 10, b"X", 0) == "abcdeX"
                && resized(10, 2, b"abc\x1b[8G", 5, b"X", 0) == "abc X",
            wide_glyphs: fold(4, 2, &[Record::Out("a\u{4e2d}b\u{6587}".into())]).cursor() == (1, 2)
                && line(4, 1, "\u{4e2d}\u{6587}\x1b[2Gx".as_bytes(), 0) == " x\u{6587}",
            sgr_attrs: fold(4, 1, &[Record::Out(b"\x1b[3;5;8;9mX\x1b[0;21mY".to_vec())])
                .cell(0, 0)
                .map(|c| c.attrs)
                == Some(attr::ITALIC | attr::BLINK | attr::CONCEAL | attr::STRIKE)
                && fold(4, 1, &[Record::Out(b"\x1b[21mY".to_vec())])
                    .cell(0, 0)
                    .map(|c| c.attrs)
                    == Some(attr::UNDERLINE),
            truecolour: fold(
                4,
                1,
                &[Record::Out(b"\x1b[38;2;1;2;300;48:2::4:5:6mX".to_vec())],
            )
            .cell(0, 0)
            .map(|c| (c.fg_rgb, c.bg_rgb))
                == Some((Some((1, 2, 255)), Some((4, 5, 6)))),
        }
    }
}
