// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Standard DECRSPS cursor-information restore (DCS 1 $ t), without drawing or saves.
//!
//! DEC VT510 DECCIR defines the ten fields and pending-wrap bit. Like xterm,
//! restoration uses physical row/column, independently of origin mode.
//! Unsupported extensions, charsets, GR G0 and 96-character G0 reject the whole report.

use std::fmt::Write as _;

use super::TerminalHandler;
use crate::grid::{CellFlags, Cursor};
use aterm_types::charset::{
    CharacterSet, CharacterSet96, CharacterSetState, GlMapping, GrMapping, SingleShift,
};

/// A bounded cursor report, including decimal padding and four SCS designators.
pub(in crate::terminal) const DATA_CAP: usize = 512;

/// The cursor state restored after painting; its charset and complete pen follow separately.
#[derive(Clone, Copy)]
pub(in crate::terminal) struct CursorRestore {
    pub(in crate::terminal) cursor: Cursor,
    pub(in crate::terminal) origin_mode: bool,
    pub(in crate::terminal) pending_wrap: bool,
    pub(in crate::terminal) protected: bool,
}

/// Emit a standard cursor-information restore from the default charset base (GR is G2).
pub(in crate::terminal) fn write_cursor_restore(out: &mut String, state: CursorRestore) {
    let flags =
        char::from(0x40 | u8::from(state.origin_mode) | (u8::from(state.pending_wrap) << 3));
    let protection = if state.protected { 'A' } else { '@' };
    let _ = write!(
        out,
        "\x1bP1$t{};{};1;@;{protection};{flags};0;2;@;BBBB\x1b\\",
        state.cursor.row + 1,
        state.cursor.col + 1
    );
}

#[derive(Clone, Copy, Debug)]
enum Designation {
    Set94(CharacterSet),
    Set96(CharacterSet96),
}

struct Report {
    cursor: Cursor,
    rendition: CellFlags,
    protected: bool,
    origin_mode: bool,
    pending_wrap: bool,
    gl: GlMapping,
    gr: GrMapping,
    single_shift: SingleShift,
    sets: [Designation; 4],
}

fn decimal(bytes: &[u8]) -> Option<u16> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0u16, |n, &byte| {
        byte.is_ascii_digit().then_some(())?;
        n.checked_mul(10)?.checked_add(u16::from(byte - b'0'))
    })
}

fn bits(bytes: &[u8], mask: u8) -> Option<u8> {
    let &[byte] = bytes else {
        return None;
    };
    (byte & !mask == 0x40).then_some(byte & mask)
}

fn designations(mut bytes: &[u8], sizes: u8) -> Option<[Designation; 4]> {
    if sizes & 1 != 0 {
        return None;
    }
    let mut sets = [Designation::Set94(CharacterSet::Ascii); 4];
    for (i, set) in sets.iter_mut().enumerate() {
        let is_96 = sizes & (1 << i) != 0;
        if bytes.starts_with(b"%5") && !is_96 {
            *set = Designation::Set94(CharacterSet::DecSupplemental);
            bytes = &bytes[2..];
        } else {
            let (&byte, rest) = bytes.split_first()?;
            bytes = rest;
            *set = if is_96 {
                Designation::Set96(CharacterSet96::from_final_byte(byte)?)
            } else {
                Designation::Set94(CharacterSet::from_final_byte(byte)?)
            };
        }
    }
    bytes.is_empty().then_some(sets)
}

