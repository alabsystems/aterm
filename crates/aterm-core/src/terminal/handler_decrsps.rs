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
#[path = "handler_decrsps_tests.rs"]
mod tests;
