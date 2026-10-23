// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Empty CSI parameters default to 0 across chunkings (ECMA-48 §5.4.2).

use aterm_core::grid::CellFlags;
use aterm_core::terminal::{CursorStyle, Terminal};

fn chunkings(bytes: &[u8]) -> Vec<Vec<&[u8]>> {
    const EXHAUSTIVE: usize = 10;
    if bytes.len() > EXHAUSTIVE {
        let mut all = vec![vec![bytes], bytes.chunks(1).collect()];
        all.extend((1..bytes.len()).map(|at| {
            let (head, tail) = bytes.split_at(at);
            vec![head, tail]
        }));
        return all;
    }
    (0..1u32 << (bytes.len() - 1))
        .map(|cuts| {
            let mut chunks = Vec::new();
            let mut start = 0;
            for end in 1..=bytes.len() {
                if end == bytes.len() || cuts & (1 << (end - 1)) != 0 {
                    chunks.push(&bytes[start..end]);
                    start = end;
                }
            }
            chunks
        })
        .collect()
}

fn every_chunking(setup: &[u8], seq: &[u8], check: impl Fn(&Terminal) -> Result<(), String>) {
    for chunks in chunkings(seq) {
        let mut term = Terminal::new(24, 80);
        term.process(setup);
        for chunk in &chunks {
            term.process(chunk);
        }
        if let Err(why) = check(&term) {
            panic!(
                "{:?} fed as {chunks:?}: {why}",
                String::from_utf8_lossy(seq)
            );
        }
    }
}

fn flags_at(term: &Terminal, row: u16, col: u16) -> CellFlags {
    term.grid().cell(row, col).expect("cell in range").flags()
}

fn fg_at_origin(term: &Terminal) -> [u8; 3] {
    term.render_row(0).first().expect("a written cell").fg
}

fn fg_after(sgr: &[u8]) -> [u8; 3] {
    let mut term = Terminal::new(24, 80);
    term.process(sgr);
    term.process(b"X");
    fg_at_origin(&term)
}

#[test]
fn a_trailing_empty_sgr_parameter_resets() {
    for seq in [
        &b"\x1b[1;mX"[..],
        b"\x1b[1;31;mX",
        b"\x1b[4:3;mX",
        b"\x1b[7;mX",
        b"\x1b[38;2;10;20;30;mX",
    ] {
        every_chunking(b"", seq, |term| {
            let flags = flags_at(term, 0, 0);
            let fg = fg_at_origin(term);
            if flags.is_empty() && fg == fg_after(b"") {
                Ok(())
            } else {
                Err(format!("X kept {flags:?}, fg {fg:?}"))
            }
        });
    }
    every_chunking(b"", b"\x1b[1mX", |term| {
        flags_at(term, 0, 0)
            .contains(CellFlags::BOLD)
            .then_some(())
            .ok_or_else(|| "bold was lost".to_owned())
    });
}

#[test]
fn count_dependent_handlers_read_a_trailing_empty_parameter_as_the_default() {
    every_chunking(b"\x1b[>4;2m", b"\x1b[>4;m", |term| {
        match term.xterm_keyboard().modify_other_keys() {
            Some(0) => Ok(()),
            other => Err(format!("modifyOtherKeys {other:?}, not reset to 0")),
        }
    });
    every_chunking(b"\x1b[>4;1f", b"\x1b[>4;f", |term| {
        match term.xterm_keyboard().format_other_keys() {
            0 => Ok(()),
            other => Err(format!("formatOtherKeys {other}, not reset to 0")),
        }
    });
    every_chunking(b"", b"\x1b[5;r", |term| {
        let region = term.grid().scroll_region();
        if (region.top, region.bottom) == (4, 23) {
            Ok(())
        } else {
            Err(format!("scroll region {region:?}"))
        }
    });
    every_chunking(b"\x1b[3;9H", b"\x1b[5;H", |term| {
        let cursor = term.cursor();
        if (cursor.row, cursor.col) == (4, 0) {
            Ok(())
        } else {
            Err(format!("cursor at {cursor:?}"))
        }
    });
    every_chunking(b"\x1b[?69h", b"\x1b[5;s", |term| {
        let margins = term.grid().horizontal_margins();
        if (margins.left, margins.right) == (4, 79) {
            Ok(())
        } else {
            Err(format!("margins {margins:?}"))
        }
    });
    every_chunking(b"\x1b[=3u", b"\x1b[=5;u", |term| {
        match term.kitty_keyboard_flags().bits() {
            5 => Ok(()),
            other => Err(format!("kitty flags {other:#b}, not set to 0b101")),
        }
    });
    every_chunking(b"", b"\x1b[4; q", |term| match term.cursor_style() {
        CursorStyle::SteadyUnderline => Ok(()),
        other => Err(format!("cursor style {other:?}")),
    });
    every_chunking(b"\x1b[1mABC\x1b[m", b"\x1b[1;1;1;2;$r", |term| {
        let flags = [0, 1, 2].map(|col| flags_at(term, 0, col));
        if flags[0].is_empty() && flags[1].is_empty() && flags[2].contains(CellFlags::BOLD) {
            Ok(())
        } else {
            Err(format!("cells after DECCARA {flags:?}"))
        }
    });
    every_chunking(b"\x1b[1;31m", b"\x1b[1;#{\x1b[22;32m\x1b[#}X", |term| {
        let bold = flags_at(term, 0, 0).contains(CellFlags::BOLD);
        let fg = fg_at_origin(term);
        if bold && fg == fg_after(b"\x1b[1;32m") {
            Ok(())
        } else {
            Err(format!("bold {bold}, fg {fg:?}"))
        }
    });
}