fn decode(data: &[u8], rows: u16, cols: u16) -> Option<Report> {
    if data.len() > DATA_CAP {
        return None;
    }
    let mut fields = data.split(|&byte| byte == b';');
    let row = decimal(fields.next()?)?;
    let col = decimal(fields.next()?)?;
    if row == 0 || row > rows || col == 0 || col > cols || decimal(fields.next()?)? != 1 {
        return None;
    }
    let rendition_bits = bits(fields.next()?, 0x0f)?;
    let protected = bits(fields.next()?, 1)? != 0;
    let flags = bits(fields.next()?, 0x0f)?;
    let gl = match decimal(fields.next()?)? {
        0 => GlMapping::G0,
        1 => GlMapping::G1,
        2 => GlMapping::G2,
        3 => GlMapping::G3,
        _ => return None,
    };
    // The wire numbers are G-set numbers, not GrMapping's internal zero-based codec.
    let gr = match decimal(fields.next()?)? {
        1 => GrMapping::G1,
        2 => GrMapping::G2,
        3 => GrMapping::G3,
        _ => return None,
    };
    let sizes = bits(fields.next()?, 0x0f)?;
    let sets = designations(fields.next()?, sizes)?;
    if fields.next().is_some() {
        return None;
    }
    let mut rendition = CellFlags::empty();
    for (bit, attribute) in [
        (1, CellFlags::BOLD),
        (2, CellFlags::UNDERLINE),
        (4, CellFlags::BLINK),
        (8, CellFlags::INVERSE),
    ] {
        if rendition_bits & bit != 0 {
            rendition.insert(attribute);
        }
    }
    Some(Report {
        cursor: Cursor::new(row - 1, col - 1),
        rendition,
        protected,
        origin_mode: flags & 1 != 0,
        pending_wrap: flags & 8 != 0,
        gl,
        gr,
        single_shift: if flags & 4 != 0 {
            SingleShift::Ss3
        } else if flags & 2 != 0 {
            SingleShift::Ss2
        } else {
            SingleShift::None
        },
        sets,
    })
}

fn apply_set(charset: &mut CharacterSetState, g: u8, set: Designation) {
    match set {
        Designation::Set94(value) => charset.designate(g, value),
        // A 96-set designation leaves the underlying 94-set as an opaque part of SCS state,
        // just as the ordinary SCS handler does.
        Designation::Set96(value) => charset.designate_96(g, value),
    }
}

impl TerminalHandler<'_> {
    /// Apply a fully validated report only after an uncanceled terminator.
    pub(super) fn handle_decrsps(&mut self) {
        let Some(report) = decode(&self.dcs.data, self.grid.rows(), self.grid.cols()) else {
            return;
        };
        if report.cursor.col >= self.grid.effective_cols_for_row(report.cursor.row) {
            return;
        }
        let [Designation::Set94(g0), g1, g2, g3] = report.sets else {
            return;
        };
        self.style.flags.remove(
            CellFlags::BOLD | CellFlags::ALL_UNDERLINES | CellFlags::BLINK | CellFlags::INVERSE,
        );
        self.style.flags.insert(report.rendition);
        self.style.protected = report.protected;
        self.sgr_style().apply_style_change();
        self.charset.designate(0, g0);
        apply_set(self.charset, 1, g1);
        apply_set(self.charset, 2, g2);
        apply_set(self.charset, 3, g3);
        self.charset.gl = report.gl;
        self.charset.gr = report.gr;
        self.charset.single_shift = report.single_shift;
        self.modes.origin_mode = report.origin_mode;
        self.grid.set_cursor(report.cursor.row, report.cursor.col);
        self.grid.set_pending_wrap(report.pending_wrap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{MAX_DCS_GLOBAL_BUDGET, SavedCursorState, Terminal};
    use aterm_types::{CharacterSetState, TerminalModes};

    #[derive(Debug, PartialEq, Eq)]
    struct Slot {
        cursor: Cursor,
        flags: [bool; 4],
        charset: CharacterSetState,
        pen: String,
        protected: bool,
        underline_color: Option<u32>,
    }

    fn slot(s: &SavedCursorState) -> Slot {
        Slot {
            cursor: s.cursor,
            flags: [s.origin_mode, s.pending_wrap, s.auto_wrap, s.resized],
            charset: s.charset,
            pen: crate::terminal::handler_decrqss::sgr_of(&s.style, s.underline_color),
            protected: s.style.protected,
            underline_color: s.underline_color,
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct View {
        rows: Vec<String>,
        cursor: Cursor,
        pending: bool,
        modes: TerminalModes,
        charset: CharacterSetState,
        pen: String,
        protected: bool,
        region: (aterm_grid::ScrollRegion, aterm_grid::HorizontalMargins),
        saved: [Option<Slot>; 2],
    }

    fn view(t: &Terminal) -> View {
        View {
            rows: (0..t.rows())
                .map(|row| t.grid().row_ansi_text_screen(row).unwrap_or_default())
                .collect(),
            cursor: t.cursor(),
            pending: t.grid().pending_wrap(),
            modes: *t.modes(),
            charset: t.charset,
            pen: crate::terminal::handler_decrqss::sgr_of(
                &t.style,
                t.transient.current_underline_color,
            ),
            protected: t.style.protected,
            region: (t.grid().scroll_region(), t.grid().horizontal_margins()),
            saved: [
                t.cursor_save.main.as_ref().map(slot),
                t.cursor_save.alt.as_ref().map(slot),
            ],
        }
    }

    fn scene() -> Terminal {
        let mut t = Terminal::new(6, 10);
        t.process(b"kept\x1b[3;3H\x1b[31;44;3m\x1b)0\x1b[1\"q\x1b7\x1b[0\"q");
        t
    }

    fn restore(t: &mut Terminal, data: &[u8]) {
        t.process(b"\x1bP1$t");
        t.process(data);
        t.process(b"\x1b\\");
    }

    #[test]
    fn a_public_report_uses_absolute_coordinates_without_drawing_or_saving() {
        let input = b"kept\x1b[?69h\x1b[3;8s\x1b[3;5r\x1b[?6h\x1b[31;44;3m";
        let mut t = Terminal::new(6, 10);
        t.process(input);
        let before = view(&t);
        restore(&mut t, b"1;10;1;@;@;I;0;2;@;BBBB");
        assert_eq!(t.cursor(), Cursor::new(0, 9));
        assert!(t.grid().pending_wrap() && t.modes().origin_mode);
        let after = view(&t);
        assert_eq!(after.rows, before.rows);
        assert_eq!(after.saved, before.saved);
        assert_eq!(after.region, before.region);
        assert_eq!(
            after.pen, before.pen,
            "CIR does not encode colors or italic"
        );
        assert_eq!(t.charset.gr, GrMapping::G2);
        let mut cup = Terminal::new(6, 10);
        cup.process(input);
        cup.process(b"\x1b[1;10H");
        assert_ne!(
            cup.cursor(),
            t.cursor(),
            "CUP is origin-relative; CIR is not"
        );
        t.process(b"X");
        assert_eq!(t.grid().resolved_char(1, 2), Some('X'));
        assert_eq!(t.row_text(0).as_deref(), Some("kept"));
    }

    #[test]
    fn a_public_report_keeps_the_wide_glyph_and_the_existing_save_or_its_absence() {
        for save in [false, true] {
            for (data, pending) in [
                (&b"1;5;1;@;@;H;0;2;@;BBBB"[..], true),
                (&b"1;5;1;@;@;@;0;2;@;BBBB"[..], false),
            ] {
                let mut t = Terminal::new(6, 10);
                if save {
                    t.process(b"\x1b[4;4H\x1b[32m\x1b7\x1b[m");
                }
                t.process("\x1b[1;5H中".as_bytes());
                let before = view(&t);
                restore(&mut t, data);
                assert_eq!(t.cursor(), Cursor::new(0, 4));
                assert_eq!(t.grid().pending_wrap(), pending);
                assert_eq!(view(&t).rows, before.rows);
                assert_eq!(view(&t).saved, before.saved);
                t.process(b"X");
                assert_eq!(t.row_text(1).unwrap_or_default().contains('X'), pending);
                assert_eq!(
                    t.grid().resolved_char(0, 4),
                    Some(if pending { '中' } else { 'X' })
                );
            }
        }
    }

    #[test]
    fn a_public_report_restores_attributes_known_sets_and_single_shifts() {
        let mut t = Terminal::new(6, 10);
        t.process(b"\x1b*0");
        restore(&mut t, b"2;3;1;C;A;F;1;3;D;B0AA");
        assert!(t.style.protected);
        assert!(
            t.style
                .flags
                .contains(CellFlags::BOLD | CellFlags::UNDERLINE)
        );
        assert_eq!(t.charset.gl, GlMapping::G1);
        assert_eq!(t.charset.gr, GrMapping::G3);
        assert_eq!(t.charset.single_shift, SingleShift::Ss3);
        assert_eq!(t.charset.g2, CharacterSet::DecLineDrawing);
        assert_eq!(
            t.charset.designated_96(2),
            Some(CharacterSet96::IsoLatin1Supplemental)
        );
        t.process(b"#q\x1bnA");
        assert_eq!(t.row_text(1).as_deref(), Some("  £─Á"));
        assert!(
            t.grid()
                .cell(1, 2)
                .unwrap()
                .flags()
                .contains(CellFlags::BOLD | CellFlags::UNDERLINE | CellFlags::PROTECTED)
        );
        let mut alias = Terminal::new(6, 10);
        restore(&mut alias, b"1;1;1;@;@;@;0;2;@;BB%5%5");
        assert_eq!(alias.charset.g2, CharacterSet::DecSupplemental);
        assert_eq!(alias.charset.g3, CharacterSet::DecSupplemental);
        restore(&mut alias, b"1;1;1;@;@;B;0;2;@;BB0B");
        alias.process(b"q");
        assert_eq!(alias.grid().resolved_char(0, 0), Some('─'));

        let mut attributes = Terminal::new(6, 10);
        attributes.process(b"\x1b[1;3;4:3;5;7;31;44;58:2::1:2:3m\x1b[1\"q");
        restore(&mut attributes, b"1;1;1;L;@;@;0;2;@;BBBB");
        let mut expected = Terminal::new(6, 10);
        expected.process(b"\x1b[3;5;7;31;44;58:2::1:2:3m");
        assert_eq!(view(&attributes), view(&expected));
        attributes.process(b"X");
        expected.process(b"X");
        assert_eq!(view(&attributes), view(&expected));
        assert!(
            !attributes
                .grid()
                .cell(0, 0)
                .unwrap()
                .flags()
                .contains(CellFlags::PROTECTED)
        );
        restore(&mut attributes, b"1;2;1;@;@;@;0;2;@;BBBB");
        expected.process(b"\x1b[25;27m");
        assert_eq!(view(&attributes), view(&expected));
    }

    #[test]
    fn a_bad_final_field_cannot_leave_a_partly_restored_cursor_or_pen() {
        let bad: &[&[u8]] = &[
            b"0;1;1;@;@;@;0;2;@;BBBB",
            b"7;1;1;@;@;@;0;2;@;BBBB",
            b"1;11;1;@;@;@;0;2;@;BBBB",
            b"65536;1;1;@;@;@;0;2;@;BBBB",
            b"1;1;2;@;@;@;0;2;@;BBBB",
            b"1;1;1;\x60;@;@;0;2;@;BBBB",
            b"1;1;1;@;B;@;0;2;@;BBBB",
            b"1;1;1;@;@;P;0;2;@;BBBB",
            b"1;1;1;@;@;@;4;2;@;BBBB",
            b"1;1;1;@;@;@;0;0;@;BBBB",
            b"1;1;1;@;@;@;0;2;A;BBBB",
            b"1;1;1;C;A;I;0;2;@;BBB?",
            b"1;1;1;C;A;I;0;2;@;BB%5%",
            b"1;1;1;C;A;I;0;2;@;BBBB;trailing",
            b"1;1;1;C;A;I;0;2;@;BBBB!",
        ];
        for data in bad {
            let mut t = scene();
            let before = view(&t);
            restore(&mut t, data);
            assert_eq!(view(&t), before, "{data:?}");
            assert!(t.parser_is_ground());
            assert_eq!(t.take_response(), None);
        }
        for head in [&b"\x1bP0$t"[..], b"\x1bP2$t", b"\x1bP1;1$t"] {
            let mut t = scene();
            let before = view(&t);
            t.process(head);
            t.process(b"1;1;1;C;A;I;0;2;@;BBBB\x1b\\");
            assert_eq!(view(&t), before, "{head:?}");
        }
    }

    const REPORT: &[u8] = b"\x1bP1$t2;5;1;C;A;H;0;2;@;BBBB\x1b\\";

    #[test]
    fn a_restore_applies_only_at_uncanceled_st_at_every_split() {
        let mut reference = scene();
        reference.process(REPORT);
        let expected = view(&reference);
        for cut in 0..=REPORT.len() {
            let mut t = scene();
            let before = view(&t);
            t.process(&REPORT[..cut]);
            if cut < REPORT.len() {
                assert_eq!(view(&t), before, "applied early at {cut}");
            }
            t.process(&REPORT[cut..]);
            assert_eq!(view(&t), expected, "split {cut}");
        }
        for cancel in [0x18, 0x1a] {
            let mut t = scene();
            let before = view(&t);
            t.process(&REPORT[..REPORT.len() - 2]);
            t.process(&[cancel]);
            t.process(b"\x1b\\");
            assert_eq!(view(&t), before, "cancel {cancel}");
            assert!(t.parser_is_ground());
        }
    }

    #[test]
    fn a_hooked_restore_refuses_carry_instead_of_silently_swallowing_mutation() {
        for cut in 0..REPORT.len() {
            let mut t = scene();
            let before = view(&t);
            t.process(&REPORT[..cut]);
            if t.partial_sequence_uncarried().is_some() {
                assert!(t.checkpoint_continuing().is_none(), "cut {cut}");
                assert!(t.checkpoint_visible().is_none(), "cut {cut}");
                assert!(t.checkpoint_pane_step(0).is_none(), "cut {cut}");
                let checkpoint = t.checkpoint_continuing_swallowing();
                let mut replay = Terminal::replay_from_checkpoint(&checkpoint);
                replay.process(&REPORT[cut..]);
                assert_eq!(view(&replay), before, "explicit swallowing at {cut}");
            } else {
                let checkpoint = t.checkpoint_continuing().expect("carried header");
                let mut replay = Terminal::replay_from_checkpoint(&checkpoint);
                replay.process(&REPORT[cut..]);
                let mut reference = scene();
                reference.process(REPORT);
                assert_eq!(view(&replay), view(&reference), "carried header {cut}");
            }
        }
        let mut t = scene();
        let before = view(&t);
        t.process(&REPORT[..REPORT.len() - 2]);
        let handback = t.manual_handback();
        assert_eq!(handback.bytes, vec![0x18]);
        t.process(&handback.bytes);
        assert_eq!(view(&t), before);
        assert!(t.parser_is_ground());
    }

    #[test]
    fn a_dropped_tail_cannot_turn_a_valid_prefix_into_a_mutating_restore() {
        let body = b"2;5;1;C;A;H;0;2;@;BBBB";
        let mut padded = vec![b'0'; DATA_CAP - body.len()];
        padded.extend_from_slice(body);
        for bulk in [false, true] {
            for global in [false, true] {
                for dropped in [false, true] {
                    let mut t = scene();
                    let before = view(&t);
                    t.process(b"\x1bP1$t");
                    let payload = if global { &body[..] } else { &padded[..] };
                    if global {
                        t.dcs.total_bytes = MAX_DCS_GLOBAL_BUDGET - payload.len();
                        t.dcs.sequence_bytes = 0;
                    }
                    if bulk {
                        let mut bytes = payload.to_vec();
                        if dropped {
                            bytes.push(b'!');
                        }
                        t.process(&bytes);
                    } else {
                        for &byte in payload {
                            t.process(&[byte]);
                        }
                        if dropped {
                            t.process(b"!");
                        }
                    }
                    assert!(t.dcs.data.len() <= DATA_CAP);
                    assert!(t.checkpoint_continuing().is_none());
                    t.process(b"\x1b\\");
                    if dropped {
                        assert_eq!(view(&t), before, "bulk={bulk} global={global}");
                    } else {
                        let mut reference = scene();
                        restore(&mut reference, body);
                        assert_eq!(view(&t), view(&reference), "bulk={bulk} global={global}");
                        assert_ne!(view(&t), before);
                    }
                }
            }
        }
    }
}
